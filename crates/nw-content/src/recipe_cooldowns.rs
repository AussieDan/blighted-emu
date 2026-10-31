// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recipe cooldowns (`CraftingRecipeData.CooldownSeconds` and `CooldownQuantity`): a recipe with
//! them may be crafted `CooldownQuantity` times until the cooldown resets.
//!
//! **Observed** (the crafting component's `MB::CraftingComponentReplicatedState` (205)
//! `m_cooldowns`, retail ledgers `refining_all_stations_single_batch_territorylvlup`,
//! `FFA_PurchaseLimits` and `well-of-fortune`, 15 states): an entry's key is the CRC-32 of the
//! lowercase `RecipeID` (`TimberT51`, `DoubloonWeeklyKey`), its value a u8 count and a u64 end
//! (unix ns). The count is the crafts made: 9 after one craft of 9 `TimberT51`, 10 for
//! `LeatherT51` (its `CooldownQuantity`). A recipe of `CooldownSeconds` 86400 ended at the next
//! day's reset (03:00 UTC, the refining ledger's server), one of 604800 at the next Tuesday's
//! (09:00 UTC, `DoubloonWeeklyKey`): the resets of the loot limits
//! ([`crate::loot_limits::ResetClock`]), with the sub-second part of the first craft.
//!
//! **Inferred**: a craft that would pass `CooldownQuantity` is refused whole; a count that ended
//! starts again at the next craft. **Unknown**: the reset of `CooldownSeconds` 79200
//! (`FlameCoreCast`) and of a `CooldownQuantity` without `CooldownSeconds` (the 9
//! `GypsumOrb*` colours): those recipes have no cooldown here
//! ([`RecipeCooldown::from_cells`]).

use std::collections::HashMap;

use nw_formats::objectstream::lowercase_crc32;

use crate::loot_limits::{ResetClock, ResetCycle};

/// `CooldownSeconds` of a cooldown that resets with each day (Observed).
const DAILY_SECONDS: i64 = 86_400;
/// `CooldownSeconds` of a cooldown that resets with each week (Observed).
const WEEKLY_SECONDS: i64 = 604_800;

/// The cooldown of one recipe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeCooldown {
    /// When its count goes back to 0.
    pub cycle: ResetCycle,
    /// `CooldownQuantity`: the crafts allowed until then.
    pub quantity: u32,
}

impl RecipeCooldown {
    /// The cooldown of a recipe with `CooldownSeconds` `seconds` and `CooldownQuantity`
    /// `quantity`; `None` without both, or for a reset that is Unknown (module notes).
    pub fn from_cells(seconds: Option<i64>, quantity: Option<i64>) -> Option<Self> {
        let cycle = match seconds? {
            DAILY_SECONDS => ResetCycle::Daily,
            WEEKLY_SECONDS => ResetCycle::Weekly,
            _ => return None,
        };
        let quantity = u32::try_from(quantity?)
            .ok()
            .filter(|quantity| *quantity > 0)?;
        Some(Self { cycle, quantity })
    }
}

/// One recipe's count of a player, as a save or a handoff keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CooldownCount {
    /// The lowercase `RecipeID`.
    pub recipe: String,
    /// The crafts made.
    pub count: u32,
    /// Unix nanoseconds when it goes back to 0.
    pub ends_ns: u64,
}

/// One entry of the client's `m_cooldowns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShownCooldown {
    /// CRC-32 of the lowercase `RecipeID`.
    pub key: u32,
    /// The crafts made.
    pub count: u32,
    /// Unix nanoseconds of its end.
    pub ends_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Count {
    count: u32,
    ends_ns: u64,
}

/// The recipe counts of one player, and whether they changed since the client last got them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipeCooldownCounts {
    by_recipe: HashMap<String, Count>,
    changed: bool,
}

impl RecipeCooldownCounts {
    /// The counts of a save or a handoff.
    pub fn from_counts(counts: impl IntoIterator<Item = CooldownCount>) -> Self {
        Self {
            by_recipe: counts
                .into_iter()
                .map(|count| {
                    (
                        count.recipe.to_lowercase(),
                        Count {
                            count: count.count,
                            ends_ns: count.ends_ns,
                        },
                    )
                })
                .collect(),
            changed: false,
        }
    }

    /// Every count, ended ones too, by recipe.
    pub fn counts(&self) -> Vec<CooldownCount> {
        let mut counts: Vec<CooldownCount> = self
            .by_recipe
            .iter()
            .map(|(recipe, count)| CooldownCount {
                recipe: recipe.clone(),
                count: count.count,
                ends_ns: count.ends_ns,
            })
            .collect();
        counts.sort_unstable_by(|one, other| one.recipe.cmp(&other.recipe));
        counts
    }

    /// The crafts of recipe `recipe` (any case) counted at Unix nanosecond `now_ns`.
    pub fn count(&self, recipe: &str, now_ns: u64) -> u32 {
        self.by_recipe
            .get(&recipe.to_lowercase())
            .filter(|count| now_ns < count.ends_ns)
            .map_or(0, |count| count.count)
    }

    /// Whether `crafts` more crafts of recipe `recipe` with `cooldown` fit at `now_ns`.
    pub fn allows(&self, recipe: &str, cooldown: RecipeCooldown, crafts: u32, now_ns: u64) -> bool {
        self.count(recipe, now_ns).saturating_add(crafts) <= cooldown.quantity
    }

    /// Counts `crafts` crafts of recipe `recipe` with `cooldown` at `now_ns`; a count that ended
    /// starts again and ends at the next reset of `clock`.
    pub fn add(
        &mut self,
        clock: ResetClock,
        recipe: &str,
        cooldown: RecipeCooldown,
        crafts: u32,
        now_ns: u64,
    ) {
        let started = Count {
            count: 0,
            ends_ns: clock.end_ns(cooldown.cycle, now_ns),
        };
        let entry = self
            .by_recipe
            .entry(recipe.to_lowercase())
            .or_insert(started);
        if now_ns >= entry.ends_ns {
            *entry = started;
        }
        entry.count = entry.count.saturating_add(crafts);
        self.changed = true;
    }

    /// The entries the client gets, by key.
    pub fn shown(&self) -> Vec<ShownCooldown> {
        let mut shown: Vec<ShownCooldown> = self
            .by_recipe
            .iter()
            .map(|(recipe, count)| ShownCooldown {
                key: lowercase_crc32(recipe),
                count: count.count,
                ends_ns: count.ends_ns,
            })
            .collect();
        shown.sort_unstable_by_key(|cooldown| cooldown.key);
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
    use crate::loot_limits::Weekday;

    /// The refining ledger's server: daily reset 03:00 UTC.
    const CLOCK: ResetClock = ResetClock {
        hour_utc: 3,
        weekly_day: Weekday::Tuesday,
    };
    /// `LeatherT51`'s retail end, `0x18b8d351b144f000`: 2026-06-14 03:00 UTC (a daily reset)
    /// with the first craft's sub-second part.
    const LEATHER_END_NS: u64 = 0x18b8_d351_b144_f000;
    /// A craft half a day before that end, with the same sub-second part.
    const CRAFT_NS: u64 = LEATHER_END_NS - 42_000 * 1_000_000_000;
    /// `CooldownQuantity` of `LeatherT51`.
    const LEATHER_QUANTITY: u32 = 10;
    /// `CooldownQuantity` of `DoubloonWeeklyKey`.
    const WEEKLY_QUANTITY: u32 = 1;
    const DAY_NS: u64 = 86_400 * 1_000_000_000;

    #[test]
    fn the_daily_and_weekly_seconds_are_known_and_the_others_are_not() {
        assert_eq!(
            RecipeCooldown::from_cells(Some(86_400), Some(10)),
            Some(RecipeCooldown {
                cycle: ResetCycle::Daily,
                quantity: LEATHER_QUANTITY,
            })
        );
        assert_eq!(
            RecipeCooldown::from_cells(Some(604_800), Some(1)).map(|cooldown| cooldown.cycle),
            Some(ResetCycle::Weekly)
        );
        assert_eq!(RecipeCooldown::from_cells(Some(79_200), Some(6)), None);
        assert_eq!(RecipeCooldown::from_cells(None, Some(1)), None);
        assert_eq!(RecipeCooldown::from_cells(Some(86_400), None), None);
    }

    #[test]
    fn crafts_count_to_the_quantity_and_end_at_the_retail_reset() {
        let leather = RecipeCooldown::from_cells(Some(86_400), Some(10)).expect("daily");
        let mut counts = RecipeCooldownCounts::default();
        assert!(counts.allows("LeatherT51", leather, LEATHER_QUANTITY, CRAFT_NS));
        counts.add(CLOCK, "LeatherT51", leather, 9, CRAFT_NS);
        assert!(counts.take_changed());
        assert!(counts.allows("leathert51", leather, 1, CRAFT_NS));
        assert!(
            !counts.allows("leathert51", leather, 2, CRAFT_NS),
            "past 10"
        );
        counts.add(CLOCK, "LeatherT51", leather, 1, CRAFT_NS);
        assert_eq!(
            counts.shown(),
            [ShownCooldown {
                key: lowercase_crc32("leathert51"),
                count: LEATHER_QUANTITY,
                ends_ns: LEATHER_END_NS,
            }],
            "the retail entry"
        );
        assert!(!counts.allows("LeatherT51", leather, 1, CRAFT_NS));
        assert!(
            counts.allows("LeatherT51", leather, 1, LEATHER_END_NS),
            "the reset"
        );
        counts.add(CLOCK, "LeatherT51", leather, 1, LEATHER_END_NS);
        assert_eq!(
            counts.count("LeatherT51", LEATHER_END_NS),
            1,
            "started again"
        );
        assert_eq!(counts.counts()[0].ends_ns, LEATHER_END_NS + DAY_NS);
    }

    #[test]
    fn a_weekly_cooldown_ends_at_the_weekly_reset_and_counts_survive_a_save() {
        let weekly = RecipeCooldown::from_cells(Some(604_800), Some(1)).expect("weekly");
        let mut counts = RecipeCooldownCounts::default();
        counts.add(
            CLOCK,
            "DoubloonWeeklyKey",
            weekly,
            WEEKLY_QUANTITY,
            CRAFT_NS,
        );
        let saved = counts.counts();
        let ends_s = saved[0].ends_ns / 1_000_000_000;
        let ends_s = i64::try_from(ends_s).expect("seconds");
        assert_eq!(
            CLOCK.next(ResetCycle::Weekly, ends_s - 1),
            ends_s,
            "a weekly reset"
        );
        let loaded = RecipeCooldownCounts::from_counts(saved.clone());
        assert_eq!(loaded.counts(), saved);
        assert!(!loaded.allows("doubloonweeklykey", weekly, 1, CRAFT_NS));
    }
}
