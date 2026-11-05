// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tradeskill side of gathers, crafts and salvages (craft and craft2 reports, run 15): a
//! gather's or a craft's game event raises the categorical progression it names
//! (`Gather_MiningT1`: Mining 64; `Craft_CookingT1`: Cooking 4.6) times the remote config's
//! multiplier for it (`javelin.xp-multipliers.progression`, retail Mining 2 and territory
//! standing 3), the territory the player is in gains the event's `TerritoryStanding`, and the
//! event pays its coins. The progression is any categorical progression
//! ([`nw_content::progression::Progression::categorical`]): a tradeskill, a territory's
//! standing or a currency, kept in the type-911 list with the masteries, so it is saved and
//! handed off with them. A tradeskill's rank gives the gathering efficiency and the recipe level
//! that gathers and crafts read.
//!
//! How a firing scales ([`EventScale`], [`nw_content::tradeskill_xp`]): a craft counts the
//! recipe's ingredient quantities (progression and standing alike), a skin the creature
//! level's `GatherableLevelModifier`, rounded down before the multiplier. The sum of a batch is
//! rounded down (Observed: Arcana 18 x 5 x 2 x 1.05 = 188.99 gave 188; 2898 x 2 x 1.05 = 6085.8
//! gave 6085).
//!
//! Observed and not explained (retail newchar 2026-09-18, `Mount-quest`): a Light Ration craft
//! gave Cooking 16 where 4.6 x 1 x 2 = 9.2, a Mount Vittles craft 216 where 6.9 x 6 x 2 = 82.8;
//! the smelting, arcana and research crafts of the other ledgers match the formula exactly. The
//! cooking factor is Unknown and not applied.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_content::progression::{Mastery, RankPerks};
use nw_content::rewards::Reward;
use nw_content::tradeskill_xp::{NEUTRAL_MODIFIER, gather_reward};

use super::quest_objects::{gain_experience, grant};
use super::{Player, SimWorld};
use crate::charting::{NEUTRAL_STANDING_MODIFIER, RANK_UP_EVENT};
use crate::player_content::PlayerContent;

/// How the reward of one firing scales.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct EventScale {
    /// The units one firing counts: a craft's ingredient quantities, 1 otherwise.
    units: f64,
    /// The gathered creature level's `GatherableLevelModifier` ([`NEUTRAL_MODIFIER`] for none).
    level_modifier: f64,
}

/// The units of a firing that counts once.
const ONE_UNIT: f64 = 1.0;

impl EventScale {
    /// A firing that gives its reward as it is (a salvage, a node gather, a fishing event).
    pub(super) const PLAIN: Self = Self {
        units: ONE_UNIT,
        level_modifier: NEUTRAL_MODIFIER,
    };

    /// A craft of a recipe whose ingredient quantities sum to `ingredients`.
    pub(super) fn craft(ingredients: u32) -> Self {
        Self {
            units: f64::from(ingredients),
            ..Self::PLAIN
        }
    }

    /// A gather of a creature whose level has modifier `level_modifier`.
    pub(super) fn gather(level_modifier: f64) -> Self {
        Self {
            level_modifier,
            ..Self::PLAIN
        }
    }

    /// The progression one firing of reward `reward` gives, before the multiplier.
    fn progression(self, reward: f64) -> f64 {
        gather_reward(reward, self.level_modifier) * self.units
    }

    /// The standing one firing of standing `standing` gives, before the multiplier.
    fn standing(self, standing: f64) -> f64 {
        standing * self.units
    }
}

/// A whole number of points of `value`: negative is 0, above `u32::MAX` is `u32::MAX`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in range: checked first; the fraction goes"
)]
fn whole_points(value: f64) -> u32 {
    if value >= f64::from(u32::MAX) {
        u32::MAX
    } else if value > 0.0 {
        value as u32
    } else {
        0
    }
}

/// The experience `times` firings of a reward `reward` give under multiplier `multiplier`,
/// rounded to the nearest point (kills, catches and songs).
pub(super) fn progression_gain(reward: f64, times: u32, multiplier: f64) -> u32 {
    whole_points((reward * f64::from(times) * multiplier).round())
}

/// The points `times` firings of `per_firing` points give under multiplier `multiplier`,
/// rounded down (gathers, crafts and salvages; see the module note). The product is
/// single precision: `180 * 1.05` is 188.99998 there and floors to 188, which is what
/// retail Arcana gave.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the retail product is single precision; the fraction goes"
)]
pub(super) fn event_gain(per_firing: f64, times: u32, multiplier: f64) -> u32 {
    let product = (per_firing as f32) * (times as f32) * (multiplier as f32);
    whole_points(f64::from(product.floor()))
}

impl SimWorld {
    /// Player `number` fires game event `event` `times` times by salvaging or fishing: as
    /// [`Self::grant_scaled_event`] with each firing counted once.
    pub(super) fn grant_event(&mut self, number: u64, event: &str, times: u32, now_ms: u64) {
        self.grant_scaled_event(number, event, times, EventScale::PLAIN, now_ms);
    }

    /// Player `number` fires game event `event` `times` times, each scaled by `scale`: the
    /// categorical progression it names gains its experience, the territory the player is in
    /// its standing, its coins (each firing rolled) go to the wallet, and the player earns its
    /// achievement. An event with a loot limit counts each firing on it; a firing at the limit
    /// grants nothing and fires the event's limit-reached event instead. An event without a
    /// grant, or a progression the content does not know, gives nothing of it (logged).
    pub(super) fn grant_scaled_event(
        &mut self,
        number: u64,
        event: &str,
        times: u32,
        scale: EventScale,
        now_ms: u64,
    ) {
        let content = Arc::clone(&self.content);
        let Some(event_grant) = content.event_grants.get(event) else {
            return;
        };
        let times = match &event_grant.loot_limit {
            Some(limit) => {
                let passed = self.limited_firings(number, limit, times);
                let held_back = times.saturating_sub(passed);
                if held_back > 0 {
                    tracing::debug!(player = number, event, limit = %limit, held_back, "event at its loot limit");
                    if let Some(reached) = &event_grant.limit_reached {
                        self.grant_scaled_event(number, reached, held_back, scale, now_ms);
                    }
                }
                passed
            }
            None => times,
        };
        if times == 0 {
            return;
        }
        let dice = &mut self.dice;
        let coins = (0..times).fold(0_u64, |total, _| {
            total.saturating_add(event_grant.coins_rolled(&mut |below| dice.below(below)))
        });
        if let Some((id, reward)) = &event_grant.progression {
            self.gain_categorical(number, event, id, scale.progression(*reward), times);
        }
        if event_grant.territory_standing > 0.0 {
            let standing = scale.standing(event_grant.territory_standing);
            match self.current_territory(number) {
                Some(territory) => {
                    self.gain_categorical(number, event, &territory, standing, times);
                }
                None => {
                    tracing::debug!(player = number, event, "standing outside a territory");
                }
            }
        }
        if coins > 0
            && let Some(mut player) = self
                .player_by_number(number)
                .and_then(|entity| self.world.get_mut::<Player>(entity))
        {
            let reward = Reward {
                coins,
                ..Reward::default()
            };
            grant(&mut player, &reward, &content.gameplay);
        }
        if let Some(achievement) = &event_grant.achievement {
            self.earn_achievement(number, achievement, now_ms);
        }
    }

    /// Player `number` gets the territory standing of game event `event` once (a quest's turn-in
    /// or a quest task's event; its coins and experience come with the quest's reward).
    pub(super) fn grant_event_standing(&mut self, number: u64, event: &str) {
        let content = Arc::clone(&self.content);
        let Some(standing) = content
            .event_grants
            .get(event)
            .map(|grant| grant.territory_standing)
            .filter(|standing| *standing > 0.0)
        else {
            return;
        };
        if let Some(territory) = self.current_territory(number) {
            self.gain_categorical(number, event, &territory, standing, ONE_FIRING);
        }
    }

    /// Player `number` gets the categorical progression and the achievement of game event
    /// `event` once: the part of a quest's success event that its reward does not hold (Observed,
    /// content bundle 25810808: `9401_BefriendHorse_Success`, the mount quest's success event,
    /// gives `Riding` 500).
    pub(super) fn grant_event_progression(&mut self, number: u64, event: &str, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let Some(event_grant) = content.event_grants.get(event) else {
            return;
        };
        if let Some((id, reward)) = &event_grant.progression {
            self.gain_categorical(number, event, id, *reward, ONE_FIRING);
        }
        if let Some(achievement) = &event_grant.achievement {
            self.earn_achievement(number, achievement, now_ms);
        }
    }

    /// Categorical progression `id` of player `number` gains `times` firings of `per_firing`
    /// points under its remote multiplier, for game event `event`.
    fn gain_categorical(
        &mut self,
        number: u64,
        event: &str,
        id: &str,
        per_firing: f64,
        times: u32,
    ) {
        let content = Arc::clone(&self.content);
        let Some(mastery) = content.progression.categorical(id) else {
            tracing::debug!(player = number, event, progression = %id, "progression of no known table");
            return;
        };
        let Some(mut player) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
        else {
            return;
        };
        let gain = event_gain(per_firing, times, content.gameplay.xp.progression(id));
        tracing::debug!(player = number, event, progression = %id, gain, "progression gained");
        gain_progression(&mut player, &content, mastery, gain);
    }

    /// The territory player `number` is in: the latest territory it entered whose id is a
    /// categorical progression (a territory's standing), or the territory of the latest area it
    /// entered (2297 names an area, not its territory: [`crate::charting`]); `None` outside
    /// every one.
    pub(super) fn current_territory(&mut self, number: u64) -> Option<String> {
        let entity = self.player_by_number(number)?;
        let territories = self.world.get::<Player>(entity)?.territories.clone();
        territories
            .into_iter()
            .rev()
            .map(|id| {
                let id = u32::from(id);
                self.content.charting.territory_of(id).unwrap_or(id)
            })
            .map(|id| id.to_string())
            .find(|id| self.content.progression.categorical(id).is_some())
    }

    /// The rank of tradeskill `id` of player `entity` and what it gives; rank 0's for a
    /// tradeskill the player has no experience in, the default for an id that is no
    /// tradeskill.
    pub(super) fn tradeskill_perks(&self, entity: Entity, id: &str) -> (u16, RankPerks) {
        let Some(mastery) = self.content.progression.tradeskill(id) else {
            return (0, RankPerks::default());
        };
        let experience = self
            .world
            .get::<Player>(entity)
            .map_or(0, |player| player.masteries.experience(mastery));
        (mastery.rank_of(experience).0, mastery.perks(experience))
    }
}

/// One firing.
pub(super) const ONE_FIRING: u32 = 1;

/// The masteries a player arrives with: `saved` (id to total experience), or for a character
/// that has none yet (a new one) the ranks its archetype pick `backstory` starts with
/// ([`nw_content::starting_ranks`]: retail `Archetype_Swordsman` logged in with Cooking,
/// Leatherworking and Fishing at rank 50), each at the start of its rank. A rank of a
/// progression the content does not know is left out.
pub(super) fn arrival_masteries(
    saved: &BTreeMap<String, u32>,
    backstory: Option<&str>,
    content: &PlayerContent,
) -> BTreeMap<String, u32> {
    if !saved.is_empty() {
        return saved.clone();
    }
    backstory
        .map(|backstory| content.starting_ranks.of(backstory))
        .unwrap_or_default()
        .iter()
        .filter_map(|(id, rank)| {
            let mastery = content.progression.categorical(id)?;
            Some((mastery.id.clone(), mastery.experience_for_rank(*rank)))
        })
        .collect()
}

/// `player`'s progression `mastery` gains `gain` points. Each rank it reaches is a rank up event:
/// its `XPReward`, times the territory's standing modifier for a territory's standing
/// ([`crate::charting`]: 100 x 1.25 = 125 at Windsward's rank 1), is the event's amount of
/// character experience. Every progression gain goes through here (tradeskills, fishing, music,
/// standing).
pub(super) fn gain_progression(
    player: &mut Player,
    content: &PlayerContent,
    mastery: &Mastery,
    gain: u32,
) {
    let (rank_before, _) = mastery.rank_of(player.masteries.experience(mastery));
    player.masteries.gain(mastery, gain);
    let (rank_after, _) = mastery.rank_of(player.masteries.experience(mastery));
    let standing_modifier = mastery
        .id
        .parse()
        .ok()
        .and_then(|territory| content.charting.experience(territory))
        .map_or(NEUTRAL_STANDING_MODIFIER, |territory| {
            territory.standing_modifier
        });
    let per_amount = content.rewards.experience(RANK_UP_EVENT);
    for rank in rank_before.saturating_add(1)..=rank_after {
        let amount = f64::from(mastery.rank_reward(rank)) * standing_modifier;
        let points = progression_gain(amount, ONE_FIRING, f64::from(per_amount));
        gain_experience(player, points, &content.gameplay.xp);
        tracing::debug!(progression = %mastery.id, rank, points, "rank up");
    }
}

#[cfg(test)]
mod tests {
    use nw_content::progression::{Progression, ProgressionSource};
    use nw_content::rewards::{EventGrant, EventGrants};

    use super::*;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, world_with_spawns};

    const PLAYER: u64 = 3;
    const RATION_EVENT: &str = "Craft_CookingT1";
    /// Retail Windsward: territory 6, standing multiplier 3.
    const WINDSWARD: u16 = 6;
    const STANDING_MULTIPLIER: f64 = 3.0;
    /// A point of interest inside it: no standing of its own.
    const POI: u16 = 30_129;

    #[test]
    fn a_gain_is_the_reward_times_the_firings_and_the_multiplier() {
        // Retail: two crafts of standing 1 under multiplier 3 gave 6.
        assert_eq!(progression_gain(1.0, 2, 3.0), 6);
        // `Gather_MiningT1` under Mining's 2.
        assert_eq!(progression_gain(64.0, 1, 2.0), 128);
        assert_eq!(progression_gain(0.2, 1, 1.0), 0);
        assert_eq!(progression_gain(-1.0, 1, 1.0), 0);
        assert_eq!(progression_gain(f64::MAX, 2, 2.0), u32::MAX);
    }

    #[test]
    fn an_event_gain_rounds_down() {
        // Retail Arcana: `Craft_ArcanaT2` 18 x 5 ingredients x 2 x 1.05 gave 188.
        assert_eq!(event_gain(18.0 * 5.0, 1, 2.0 * 1.05), 188);
        // `IngotT3`: 46 x 6 ingredients, 894 crafts under 2: 493,488.
        assert_eq!(
            event_gain(EventScale::craft(6).progression(46.0), 894, 2.0),
            493_488
        );
        // A level-3 boar: 95 x 1.1027 = 104 before Skinning's 2.
        assert_eq!(
            event_gain(EventScale::gather(1.102_657_5).progression(95.0), 1, 2.0),
            208
        );
        assert_eq!(event_gain(4.6, 2, 2.0), 18);
        assert_eq!(event_gain(-1.0, 1, 1.0), 0);
        assert_eq!(EventScale::PLAIN.standing(1.0), 1.0);
    }

    #[test]
    fn a_new_character_starts_with_its_archetypes_ranks() {
        use nw_content::progression::{TradeskillRankRow, TradeskillSource};
        use nw_content::starting_ranks::StartingRanks;
        const ARCHETYPE: &str = "Archetype_Swordsman";
        const STARTING_RANK: u16 = 2;
        let content = PlayerContent {
            progression: Progression::default().with_tradeskills([TradeskillSource {
                id: "Cooking".to_owned(),
                max_levels: vec![200],
                ranks: (0..4)
                    .map(|rank| TradeskillRankRow {
                        rank,
                        cap: 3.0,
                        perks: RankPerks::default(),
                    })
                    .collect(),
            }]),
            starting_ranks: StartingRanks::build([(
                ARCHETYPE.to_owned(),
                vec![
                    ("Cooking".to_owned(), STARTING_RANK),
                    ("Unknown".to_owned(), 5),
                ],
            )]),
            ..PlayerContent::default()
        };
        let fresh = arrival_masteries(&BTreeMap::new(), Some(ARCHETYPE), &content);
        assert_eq!(fresh, BTreeMap::from([("Cooking".to_owned(), 6)]));
        let saved = BTreeMap::from([("Cooking".to_owned(), 1)]);
        assert_eq!(arrival_masteries(&saved, Some(ARCHETYPE), &content), saved);
        assert!(arrival_masteries(&BTreeMap::new(), None, &content).is_empty());
    }

    #[test]
    fn a_craft_raises_the_standing_of_the_territory_the_player_is_in() {
        let mut world = world_with_spawns(Vec::new());
        let mut content = PlayerContent {
            progression: Progression::default().with_progressions([ProgressionSource {
                id: WINDSWARD.to_string(),
                max_level: 300,
                auto_rank_up: true,
                ranks: vec![(0, 150), (1, 375)],
            }]),
            event_grants: EventGrants::build([(
                RATION_EVENT.to_owned(),
                EventGrant {
                    progression: None,
                    coins: (0, 0),
                    territory_standing: 1.0,
                    achievement: None,
                    ..EventGrant::default()
                },
            )]),
            ..PlayerContent::default()
        };
        content.gameplay.xp.progression = [(WINDSWARD.to_string(), STANDING_MULTIPLIER)].into();
        world.content = Arc::new(content);
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(PLAYER).expect("player");
        let standing = |world: &SimWorld| {
            let mastery = world
                .content
                .progression
                .categorical(&WINDSWARD.to_string())
                .expect("standing")
                .clone();
            world
                .world
                .get::<Player>(entity)
                .map(|player| player.masteries.experience(&mastery))
        };
        world.grant_scaled_event(PLAYER, RATION_EVENT, 2, EventScale::craft(1), NOW);
        assert_eq!(standing(&world), Some(0), "outside every territory");
        world.territory_changed(entity, WINDSWARD, true);
        world.territory_changed(entity, POI, true);
        // Retail newchar: two Light Rations (one pork each) gave territory 6 standing 6.
        world.grant_scaled_event(PLAYER, RATION_EVENT, 2, EventScale::craft(1), NOW);
        assert_eq!(standing(&world), Some(6));
        world.grant_event_standing(PLAYER, RATION_EVENT);
        assert_eq!(standing(&world), Some(9));
    }

    #[test]
    fn an_event_with_a_loot_limit_grants_up_to_the_limit_then_its_reached_event() {
        use nw_content::loot_limits::{LimitRow, LootLimitRules, LootLimits, ResetClock, Weekday};
        // Content bundle 25810808: `Mount_GrantRidingXP_Small` gives `Riding` 2,500 and counts on
        // `Mount_FoodDailyXP_Limit` (3 a day); its limit-reached event gives `Riding` 0, here a
        // test progression to see it fire.
        const FOOD_EVENT: &str = "Mount_GrantRidingXP_Small";
        const REACHED_EVENT: &str = "Mount_FoodDailyXP_LimitReached";
        const LIMIT: &str = "Mount_FoodDailyXP_Limit";
        const RIDING: &str = "Riding";
        const REACHED_MARK: &str = "Test_Limit_Reached";
        const RIDING_REWARD: f64 = 2_500.0;
        const DAILY_USES: u32 = 3;
        let mut world = world_with_spawns(Vec::new());
        let progression = |id: &str| ProgressionSource {
            id: id.to_owned(),
            max_level: 200,
            auto_rank_up: true,
            ranks: vec![(0, 1_000_000)],
        };
        world.content = Arc::new(PlayerContent {
            progression: Progression::default()
                .with_progressions([progression(RIDING), progression(REACHED_MARK)]),
            event_grants: EventGrants::build([
                (
                    FOOD_EVENT.to_owned(),
                    EventGrant {
                        progression: Some((RIDING.to_owned(), RIDING_REWARD)),
                        loot_limit: Some(LIMIT.to_owned()),
                        limit_reached: Some(REACHED_EVENT.to_owned()),
                        ..EventGrant::default()
                    },
                ),
                (
                    REACHED_EVENT.to_owned(),
                    EventGrant {
                        progression: Some((REACHED_MARK.to_owned(), 1.0)),
                        ..EventGrant::default()
                    },
                ),
            ]),
            loot_limits: Some(LootLimitRules {
                limits: LootLimits::build([LimitRow {
                    id: LIMIT,
                    count_limit: i64::from(DAILY_USES),
                    expire_seconds: 86_400,
                    max_limit_mult: 64_800,
                    replicated: true,
                }]),
                clock: ResetClock {
                    hour_utc: 3,
                    weekly_day: Weekday::Tuesday,
                },
            }),
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(PLAYER).expect("player");
        let experience = |world: &SimWorld, id: &str| {
            let mastery = world
                .content
                .progression
                .categorical(id)
                .expect("progression");
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .masteries
                .experience(mastery)
        };
        for _ in 0..DAILY_USES {
            world.grant_event(PLAYER, FOOD_EVENT, ONE_FIRING, NOW);
        }
        assert_eq!(
            experience(&world, RIDING),
            RIDING_REWARD as u32 * DAILY_USES
        );
        assert_eq!(experience(&world, REACHED_MARK), 0);
        world.grant_event(PLAYER, FOOD_EVENT, ONE_FIRING, NOW);
        assert_eq!(
            experience(&world, RIDING),
            RIDING_REWARD as u32 * DAILY_USES,
            "the fourth firing is at the limit"
        );
        assert_eq!(
            experience(&world, REACHED_MARK),
            1,
            "it fired the reached event"
        );
    }

    #[test]
    fn a_quest_s_success_event_gives_its_progression_and_achievement() {
        // Content bundle 25810808: `9401_BefriendHorse_Success`, the mount quest's success
        // event, gives `Riding` 500; its reward holds no progression.
        const SUCCESS_EVENT: &str = "9401_BefriendHorse_Success";
        const RIDING: &str = "Riding";
        const RIDING_REWARD: f64 = 500.0;
        const MARK: &str = "Test_Event_Achievement";
        const MARK_BIT: u32 = 77;
        let mut world = world_with_spawns(Vec::new());
        let indexes =
            nw_content::achievements::AchievementIndexes::build([(MARK.to_owned(), MARK_BIT)]);
        world.achievement_rules = crate::world::level_flow::AchievementRules::of(&indexes, &[]);
        world.content = Arc::new(PlayerContent {
            progression: Progression::default().with_progressions([ProgressionSource {
                id: RIDING.to_owned(),
                max_level: 200,
                auto_rank_up: true,
                ranks: vec![(0, 1_000), (1, 2_000)],
            }]),
            event_grants: EventGrants::build([(
                SUCCESS_EVENT.to_owned(),
                EventGrant {
                    progression: Some((RIDING.to_owned(), RIDING_REWARD)),
                    achievement: Some(MARK.to_owned()),
                    ..EventGrant::default()
                },
            )]),
            achievements: indexes,
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.player_by_number(PLAYER).expect("player");
        world.grant_event_progression(PLAYER, SUCCESS_EVENT, NOW);
        let riding = world
            .content
            .progression
            .categorical(RIDING)
            .expect("riding")
            .clone();
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!(player.masteries.experience(&riding), 500);
        assert!(player.flow.has_achievement(&world.achievement_rules, MARK));
    }

    #[test]
    fn a_faction_rank_task_reads_the_faction_s_progression() {
        use crate::objectives::{TaskBook, TaskRow};
        use crate::player_content::{ObjectiveRow, QuestBook};
        // Content bundle 25810808: `9804_M_0001_d_02_Reach_MarauderRank_1` checks 3,000 points
        // of the categorical progression `Marauder`.
        const MARAUDER: &str = "Marauder";
        const RANK_POINTS: i64 = 3_000;
        const MISSION_EVENT: &str = "Test_Marauder_Mission";
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            progression: Progression::default().with_progressions([ProgressionSource {
                id: MARAUDER.to_owned(),
                max_level: 10,
                auto_rank_up: true,
                ranks: vec![(0, 3_000), (1, 6_000)],
            }]),
            event_grants: EventGrants::build([(
                MISSION_EVENT.to_owned(),
                EventGrant {
                    progression: Some((MARAUDER.to_owned(), 3_000.0)),
                    ..EventGrant::default()
                },
            )]),
            quests: QuestBook::build(
                [ObjectiveRow {
                    id: "q_rank".to_owned(),
                    root: "t_root".to_owned(),
                    ..ObjectiveRow::default()
                }],
                TaskBook::new(vec![
                    task("t_root", "ConsecutiveTaskContainer", &["t_rank", "t_after"]),
                    TaskRow {
                        kill_enemy_type: Some("Test_Never".to_owned()),
                        target_qty: Some(1),
                        ..task("t_after", "TaskGameEvent", &[])
                    },
                    TaskRow {
                        item_name: Some(MARAUDER.to_owned()),
                        target_qty: Some(RANK_POINTS),
                        should_check_points: true,
                        ..task("t_rank", "TaskHaveLevel", &[])
                    },
                ]),
            ),
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        world.start_quest(PLAYER, "q_rank", NOW).expect("start");
        let rank_done = |world: &SimWorld| world.quests_of_player(PLAYER)[0].run.is_done("t_rank");
        world.tick(NOW, SERVER_TIME).expect("tick");
        assert!(!rank_done(&world), "no points yet");
        world.grant_event_progression(PLAYER, MISSION_EVENT, NOW);
        world.tick(NOW + 50, SERVER_TIME + 50).expect("tick");
        assert!(rank_done(&world), "3,000 Marauder points");
    }
}
