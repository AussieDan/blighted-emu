// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's meta achievements, the achievements window (progression note, section 8): the
//! counters its kills, gathers and deaths raise, the completed meta achievements in completion
//! order, and the client's calls of the window: the summary (3092, answered by 1679), a tab's
//! progress (3794, answered by 3800), the waiting rewards (843, answered by 3802) and their
//! clearing (3227). A completion goes to the client as a 331 notice, and so does 75 % of a meta
//! achievement whose `NotificationVerbosity` asks for it.
//!
//! What a meta achievement measures comes from `MetaAchievementData` ([`MetaKind`]): kills,
//! gathers and deaths are counted here; the level, the categorical progression ranks, the
//! achievements, the done quests, the titles and the completed meta achievements are read from
//! the player. A measure of a system this server does not run never progresses.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::meta_achievements::{MetaKind, MetaRule};
use nw_content::progression::WEAPON_MASTERY_RANK_TABLE;
use nw_content::titles::TitleProgress;
use nw_messages::{
    FacetHeader, MetaAchievementProgressMsg, MetaAchievementRequest, MetaAchievementSummaryMsg,
    MetaProgress, MetaProgressChangedMsg, PendingMetaAchievementsMsg, ServerMessage,
};

use super::{Player, SimWorld, send_now};
use crate::player_content::PlayerContent;

/// How many of the latest completions a summary lists (Observed: two in all 9 retail summaries).
const LATEST_SHOWN: usize = 2;
/// How many meta achievements nearest completion a summary lists at most (Observed: two in four
/// sessions, none in two).
const NEAREST_SHOWN: usize = 2;
/// A meta achievement is near completion from three quarters of its total: the
/// `SeventyFiveOneHundred` notice threshold (Inferred: the six retail entries were at 82 % to
/// 97 %, and a character with 214 completed had none).
const NEAR_NUMERATOR: u64 = 3;
const NEAR_DENOMINATOR: u64 = 4;

/// What a player did that a meta achievement counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaEvent<'a> {
    /// It killed a mob of `categories` (`VitalsCategories`).
    Kill {
        /// The mob's categories.
        categories: &'a [String],
    },
    /// It gathered with `tradeskill`.
    Gathered {
        /// The gatherable's `Tradeskill`.
        tradeskill: &'a str,
    },
    /// It died.
    Died,
}

/// What the player's meta achievements were last checked against: its level, the version of
/// its categorical progressions, its achievements and its done quests.
type Fingerprint = (u32, u64, u64, usize);

/// A player's meta achievements.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct MetaState {
    /// The counters, by lowercase counter key.
    counters: BTreeMap<String, u64>,
    /// The completed meta achievements in completion order, each with whether its reward (a
    /// title) waits for the player to see it.
    completed: Vec<(String, bool)>,
    /// The meta achievements whose 75 % notice went out (this session).
    near_noticed: BTreeSet<String>,
    /// The state the meta achievements were last checked against; `None` checks at the next
    /// tick.
    checked: Option<Fingerprint>,
}

impl MetaState {
    /// The state of the saved `counters` and `completed`.
    pub(super) fn of(counters: &BTreeMap<String, u64>, completed: &[(String, bool)]) -> Self {
        Self {
            counters: counters.clone(),
            completed: completed.to_vec(),
            ..Self::default()
        }
    }

    /// The counters, as a save or a handoff carries them.
    pub(super) fn carried_counters(&self) -> BTreeMap<String, u64> {
        self.counters.clone()
    }

    /// The completed meta achievements, as a save or a handoff carries them.
    pub(super) fn carried_completed(&self) -> Vec<(String, bool)> {
        self.completed.clone()
    }

    /// Whether meta achievement `id` (any case) is completed.
    pub(super) fn is_completed(&self, id: &str) -> bool {
        self.completed
            .iter()
            .any(|(done, _)| done.eq_ignore_ascii_case(id))
    }

    /// Raises the counter of each meta achievement that `event` counts for, once per counter.
    fn count(&mut self, rules: &[MetaRule], event: MetaEvent<'_>) {
        let keys: BTreeSet<&str> = rules
            .iter()
            .filter(|rule| match (event, &rule.kind) {
                (MetaEvent::Kill { categories }, kind) => kind.counts_kill(categories),
                (MetaEvent::Gathered { tradeskill }, MetaKind::Gathering(wanted)) => {
                    wanted.eq_ignore_ascii_case(tradeskill)
                }
                (MetaEvent::Died, MetaKind::Deaths) => true,
                _ => false,
            })
            .map(|rule| rule.counter.as_str())
            .collect();
        if keys.is_empty() {
            return;
        }
        for key in keys {
            let counter = self.counters.entry(key.to_owned()).or_default();
            *counter = counter.saturating_add(1);
        }
        self.checked = None;
    }

    /// The ids of the completed meta achievements whose reward waits.
    fn pending(&self) -> impl Iterator<Item = &str> {
        self.completed
            .iter()
            .filter(|(_, pending)| *pending)
            .map(|(id, _)| id.as_str())
    }
}

impl SimWorld {
    /// Player `entity` did `event`: the meta achievements that count it progress, and the next
    /// tick checks them.
    pub(super) fn meta_event(&mut self, entity: Entity, event: MetaEvent<'_>) {
        let content = Arc::clone(&self.content);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.meta.count(content.meta_achievements.rules(), event);
        }
    }

    /// Checks the meta achievements of every player in the world whose state changed since
    /// the last check: a completion is kept and noticed (331), and so is the 75 % of one that
    /// asks for it.
    pub(super) fn tick_meta_achievements(&mut self) {
        let content = Arc::clone(&self.content);
        let rules = content.meta_achievements.rules();
        let mut notices: Vec<(ConnectionRef, ServerMessage)> = Vec::new();
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            if !player.own_card_sent {
                continue;
            }
            let fingerprint = fingerprint(&content, &player);
            if player.meta.checked == Some(fingerprint) {
                continue;
            }
            for entry in check(&content, &self.achievement_rules, &mut player, rules) {
                notices.push((
                    player.connection.clone(),
                    ServerMessage::MetaProgressChanged(MetaProgressChangedMsg::of(
                        player.number,
                        entry,
                    )),
                ));
            }
            player.meta.checked = Some(fingerprint);
        }
        for (connection, notice) in notices {
            send_now(&mut self.combat_messages, &connection, &[notice]);
        }
    }

    /// Answers the meta achievement call `request` of the client of `connection`.
    pub fn meta_achievement_call(
        &mut self,
        connection: &ConnectionRef,
        request: &MetaAchievementRequest,
    ) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let content = Arc::clone(&self.content);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let answer = match *request {
            MetaAchievementRequest::Summary { header } => {
                let summary = summary(&content, &self.achievement_rules, &player, header);
                Some(ServerMessage::MetaAchievementSummary(summary))
            }
            MetaAchievementRequest::CategoryProgress { header, category } => {
                let tab = content.meta_achievements.in_category(category);
                let progress = tab
                    .iter()
                    .map(|rule| MetaProgress {
                        id: rule.crc,
                        progress: clamp(progress_of(
                            &content,
                            &self.achievement_rules,
                            &player,
                            rule,
                        )),
                    })
                    .collect();
                let completed = tab
                    .iter()
                    .filter(|rule| player.meta.is_completed(&rule.id))
                    .map(|rule| rule.crc)
                    .collect();
                Some(ServerMessage::MetaAchievementProgress(
                    MetaAchievementProgressMsg {
                        header,
                        progress,
                        completed,
                    },
                ))
            }
            MetaAchievementRequest::ClearPending { .. } => {
                for (_, pending) in &mut player.meta.completed {
                    *pending = false;
                }
                None
            }
        };
        if let Some(answer) = answer {
            send_now(&mut self.combat_messages, connection, &[answer]);
        }
    }

    /// The answer to 843: the completed meta achievements of player `entity` whose reward
    /// waits (retail linux 2026-09-12 sent `AchievementsEarnedCount3`, whose title waited).
    pub(super) fn pending_meta_achievements(
        &self,
        entity: Entity,
        header: FacetHeader,
    ) -> Option<ServerMessage> {
        let player = self.world.get::<Player>(entity)?;
        let rules = &self.content.meta_achievements;
        Some(ServerMessage::PendingMetaAchievements(
            PendingMetaAchievementsMsg {
                header,
                achievements: player
                    .meta
                    .pending()
                    .filter_map(|id| rules.rule(id).map(|rule| rule.crc))
                    .collect(),
            },
        ))
    }
}

/// What the meta achievements of `player` depend on now.
fn fingerprint(content: &PlayerContent, player: &Player) -> Fingerprint {
    (
        content.progression.level_of(player.experience),
        player.masteries.versions().0,
        player.flow.achievement_version(),
        player.done_quests.len(),
    )
}

/// Completes every meta achievement of `rules` that `player` reached, until none more does (a
/// completion counts for the meta achievement count), and returns the notices to send.
fn check(
    content: &PlayerContent,
    achievements: &super::level_flow::AchievementRules,
    player: &mut Player,
    rules: &[MetaRule],
) -> Vec<MetaProgress> {
    let mut notices = Vec::new();
    loop {
        let mut completed_now = Vec::new();
        for rule in rules {
            if player.meta.is_completed(&rule.id) {
                continue;
            }
            let progress = progress_of(content, achievements, player, rule);
            let total = u64::from(rule.total);
            let predecessors_done = rule
                .predecessors
                .iter()
                .all(|id| player.meta.is_completed(id));
            if progress >= total && predecessors_done {
                completed_now.push((rule, progress));
            } else if rule.notify_three_quarters
                && progress.saturating_mul(NEAR_DENOMINATOR) >= total * NEAR_NUMERATOR
                && progress < total
                && !player.meta.near_noticed.contains(&rule.id)
            {
                player.meta.near_noticed.insert(rule.id.clone());
                notices.push(MetaProgress {
                    id: rule.crc,
                    progress: clamp(progress),
                });
            }
        }
        if completed_now.is_empty() {
            return notices;
        }
        for (rule, progress) in completed_now {
            let pending = content.titles.meta_achievement_earns(&rule.id);
            player.meta.completed.push((rule.id.clone(), pending));
            tracing::info!(player = player.number, meta_achievement = %rule.id, "meta achievement completed");
            notices.push(MetaProgress {
                id: rule.crc,
                progress: clamp(progress),
            });
        }
    }
}

/// The progress of `player` on `rule`.
fn progress_of(
    content: &PlayerContent,
    achievements: &super::level_flow::AchievementRules,
    player: &Player,
    rule: &MetaRule,
) -> u64 {
    let progression = &content.progression;
    let has = |id: &str| player.flow.has_achievement(achievements, id);
    match &rule.kind {
        MetaKind::Kills { .. } | MetaKind::Gathering(_) | MetaKind::Deaths => player
            .meta
            .counters
            .get(&rule.counter)
            .copied()
            .unwrap_or_default(),
        MetaKind::CharacterLevel => u64::from(progression.level_of(player.experience)),
        MetaKind::Progression(id) => u64::from(player.masteries.rank(progression, id)),
        MetaKind::RankTableAny(table) if table.eq_ignore_ascii_case(WEAPON_MASTERY_RANK_TABLE) => {
            u64::from(player.masteries.highest_rank(progression, |id| {
                progression
                    .masteries()
                    .iter()
                    .any(|mastery| mastery.id.eq_ignore_ascii_case(id))
            }))
        }
        MetaKind::MetaAchievementCount => count(player.meta.completed.len()),
        MetaKind::TitleCount => title_count(content, achievements, player),
        MetaKind::Achievements(ids) => u64::from(!ids.is_empty() && ids.iter().all(|id| has(id))),
        MetaKind::AchievementCategory(category) => count(
            content
                .meta_achievements
                .category_achievements(category)
                .iter()
                .filter(|id| has(id))
                .count(),
        ),
        MetaKind::QuestTotal => count(player.done_quests.len()),
        MetaKind::QuestArc(tag) => {
            let arc = content.meta_achievements.arc_objectives(tag);
            u64::from(
                !arc.is_empty()
                    && arc.iter().all(|objective| {
                        player
                            .done_quests
                            .iter()
                            .any(|done| done.eq_ignore_ascii_case(objective))
                    }),
            )
        }
        MetaKind::RankTableAny(_) | MetaKind::Untracked(_) => 0,
    }
}

/// The titles `player` earned: by its level, ranks and achievements, and by its completed meta
/// achievements.
fn title_count(
    content: &PlayerContent,
    achievements: &super::level_flow::AchievementRules,
    player: &Player,
) -> u64 {
    let progression = &content.progression;
    let rank = |id: &str| u32::from(player.masteries.rank(progression, id));
    let achievement = |id: &str| player.flow.has_achievement(achievements, id);
    let completed = |id: &str| player.meta.is_completed(id);
    let progress = TitleProgress {
        level: progression.level_of(player.experience),
        rank: &rank,
        achievement: &achievement,
        meta_achievement: &completed,
    };
    count(content.titles.earned(&progress).count())
}

/// The summary (1679) of `player` on `header`.
fn summary(
    content: &PlayerContent,
    achievements: &super::level_flow::AchievementRules,
    player: &Player,
    header: FacetHeader,
) -> MetaAchievementSummaryMsg {
    let rules = &content.meta_achievements;
    let crc_of = |id: &str| rules.rule(id).map(|rule| rule.crc);
    let completed: Vec<u32> = player
        .meta
        .completed
        .iter()
        .filter_map(|(id, _)| crc_of(id))
        .collect();
    let latest = completed[completed.len().saturating_sub(LATEST_SHOWN)..].to_vec();
    let mut near: Vec<(&MetaRule, u64)> = rules
        .rules()
        .iter()
        .filter(|rule| !rule.hidden && rule.total > 0 && !player.meta.is_completed(&rule.id))
        .map(|rule| (rule, progress_of(content, achievements, player, rule)))
        .filter(|(rule, progress)| {
            progress.saturating_mul(NEAR_DENOMINATOR) >= u64::from(rule.total) * NEAR_NUMERATOR
        })
        .collect();
    // The share of the total, highest first: a / b > c / d when a d > c b.
    near.sort_by(|(a, a_progress), (b, b_progress)| {
        (u128::from(*b_progress) * u128::from(a.total))
            .cmp(&(u128::from(*a_progress) * u128::from(b.total)))
            .then(a.crc.cmp(&b.crc))
    });
    MetaAchievementSummaryMsg {
        header,
        latest,
        nearest: near
            .into_iter()
            .take(NEAREST_SHOWN)
            .map(|(rule, progress)| MetaProgress {
                id: rule.crc,
                progress: clamp(progress),
            })
            .collect(),
        completed,
    }
}

/// A count as a progress.
fn count(count: usize) -> u64 {
    u64::try_from(count).unwrap_or(u64::MAX)
}

/// A progress as the wire's `u32`.
fn clamp(progress: u64) -> u32 {
    u32::try_from(progress).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use nw_content::meta_achievements::{MetaAchievementRules, MetaRow};
    use nw_content::titles::{TitleRequirement, TitleRules, TitleScope};
    use nw_formats::objectstream::lowercase_crc32;
    use nw_messages::LookupRequest;

    use super::*;
    use crate::world::tests::{
        NOW, SERVER_TIME, admit_at, connection, messages_to, world_with_spawns,
    };

    const PLAYER: u64 = 9;
    const FIRST: &str = "PvE_TotalKills1";
    const SECOND: &str = "PvE_TotalKills2";
    const TAB: &str = "PvE_Kill_Counts";
    const HEADER: FacetHeader = FacetHeader {
        target: 1,
        entity: 2,
    };

    fn rules() -> MetaAchievementRules {
        MetaAchievementRules::build(
            [
                MetaRow {
                    id: FIRST,
                    kind: "TotalKills",
                    category: TAB,
                    total: Some(2),
                    progress_id: Some("PvE_TotalKills"),
                    verbosity: "OneHundred",
                    is_pvp: Some("NO"),
                    in_group: Some("ANY"),
                    faction: Some("Any"),
                    ..MetaRow::default()
                },
                MetaRow {
                    id: SECOND,
                    kind: "TotalKills",
                    category: TAB,
                    total: Some(4),
                    progress_parent: Some("PvE_TotalKills"),
                    verbosity: "SeventyFiveOneHundred",
                    predecessors: Some(FIRST),
                    ..MetaRow::default()
                },
            ],
            [],
            [],
        )
    }

    fn world() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            meta_achievements: rules(),
            titles: TitleRules::build([(
                "PlayerTitle_Killer",
                TitleScope::Account,
                TitleRequirement::MetaAchievement(vec![FIRST.to_owned()]),
            )]),
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(PLAYER).expect("player");
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .own_card_sent = true;
        world
    }

    fn kill(world: &mut SimWorld) {
        let entity = world.player_by_number(PLAYER).expect("player");
        world.meta_event(
            entity,
            MetaEvent::Kill {
                categories: &["Hostile".to_owned()],
            },
        );
    }

    fn notices(world: &mut SimWorld) -> Vec<(u32, u32)> {
        world.tick_meta_achievements();
        messages_to(&world.take_messages(), PLAYER)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::MetaProgressChanged(notice) => {
                    Some((notice.entry.id, notice.entry.progress))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn kills_complete_tiers_notice_them_and_fill_the_window() {
        let mut world = world();
        let (first, second) = (lowercase_crc32(FIRST), lowercase_crc32(SECOND));
        assert!(notices(&mut world).is_empty());
        kill(&mut world);
        assert!(notices(&mut world).is_empty(), "1 of 2");
        kill(&mut world);
        assert_eq!(
            notices(&mut world),
            [(first, 2)],
            "the first tier completes"
        );
        assert!(notices(&mut world).is_empty(), "noticed once");
        kill(&mut world);
        assert_eq!(
            notices(&mut world),
            [(second, 3)],
            "75 % of the second tier"
        );
        world.meta_achievement_call(
            &connection(PLAYER),
            &MetaAchievementRequest::Summary { header: HEADER },
        );
        world.meta_achievement_call(
            &connection(PLAYER),
            &MetaAchievementRequest::CategoryProgress {
                header: HEADER,
                category: lowercase_crc32(TAB),
            },
        );
        let entity = world.player_by_number(PLAYER).expect("player");
        let pending = world
            .pending_meta_achievements(entity, HEADER)
            .expect("a player");
        let answers = messages_to(&world.take_messages(), PLAYER);
        let summary = MetaAchievementSummaryMsg {
            header: HEADER,
            latest: vec![first],
            nearest: vec![MetaProgress {
                id: second,
                progress: 3,
            }],
            completed: vec![first],
        };
        let mut tab = vec![
            MetaProgress {
                id: first,
                progress: 3,
            },
            MetaProgress {
                id: second,
                progress: 3,
            },
        ];
        tab.sort_by_key(|entry| entry.id);
        assert_eq!(
            answers,
            [
                ServerMessage::MetaAchievementSummary(summary),
                ServerMessage::MetaAchievementProgress(MetaAchievementProgressMsg {
                    header: HEADER,
                    progress: tab,
                    completed: vec![first],
                }),
            ]
        );
        // The first tier earns a title: its reward waits until the client clears it.
        assert_eq!(
            pending,
            ServerMessage::PendingMetaAchievements(PendingMetaAchievementsMsg {
                header: HEADER,
                achievements: vec![first],
            })
        );
        world.meta_achievement_call(
            &connection(PLAYER),
            &MetaAchievementRequest::ClearPending {
                header: HEADER,
                flag: 0,
            },
        );
        world.lookup(
            &connection(PLAYER),
            &LookupRequest::PendingMetaAchievements { header: HEADER },
        );
        assert!(matches!(
            &messages_to(&world.take_messages(), PLAYER)[..],
            [ServerMessage::PendingMetaAchievements(none)] if none.achievements.is_empty()
        ));
        let saved = world
            .positions()
            .into_iter()
            .find_map(|departure| departure.progression)
            .expect("a save");
        assert_eq!(saved.meta_completed, [(FIRST.to_owned(), false)]);
        assert_eq!(
            saved.meta_counters,
            BTreeMap::from([("pve_totalkills".to_owned(), 3)])
        );
    }
}
