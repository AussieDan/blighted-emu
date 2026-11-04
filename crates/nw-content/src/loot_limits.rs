// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loot limits (`LootLimitData`, after the `javelin.dynamic-datatable-config` overrides): how
//! many of an item a player may loot, or how often a loot table may roll for it, before the
//! limit resets.
//!
//! **Observed** (retail captures, the loot tracker's `m_lootLimitDataMap`, scanned over 707
//! ledgers, 1,406 entries, `local-data/scratch-work/cfgvalues/lim-all.txt`): an entry is the
//! CRC-32 of the lowercase `LootLimitID`, two u64 times (unix ns), a u16 count and a u8. At the
//! kill of the newchar ledger (`newchar-retail-2026-09-18`, 2026-10-22 19:22 UTC) the drop of
//! `2hWarhammer_FTUE_CorruptedSailer_V2` made the entry of the limit with that id with count 1:
//! a limit counts the drops of the item whose id it has. Its end time was the next Tuesday
//! 09:00 UTC. Every entry of a limit with `LimitExpireSeconds` 604800 ends on a Tuesday (853
//! entries), every one with 64800 or 86400 at the next day's reset (553 entries), always at the
//! same hour of the server: 09:00 UTC on 31 ledgers (the newchar ledger among them) and 03:00 UTC
//! on 417. The reset hour is the server's ([`ResetClock`]).
//!
//! **Observed, the two times.** The second time is the end of the count, with a sub-second
//! part: the newchar entry ends at 1793091600.894621 s, its kill bundle came at
//! 1792696951.965 s. Inferred: the end is whole seconds after the moment the count started
//! (the `nightaven` `UmbralCurrency` end is 0.11 ms after its first count's sub-second part,
//! across 286,815 s). The first time is the moment of the last
//! count (`nightaven` ledger: the `UmbralCurrency` entry's first time moved with each of its
//! counts 45, 90, 135, while its end stayed). At the limit it is the end time instead, for the
//! limits whose `MaxLimitMult` is at least their `LimitExpireSeconds` (478 entries of 9 limits,
//! the newchar entry among them); `DailySoloTrialLimit` (`MaxLimitMult` 0) keeps the moment of
//! its last count at and over its limit (96 entries). Retail keeps an entry after its end and
//! sends it until a new count starts it again (the `nightaven` login state listed entries that
//! ended years before).
//!
//! **Inferred.** An item stops dropping once its count reached `CountLimit`; a drop counts its
//! quantity (the `UmbralCurrency` limit is 100,000 after the retail override, an amount). A
//! loot table with the condition `[LIM]X` rolls only while the count of limit `X` is below its
//! `CountLimit`, and one with `TriggerLimitOnVisit` counts 1 on `X` each time it rolls (from the
//! column names; the `PUGFinder_Expedition_*` entries count 1 to 3 against a limit of 3).
//! **Unknown.** The resets of the other `LimitExpireSeconds` values (0, 300, 1209600,
//! 2678400): those limits are not applied ([`LootLimits::unknown_reset`]), so their items drop
//! and their `[LIM]` tables roll freely. Whether a drop that passes the limit is cut to the rest
//! (here it drops whole), what `MinLimitSeconds`, `MaxLimitSeconds` and `MinLimitMult` change,
//! the first time at the limit when `MaxLimitMult` is between 0 and `LimitExpireSeconds` (sent
//! here as the moment of the last count), a count above the u16 of the wire (sent as its
//! largest value), and the u8 of an entry (0 in all 1,406).

use std::collections::HashMap;

use nw_common::time::SECONDS_PER_DAY;
use nw_formats::objectstream::lowercase_crc32;
use serde::Deserialize;

use crate::Tables;

/// `LimitExpireSeconds` of the limits that reset with each day (Observed).
const DAILY_EXPIRES: [i64; 2] = [64_800, 86_400];
/// `LimitExpireSeconds` of the limits that reset with each week (Observed).
const WEEKLY_EXPIRES: i64 = 604_800;
const SECONDS_PER_HOUR: i64 = 3_600;
/// Hours in one day: a reset hour is below it.
pub const HOURS_PER_DAY: u32 = 24;
const DAYS_PER_WEEK: i64 = 7;
/// 1970-01-01 was a Thursday: the weekday index (Monday 0) of Unix day 0.
const UNIX_DAY_ZERO_WEEKDAY: i64 = 3;
/// Nanoseconds in one second.
const NS_PER_SECOND: u64 = 1_000_000_000;
/// The count a [`LootLimitCounts::visit`] adds.
const VISIT_COUNT: u64 = 1;

/// When a limit's count goes back to 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetCycle {
    /// At the next daily reset.
    Daily,
    /// At the next weekly reset.
    Weekly,
}

/// One limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LootLimit {
    /// `CountLimit`.
    pub count_limit: u64,
    /// The reset of its count.
    pub cycle: ResetCycle,
    /// `IsReplicated`: the client gets its count.
    pub replicated: bool,
    /// Whether a count at the limit sends its end time as the time of its last count
    /// (`MaxLimitMult` at least `LimitExpireSeconds`, module notes).
    pub reached_shows_end: bool,
}

/// One `LootLimitData` row as [`LootLimits::build`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitRow<'a> {
    /// `LootLimitID`.
    pub id: &'a str,
    /// `CountLimit`.
    pub count_limit: i64,
    /// `LimitExpireSeconds`.
    pub expire_seconds: i64,
    /// `MaxLimitMult`.
    pub max_limit_mult: i64,
    /// `IsReplicated`.
    pub replicated: bool,
}

/// Every applied limit by lowercase `LootLimitID`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LootLimits {
    by_id: HashMap<String, LootLimit>,
    unknown_reset: Vec<String>,
}

impl LootLimits {
    /// The limits of `tables`.
    pub fn from_tables(tables: &Tables) -> Self {
        Self::build(
            tables
                .loot_limit_data
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| LimitRow {
                    id: row.loot_limit_id.as_str(),
                    count_limit: row.count_limit,
                    expire_seconds: row.limit_expire_seconds,
                    max_limit_mult: row.max_limit_mult,
                    replicated: row.is_replicated,
                }),
        )
    }

    /// The limits of `rows`; the first row of an id wins.
    pub fn build<'a>(rows: impl IntoIterator<Item = LimitRow<'a>>) -> Self {
        let mut limits = Self::default();
        for row in rows {
            let cycle = if DAILY_EXPIRES.contains(&row.expire_seconds) {
                ResetCycle::Daily
            } else if row.expire_seconds == WEEKLY_EXPIRES {
                ResetCycle::Weekly
            } else {
                limits.unknown_reset.push(row.id.to_owned());
                continue;
            };
            limits
                .by_id
                .entry(row.id.to_lowercase())
                .or_insert(LootLimit {
                    count_limit: u64::try_from(row.count_limit).unwrap_or_default(),
                    cycle,
                    replicated: row.replicated,
                    reached_shows_end: row.max_limit_mult > 0
                        && row.max_limit_mult >= row.expire_seconds,
                });
        }
        limits
    }

    /// The limit with id `id` (any case): an item's own, or the one a `[LIM]` condition names.
    pub fn get(&self, id: &str) -> Option<&LootLimit> {
        self.by_id.get(&id.to_lowercase())
    }

    /// The ids of the limits whose reset is Unknown, which are not applied.
    pub fn unknown_reset(&self) -> &[String] {
        &self.unknown_reset
    }

    /// Applied limits.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether no limit applies.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Weekday {
    /// Monday.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday.
    Sunday,
}

impl Weekday {
    /// Its index, Monday 0.
    const fn index(self) -> i64 {
        match self {
            Self::Monday => 0,
            Self::Tuesday => 1,
            Self::Wednesday => 2,
            Self::Thursday => 3,
            Self::Friday => 4,
            Self::Saturday => 5,
            Self::Sunday => 6,
        }
    }
}

/// The server's resets: each day at `hour_utc`, and each week on `weekly_day` at that hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetClock {
    /// The reset hour, UTC, below [`HOURS_PER_DAY`].
    pub hour_utc: u32,
    /// The day of the weekly reset.
    pub weekly_day: Weekday,
}

impl ResetClock {
    /// The first reset of `cycle` after Unix second `now_s`.
    pub fn next(&self, cycle: ResetCycle, now_s: i64) -> i64 {
        let hour_s = i64::from(self.hour_utc) * SECONDS_PER_HOUR;
        let mut day = (now_s - hour_s).div_euclid(SECONDS_PER_DAY) + 1;
        if cycle == ResetCycle::Weekly {
            let weekday = (day + UNIX_DAY_ZERO_WEEKDAY).rem_euclid(DAYS_PER_WEEK);
            day += (self.weekly_day.index() - weekday).rem_euclid(DAYS_PER_WEEK);
        }
        day * SECONDS_PER_DAY + hour_s
    }

    /// The end of a count of `cycle` that starts at Unix nanosecond `now_ns`: whole seconds
    /// after it, at the first reset after it (module notes: the end keeps the start's
    /// sub-second part).
    pub fn end_ns(&self, cycle: ResetCycle, now_ns: u64) -> u64 {
        let now_s = i64::try_from(now_ns / NS_PER_SECOND).unwrap_or(i64::MAX);
        let ahead_s = u64::try_from(self.next(cycle, now_s) - now_s).unwrap_or_default();
        now_ns.saturating_add(ahead_s.saturating_mul(NS_PER_SECOND))
    }
}

/// The applied limits with the server's reset clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LootLimitRules {
    /// The limits.
    pub limits: LootLimits,
    /// The resets.
    pub clock: ResetClock,
}

/// One limit's count of a player, as a save or a handoff keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitCount {
    /// The lowercase `LootLimitID`.
    pub id: String,
    /// The count.
    pub count: u64,
    /// Unix nanoseconds of its last count.
    pub counted_ns: u64,
    /// Unix nanoseconds when it goes back to 0.
    pub ends_ns: u64,
}

/// One limit's count as the client gets it (`m_lootLimitDataMap`, module notes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownLimit {
    /// The CRC-32 of the lowercase `LootLimitID`.
    pub key: u32,
    /// The first time: the last count, or the end at the limit.
    pub first_ns: u64,
    /// The end of the count.
    pub ends_ns: u64,
    /// The count.
    pub count: u64,
}

/// One limit's count of a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Count {
    count: u64,
    counted_ns: u64,
    ends_ns: u64,
}

/// The counts of one player's limits, and whether they changed since the client last got them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LootLimitCounts {
    by_id: HashMap<String, Count>,
    changed: bool,
}

impl LootLimitCounts {
    /// The counts of a save or a handoff.
    pub fn from_counts(counts: impl IntoIterator<Item = LimitCount>) -> Self {
        Self {
            by_id: counts
                .into_iter()
                .map(|count| {
                    (
                        count.id.to_lowercase(),
                        Count {
                            count: count.count,
                            counted_ns: count.counted_ns,
                            ends_ns: count.ends_ns,
                        },
                    )
                })
                .collect(),
            changed: false,
        }
    }

    /// Every count, ended ones too, by id.
    pub fn counts(&self) -> Vec<LimitCount> {
        let mut counts: Vec<LimitCount> = self
            .by_id
            .iter()
            .map(|(id, count)| LimitCount {
                id: id.clone(),
                count: count.count,
                counted_ns: count.counted_ns,
                ends_ns: count.ends_ns,
            })
            .collect();
        counts.sort_unstable_by(|one, other| one.id.cmp(&other.id));
        counts
    }

    /// The drops of a roll that the player's limits let through at Unix nanosecond `now_ns`:
    /// an item whose limit is reached is left out; the others count.
    pub fn admit(
        &mut self,
        rules: &LootLimitRules,
        drops: Vec<(String, u32)>,
        now_ns: u64,
    ) -> Vec<(String, u32)> {
        drops
            .into_iter()
            .filter(|(item, quantity)| match rules.limits.get(item) {
                Some(limit) => self.add(rules.clock, limit, item, u64::from(*quantity), now_ns),
                None => true,
            })
            .collect()
    }

    /// Limit `id` counts 1 at Unix nanosecond `now_ns` (a roll visited a table with
    /// `TriggerLimitOnVisit` that names it, or a game event with its `LootLimitID` fired) unless
    /// it is at its limit. Returns whether it let the count through: `false` at the limit, `true`
    /// for a limit that is not applied, which counts nothing.
    pub fn count_one(&mut self, rules: &LootLimitRules, id: &str, now_ns: u64) -> bool {
        rules
            .limits
            .get(id)
            .is_none_or(|limit| self.add(rules.clock, limit, id, VISIT_COUNT, now_ns))
    }

    /// Counts `amount` on limit `limit` (id `id`) at `now_ns` unless it is reached; a count
    /// that ended starts again. Returns whether it counted.
    fn add(
        &mut self,
        clock: ResetClock,
        limit: &LootLimit,
        id: &str,
        amount: u64,
        now_ns: u64,
    ) -> bool {
        let entry = self.by_id.entry(id.to_lowercase()).or_insert(Count {
            count: 0,
            counted_ns: now_ns,
            ends_ns: clock.end_ns(limit.cycle, now_ns),
        });
        if now_ns >= entry.ends_ns {
            *entry = Count {
                count: 0,
                counted_ns: now_ns,
                ends_ns: clock.end_ns(limit.cycle, now_ns),
            };
        }
        if entry.count >= limit.count_limit {
            return false;
        }
        entry.count = entry.count.saturating_add(amount);
        entry.counted_ns = now_ns;
        self.changed = true;
        true
    }

    /// The count of limit `id` (any case) at Unix nanosecond `now_ns`.
    pub fn count(&self, id: &str, now_ns: u64) -> u64 {
        self.by_id
            .get(&id.to_lowercase())
            .filter(|count| now_ns < count.ends_ns)
            .map_or(0, |count| count.count)
    }

    /// The lowercase ids of the applied limits whose count is at the limit at `now_ns`: their
    /// `[LIM]` tables do not roll.
    pub fn reached(&self, rules: &LootLimitRules, now_ns: u64) -> Vec<String> {
        self.by_id
            .iter()
            .filter(|(id, count)| {
                now_ns < count.ends_ns
                    && rules
                        .limits
                        .get(id)
                        .is_some_and(|limit| count.count >= limit.count_limit)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The counts the client gets: those of the applied limits with `IsReplicated`, by key.
    pub fn shown(&self, rules: &LootLimitRules) -> Vec<ShownLimit> {
        let mut shown: Vec<ShownLimit> = self
            .by_id
            .iter()
            .filter_map(|(id, count)| {
                let limit = rules.limits.get(id).filter(|limit| limit.replicated)?;
                let at_end = limit.reached_shows_end && count.count >= limit.count_limit;
                Some(ShownLimit {
                    key: lowercase_crc32(id),
                    first_ns: if at_end {
                        count.ends_ns
                    } else {
                        count.counted_ns
                    },
                    ends_ns: count.ends_ns,
                    count: count.count,
                })
            })
            .collect();
        shown.sort_unstable_by_key(|limit| limit.key);
        shown
    }

    /// Whether a count changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-22 19:22:31 UTC, the newchar ledger's kill (a Thursday).
    const KILL_S: i64 = 1_792_696_951;
    /// The kill in unix ns: the sub-second part of the retail entry's end.
    const KILL_NS: u64 = 1_792_696_951_894_621_000;
    /// The retail entry's end, `0x18e2574f910f7548`: Tuesday 2026-10-27 09:00 UTC with the
    /// kill's sub-second part.
    const WEEKLY_END_NS: u64 = 0x18e2_574f_910f_7548;
    /// Tuesday 2026-10-27 09:00 UTC.
    const WEEKLY_END_S: i64 = 1_793_091_600;
    /// The next day's reset: Friday 2026-10-23 09:00 UTC.
    const DAILY_END_S: i64 = 1_792_746_000;
    const RETAIL_CLOCK: ResetClock = ResetClock {
        hour_utc: 9,
        weekly_day: Weekday::Tuesday,
    };
    const WARHAMMER: &str = "2hWarhammer_FTUE_CorruptedSailer_V2";
    const WEEKLY_EXPIRE: i64 = 604_800;
    const DAILY_EXPIRE: i64 = 64_800;
    const ONE_SECOND_NS: u64 = NS_PER_SECOND;

    fn row(id: &str, count_limit: i64, expire_seconds: i64, max_limit_mult: i64) -> LimitRow<'_> {
        LimitRow {
            id,
            count_limit,
            expire_seconds,
            max_limit_mult,
            replicated: true,
        }
    }

    fn limits() -> LootLimits {
        LootLimits::build([
            row(WARHAMMER, 1, WEEKLY_EXPIRE, WEEKLY_EXPIRE),
            row("GypsumGreen", 30, DAILY_EXPIRE, 0),
            row("Kill_TerrorTurkey", 15, 2_678_400, 0),
            LimitRow {
                replicated: false,
                ..row("PvPGM725Gear_Limit", 1, WEEKLY_EXPIRE, WEEKLY_EXPIRE)
            },
        ])
    }

    fn rules() -> LootLimitRules {
        LootLimitRules {
            limits: limits(),
            clock: RETAIL_CLOCK,
        }
    }

    const fn ns(seconds: i64) -> u64 {
        seconds.unsigned_abs() * NS_PER_SECOND
    }

    #[test]
    fn the_resets_fall_on_the_captured_times() {
        assert_eq!(RETAIL_CLOCK.next(ResetCycle::Weekly, KILL_S), WEEKLY_END_S);
        assert_eq!(RETAIL_CLOCK.next(ResetCycle::Daily, KILL_S), DAILY_END_S);
        // At the reset itself the next one is a cycle later.
        assert_eq!(
            RETAIL_CLOCK.next(ResetCycle::Daily, DAILY_END_S),
            DAILY_END_S + SECONDS_PER_DAY
        );
        assert_eq!(
            RETAIL_CLOCK.next(ResetCycle::Weekly, WEEKLY_END_S),
            WEEKLY_END_S + DAYS_PER_WEEK * SECONDS_PER_DAY
        );
        assert_eq!(
            RETAIL_CLOCK.end_ns(ResetCycle::Weekly, KILL_NS),
            WEEKLY_END_NS,
            "the end keeps the kill's sub-second part"
        );
    }

    #[test]
    fn a_limit_with_an_unknown_reset_is_not_applied() {
        let limits = limits();
        assert_eq!(limits.len(), 3);
        assert_eq!(limits.unknown_reset(), ["Kill_TerrorTurkey".to_owned()]);
        assert!(limits.get("kill_terrorturkey").is_none());
        assert_eq!(
            limits.get("gypsumgreen"),
            Some(&LootLimit {
                count_limit: 30,
                cycle: ResetCycle::Daily,
                replicated: true,
                reached_shows_end: false,
            })
        );
        assert!(
            limits
                .get(WARHAMMER)
                .is_some_and(|limit| limit.reached_shows_end)
        );
    }

    #[test]
    fn an_item_stops_dropping_at_its_limit_until_the_reset() {
        let rules = rules();
        let mut counts = LootLimitCounts::default();
        let roll = || vec![(WARHAMMER.to_owned(), 1), ("Boar_Meat".to_owned(), 2)];
        assert_eq!(
            counts.admit(&rules, roll(), KILL_NS),
            roll(),
            "the first one drops"
        );
        assert!(counts.take_changed());
        assert_eq!(counts.count(WARHAMMER, KILL_NS), 1);
        assert_eq!(
            counts.admit(&rules, roll(), KILL_NS + ONE_SECOND_NS),
            vec![("Boar_Meat".to_owned(), 2)],
            "the limit of 1 is reached; an item without a limit still drops"
        );
        assert!(!counts.take_changed(), "nothing counted");
        assert_eq!(
            counts.admit(&rules, roll(), WEEKLY_END_NS),
            roll(),
            "the weekly reset clears the count"
        );
        assert_eq!(counts.count(WARHAMMER, WEEKLY_END_NS), 1);
    }

    #[test]
    fn a_drop_counts_its_quantity() {
        let rules = rules();
        let mut counts = LootLimitCounts::default();
        let gypsum = |quantity| vec![("GypsumGreen".to_owned(), quantity)];
        assert_eq!(counts.admit(&rules, gypsum(29), KILL_NS), gypsum(29));
        assert_eq!(
            counts.admit(&rules, gypsum(3), KILL_NS),
            gypsum(3),
            "29 is below the limit of 30: the drop passes whole"
        );
        assert_eq!(counts.count("gypsumgreen", KILL_NS), 32);
        assert!(counts.admit(&rules, gypsum(1), KILL_NS).is_empty());
        assert_eq!(
            counts.count("GypsumGreen", ns(DAILY_END_S) + ONE_SECOND_NS),
            0,
            "the daily reset"
        );
    }

    #[test]
    fn a_count_counts_one_until_the_limit_and_a_reached_limit_is_listed() {
        let rules = rules();
        let mut counts = LootLimitCounts::default();
        assert!(counts.count_one(&rules, "pvpgm725gear_limit", KILL_NS));
        assert!(
            counts.count_one(&rules, "Kill_TerrorTurkey", KILL_NS),
            "a limit that is not applied lets it through"
        );
        assert!(
            !counts.count_one(&rules, "PvPGM725Gear_Limit", KILL_NS),
            "the limit of 1 is reached"
        );
        assert_eq!(counts.count("PvPGM725Gear_Limit", KILL_NS), 1);
        assert_eq!(counts.count("Kill_TerrorTurkey", KILL_NS), 0, "not applied");
        assert_eq!(
            counts.reached(&rules, KILL_NS),
            vec!["pvpgm725gear_limit".to_owned()]
        );
        assert!(
            counts
                .reached(&rules, ns(WEEKLY_END_S) + ONE_SECOND_NS)
                .is_empty(),
            "an ended count is not at its limit"
        );
    }

    #[test]
    fn the_client_gets_the_replicated_counts_with_the_retail_times() {
        let rules = rules();
        let mut counts = LootLimitCounts::default();
        counts.admit(&rules, vec![(WARHAMMER.to_owned(), 1)], KILL_NS);
        counts.admit(&rules, vec![("GypsumGreen".to_owned(), 2)], KILL_NS);
        counts.count_one(&rules, "PvPGM725Gear_Limit", KILL_NS);
        let mut expected = vec![
            // The newchar entry: at its limit, both times are its end.
            ShownLimit {
                key: 0x32c7_7cf4,
                first_ns: WEEKLY_END_NS,
                ends_ns: WEEKLY_END_NS,
                count: 1,
            },
            // Below its limit: the time of its last count.
            ShownLimit {
                key: lowercase_crc32("GypsumGreen"),
                first_ns: KILL_NS,
                ends_ns: RETAIL_CLOCK.end_ns(ResetCycle::Daily, KILL_NS),
                count: 2,
            },
        ];
        expected.sort_unstable_by_key(|limit| limit.key);
        assert_eq!(
            counts.shown(&rules),
            expected,
            "the not replicated limit is not shown"
        );
    }

    #[test]
    fn the_counts_survive_a_save() {
        let rules = rules();
        let mut counts = LootLimitCounts::default();
        counts.admit(&rules, vec![("GypsumGreen".to_owned(), 5)], KILL_NS);
        counts.count_one(&rules, "PvPGM725Gear_Limit", KILL_NS + ONE_SECOND_NS);
        let saved = counts.counts();
        assert_eq!(
            saved
                .iter()
                .map(|count| count.id.as_str())
                .collect::<Vec<_>>(),
            ["gypsumgreen", "pvpgm725gear_limit"]
        );
        let mut loaded = LootLimitCounts::from_counts(saved.clone());
        assert!(!loaded.take_changed(), "a load is no change");
        assert_eq!(loaded.counts(), saved);
        assert_eq!(loaded.count("GypsumGreen", KILL_NS), 5);
        assert_eq!(loaded.shown(&rules), counts.shown(&rules));
    }
}
