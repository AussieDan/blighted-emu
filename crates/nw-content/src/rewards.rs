// SPDX-License-Identifier: AGPL-3.0-or-later
//! The rewards of game events (beach-quests note, section 1): a quest's `SuccessGameEventID`
//! names a `GameEventData` row whose `CurrencyReward` coins, `UniversalExpAmount` experience and
//! `ItemReward` the player gets at the turn-in (retail: 150 + 225 coins after the first two beach
//! quests). An event can also name a whisper, a voice line the client plays (retail 2026-09-18:
//! `01_IsabellasFirstWhisper_Isabella_01` in Amidst the Wreckage).

use std::collections::HashMap;

use crate::Tables;

/// What one game event gives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reward {
    /// `CurrencyReward`: coins.
    pub coins: u64,
    /// `UniversalExpAmount`: experience points.
    pub experience: u32,
    /// `ItemReward` and `ItemRewardQty`.
    pub item: Option<(String, u32)>,
    /// `WhisperID`, else `WhisperId`: the whisper the client plays.
    pub whisper: Option<String>,
}

/// Every game event's reward by lowercase event id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewards {
    by_id: HashMap<String, Reward>,
}

/// A number cell (`"225"`); 0 for an empty or other cell.
fn number_of<T: std::str::FromStr + Default>(cell: Option<&str>) -> T {
    cell.and_then(|text| text.trim().parse().ok())
        .unwrap_or_default()
}

impl Rewards {
    /// The rewards of `GameEventData` in `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .game_event_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    let item =
                        row.item_reward
                            .clone()
                            .filter(|item| !item.is_empty())
                            .map(|item| {
                                let quantity = row
                                    .item_reward_qty
                                    .and_then(|quantity| u32::try_from(quantity).ok())
                                    .filter(|quantity| *quantity > 0)
                                    .unwrap_or(1);
                                (item, quantity)
                            });
                    (
                        row.event_id.clone(),
                        Reward {
                            coins: number_of(row.currency_reward.as_deref()),
                            experience: number_of(row.universal_exp_amount.as_deref()),
                            item,
                            whisper: row
                                .whisper_id
                                .clone()
                                .or_else(|| row.whisper_id_2.clone())
                                .filter(|whisper| !whisper.is_empty()),
                        },
                    )
                }),
        )
    }

    /// The rewards of `events` (event id, reward); the first row of an id wins.
    pub fn build(events: impl IntoIterator<Item = (String, Reward)>) -> Self {
        let mut by_id = HashMap::new();
        for (id, reward) in events {
            by_id.entry(id.to_lowercase()).or_insert(reward);
        }
        Self { by_id }
    }

    /// The reward of event `id` (any case).
    pub fn get(&self, id: &str) -> Option<&Reward> {
        self.by_id.get(&id.to_lowercase())
    }
}

/// Separates the low and high end of a `CurrencyReward` range (`2000-5000`).
const RANGE_SEPARATOR: char = '-';

/// What a game event grants besides its quest reward: the categorical progression it raises
/// and its coins as a range.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventGrant {
    /// `CategoricalProgressionId` and `CategoricalProgressionReward`: the tradeskill (or other
    /// categorical progression) and the experience one firing gives it, before the remote
    /// config's progression multiplier (Observed, content bundle 25810808: `Gather_MiningT1`
    /// gives Mining 64, `Craft_CookingT1` gives Cooking 4.6).
    pub progression: Option<(String, f64)>,
    /// `CurrencyReward` as its low and high end: a single number is both (`SalvageCoinGrantT2`
    /// 25), a range rolls between them (`EventBagofJuniperBerriesLVL30` `2000-5000`).
    pub coins: (u64, u64),
    /// `TerritoryStanding`: the standing one firing gives the territory the player is in,
    /// before the remote config's multiplier of that territory (Observed, retail newchar
    /// 2026-09-18: two Light Ration crafts of `Craft_CookingT1`, standing 1, gave territory 6
    /// standing 6 under multiplier 3). 0 for an event without one.
    pub territory_standing: f64,
    /// `AchievementId`: the achievement one firing earns (Observed, content bundle 25810808:
    /// `Reward_MountReward_Box_1`, the salvage event of the mount quest's reward box, earns
    /// `AwardAchievement_Mount_Horse_1`, the achievement that owns the horse `Mount_Horse_1`).
    pub achievement: Option<String>,
    /// `LootLimitID`: the loot limit each firing counts 1 on (the mount foods'
    /// `Mount_FoodDailyXP_Limit`, 3 a day). Inferred from the column names: a firing at the
    /// limit grants nothing and fires `limit_reached` instead.
    pub loot_limit: Option<String>,
    /// `LootLimitReachedGameEventID`: the event a firing at the limit fires
    /// (`Mount_FoodDailyXP_LimitReached`, `Riding` 0).
    pub limit_reached: Option<String>,
}

impl EventGrant {
    /// Its coins, with `below(n)` a random number below `n`.
    pub fn coins_rolled(&self, below: &mut impl FnMut(u32) -> u32) -> u64 {
        let (low, high) = self.coins;
        let spread = u32::try_from(high.saturating_sub(low)).unwrap_or(u32::MAX);
        low.saturating_add(u64::from(below(spread.saturating_add(1))))
    }
}

/// The grant of every game event by lowercase event id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventGrants {
    by_id: HashMap<String, EventGrant>,
}

/// A `CurrencyReward` cell as its low and high end; `(0, 0)` for an empty or other cell.
fn coin_range(cell: Option<&str>) -> (u64, u64) {
    let Some(text) = cell.map(str::trim).filter(|text| !text.is_empty()) else {
        return (0, 0);
    };
    match text.split_once(RANGE_SEPARATOR) {
        Some((low, high)) => {
            let low = number_of::<u64>(Some(low));
            (low, number_of::<u64>(Some(high)).max(low))
        }
        None => {
            let coins = number_of(Some(text));
            (coins, coins)
        }
    }
}

impl EventGrants {
    /// The grants of `GameEventData` in `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .game_event_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| {
                    let progression = row
                        .categorical_progression_id
                        .clone()
                        .filter(|id| !id.is_empty())
                        .zip(row.categorical_progression_reward)
                        .filter(|(_, amount)| *amount > 0.0);
                    (
                        row.event_id.clone(),
                        EventGrant {
                            progression,
                            coins: coin_range(row.currency_reward.as_deref()),
                            territory_standing: number_of(row.territory_standing.as_deref()),
                            achievement: row
                                .achievement_id
                                .clone()
                                .filter(|id| !id.trim().is_empty()),
                            loot_limit: row
                                .loot_limit_id
                                .clone()
                                .filter(|id| !id.trim().is_empty()),
                            limit_reached: row
                                .loot_limit_reached_game_event_id
                                .clone()
                                .filter(|id| !id.trim().is_empty()),
                        },
                    )
                }),
        )
    }

    /// The grants of `events` (event id, grant); the first row of an id wins.
    pub fn build(events: impl IntoIterator<Item = (String, EventGrant)>) -> Self {
        let mut by_id = HashMap::new();
        for (id, grant) in events {
            by_id.entry(id.to_lowercase()).or_insert(grant);
        }
        Self { by_id }
    }

    /// The grant of event `id` (any case).
    pub fn get(&self, id: &str) -> Option<&EventGrant> {
        self.by_id.get(&id.to_lowercase())
    }
}

/// The experience of a mob kill by the mob's level: `RewardData.KillExperience` (Observed,
/// content bundle 25810808: 10 at level 1, 11 at level 2). Retail gave 20 for each level 1 kill
/// of the tutorial (tutorial note, section 8): this base times
/// `javelin.xp-multipliers.character` 2 (Inferred).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KillExperience {
    by_level: HashMap<u32, u32>,
}

impl KillExperience {
    /// The kill experience of `RewardData` in `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .reward_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| (row.level, row.kill_experience)),
        )
    }

    /// The kill experience of `rows` (level, points); the first row of a level wins, and rows
    /// out of range are left out.
    pub fn build(rows: impl IntoIterator<Item = (i64, i64)>) -> Self {
        let mut by_level = HashMap::new();
        for (level, points) in rows {
            if let (Ok(level), Ok(points)) = (u32::try_from(level), u32::try_from(points)) {
                by_level.entry(level).or_insert(points);
            }
        }
        Self { by_level }
    }

    /// The experience of a kill of a level `level` mob, before the multipliers.
    pub fn of(&self, level: u32) -> Option<u32> {
        self.by_level.get(&level).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_gives_its_coins_experience_and_item() {
        let rewards = Rewards::build([(
            "01_skinningknife_success".to_owned(),
            Reward {
                coins: number_of(Some("225")),
                experience: number_of(Some("275")),
                item: Some(("QuestReward_MB_Meat_Pouch".to_owned(), 1)),
                whisper: None,
            },
        )]);
        let reward = rewards.get("01_SkinningKnife_Success").expect("reward");
        assert_eq!(reward.coins, 225);
        assert_eq!(reward.experience, 275);
        assert_eq!(number_of::<u64>(Some("")), 0);
        assert!(rewards.get("other").is_none());
    }

    #[test]
    fn an_event_grants_its_progression_and_a_coin_range() {
        assert_eq!(coin_range(Some("25")), (25, 25));
        assert_eq!(coin_range(Some("2000-5000")), (2000, 5000));
        assert_eq!(coin_range(Some("")), (0, 0));
        assert_eq!(coin_range(None), (0, 0));
        let grants = EventGrants::build([
            (
                "Gather_MiningT1".to_owned(),
                EventGrant {
                    progression: Some(("Mining".to_owned(), 64.0)),
                    coins: (0, 0),
                    territory_standing: 0.0,
                    achievement: None,
                    ..EventGrant::default()
                },
            ),
            (
                "EventBagofJuniperBerriesLVL30".to_owned(),
                EventGrant {
                    progression: None,
                    coins: coin_range(Some("2000-5000")),
                    territory_standing: number_of(Some("1")),
                    achievement: None,
                    ..EventGrant::default()
                },
            ),
        ]);
        let mining = grants.get("gather_miningt1").expect("mining");
        assert_eq!(mining.progression, Some(("Mining".to_owned(), 64.0)));
        let bag = grants.get("EVENTBAGOFJUNIPERBERRIESLVL30").expect("bag");
        assert_eq!(bag.coins_rolled(&mut |_| 0), 2000);
        assert_eq!(bag.coins_rolled(&mut |below| below - 1), 5000);
        assert_eq!(bag.territory_standing, 1.0);
        assert_eq!(mining.territory_standing, 0.0);
        assert!(grants.get("other").is_none());
    }

    #[test]
    fn a_kill_gives_the_experience_of_the_mob_level() {
        let kills = KillExperience::build([(1, 10), (2, 11), (1, 99), (-1, 5)]);
        assert_eq!(kills.of(1), Some(10), "the first row of a level wins");
        assert_eq!(kills.of(2), Some(11));
        assert_eq!(kills.of(3), None);
    }
}
