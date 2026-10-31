// SPDX-License-Identifier: AGPL-3.0-or-later
//! `nw-testclient hunt` (M6-B spec section 9): many bots log in, walk to the nearest mob they
//! see (or, with none in view, to the hunt start nearest where they appeared), stop in reach and
//! attack it with rising edges of attack bit 9, looking at it. A bot that gets a 4299 waits, asks
//! to respawn with a 1938, and lives again when the server places it at a respawn point. The
//! server sends a player its own place only when it moves the player (retail own states), so a
//! bot is where it last sent itself, and goes on from each place the server sends. The checks
//! read what each bot saw, and the mob deaths, refused moves and tick times from the metrics of
//! every sim.
//!
//! The world director puts a new character at its `start_positions` (in turn, by entity
//! number), a known one at its saved spot. Run the hunt with new hunt characters on a director
//! whose `start_positions` are the hunt starts (`NW_DIRECTOR_START_POSITIONS`); a bot that
//! appears farther than [`START_RANGE_M`] from every start fails [`BOTS_NEAR_START`].

use std::sync::Arc;
use std::time::Duration;

use nw_messages::{
    BodyCodec, ClientInputMsg, InputPosition, RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX,
    REQUEST_RESPAWN_TYPE_INDEX, RequestRespawnMsg, WorldPosition,
};
use nw_reflect::REGISTRATION_STATUS_AUTHORIZED;
use nw_sim_core::{Ground, Position, yaw_toward};
use reqwest::Client;

use crate::fleet::{
    BOTS_CONNECTED, BOTS_REGISTERED, BUNDLES_STEADY, BundleClock, FleetBot, FleetRun,
    FleetSettings, NewBot, NewClient, error_kinds, ratio, run_fleet,
};
use crate::pair::{Check, INPUT_CHANNEL, SEND_INTERVAL};
use crate::rep::{RepClient, RepError};
use crate::udp::RepOutcome;
use crate::view::CombatView;
use tokio::sync::watch;

/// Check: every bot got at least one 2071.
pub const BOTS_HIT: &str = "every bot hit a mob";
/// Check: the sims counted at least one mob death per [`BOTS_PER_MOB_DEATH`] bots.
pub const MOBS_DIED: &str = "mobs died";
/// Check: every bot that asked to respawn lived again.
pub const BOTS_LIVED_AGAIN: &str = "every dead bot lived again after its request";
/// Check: every message of the server decoded.
pub const MESSAGES_READ: &str = "every server message was read";
/// Check: every bot appeared within [`START_RANGE_M`] of a hunt start.
pub const BOTS_NEAR_START: &str = "every bot began near a hunt start";
/// A bot must appear this near a hunt start to find its mobs: the starts were picked with no
/// stronger spawn within 70 m, sight plus leash (M6-B record, section 4).
pub const START_RANGE_M: f32 = 70.0;
/// Bots per mob death that the run must reach (M6-B spec section 9.3).
pub const BOTS_PER_MOB_DEATH: f64 = 10.0;
/// A bot stops this share of its reach from its mob.
const STOP_SHARE: f32 = 0.5;
/// Swings at one mob without a hit after which a bot leaves it: a mob that walks home after
/// its fight takes no hit, and the bot does not see that.
pub const MAX_SWINGS_WITHOUT_HIT: u32 = 5;
/// How long a bot leaves such a mob before it may pick it again.
pub const LEAVE_MOB: Duration = Duration::from_secs(15);
/// Bot homes lie on a ring this far around their start, so that the bots of one start do not
/// stand on one point.
pub const START_SPREAD_M: f32 = 8.0;
/// The golden angle, in radians: homes in turn on the ring stay far apart for any count.
const GOLDEN_ANGLE: f32 = 2.399_963;
/// Most bots per start: each player sees its 47 nearest entities, bots and mobs together, and a
/// hunt start has up to 17 mobs (M6-B spec section 14, amendment 5).
pub const MAX_BOTS_PER_START: usize = 30;
/// A request older than this at the end of the run must have been answered.
pub const RESPAWN_PROOF_WAIT: Duration = Duration::from_secs(3);

/// What the hunt needs besides the clients and the ground.
#[derive(Debug, Clone, PartialEq)]
pub struct HuntSettings {
    /// The bots and how they log in.
    pub fleet: FleetSettings,
    /// The hunt starts, east and north, in metres; a bot that sees no mob goes to its home
    /// around the start nearest where it appeared.
    pub starts: Vec<[f32; 2]>,
    /// Walking speed, in metres per second.
    pub speed_mps: f32,
    /// A bot attacks a mob within this distance, in metres.
    pub reach_m: f32,
    /// Time between two attacks of a bot.
    pub attack_interval: Duration,
    /// Time from a 4299 to the bot's 1938.
    pub respawn_wait: Duration,
    /// Longest gap between two own bundles.
    pub max_bundle_gap: Duration,
}

/// What a bot does on one tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum HuntAction {
    /// A position input.
    Move(InputPosition),
    /// An attack input at the position, looking at the mob with the yaw: the swing points
    /// where the client looks.
    Attack {
        /// Where the bot stands.
        at: InputPosition,
        /// Where it looks: the yaw toward the mob.
        yaw: f32,
    },
    /// A respawn request.
    Respawn,
}

/// Where a bot is in its life.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Hunting,
    /// Dead since a time.
    Dead {
        since: Duration,
    },
    /// Asked to respawn at a time; lives again at the next place the server sends.
    Asked {
        at_time: Duration,
    },
}

/// The decisions of one bot.
#[derive(Debug, Clone)]
pub struct Hunter {
    settings: Arc<HuntSettings>,
    /// The bot's angle on the ring around its start.
    ring_angle: f32,
    /// Where the bot waits while it sees no mob: a point on the ring around the start nearest
    /// its first position; set at the first position.
    home: Option<[f32; 2]>,
    /// How far the first position was from that start, in metres.
    start_distance_m: Option<f32>,
    /// The index of that start in the hunt starts.
    start: Option<usize>,
    phase: Phase,
    deaths_seen: u64,
    last_sent: Option<Duration>,
    last_attack: Option<Duration>,
    /// The last own place the server sent; a new one means the server placed the bot (its
    /// spawn, a respawn, a correction).
    server_place: Option<WorldPosition>,
    /// Where the bot is: the last position it sent, or the server's newer place.
    here: Option<[f32; 3]>,
    lived_again: u64,
    /// The bot's 2071 count so far.
    hits_seen: u64,
    /// The mob it swings at, and its swings at it since its last hit.
    swinging_at: Option<(u64, u32)>,
    /// The mobs it left, until when.
    left: std::collections::BTreeMap<u64, Duration>,
}

impl Hunter {
    /// Bot `n` of the hunt.
    pub fn new(settings: Arc<HuntSettings>, n: usize) -> Self {
        Self {
            settings,
            ring_angle: n as f32 * GOLDEN_ANGLE,
            home: None,
            start_distance_m: None,
            start: None,
            phase: Phase::Hunting,
            deaths_seen: 0,
            last_sent: None,
            last_attack: None,
            server_place: None,
            here: None,
            lived_again: 0,
            hits_seen: 0,
            swinging_at: None,
            left: std::collections::BTreeMap::new(),
        }
    }

    /// What to do `since` the registration, with the last own place the server sent `own`, the
    /// mobs in view and the 4299 count so far.
    pub fn step(
        &mut self,
        own: WorldPosition,
        mobs: &[(u64, WorldPosition)],
        deaths: u64,
        since: Duration,
        ground: &(dyn Ground + Send + Sync),
    ) -> Option<HuntAction> {
        if self.server_place != Some(own) {
            let placed_again = self.server_place.is_some();
            self.server_place = Some(own);
            self.here = Some([own.x, own.y, own.z()]);
            if placed_again && matches!(self.phase, Phase::Asked { .. }) {
                self.lived_again += 1;
                self.phase = Phase::Hunting;
                return None;
            }
        }
        if self.home.is_none() {
            self.settle(own);
        }
        if deaths > self.deaths_seen {
            self.deaths_seen = deaths;
            if matches!(self.phase, Phase::Asked { .. }) {
                self.lived_again += 1;
            }
            self.phase = Phase::Dead { since };
            return None;
        }
        match self.phase {
            Phase::Dead { since: died } => {
                if since.saturating_sub(died) < self.settings.respawn_wait {
                    return None;
                }
                self.phase = Phase::Asked { at_time: since };
                Some(HuntAction::Respawn)
            }
            Phase::Asked { .. } => None,
            Phase::Hunting => self.hunt(mobs, since, ground),
        }
    }

    /// The bot's 2071 count is `hits`: a new hit starts its swings at its mob over.
    pub fn saw_hits(&mut self, hits: u64) {
        if hits > self.hits_seen {
            self.hits_seen = hits;
            if let Some((_, swings)) = &mut self.swinging_at {
                *swings = 0;
            }
        }
    }

    /// Takes the start nearest the first position `own` and the bot's home on its ring.
    fn settle(&mut self, own: WorldPosition) {
        let nearest = self
            .settings
            .starts
            .iter()
            .enumerate()
            .map(|(index, &start)| (index, start, horizontal(own, start)))
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let Some((index, start, distance)) = nearest else {
            return;
        };
        self.start = Some(index);
        self.home = Some([
            start[0] + START_SPREAD_M * self.ring_angle.cos(),
            start[1] + START_SPREAD_M * self.ring_angle.sin(),
        ]);
        self.start_distance_m = Some(distance);
    }

    fn hunt(
        &mut self,
        mobs: &[(u64, WorldPosition)],
        since: Duration,
        ground: &(dyn Ground + Send + Sync),
    ) -> Option<HuntAction> {
        if !self.may_send(since) {
            return None;
        }
        let (home, here) = (self.home?, self.here?);
        let reach_m = self.settings.reach_m;
        // A bot walks on the terrain: a mob on a rock or a ledge above it is out of its reach
        // (live hunt 2026-10-31: bots under a mob 5.6 m up swung at it all run).
        let walkable = |mob: &WorldPosition| {
            ground
                .height_at(mob.x, mob.y)
                .is_none_or(|height| (mob.z() - height).abs() <= reach_m)
        };
        self.left.retain(|_, until| *until > since);
        let left = &self.left;
        let nearest = mobs
            .iter()
            .filter(|(number, mob)| walkable(mob) && !left.contains_key(number))
            .map(|(number, mob)| (*number, *mob, planar(here, [mob.x, mob.y])))
            .min_by(|a, b| a.2.total_cmp(&b.2));
        match nearest {
            Some((number, mob, distance)) if distance <= reach_m => {
                let rested = self
                    .last_attack
                    .is_none_or(|last| since.saturating_sub(last) >= self.settings.attack_interval);
                if !rested {
                    return None;
                }
                let swings = match self.swinging_at {
                    Some((at, swings)) if at == number => swings + 1,
                    _ => 1,
                };
                if swings > MAX_SWINGS_WITHOUT_HIT {
                    self.left.insert(number, since + LEAVE_MOB);
                    self.swinging_at = None;
                    return None;
                }
                self.swinging_at = Some((number, swings));
                self.last_attack = Some(since);
                self.last_sent = Some(since);
                let from = Position {
                    x: here[0],
                    y: here[1],
                    z: here[2],
                };
                let to = Position {
                    x: mob.x,
                    y: mob.y,
                    z: mob.z(),
                };
                let yaw = yaw_toward(from, to);
                InputPosition::from_metres(here[0], here[1], here[2])
                    .map(|at| HuntAction::Attack { at, yaw })
            }
            Some((_, mob, _)) => {
                self.move_toward(here, [mob.x, mob.y], reach_m * STOP_SHARE, since, ground)
            }
            None if planar(here, home) > reach_m => {
                self.move_toward(here, home, 0.0, since, ground)
            }
            None => None,
        }
    }

    /// Whether a send interval passed since the last input.
    fn may_send(&self, since: Duration) -> bool {
        self.last_sent
            .is_none_or(|sent| since.saturating_sub(sent) >= SEND_INTERVAL)
    }

    /// One walk step from `from` toward `goal`, stopping `stop_m` short of it, on the ground.
    fn move_toward(
        &mut self,
        from: [f32; 3],
        goal: [f32; 2],
        stop_m: f32,
        since: Duration,
        ground: &(dyn Ground + Send + Sync),
    ) -> Option<HuntAction> {
        let (east, north) = (goal[0] - from[0], goal[1] - from[1]);
        let distance = east.hypot(north);
        let elapsed = self
            .last_sent
            .map_or(SEND_INTERVAL, |sent| since.saturating_sub(sent));
        let length = (self.settings.speed_mps * elapsed.as_secs_f32()).min(distance - stop_m);
        if length <= 0.0 {
            return None;
        }
        let (x, y) = (
            from[0] + east / distance * length,
            from[1] + north / distance * length,
        );
        let z = ground.height_at(x, y).unwrap_or(from[2]);
        let next = InputPosition::from_metres(x, y, z)?;
        self.here = Some([x, y, z]);
        self.last_sent = Some(since);
        Some(HuntAction::Move(next))
    }

    /// Where the bot waits while it sees no mob; `None` before its first position.
    pub const fn home(&self) -> Option<[f32; 2]> {
        self.home
    }

    /// How far the first position was from the nearest start; `None` before it.
    pub const fn start_distance_m(&self) -> Option<f32> {
        self.start_distance_m
    }

    /// The index of the bot's hunt start; `None` before its first position.
    pub const fn start(&self) -> Option<usize> {
        self.start
    }

    /// Respawns seen to work: a step after the request, or a later death.
    pub const fn lived_again(&self) -> u64 {
        self.lived_again
    }

    /// The time of a request not answered yet.
    pub const fn waiting_since(&self) -> Option<Duration> {
        match self.phase {
            Phase::Asked { at_time, .. } => Some(at_time),
            Phase::Hunting | Phase::Dead { .. } => None,
        }
    }
}

fn horizontal(own: WorldPosition, to: [f32; 2]) -> f32 {
    planar([own.x, own.y, own.z()], to)
}

/// The horizontal distance from `from` to `to`.
fn planar(from: [f32; 3], to: [f32; 2]) -> f32 {
    (from[0] - to[0]).hypot(from[1] - to[1])
}

/// What one bot saw.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HuntBotReport {
    /// Registration status 0.
    pub registered: bool,
    /// The server closed the connection.
    pub closed: bool,
    /// Longest gap between two own bundles.
    pub max_gap: Duration,
    /// 2071 messages for the bot's hits.
    pub hits_dealt: u64,
    /// 3601 messages for hits the bot took.
    pub hits_taken: u64,
    /// 4299 messages for the bot.
    pub deaths: u64,
    /// Respawns seen to work.
    pub lived_again: u64,
    /// A request that was not answered within [`RESPAWN_PROOF_WAIT`] before the end.
    pub unanswered_request: bool,
    /// How far the bot appeared from the nearest hunt start, in metres; `None`: no position.
    pub start_distance_m: Option<f32>,
    /// The index of its hunt start; `None`: no position.
    pub start: Option<usize>,
    /// Mob cards the bot was sent.
    pub mob_cards: u64,
    /// Messages that did not decode.
    pub unread_messages: u64,
    /// Why the first of them did not decode.
    pub first_unread: Option<String>,
    /// Why the bot could not run, if it could not.
    pub error: Option<String>,
}

/// Every bot and the sim totals of the run.
pub type HuntReport = FleetRun<HuntBotReport>;

impl HuntReport {
    /// Every check, in order (M6-B spec section 9.3).
    pub fn checks(&self, settings: &HuntSettings) -> Vec<Check> {
        let all = |rule: &dyn Fn(&HuntBotReport) -> bool| {
            !self.bots.is_empty() && self.bots.iter().all(rule)
        };
        let bots = settings.fleet.bots;
        let mut checks = vec![
            Check {
                name: BOTS_REGISTERED,
                passed: self.bots.len() == bots && all(&|bot| bot.registered),
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
                name: BOTS_NEAR_START,
                passed: all(&|bot| {
                    bot.start_distance_m
                        .is_some_and(|distance| distance <= START_RANGE_M)
                }),
            },
            Check {
                name: BOTS_HIT,
                passed: all(&|bot| bot.hits_dealt > 0),
            },
            Check {
                name: MOBS_DIED,
                passed: self.sims.mob_deaths >= bots as f64 / BOTS_PER_MOB_DEATH,
            },
            Check {
                name: BOTS_LIVED_AGAIN,
                passed: all(&|bot| !bot.unanswered_request),
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

    /// Whether every check passed.
    pub fn passed(&self, settings: &HuntSettings) -> bool {
        self.checks(settings).iter().all(|check| check.passed)
    }

    /// How many bots stopped with each error, by error text.
    pub fn error_kinds(&self) -> std::collections::BTreeMap<&str, usize> {
        error_kinds(self.bots.iter().filter_map(|bot| bot.error.as_deref()))
    }

    /// One line of numbers for the operator.
    pub fn summary(&self) -> String {
        let sum = |count: fn(&HuntBotReport) -> u64| self.bots.iter().map(count).sum::<u64>();
        let worst_gap = self.bots.iter().map(|bot| bot.max_gap).max();
        let errors = self.bots.iter().filter(|bot| bot.error.is_some()).count();
        let no_hit = self.bots.iter().filter(|bot| bot.hits_dealt == 0).count();
        let mut no_hit_by_start = std::collections::BTreeMap::new();
        for bot in self.bots.iter().filter(|bot| bot.hits_dealt == 0) {
            *no_hit_by_start.entry(bot.start).or_insert(0_usize) += 1;
        }
        let farthest_start = self
            .bots
            .iter()
            .filter_map(|bot| bot.start_distance_m)
            .fold(0.0_f32, f32::max);
        let first_unread = self
            .bots
            .iter()
            .find_map(|bot| bot.first_unread.as_deref())
            .unwrap_or("none");
        format!(
            "bots {}, failed to start {errors}, farthest from a start {farthest_start:.0} m, \
             bots without a hit {no_hit} (by start {no_hit_by_start:?}), hits {}, hits taken {}, deaths {}, lived again {}, mob \
             deaths {}, moves after a death {}, worst bundle gap {} ms, ticks within one tick \
             {:.4}, mob cards {}, messages not read {} (first: {first_unread})",
            self.bots.len(),
            sum(|bot| bot.hits_dealt),
            sum(|bot| bot.hits_taken),
            sum(|bot| bot.deaths),
            sum(|bot| bot.lived_again),
            self.sims.mob_deaths,
            self.sims.dead_moves,
            worst_gap.unwrap_or_default().as_millis(),
            ratio(self.sims.ticks_within_tick, self.sims.ticks),
            sum(|bot| bot.mob_cards),
            sum(|bot| bot.unread_messages),
        )
    }
}

/// The input records of one attack: the press and the release.
const ATTACK_RECORDS: u16 = 2;
/// A bot looks level.
const LEVEL_PITCH: f32 = 0.0;
/// The counter of a bot's first input record; the server reads only records after the last.
const FIRST_INPUT_COUNTER: u16 = 1;

/// One bot during its run.
struct HuntBot {
    hunter: Hunter,
    ground: Arc<dyn Ground + Send + Sync>,
    view: CombatView,
    own: Option<WorldPosition>,
    clock: BundleClock,
    /// The counter of the next attack's input records; each attack takes two.
    input_counter: u16,
    unread_messages: u64,
    first_unread: Option<String>,
}

impl FleetBot for HuntBot {
    type Report = HuntBotReport;

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
        // Every bundle counts: the own place comes only while the bot moves (retail own states).
        let bundles = self.view.bundles() - bundles_before;
        self.clock
            .saw(usize::try_from(bundles).unwrap_or(usize::MAX), since);
        let Some(own) = self.own else {
            return Ok(());
        };
        let mobs: Vec<(u64, WorldPosition)> = self.view.mobs().collect();
        self.hunter.saw_hits(self.view.hits_dealt());
        let action = self
            .hunter
            .step(own, &mobs, self.view.deaths(), since, &*self.ground);
        match action {
            None => Ok(()),
            Some(HuntAction::Move(position)) => {
                send_input(client, &ClientInputMsg::position(position))
            }
            Some(HuntAction::Attack { at, yaw }) => {
                let counter = self.input_counter;
                self.input_counter = counter.wrapping_add(ATTACK_RECORDS);
                let input = ClientInputMsg::attack(at, counter).looking([yaw, LEVEL_PITCH]);
                send_input(client, &input)
            }
            Some(HuntAction::Respawn) => client.send_message(
                INPUT_CHANNEL,
                true,
                REQUEST_RESPAWN_TYPE_INDEX,
                RequestRespawnMsg::default().encode()?,
            ),
        }
    }

    fn report(self, outcome: &RepOutcome) -> HuntBotReport {
        let unanswered_request = self
            .hunter
            .waiting_since()
            .is_some_and(|asked| outcome.held.saturating_sub(asked) > RESPAWN_PROOF_WAIT);
        HuntBotReport {
            registered: outcome.status == Some(REGISTRATION_STATUS_AUTHORIZED),
            closed: outcome.closed_by_server,
            max_gap: self.clock.max_gap(outcome.held),
            hits_dealt: self.view.hits_dealt(),
            hits_taken: self.view.hits_taken(),
            deaths: self.view.deaths(),
            lived_again: self.hunter.lived_again(),
            unanswered_request,
            start_distance_m: self.hunter.start_distance_m(),
            start: self.hunter.start(),
            mob_cards: self.view.mob_cards(),
            unread_messages: self.unread_messages,
            first_unread: self.first_unread,
            error: None,
        }
    }

    fn failed(error: String) -> HuntBotReport {
        HuntBotReport {
            error: Some(error),
            ..HuntBotReport::default()
        }
    }
}

/// Sends `input` as the swarm does: unreliable, on the input channel.
fn send_input(client: &mut RepClient, input: &ClientInputMsg) -> Result<(), RepError> {
    client.send_message(
        INPUT_CHANNEL,
        false,
        RECEIVE_INPUT_FROM_CLIENT_TYPE_INDEX,
        input.encode()?,
    )
}

/// Runs every bot and reads the sim metrics before and after.
pub async fn run_hunt(
    http: &Client,
    settings: Arc<HuntSettings>,
    ground: Arc<dyn Ground + Send + Sync>,
    new_client: NewClient,
    stop: watch::Receiver<bool>,
    report: impl FnMut(String),
) -> HuntReport {
    let fleet = Arc::new(settings.fleet.clone());
    let new_bot: NewBot<HuntBot> = Arc::new(move |n| HuntBot {
        hunter: Hunter::new(Arc::clone(&settings), n),
        ground: Arc::clone(&ground),
        view: CombatView::default(),
        own: None,
        clock: BundleClock::new(),
        input_counter: FIRST_INPUT_COUNTER,
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
    use crate::fleet::{METRICS_READ, NO_REFUSED_MOVES, SimTotals, TICKS_FAST};

    const ORIGIN: f32 = 1000.0;
    const HEIGHT: f32 = 50.0;
    const SPEED_MPS: f32 = 4.0;
    const REACH_M: f32 = 2.5;
    const ATTACK_INTERVAL: Duration = Duration::from_millis(600);
    const RESPAWN_WAIT: Duration = Duration::from_millis(2500);
    const DURATION: Duration = Duration::from_secs(120);
    /// How far the server puts a respawn: retail moved its open-world deaths 305 to 541 m.
    const RESPAWN_POINT_M: f32 = 305.0;
    const MOB: u64 = (1 << 34) + 1;
    const FAR_MOB: u64 = MOB + 1;

    fn settings() -> Arc<HuntSettings> {
        Arc::new(HuntSettings {
            fleet: test_fleet(DURATION),
            starts: vec![[ORIGIN, ORIGIN]],
            speed_mps: SPEED_MPS,
            reach_m: REACH_M,
            attack_interval: ATTACK_INTERVAL,
            respawn_wait: RESPAWN_WAIT,
            max_bundle_gap: Duration::from_millis(250),
        })
    }

    fn at(x: f32) -> WorldPosition {
        WorldPosition::from_metres(x, ORIGIN, HEIGHT).expect("finite")
    }

    fn x_of(action: Option<HuntAction>) -> f32 {
        match action {
            Some(HuntAction::Move(position) | HuntAction::Attack { at: position, .. }) => {
                position.x
            }
            other => panic!("not a position: {other:?}"),
        }
    }

    /// The server sends no own place while the bot walks: it goes on from where it sent itself.
    #[test]
    fn a_bot_walks_to_the_nearest_mob_and_stops_in_reach() {
        let mut hunter = Hunter::new(settings(), 0);
        let mobs = [(FAR_MOB, at(ORIGIN + 20.0)), (MOB, at(ORIGIN - 10.0))];
        let own = at(ORIGIN);
        let mut since = SEND_INTERVAL;
        let first = hunter.step(own, &mobs, 0, since, &NoGround);
        assert!(matches!(first, Some(HuntAction::Move(_))));
        assert!(x_of(first) < ORIGIN, "toward the nearer mob, west");
        let step_m = SPEED_MPS * SEND_INTERVAL.as_secs_f32();
        assert!(
            (ORIGIN - x_of(first) - step_m).abs() < 0.01,
            "at walking speed"
        );
        let mut attacked_at = None;
        for _ in 0..100 {
            since += SEND_INTERVAL;
            match hunter.step(own, &mobs, 0, since, &NoGround) {
                Some(HuntAction::Move(_)) => {}
                Some(HuntAction::Attack { at, yaw }) => {
                    attacked_at = Some((at.x, yaw));
                    break;
                }
                other => panic!("{other:?}"),
            }
        }
        let (attacked_at, yaw) = attacked_at.expect("an attack in reach");
        assert!((attacked_at - (ORIGIN - 10.0)).abs() <= REACH_M);
        assert!(
            (yaw - std::f32::consts::FRAC_PI_2).abs() < 0.01,
            "the mob is west: looking west, {yaw}"
        );
        assert_eq!(
            hunter.step(own, &mobs, 0, since + SEND_INTERVAL, &NoGround),
            None,
            "the next attack waits for the interval"
        );
        assert!(matches!(
            hunter.step(own, &mobs, 0, since + ATTACK_INTERVAL, &NoGround),
            Some(HuntAction::Attack { .. })
        ));
    }

    /// Ground at [`HEIGHT`] everywhere.
    struct Flat;

    impl Ground for Flat {
        fn height_at(&self, _: f32, _: f32) -> Option<f32> {
            Some(HEIGHT)
        }
    }

    #[test]
    fn a_bot_leaves_a_mob_on_a_ledge_above_its_ground() {
        const LEDGE_M: f32 = 5.6;
        let mut hunter = Hunter::new(settings(), 0);
        let on_ledge =
            WorldPosition::from_metres(ORIGIN + 1.0, ORIGIN, HEIGHT + LEDGE_M).expect("finite");
        let mobs = [(MOB, on_ledge), (FAR_MOB, at(ORIGIN - 10.0))];
        let first = hunter.step(at(ORIGIN), &mobs, 0, SEND_INTERVAL, &Flat);
        assert!(
            matches!(first, Some(HuntAction::Move(_))) && x_of(first) < ORIGIN,
            "not the ledge mob 1 m east, the far one west: {first:?}"
        );
    }

    #[test]
    fn a_bot_leaves_a_mob_its_swings_do_not_hit() {
        let mut hunter = Hunter::new(settings(), 0);
        let own = at(ORIGIN);
        let mobs = [(MOB, at(ORIGIN + 1.0)), (FAR_MOB, at(ORIGIN - 30.0))];
        let mut since = SEND_INTERVAL;
        let swing =
            |hunter: &mut Hunter, since: Duration| hunter.step(own, &mobs, 0, since, &NoGround);
        for _ in 0..MAX_SWINGS_WITHOUT_HIT {
            assert!(matches!(
                swing(&mut hunter, since),
                Some(HuntAction::Attack { .. })
            ));
            since += ATTACK_INTERVAL;
        }
        hunter.saw_hits(1);
        assert!(
            matches!(swing(&mut hunter, since), Some(HuntAction::Attack { .. })),
            "a hit starts the count over"
        );
        for _ in 0..MAX_SWINGS_WITHOUT_HIT {
            since += ATTACK_INTERVAL;
            swing(&mut hunter, since);
        }
        since += ATTACK_INTERVAL;
        let next = swing(&mut hunter, since);
        assert!(
            matches!(next, Some(HuntAction::Move(_))) && x_of(next) < ORIGIN,
            "after its swings without a hit it walks to the other mob: {next:?}"
        );
        since += LEAVE_MOB;
        let later = swing(&mut hunter, since);
        assert!(
            matches!(later, Some(HuntAction::Move(_))) && x_of(later) > x_of(next),
            "later it may pick the mob again, back east: {later:?}"
        );
    }

    #[test]
    fn a_new_server_place_moves_the_bot() {
        let mut hunter = Hunter::new(settings(), 0);
        hunter.step(at(ORIGIN), &[], 0, SEND_INTERVAL, &NoGround);
        let pushed = at(ORIGIN - 5.0);
        let next = hunter.step(pushed, &[], 0, SEND_INTERVAL * 2, &NoGround);
        let step_m = SPEED_MPS * SEND_INTERVAL.as_secs_f32();
        assert!(
            (x_of(next) - (ORIGIN - 5.0)).abs() <= step_m + 0.01,
            "on from the server's place: {next:?}"
        );
    }

    /// The home of bot `n` after its first position `own`.
    fn home_after(settings: &Arc<HuntSettings>, n: usize, own: WorldPosition) -> [f32; 2] {
        let mut hunter = Hunter::new(Arc::clone(settings), n);
        assert_eq!(hunter.home(), None, "no home before a position");
        hunter.step(own, &[], 0, SEND_INTERVAL, &NoGround);
        hunter.home().expect("a home")
    }

    #[test]
    fn a_bot_without_a_mob_walks_to_its_home() {
        let mut hunter = Hunter::new(settings(), 0);
        let away = at(ORIGIN + 30.0);
        assert!(x_of(hunter.step(away, &[], 0, SEND_INTERVAL, &NoGround)) < ORIGIN + 30.0);
        let [x, y] = home_after(&settings(), 0, at(ORIGIN));
        let home = WorldPosition::from_metres(x, y, HEIGHT).expect("finite");
        let mut waiting = Hunter::new(settings(), 0);
        assert_eq!(waiting.step(home, &[], 0, SEND_INTERVAL, &NoGround), None);
    }

    #[test]
    fn a_bot_hunts_at_the_start_nearest_where_it_appeared() {
        const FAR_START_M: f32 = 2700.0;
        let two = Arc::new(HuntSettings {
            starts: vec![[ORIGIN + FAR_START_M, ORIGIN], [ORIGIN, ORIGIN]],
            ..(*settings()).clone()
        });
        let mut hunter = Hunter::new(Arc::clone(&two), 0);
        let appeared = at(ORIGIN + 20.0);
        hunter.step(appeared, &[], 0, SEND_INTERVAL, &NoGround);
        let home = hunter.home().expect("a home");
        assert!(
            (home[0] - ORIGIN).hypot(home[1] - ORIGIN) <= START_SPREAD_M + 0.01,
            "the near start, not start 0: {home:?}"
        );
        let distance = hunter.start_distance_m().expect("a distance");
        assert!((distance - 20.0).abs() < 0.01, "{distance}");
    }

    #[test]
    fn bots_of_one_start_stand_apart_around_it() {
        let three = Arc::new(HuntSettings {
            starts: vec![[ORIGIN, ORIGIN], [0.0, 0.0], [0.0, 0.0]],
            ..(*settings()).clone()
        });
        let homes: Vec<[f32; 2]> = [0, 1, 2]
            .iter()
            .map(|n| home_after(&three, *n, at(ORIGIN)))
            .collect();
        for home in &homes {
            let from_start = (home[0] - ORIGIN).hypot(home[1] - ORIGIN);
            assert!(from_start <= START_SPREAD_M + 0.01, "{home:?}");
        }
        for (a, b) in [(0, 1), (0, 2), (1, 2)] {
            let apart = (homes[a][0] - homes[b][0]).hypot(homes[a][1] - homes[b][1]);
            assert!(apart >= 1.0, "{homes:?}");
        }
    }

    #[test]
    fn a_dead_bot_asks_to_respawn_after_its_wait() {
        let mut hunter = Hunter::new(settings(), 0);
        let mobs = [(MOB, at(ORIGIN + 1.0))];
        let died = Duration::from_secs(10);
        let own = at(ORIGIN);
        assert_eq!(hunter.step(own, &mobs, 1, died, &NoGround), None);
        assert_eq!(
            hunter.step(
                own,
                &mobs,
                1,
                died + RESPAWN_WAIT - SEND_INTERVAL,
                &NoGround
            ),
            None,
            "dead: no move, no attack"
        );
        assert_eq!(
            hunter.step(own, &mobs, 1, died + RESPAWN_WAIT, &NoGround),
            Some(HuntAction::Respawn)
        );
        let asked = died + RESPAWN_WAIT;
        assert_eq!(hunter.waiting_since(), Some(asked));
        assert_eq!(
            hunter.step(own, &mobs, 1, asked + SEND_INTERVAL, &NoGround),
            None,
            "waits for the server's place"
        );
        let respawn_point = at(ORIGIN + RESPAWN_POINT_M);
        assert_eq!(
            hunter.step(respawn_point, &mobs, 1, asked * 2, &NoGround),
            None
        );
        assert_eq!((hunter.lived_again(), hunter.waiting_since()), (1, None));
    }

    /// A check and the fault that must fail it alone.
    type Fault = (&'static str, fn(&mut HuntReport));

    fn good_bot() -> HuntBotReport {
        HuntBotReport {
            registered: true,
            closed: false,
            max_gap: Duration::from_millis(100),
            hits_dealt: 5,
            hits_taken: 2,
            deaths: 1,
            lived_again: 1,
            unanswered_request: false,
            start_distance_m: Some(0.0),
            start: Some(0),
            mob_cards: 3,
            unread_messages: 0,
            first_unread: None,
            error: None,
        }
    }

    fn good_report() -> HuntReport {
        HuntReport {
            bots: vec![good_bot(), good_bot()],
            sims: SimTotals {
                ticks_within_tick: 999.0,
                ticks: 1000.0,
                mob_deaths: 1.0,
                ..SimTotals::default()
            },
            unread_metrics: 0,
        }
    }

    #[test]
    fn the_summary_counts_the_hits_the_mobs_made() {
        let mut report = good_report();
        report.bots[0].hits_taken = 3;
        report.bots[1].hits_taken = 4;
        report.bots[1].start_distance_m = Some(12.4);
        assert!(
            report.summary().contains("hits taken 7"),
            "{}",
            report.summary()
        );
        assert!(
            report.summary().contains("farthest from a start 12 m"),
            "{}",
            report.summary()
        );
    }

    #[test]
    fn the_hunt_checks_fail_on_each_fault() {
        let settings = settings();
        assert!(good_report().passed(&settings));
        let faults: [Fault; 12] = [
            (BOTS_REGISTERED, |report| report.bots[0].registered = false),
            (BOTS_CONNECTED, |report| report.bots[1].closed = true),
            (MESSAGES_READ, |report| report.bots[0].unread_messages = 1),
            (BOTS_NEAR_START, |report| {
                report.bots[0].start_distance_m = Some(START_RANGE_M + 1.0);
            }),
            (BOTS_NEAR_START, |report| {
                report.bots[1].start_distance_m = None
            }),
            (BOTS_HIT, |report| report.bots[0].hits_dealt = 0),
            (MOBS_DIED, |report| report.sims.mob_deaths = 0.0),
            (BOTS_LIVED_AGAIN, |report| {
                report.bots[1].unanswered_request = true;
            }),
            (BUNDLES_STEADY, |report| {
                report.bots[0].max_gap = Duration::from_secs(1);
            }),
            (TICKS_FAST, |report| report.sims.ticks_within_tick = 900.0),
            (NO_REFUSED_MOVES, |report| report.sims.refused_moves = 1.0),
            (METRICS_READ, |report| report.unread_metrics = 1),
        ];
        for (name, fault) in faults {
            let mut report = good_report();
            fault(&mut report);
            let failed: Vec<&str> = report
                .checks(&settings)
                .iter()
                .filter(|check| !check.passed)
                .map(|check| check.name)
                .collect();
            assert_eq!(failed, vec![name]);
        }
        let mut missing = good_report();
        missing.bots.pop();
        assert!(!missing.passed(&settings));
        assert!(!HuntReport::default().passed(&settings));
    }
}
