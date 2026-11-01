// SPDX-License-Identifier: AGPL-3.0-or-later
//! `nw-testclient gather`: many bots log in with an archetype pick (so they wear its starter kit
//! and, on a test world with the sim's `start_tools`, the T1 logging axe, sickle and pick), walk
//! to the gather nodes of the level near where they appear, stop in reach and gather each: the
//! gather goes in every input (bit 8, as the client sends it) until the server answers (3743)
//! and the gather ends (3746). A refused node (another bot gathers it, or it is depleted) and a
//! node with no answer are left for the next. The nodes are the world file's gatherable points
//! of the tool gatherables a new character may gather ([`GatherSettings::nodes`]).
//!
//! The server keys a node by the object id the gather names and takes the client's word for it;
//! a bot names each node by its index in the node list, so every bot names one node alike.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{Bit8, ClientInputMsg, InputPosition, WorldPosition};
use nw_reflect::REGISTRATION_STATUS_AUTHORIZED;
use nw_sim_core::Ground;
use reqwest::Client;
use tokio::sync::watch;

use crate::fleet::{
    BOTS_CONNECTED, BOTS_REGISTERED, BUNDLES_STEADY, BundleClock, FleetBot, FleetRun,
    FleetSettings, NewBot, NewClient, error_kinds, run_fleet,
};
use crate::hunt::{planar, send_input, walk_step};
use crate::pair::{Check, SEND_INTERVAL};
use crate::rep::{RepClient, RepError};
use crate::udp::RepOutcome;
use crate::view::{CombatView, InteractionAnswer};

/// Check: every server message decoded.
pub const MESSAGES_READ: &str = "every server message was read";
/// Check: every bot had gather nodes within its search radius.
pub const BOTS_FOUND_NODES: &str = "every bot found gather nodes near it";
/// Check: every bot finished at least one gather (its 3746).
pub const BOTS_GATHERED: &str = "every bot gathered";
/// Check: the server answered at least this share of the gathers the bots asked for.
pub const GATHERS_ANSWERED: &str = "the server answered the gathers";
/// Share of asked gathers that must have an answer (3743) in time.
pub const MIN_ANSWERED_SHARE: f64 = 0.95;
/// The status of a taken gather (3743).
const ACCEPTED: u8 = 1;
/// The marker of a bot's node object ids, in the high half of their facet entity: `GATH`.
const NODE_ID_MARKER: u64 = 0x4741_5448_0000_0000;
/// A bot with no node within its search radius walks to this many of the nearest.
const FAR_NODES: usize = 10;
/// Interaction ids of bot `n` start at `n` times this.
const INTERACTIONS_PER_BOT: u64 = 1 << 32;

/// One node a bot may gather: its gatherable and place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GatherNode {
    /// The CRC-32 of the lowercase `GatherableID`.
    pub gatherable: u32,
    /// East, north and up, in metres.
    pub at: [f32; 3],
}

/// What the gather run needs besides the clients and the ground.
#[derive(Debug, Clone, PartialEq)]
pub struct GatherSettings {
    /// The bots and how they log in.
    pub fleet: FleetSettings,
    /// Every node of the level the bots may gather, in world file order.
    pub nodes: Arc<[GatherNode]>,
    /// A bot gathers the nodes within this distance of where it appeared, in metres.
    pub search_m: f32,
    /// Walking speed, in metres per second.
    pub speed_mps: f32,
    /// A bot gathers a node within this distance, in metres.
    pub reach_m: f32,
    /// Longest wait for the server's answer to a gather.
    pub answer_wait: Duration,
    /// Longest wait for the end of a taken gather.
    pub gather_wait: Duration,
    /// Longest gap between two own bundles.
    pub max_bundle_gap: Duration,
}

/// Where a bot is with its node.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Picking its next node.
    Choosing,
    /// Walking to node `node`.
    Walking { node: usize },
    /// Asked to gather node `node` with interaction `interaction` at `since`.
    Asking {
        node: usize,
        interaction: u64,
        since: Duration,
    },
    /// Gathering node `node` since `since`.
    Gathering {
        node: usize,
        interaction: u64,
        since: Duration,
    },
}

/// What a bot does on one tick.
#[derive(Debug, Clone, PartialEq)]
pub enum GatherAction {
    /// A position input.
    Move(InputPosition),
    /// A position input with the gather of `node` in it.
    Gather {
        /// Where the bot stands.
        at: InputPosition,
        /// The node's index in the node list.
        node: usize,
        /// The interaction id.
        interaction: u64,
    },
}

/// The counts of one bot's gathers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GatherCounts {
    /// Gathers asked for (a new interaction each).
    pub asked: u64,
    /// Gathers the server took (3743 status 1).
    pub accepted: u64,
    /// Gathers the server refused (3743, another status).
    pub refused: u64,
    /// Gathers that ended (3746).
    pub gathered: u64,
    /// Gathers with no answer in [`GatherSettings::answer_wait`].
    pub unanswered: u64,
    /// Taken gathers with no end in [`GatherSettings::gather_wait`].
    pub unfinished: u64,
}

/// The decisions of one bot.
#[derive(Debug, Clone)]
pub struct Gatherer {
    settings: Arc<GatherSettings>,
    n: usize,
    /// The nodes near where it appeared, nearest first, from a place on the list that depends
    /// on the bot (so bots of one start spread over the nodes); `None` before its first place.
    route: Option<Vec<usize>>,
    /// Its next node on the route.
    next: usize,
    /// The nodes it gathered, or left after a refusal or no answer.
    done: BTreeSet<usize>,
    phase: Phase,
    last_sent: Option<Duration>,
    server_place: Option<WorldPosition>,
    here: Option<[f32; 3]>,
    interactions: u64,
    counts: GatherCounts,
    /// Its finished gathers by gatherable (CRC-32 of the lowercase `GatherableID`).
    kinds: BTreeMap<u32, u64>,
    /// Its refused gathers by gatherable.
    refused_kinds: BTreeMap<u32, u64>,
}

impl Gatherer {
    /// Bot `n` of the run.
    pub fn new(settings: Arc<GatherSettings>, n: usize) -> Self {
        Self {
            settings,
            n,
            route: None,
            next: 0,
            done: BTreeSet::new(),
            phase: Phase::Choosing,
            last_sent: None,
            server_place: None,
            here: None,
            interactions: 0,
            counts: GatherCounts::default(),
            kinds: BTreeMap::new(),
            refused_kinds: BTreeMap::new(),
        }
    }

    /// What to do `since` the registration, with the last own place the server sent `own` and
    /// the server's answers to the bot's interactions since the last step.
    pub fn step(
        &mut self,
        own: WorldPosition,
        answers: &[InteractionAnswer],
        since: Duration,
        ground: &(dyn Ground + Send + Sync),
    ) -> Option<GatherAction> {
        if self.server_place != Some(own) {
            self.server_place = Some(own);
            self.here = Some([own.x, own.y, own.z()]);
        }
        if self.route.is_none() {
            self.plan(own);
        }
        for answer in answers {
            self.answered(*answer, since);
        }
        self.time_out(since);
        if self
            .last_sent
            .is_some_and(|sent| since.saturating_sub(sent) < SEND_INTERVAL)
        {
            return None;
        }
        let here = self.here?;
        if self.phase == Phase::Choosing {
            self.phase = Phase::Walking { node: self.pick()? };
        }
        let at = InputPosition::from_metres(here[0], here[1], here[2])?;
        match self.phase {
            Phase::Choosing => None,
            Phase::Walking { node } => {
                let target = self.settings.nodes[node].at;
                let elapsed = self
                    .last_sent
                    .map_or(SEND_INTERVAL, |sent| since.saturating_sub(sent));
                let reach_m = self.settings.reach_m;
                if planar(here, [target[0], target[1]]) <= reach_m {
                    self.interactions += 1;
                    let interaction = self.n as u64 * INTERACTIONS_PER_BOT + self.interactions;
                    self.counts.asked += 1;
                    self.phase = Phase::Asking {
                        node,
                        interaction,
                        since,
                    };
                    self.last_sent = Some(since);
                    return Some(GatherAction::Gather {
                        at,
                        node,
                        interaction,
                    });
                }
                let (next, place) = walk_step(
                    here,
                    [target[0], target[1]],
                    reach_m / 2.0,
                    self.settings.speed_mps,
                    elapsed,
                    ground,
                )?;
                self.here = Some(place);
                self.last_sent = Some(since);
                Some(GatherAction::Move(next))
            }
            Phase::Asking {
                node, interaction, ..
            }
            | Phase::Gathering {
                node, interaction, ..
            } => {
                self.last_sent = Some(since);
                Some(GatherAction::Gather {
                    at,
                    node,
                    interaction,
                })
            }
        }
    }

    /// The nodes within the search radius of the first place `own` (the [`FAR_NODES`] nearest
    /// for a bot with none within it), nearest first, from the bot's own place on that list.
    fn plan(&mut self, own: WorldPosition) {
        let here = [own.x, own.y, own.z()];
        let mut all: Vec<(usize, f32)> = self
            .settings
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (index, planar(here, [node.at[0], node.at[1]])))
            .collect();
        all.sort_by(|a, b| a.1.total_cmp(&b.1));
        let within = all
            .iter()
            .take_while(|(_, distance)| *distance <= self.settings.search_m)
            .count();
        let mut route: Vec<usize> = all
            .into_iter()
            .take(if within == 0 { FAR_NODES } else { within })
            .map(|(index, _)| index)
            .collect();
        if !route.is_empty() {
            let shift = self.n % route.len();
            route.rotate_left(shift);
        }
        self.route = Some(route);
    }

    /// The next node on the route that the bot has not done; `None` when it did them all.
    fn pick(&mut self) -> Option<usize> {
        let route = self.route.as_ref()?;
        let count = route.len();
        (0..count)
            .map(|step| route[(self.next + step) % count])
            .find(|node| !self.done.contains(node))
            .inspect(|node| {
                self.next = route
                    .iter()
                    .position(|own| own == node)
                    .map_or(0, |at| at + 1);
            })
    }

    /// Takes the server's `answer` at `since`.
    fn answered(&mut self, answer: InteractionAnswer, since: Duration) {
        match (self.phase, answer) {
            (
                Phase::Asking {
                    node, interaction, ..
                },
                InteractionAnswer::Validated {
                    interaction: answered,
                    status,
                },
            ) if answered == interaction => {
                if status == ACCEPTED {
                    self.counts.accepted += 1;
                    self.phase = Phase::Gathering {
                        node,
                        interaction,
                        since,
                    };
                } else {
                    self.counts.refused += 1;
                    *self
                        .refused_kinds
                        .entry(self.settings.nodes[node].gatherable)
                        .or_default() += 1;
                    self.leave(node);
                }
            }
            (Phase::Gathering { node, .. }, InteractionAnswer::Ended) => {
                self.counts.gathered += 1;
                *self
                    .kinds
                    .entry(self.settings.nodes[node].gatherable)
                    .or_default() += 1;
                self.leave(node);
            }
            _ => {}
        }
    }

    /// A gather with no answer, or no end, in time is given up.
    fn time_out(&mut self, since: Duration) {
        match self.phase {
            Phase::Asking {
                node, since: at, ..
            } if since.saturating_sub(at) > self.settings.answer_wait => {
                self.counts.unanswered += 1;
                self.leave(node);
            }
            Phase::Gathering {
                node, since: at, ..
            } if since.saturating_sub(at) > self.settings.gather_wait => {
                self.counts.unfinished += 1;
                self.leave(node);
            }
            _ => {}
        }
    }

    fn leave(&mut self, node: usize) {
        self.done.insert(node);
        self.phase = Phase::Choosing;
    }

    /// The nodes near where the bot appeared; 0 before its first place.
    pub fn nodes_near(&self) -> usize {
        self.route.as_ref().map_or(0, Vec::len)
    }

    /// The counts of its gathers.
    pub const fn counts(&self) -> GatherCounts {
        self.counts
    }

    /// Its finished gathers by gatherable.
    pub fn kinds(&self) -> &BTreeMap<u32, u64> {
        &self.kinds
    }

    /// Its refused gathers by gatherable.
    pub fn refused_kinds(&self) -> &BTreeMap<u32, u64> {
        &self.refused_kinds
    }
}

/// The object id a bot names node `node` by, and its facet entity.
pub fn node_id(node: usize) -> ([u8; RAW_ID_LEN], u64) {
    let entity = NODE_ID_MARKER | node as u64;
    let mut target = [0; RAW_ID_LEN];
    target[..8].copy_from_slice(&entity.to_le_bytes());
    (target, entity)
}

/// What one bot saw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GatherBotReport {
    /// Registration status 0.
    pub registered: bool,
    /// The server closed the connection.
    pub closed: bool,
    /// Longest gap between two own bundles.
    pub max_gap: Duration,
    /// The nodes near where it appeared.
    pub nodes_near: usize,
    /// Its gathers.
    pub counts: GatherCounts,
    /// Its finished gathers by gatherable (CRC-32 of the lowercase `GatherableID`).
    pub kinds: BTreeMap<u32, u64>,
    /// Its refused gathers by gatherable.
    pub refused_kinds: BTreeMap<u32, u64>,
    /// Messages that did not decode.
    pub unread_messages: u64,
    /// Why the first of them did not decode.
    pub first_unread: Option<String>,
    /// Why the bot could not run, if it could not.
    pub error: Option<String>,
}

/// Every bot and the sim totals of the run.
pub type GatherReport = FleetRun<GatherBotReport>;

impl GatherReport {
    /// Every check, in order.
    pub fn checks(&self, settings: &GatherSettings) -> Vec<Check> {
        let all = |rule: &dyn Fn(&GatherBotReport) -> bool| {
            !self.bots.is_empty() && self.bots.iter().all(rule)
        };
        let total = self.total();
        #[allow(clippy::cast_precision_loss, reason = "counts of gathers")]
        let answered = (total.accepted + total.refused) as f64 / total.asked.max(1) as f64;
        let mut checks = vec![
            Check {
                name: BOTS_REGISTERED,
                passed: self.bots.len() == settings.fleet.bots && all(&|bot| bot.registered),
            },
            Check {
                name: BOTS_CONNECTED,
                passed: all(&|bot| !bot.closed),
            },
            Check {
                name: MESSAGES_READ,
                passed: all(&|bot| bot.unread_messages == 0),
            },
            Check {
                name: BOTS_FOUND_NODES,
                passed: all(&|bot| bot.nodes_near > 0),
            },
            Check {
                name: BOTS_GATHERED,
                passed: all(&|bot| bot.counts.gathered > 0),
            },
            Check {
                name: GATHERS_ANSWERED,
                passed: total.asked > 0 && answered >= MIN_ANSWERED_SHARE,
            },
            Check {
                name: BUNDLES_STEADY,
                passed: all(&|bot| bot.max_gap <= settings.max_bundle_gap),
            },
        ];
        checks.extend(
            self.sims
                .checks(self.unread_metrics, &settings.fleet.metrics_urls),
        );
        checks
    }

    /// The gathers of every bot together.
    pub fn total(&self) -> GatherCounts {
        self.bots
            .iter()
            .fold(GatherCounts::default(), |sum, bot| GatherCounts {
                asked: sum.asked + bot.counts.asked,
                accepted: sum.accepted + bot.counts.accepted,
                refused: sum.refused + bot.counts.refused,
                gathered: sum.gathered + bot.counts.gathered,
                unanswered: sum.unanswered + bot.counts.unanswered,
                unfinished: sum.unfinished + bot.counts.unfinished,
            })
    }

    /// The finished gathers of every bot together, by gatherable.
    pub fn kinds(&self) -> BTreeMap<u32, u64> {
        sum_kinds(self.bots.iter().map(|bot| &bot.kinds))
    }

    /// The refused gathers of every bot together, by gatherable.
    pub fn refused_kinds(&self) -> BTreeMap<u32, u64> {
        sum_kinds(self.bots.iter().map(|bot| &bot.refused_kinds))
    }

    /// Whether every check passed.
    pub fn passed(&self, settings: &GatherSettings) -> bool {
        self.checks(settings).iter().all(|check| check.passed)
    }

    /// How many bots stopped with each error, by error text.
    pub fn error_kinds(&self) -> std::collections::BTreeMap<&str, usize> {
        error_kinds(self.bots.iter().filter_map(|bot| bot.error.as_deref()))
    }

    /// One line of numbers for the operator.
    pub fn summary(&self) -> String {
        let total = self.total();
        let errors = self.bots.iter().filter(|bot| bot.error.is_some()).count();
        let no_nodes = self.bots.iter().filter(|bot| bot.nodes_near == 0).count();
        let no_gather = self
            .bots
            .iter()
            .filter(|bot| bot.counts.gathered == 0)
            .count();
        let worst_gap = self.bots.iter().map(|bot| bot.max_gap).max();
        let first_unread = self
            .bots
            .iter()
            .find_map(|bot| bot.first_unread.as_deref())
            .unwrap_or("none");
        format!(
            "bots {}, failed to start {errors}, without nodes {no_nodes}, without a gather \
             {no_gather}, asked {}, taken {}, refused {}, gathered {}, no answer {}, no end {}, \
             worst bundle gap {} ms, messages not read {} (first: {first_unread})",
            self.bots.len(),
            total.asked,
            total.accepted,
            total.refused,
            total.gathered,
            total.unanswered,
            total.unfinished,
            worst_gap.unwrap_or_default().as_millis(),
            self.bots.iter().map(|bot| bot.unread_messages).sum::<u64>(),
        )
    }
}

/// The counts of `each` added up, by gatherable.
fn sum_kinds<'a>(each: impl Iterator<Item = &'a BTreeMap<u32, u64>>) -> BTreeMap<u32, u64> {
    let mut kinds = BTreeMap::new();
    for (gatherable, count) in each.flatten() {
        *kinds.entry(*gatherable).or_default() += count;
    }
    kinds
}

/// One bot during its run.
struct GatherBot {
    gatherer: Gatherer,
    ground: Arc<dyn Ground + Send + Sync>,
    view: CombatView,
    own: Option<WorldPosition>,
    clock: BundleClock,
    unread_messages: u64,
    first_unread: Option<String>,
}

impl FleetBot for GatherBot {
    type Report = GatherBotReport;

    fn tick(
        &mut self,
        client: &mut RepClient,
        outcome: &mut RepOutcome,
        since: Duration,
    ) -> Result<(), RepError> {
        let bundles_before = self.view.bundles();
        for received in outcome.messages.drain(..) {
            match self.view.read(&received) {
                Ok(Some(position)) => self.own = Some(position),
                Ok(None) => {}
                Err(error) => {
                    self.unread_messages += 1;
                    self.first_unread.get_or_insert_with(|| error.to_string());
                }
            }
        }
        let bundles = self.view.bundles() - bundles_before;
        self.clock
            .saw(usize::try_from(bundles).unwrap_or(usize::MAX), since);
        let Some(own) = self.own else {
            return Ok(());
        };
        let answers = self.view.take_interactions();
        match self.gatherer.step(own, &answers, since, &*self.ground) {
            None => Ok(()),
            Some(GatherAction::Move(position)) => {
                send_input(client, &ClientInputMsg::position(position))
            }
            Some(GatherAction::Gather {
                at,
                node,
                interaction,
            }) => {
                let (target, entity) = node_id(node);
                let gatherable = self.gatherer.settings.nodes[node].gatherable;
                let body = Bit8::gather(target, entity, interaction, gatherable)?;
                send_input(client, &ClientInputMsg::position(at).interacting(body))
            }
        }
    }

    fn report(self, outcome: &RepOutcome) -> GatherBotReport {
        GatherBotReport {
            registered: outcome.status == Some(REGISTRATION_STATUS_AUTHORIZED),
            closed: outcome.closed_by_server,
            max_gap: self.clock.max_gap(outcome.held),
            nodes_near: self.gatherer.nodes_near(),
            counts: self.gatherer.counts(),
            kinds: self.gatherer.kinds().clone(),
            refused_kinds: self.gatherer.refused_kinds().clone(),
            unread_messages: self.unread_messages,
            first_unread: self.first_unread,
            error: None,
        }
    }

    fn failed(error: String) -> GatherBotReport {
        GatherBotReport {
            error: Some(error),
            ..GatherBotReport::default()
        }
    }
}

/// Runs every bot and reads the sim metrics before and after.
pub async fn run_gather(
    http: &Client,
    settings: Arc<GatherSettings>,
    ground: Arc<dyn Ground + Send + Sync>,
    new_client: NewClient,
    stop: watch::Receiver<bool>,
    report: impl FnMut(String),
) -> GatherReport {
    let fleet = Arc::new(settings.fleet.clone());
    let new_bot: NewBot<GatherBot> = Arc::new(move |n| GatherBot {
        gatherer: Gatherer::new(Arc::clone(&settings), n),
        ground: Arc::clone(&ground),
        view: CombatView::default(),
        own: None,
        clock: BundleClock::new(),
        unread_messages: 0,
        first_unread: None,
    });
    run_fleet(http, fleet, new_client, new_bot, stop, report).await
}

#[cfg(test)]
mod tests {
    use nw_sim_core::test_support::NoGround;

    use super::*;
    use crate::fleet::test_support::test_fleet;

    const ORIGIN: f32 = 1000.0;
    const HEIGHT: f32 = 50.0;
    const SPEED_MPS: f32 = 4.0;
    const REACH_M: f32 = 2.0;
    const LOGGING_TREE: u32 = 0x1111_1111;
    const BUSH: u32 = 0x2222_2222;
    const ANSWER_WAIT: Duration = Duration::from_secs(5);
    const GATHER_WAIT: Duration = Duration::from_secs(30);

    fn settings() -> Arc<GatherSettings> {
        Arc::new(GatherSettings {
            fleet: test_fleet(Duration::from_secs(120)),
            nodes: Arc::from([
                GatherNode {
                    gatherable: LOGGING_TREE,
                    at: [ORIGIN + 10.0, ORIGIN, HEIGHT],
                },
                GatherNode {
                    gatherable: BUSH,
                    at: [ORIGIN + 20.0, ORIGIN, HEIGHT],
                },
                GatherNode {
                    gatherable: BUSH,
                    at: [ORIGIN + 500.0, ORIGIN, HEIGHT],
                },
            ]),
            search_m: 60.0,
            speed_mps: SPEED_MPS,
            reach_m: REACH_M,
            answer_wait: ANSWER_WAIT,
            gather_wait: GATHER_WAIT,
            max_bundle_gap: Duration::from_millis(250),
        })
    }

    fn at(x: f32) -> WorldPosition {
        WorldPosition::from_metres(x, ORIGIN, HEIGHT).expect("finite")
    }

    /// Steps the bot every send interval from `from` until it asks to gather; the time and the
    /// asked node and interaction.
    fn walk_to_gather(
        bot: &mut Gatherer,
        own: WorldPosition,
        from: Duration,
    ) -> (Duration, usize, u64) {
        let mut since = from;
        for _ in 0..1000 {
            since += SEND_INTERVAL;
            if let Some(GatherAction::Gather {
                node, interaction, ..
            }) = bot.step(own, &[], since, &NoGround)
            {
                return (since, node, interaction);
            }
        }
        panic!("the bot never gathered");
    }

    #[test]
    fn a_bot_walks_to_its_nearest_node_gathers_it_and_goes_on() {
        let mut bot = Gatherer::new(settings(), 0);
        let own = at(ORIGIN);
        let (since, node, interaction) = walk_to_gather(&mut bot, own, Duration::ZERO);
        assert_eq!(node, 0, "the nearest node first");
        assert_eq!(bot.nodes_near(), 2, "the far node is out of its search");
        let mut far = Gatherer::new(settings(), 0);
        far.step(at(ORIGIN - 2_000.0), &[], SEND_INTERVAL, &NoGround);
        assert_eq!(
            far.nodes_near(),
            3,
            "a bot far from every node takes the nearest"
        );
        let taken = InteractionAnswer::Validated {
            interaction,
            status: ACCEPTED,
        };
        let held = bot.step(own, &[taken], since + SEND_INTERVAL, &NoGround);
        assert!(
            matches!(held, Some(GatherAction::Gather { node: 0, .. })),
            "the gather stays in the input while it runs: {held:?}"
        );
        bot.step(
            own,
            &[InteractionAnswer::Ended],
            since + SEND_INTERVAL * 2,
            &NoGround,
        );
        let (_, next, again) = walk_to_gather(&mut bot, own, since + SEND_INTERVAL * 2);
        assert_eq!(next, 1, "then the next node");
        assert_ne!(again, interaction, "a new interaction");
        let counts = bot.counts();
        assert_eq!((counts.asked, counts.accepted, counts.gathered), (2, 1, 1));
        assert_eq!(bot.kinds(), &BTreeMap::from([(LOGGING_TREE, 1)]));
    }

    #[test]
    fn a_refused_or_silent_gather_leaves_the_node() {
        let mut bot = Gatherer::new(settings(), 0);
        let own = at(ORIGIN);
        let (since, _, interaction) = walk_to_gather(&mut bot, own, Duration::ZERO);
        let busy = InteractionAnswer::Validated {
            interaction,
            status: 2,
        };
        bot.step(own, &[busy], since + SEND_INTERVAL, &NoGround);
        let (since, node, _) = walk_to_gather(&mut bot, own, since + SEND_INTERVAL);
        assert_eq!(node, 1, "the refused node is left");
        bot.step(own, &[], since + ANSWER_WAIT * 2, &NoGround);
        let counts = bot.counts();
        assert_eq!((counts.refused, counts.unanswered), (1, 1));
        assert_eq!(
            bot.step(own, &[], since + ANSWER_WAIT * 3, &NoGround),
            None,
            "every near node is done"
        );
    }

    #[test]
    fn bots_of_one_start_begin_at_other_nodes() {
        let first = Gatherer::new(settings(), 0);
        let mut second = Gatherer::new(settings(), 1);
        let (_, node, interaction) = walk_to_gather(&mut second, at(ORIGIN), Duration::ZERO);
        assert_eq!(node, 1, "the second bot starts one node on");
        assert_eq!(interaction >> 32, 1, "its own interaction ids");
        assert_eq!(first.nodes_near(), 0, "no place yet");
    }

    #[test]
    fn node_ids_are_the_same_for_every_bot_and_apart_per_node() {
        assert_eq!(node_id(3), node_id(3));
        assert_ne!(node_id(3).0, node_id(4).0);
        assert_eq!(node_id(3).1, NODE_ID_MARKER | 3);
    }
}
