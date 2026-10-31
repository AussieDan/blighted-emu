// SPDX-License-Identifier: AGPL-3.0-or-later
//! The game data of travel (run 15 travel study): the level's landmarks a fast travel goes to,
//! the territories a player must have discovered first, the level entities a respawn, an inn and
//! a respawn camp name, the player's encumbrance limit, and the timings of fast travel and
//! unstuck.
//!
//! Timings come from the remote config keys the client registers, when a layer sets them; no
//! retail layer does (`local-data/remote-config/retail-2026-09-18`), so the values the retail
//! captures show stand in: a fast travel's channel ran 10.0 s (3713 to 3951, 22 travels, 10.04
//! to 10.10 s with the network), an unstuck moved the player 10 s after its acceptance (3980
//! carries 10 s in all 50 bodies) and the next request was denied until 120 s after the accepted
//! one (29 denials, each carrying 120.0 s less the time since). The breadcrumb keys have no
//! retail value: the landings of 31 retail unstucks were on the player's own trail, 39 to 264 m
//! from the request (median 97 m); [`OBSERVED_BREADCRUMB_SPACING_M`] times
//! [`OBSERVED_BREADCRUMBS`] reaches the farthest one (Inferred).

use std::collections::{HashMap, HashSet};

use nw_content::Tables;
use nw_content::equip_load::PLAYER_CONTAINER;
use nw_content::gameplay_config::{FastTravelCostKeys, GameplayConfig, TravelKeys};
use nw_content::starter::StarterKits;
use nw_formats::objectstream::lowercase_crc32;
use nw_world::WorldFile;
use nw_world::records::{Capital, Landmark};
use nw_world::travel_slices::{HousingPlot, Teleporter};

/// Milliseconds in a second.
const MS_PER_SECOND: f64 = 1_000.0;
/// The channel of a fast travel when no layer sets it (Observed, see the module note).
const OBSERVED_CHANNEL_MS: u64 = 10_000;
/// The wait from an accepted unstuck to its move when no layer sets it (Observed).
const OBSERVED_UNSTUCK_DELAY_MS: u64 = 10_000;
/// The cooldown of an unstuck when no layer sets it (Observed).
const OBSERVED_UNSTUCK_COOLDOWN_MS: u64 = 120_000;
/// The breadcrumbs kept when no layer sets it (Inferred, see the module note).
pub const OBSERVED_BREADCRUMBS: usize = 11;
/// The spacing of the breadcrumbs when no layer sets it, metres (Inferred).
pub const OBSERVED_BREADCRUMB_SPACING_M: f32 = 24.0;
/// The tag of a capital's entity id: its low 3 bits (GDE id note, section 3).
const CAPITAL_TAG: u64 = 3;
/// The bit a capital's id clears before its tag.
const CAPITAL_CLEARED_BIT: u64 = 4;
/// Hex digits of one half of a capital's GUID.
const GUID_HALF_DIGITS: usize = 16;
/// Radix of a GUID's digits.
const HEX_RADIX: u32 = 16;
/// The GUID separator.
const GUID_SEPARATOR: char = '-';

/// Milliseconds in a minute.
const MS_PER_MINUTE: u64 = 60_000;
/// Nanoseconds in a millisecond.
const NS_PER_MS: u64 = 1_000_000;
/// The base azoth of a fast travel to a settlement or a shrine when no layer sets it. Observed
/// (the travel cost survey of 256 retail ledgers, 2026-10-31): 84 travels to another territory
/// took 20 azoth and 111 within the player's territory took 10, in the ms of the loading screen.
/// The client default (500, `FUN_14356a2f0`) is not what the retail server charges.
const OBSERVED_SETTLEMENT_COST: u64 = 20;
/// The distance a fast travel goes free, metres (client default).
const DEFAULT_MIN_DISTANCE_M: f64 = 1_500.0;
/// The metres of one distance cost step (client default).
const DEFAULT_DISTANCE_STEP_M: f64 = 100.0;
/// The azoth of one distance cost step when no layer sets it. Observed: the retail cost does not
/// grow with the distance (20 azoth from 398 m to 6,944 m, 10 from 256 m to 2,233 m).
const OBSERVED_DISTANCE_COST: u64 = 0;
/// The weight of one encumbrance cost step (client default).
const DEFAULT_ENCUMBRANCE_STEP: u64 = 10;
/// The azoth of one encumbrance cost step when no layer sets it. Observed: no retail travel cost
/// more than the 20 base (203 paid travels of players with any bag).
const OBSERVED_ENCUMBRANCE_COST: u64 = 0;
/// The share taken off a fast travel within the player's territory (client default; Observed:
/// all 111 retail travels whose landmark lay in the player's territory cost half, and none of the
/// 84 to another territory did).
const DEFAULT_TERRITORY_DISCOUNT: f64 = 0.5;
/// The azoth of a home cooldown reset with the whole cooldown left (client default).
const DEFAULT_RESET_COST: u64 = 200;
/// The least azoth of a home cooldown reset (client default).
const DEFAULT_RESET_COST_MIN: u64 = 10;
/// Whether fast travel costs no azoth when no layer sets it. Observed: retail charges (203 of
/// 266 retail travels to a settlement or a shrine took azoth; an inn recall or a home travel
/// never did, 58 and 25 travels).
const OBSERVED_COSTS_DISABLED: bool = false;
/// The multiplier of a second and a third house when no layer sets one (the client default 0.0
/// would make them free; best guess: the table cost, as the first house).
const BEST_GUESS_HOUSE_MULTIPLIER: f64 = 1.0;
/// The tax period: due 7 days after the purchase, and 7 days later at each payment (Observed:
/// house_purchase exactly 7.000000 days; paying_house_upkeep due + 7 days to the 0.1 ms).
const OBSERVED_TAX_PERIOD_MS: u64 = 7 * 24 * 60 * MS_PER_MINUTE;
/// The weekly tax as a share of the house cost. Best guess: the mean of the 5 retail amounts
/// (8160 and 6711 for tier 2, 11605, 12896 and 14914 for tier 4: 0.58 to 0.82 % of the cost;
/// the territory governance that sets the rest is not run here).
const BEST_GUESS_TAX_RATE: f64 = 0.0069;
/// The period of the top-house evaluation, ms (`javelin.housing.evaluate-topHouses-delay`
/// client default 172800 s; retail 3692 next-recompute steps of 172800 s).
const TOP_HOUSES_PERIOD_MS: u64 = 172_800_000;
/// The house score of one `RankingPoints` unit. Best guess: retail scores are whole numbers (89,
/// 153) and the `RankingPoints` of a piece are hundredths (0.02, 0.12).
const BEST_GUESS_SCORE_PER_POINT: f64 = 100.0;
/// The inn recall cooldown: the cooldown of every retail inn home point (449 of 449 elements of
/// type 3652: 900 s).
const OBSERVED_INN_COOLDOWN_MS: u64 = 900_000;
/// How far from a plot's slice origin a player may stand to name it when its id is not a level
/// plot's (best guess: the plots of a street are 20 to 40 m apart).
const BEST_GUESS_PLOT_REACH_M: f32 = 30.0;
/// A teleporter whose `m_channelDuration` is 0 channels as a fast travel (Observed: 9 retail
/// events answered 10.0 s later; 1 for the 1 s ones).
const TELEPORTER_CHANNEL_UNIT_MS: u64 = 1_000;
/// The share of a cost the client's discounts can take at most (its clamp to 1.0).
const FULL_DISCOUNT: f64 = 1.0;

/// A house type (`HouseTypeData`).
#[derive(Debug, Clone, PartialEq)]
pub struct HouseType {
    /// `HouseTypeID`.
    pub id: String,
    /// The CRC-32 of the lowercase id, as the housing state names it.
    pub crc: u32,
    /// `Cost`, coin hundredths.
    pub cost: u64,
    /// `Fast Travel Cooldown Time (Minutes)`, ms.
    pub cooldown_ms: u64,
    /// `Max Number Total Housing Items (Without Perks)`.
    pub max_items: usize,
}

/// A house plot of the level and its house type (an index of the house types).
#[derive(Debug, Clone, PartialEq)]
pub struct Plot {
    /// Its GDE id.
    pub id: u64,
    /// Its slice origin.
    pub position: [f32; 3],
    /// Its house type.
    pub house_type: usize,
}

/// Where a teleporter goes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TeleporterRule {
    /// The destination.
    pub destination: [f32; 3],
    /// The facing there, radians.
    pub yaw_rad: f32,
    /// From the interaction to the move, ms.
    pub delay_ms: u64,
}

/// A piece of furniture: its `HouseItemID` and `RankingPoints`.
#[derive(Debug, Clone, PartialEq)]
pub struct Furniture {
    /// The bag item it is.
    pub item_id: String,
    /// `RankingPoints`.
    pub ranking_points: f64,
}

/// The housing rules.
#[derive(Debug, Clone, PartialEq)]
pub struct HousingRules {
    /// `javelin.housing.house-level-reqs`: the character level each further house needs; its
    /// length is the most houses.
    pub level_reqs: Vec<u32>,
    /// The cost multipliers of a second and a third house.
    pub multipliers: [f64; 2],
    /// The azoth of a cooldown reset with the whole cooldown left, and the least.
    pub reset_cost: (u64, u64),
}

impl Default for HousingRules {
    fn default() -> Self {
        Self::of(&[], &FastTravelCostKeys::default())
    }
}

impl HousingRules {
    fn of(level_reqs: &[u32], keys: &FastTravelCostKeys) -> Self {
        let multiplier = |key: Option<f64>| {
            key.filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(BEST_GUESS_HOUSE_MULTIPLIER)
        };
        Self {
            level_reqs: level_reqs.to_vec(),
            multipliers: [
                multiplier(keys.second_house_multiplier),
                multiplier(keys.third_house_multiplier),
            ],
            reset_cost: (
                keys.cooldown_reset_cost.unwrap_or(DEFAULT_RESET_COST),
                keys.cooldown_reset_cost_min
                    .unwrap_or(DEFAULT_RESET_COST_MIN),
            ),
        }
    }

    /// The coin cost of a house of `house_type` for a player who owns `owned` houses.
    pub fn cost(&self, house_type: &HouseType, owned: usize) -> u64 {
        let multiplier = owned
            .checked_sub(1)
            .and_then(|further| self.multipliers.get(further))
            .copied()
            .unwrap_or(BEST_GUESS_HOUSE_MULTIPLIER);
        // A house cost of a few million fits f64 and back.
        (house_type.cost as f64 * multiplier).round() as u64
    }

    /// The level a player who owns `owned` houses needs for one more (`CanPurchasePlot`:
    /// refused when level + 1 is below it); `None` when it owns the most.
    pub fn level_for(&self, owned: usize) -> Option<u32> {
        self.level_reqs.get(owned).copied()
    }

    /// The weekly tax of a house of `house_type`, coin hundredths.
    pub fn tax(&self, house_type: &HouseType) -> u32 {
        // A tax of at most a few hundred thousand fits u32.
        (house_type.cost as f64 * BEST_GUESS_TAX_RATE).round() as u32
    }

    /// The taxes due time after a payment at `now_ms` of taxes due at `due_ms` (an early payment
    /// adds a period to the due time, a late one starts it now).
    pub fn paid_until(due_ms: u64, now_ms: u64) -> u64 {
        due_ms.max(now_ms).saturating_add(OBSERVED_TAX_PERIOD_MS)
    }

    /// The taxes due time of a house bought at `now_ms`.
    pub fn first_due(now_ms: u64) -> u64 {
        now_ms.saturating_add(OBSERVED_TAX_PERIOD_MS)
    }

    /// The azoth of a reset of a cooldown of `cooldown_ms` with `left_ms` left (`FUN_14357b0a0`:
    /// `max(min, cost * left / cooldown)`).
    pub fn reset_cost(&self, left_ms: u64, cooldown_ms: u64) -> u32 {
        let (cost, least) = self.reset_cost;
        let share = if cooldown_ms == 0 {
            0
        } else {
            u64::try_from(u128::from(cost) * u128::from(left_ms) / u128::from(cooldown_ms))
                .unwrap_or(u64::MAX)
        };
        u32::try_from(share.max(least)).unwrap_or(u32::MAX)
    }

    /// The next top-house evaluation after `now_ms`, Unix ns.
    pub fn next_recompute_ns(now_ms: u64) -> u64 {
        let next = (now_ms / TOP_HOUSES_PERIOD_MS + 1) * TOP_HOUSES_PERIOD_MS;
        next.saturating_mul(NS_PER_MS)
    }

    /// The house score of furniture with `points` `RankingPoints` in all.
    pub fn house_score(points: f64) -> u32 {
        // A score of a few thousand fits u32.
        (points * BEST_GUESS_SCORE_PER_POINT).round().max(0.0) as u32
    }
}

/// The azoth cost of a fast travel (`FUN_14357bdb0` with `FUN_14357c470` distance,
/// `FUN_14357b9e0` encumbrance and `FUN_14357b690` base). The player modifiers (perks and status
/// effects such as `Azoth_FastTravel_*_Reduction`) and the company and faction discounts of a
/// governed territory are 0 here: this server runs no territory governance and gives no such
/// modifier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FastTravelCosts {
    /// No cost at all (`javelin.playtest-disable-fast-travel-costs`).
    pub disabled: bool,
    /// The base cost.
    pub base: u64,
    /// The free distance, metres.
    pub min_distance_m: f64,
    /// The metres of one distance step.
    pub distance_step_m: f64,
    /// The azoth of one distance step.
    pub distance_cost: u64,
    /// The weight of one encumbrance step.
    pub encumbrance_step: u64,
    /// The azoth of one encumbrance step.
    pub encumbrance_cost: u64,
    /// The share taken off within the player's territory.
    pub territory_discount: f64,
}

impl Default for FastTravelCosts {
    fn default() -> Self {
        Self::of(&FastTravelCostKeys::default())
    }
}

impl FastTravelCosts {
    fn of(keys: &FastTravelCostKeys) -> Self {
        Self {
            disabled: keys.disabled.unwrap_or(OBSERVED_COSTS_DISABLED),
            base: keys.settlement_cost.unwrap_or(OBSERVED_SETTLEMENT_COST),
            min_distance_m: keys.min_distance_m.unwrap_or(DEFAULT_MIN_DISTANCE_M),
            distance_step_m: keys.distance_step_m.unwrap_or(DEFAULT_DISTANCE_STEP_M),
            distance_cost: keys.distance_cost.unwrap_or(OBSERVED_DISTANCE_COST),
            encumbrance_step: keys.encumbrance_step.unwrap_or(DEFAULT_ENCUMBRANCE_STEP),
            encumbrance_cost: keys.encumbrance_cost.unwrap_or(OBSERVED_ENCUMBRANCE_COST),
            territory_discount: keys
                .territory_discount
                .unwrap_or(DEFAULT_TERRITORY_DISCOUNT),
        }
    }

    /// The azoth of a fast travel of `distance_m` with a bag of weight `load`, `in_territory`
    /// when the player stands in the destination's territory.
    pub fn cost(&self, distance_m: f64, load: f64, in_territory: bool) -> u32 {
        if self.disabled {
            return 0;
        }
        let distance_steps = if self.distance_step_m > 0.0 {
            ((distance_m - self.min_distance_m).max(0.0) / self.distance_step_m).ceil()
        } else {
            0.0
        };
        let encumbrance_steps = if self.encumbrance_step > 0 {
            (load.max(0.0) / self.encumbrance_step as f64).ceil()
        } else {
            0.0
        };
        let total = (self.base as f64
            + distance_steps * self.distance_cost as f64
            + encumbrance_steps * self.encumbrance_cost as f64)
            .round();
        let discount = if in_territory {
            (total * self.territory_discount.clamp(0.0, FULL_DISCOUNT)).round()
        } else {
            0.0
        };
        // A cost of a few thousand fits u32.
        (total - discount).max(0.0) as u32
    }
}

/// A landmark a fast travel may go to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TravelLandmark {
    /// Its type (`eTerritoryLandmarkType`, as the client's 3713 names it).
    pub landmark_type: u8,
    /// Its position, metres.
    pub position: [f32; 3],
    /// Its radius, metres.
    pub radius: f32,
    /// The territory (`IsTerritory`) that holds it, of the level's territory ids of its point.
    pub territory: Option<u32>,
}

/// The game data and timings of travel.
#[derive(Debug, Clone, PartialEq)]
pub struct TravelRules {
    /// The level's landmarks.
    landmarks: Vec<TravelLandmark>,
    /// The achievement that marks each discoverable territory discovered, by `TerritoryID`.
    discovered: HashMap<u32, String>,
    /// The position of each capital of the level, by its entity id (respawn points, inns and
    /// respawn camps are capitals: retail's 1938 named the ids of the bound ones).
    capitals: HashMap<u64, [f32; 3]>,
    /// The player's encumbrance limit (`EncumbranceBaseMax` of the player container).
    pub encumbrance_max: Option<f64>,
    /// The channel of a fast travel, milliseconds.
    pub channel_ms: u64,
    /// The wait from an accepted unstuck to its move, milliseconds.
    pub unstuck_delay_ms: u64,
    /// The cooldown of an unstuck, milliseconds.
    pub unstuck_cooldown_ms: u64,
    /// The most breadcrumbs kept of a player.
    pub max_breadcrumbs: usize,
    /// The least distance between two breadcrumbs, metres.
    pub breadcrumb_spacing_m: f32,
    /// The house types.
    house_types: Vec<HouseType>,
    /// The level's house plots.
    plots: Vec<Plot>,
    /// The level's teleporters, by their capital's entity id.
    teleporters: HashMap<u64, TeleporterRule>,
    /// The furniture, by client item index.
    furniture: HashMap<u32, Furniture>,
    /// The territories where a camp may be placed (`CanPlaceCamp`).
    camp_territories: HashSet<u32>,
    /// The territories that are territories (`IsTerritory`), not areas within one.
    territories: HashSet<u32>,
    /// The housing rules.
    pub housing: HousingRules,
    /// The fast travel cost.
    pub costs: FastTravelCosts,
    /// The inn recall cooldown, ms.
    pub inn_cooldown_ms: u64,
}

impl Default for TravelRules {
    fn default() -> Self {
        Self::build(
            Vec::new(),
            HashMap::new(),
            HashMap::new(),
            None,
            TravelKeys::default(),
        )
    }
}

/// Milliseconds of a key in seconds, or `observed` without it.
fn millis(seconds: Option<f64>, observed: u64) -> u64 {
    seconds
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        // A non-negative finite number of seconds fits u64 milliseconds for any real config.
        .map_or(observed, |seconds| (seconds * MS_PER_SECOND).round() as u64)
}

/// The entity id of the capital of GUID text `guid`: `(hi ^ lo) & !4 | 3`, the halves read
/// big-endian (GDE id note, section 3; all 7,584 tag-3 ids of the captures).
pub fn capital_id(guid: &str) -> Option<u64> {
    let digits: String = guid.chars().filter(|c| *c != GUID_SEPARATOR).collect();
    if digits.len() != 2 * GUID_HALF_DIGITS {
        return None;
    }
    let (high, low) = digits.split_at(GUID_HALF_DIGITS);
    let high = u64::from_str_radix(high, HEX_RADIX).ok()?;
    let low = u64::from_str_radix(low, HEX_RADIX).ok()?;
    Some((high ^ low) & !CAPITAL_CLEARED_BIT | CAPITAL_TAG)
}

impl TravelRules {
    /// The rules of the level `level` with the tables `tables` and the remote config `keys`.
    pub fn of(
        tables: &Tables,
        level: &WorldFile,
        gameplay: &GameplayConfig,
        kits: &StarterKits,
    ) -> Self {
        let keys = gameplay.travel;
        let discovered = tables
            .territory_definition
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.is_discoverable.unwrap_or_default())
            .filter_map(|row| {
                let territory = u32::try_from(row.territory_id).ok()?;
                let achievement = row.discovered_achievement.clone()?;
                Some((territory, achievement))
            })
            .collect();
        #[allow(
            clippy::cast_precision_loss,
            reason = "an encumbrance limit of a few thousand"
        )]
        let encumbrance_max = tables
            .encumbrance_data
            .iter()
            .flat_map(|table| &table.rows)
            .find(|row| row.container_type_id == PLAYER_CONTAINER)
            .map(|row| row.encumbrance_base_max as f64);
        let camp_territories = tables
            .territory_definition
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.can_place_camp.unwrap_or_default())
            .filter_map(|row| u32::try_from(row.territory_id).ok())
            .collect();
        let territories: HashSet<u32> = tables
            .territory_definition
            .iter()
            .flat_map(|table| &table.rows)
            .filter(|row| row.is_territory.unwrap_or_default())
            .filter_map(|row| u32::try_from(row.territory_id).ok())
            .collect();
        let mut rules = Self::build(
            level
                .landmarks
                .iter()
                .map(|landmark| TravelLandmark::of(landmark, &territories))
                .collect(),
            discovered,
            capitals(&level.capitals),
            encumbrance_max,
            keys,
        )
        .with_housing(house_types(tables), &level.plots, furniture(tables, kits))
        .with_teleporters(&level.teleporters);
        rules.camp_territories = camp_territories;
        rules.territories = territories;
        rules.housing = HousingRules::of(&gameplay.house_level_reqs, &gameplay.fast_travel_costs);
        rules.costs = FastTravelCosts::of(&gameplay.fast_travel_costs);
        rules
    }

    /// These rules with the house types `house_types`, the level's plots `plots` (a plot of an
    /// unknown house type is left out) and the furniture `furniture`.
    pub fn with_housing(
        mut self,
        house_types: Vec<HouseType>,
        plots: &[HousingPlot],
        furniture: HashMap<u32, Furniture>,
    ) -> Self {
        self.plots = plots
            .iter()
            .filter_map(|plot| {
                let house_type = house_types
                    .iter()
                    .position(|house_type| house_type.id.eq_ignore_ascii_case(&plot.house_type))?;
                Some(Plot {
                    id: plot.id,
                    position: plot.position,
                    house_type,
                })
            })
            .collect();
        self.house_types = house_types;
        self.furniture = furniture;
        self
    }

    /// These rules with the level's teleporters `teleporters` (one whose capital GUID does not
    /// read is left out).
    pub fn with_teleporters(mut self, teleporters: &[Teleporter]) -> Self {
        let channel_ms = self.channel_ms;
        self.teleporters = teleporters
            .iter()
            .filter_map(|teleporter| {
                let delay_ms = match teleporter.channel {
                    0 => channel_ms,
                    seconds => u64::from(seconds).saturating_mul(TELEPORTER_CHANNEL_UNIT_MS),
                };
                Some((
                    capital_id(&teleporter.capital)?,
                    TeleporterRule {
                        destination: teleporter.destination,
                        yaw_rad: teleporter.yaw_rad,
                        delay_ms,
                    },
                ))
            })
            .collect();
        self
    }

    /// These rules with camps allowed in the territories `territories`.
    pub fn with_camp_territories(mut self, territories: impl IntoIterator<Item = u32>) -> Self {
        self.camp_territories = territories.into_iter().collect();
        self
    }

    /// The plot of GDE id `id`, else the nearest plot whose origin is within reach of `near`.
    pub fn plot(&self, id: u64, near: [f32; 3]) -> Option<&Plot> {
        self.plots.iter().find(|plot| plot.id == id).or_else(|| {
            self.plots
                .iter()
                .map(|plot| {
                    let distance = (plot.position[0] - near[0]).hypot(plot.position[1] - near[1]);
                    (plot, distance)
                })
                .filter(|(_, distance)| *distance <= BEST_GUESS_PLOT_REACH_M)
                .min_by(|(_, a), (_, b)| a.total_cmp(b))
                .map(|(plot, _)| plot)
        })
    }

    /// The house type of index `index`.
    pub fn house_type(&self, index: usize) -> Option<&HouseType> {
        self.house_types.get(index)
    }

    /// The house type of CRC `crc`.
    pub fn house_type_of_crc(&self, crc: u32) -> Option<&HouseType> {
        self.house_types
            .iter()
            .find(|house_type| house_type.crc == crc)
    }

    /// The teleporter of capital `id`.
    pub fn teleporter(&self, id: u64) -> Option<&TeleporterRule> {
        self.teleporters.get(&id)
    }

    /// The furniture of client item index `index`.
    pub fn furniture(&self, index: u32) -> Option<&Furniture> {
        self.furniture.get(&index)
    }

    /// These rules with the territories `territories` (`IsTerritory`).
    pub fn with_territories(mut self, territories: impl IntoIterator<Item = u32>) -> Self {
        self.territories = territories.into_iter().collect();
        self
    }

    /// Whether `territory` is a territory (`IsTerritory`), not an area within one.
    pub fn is_territory(&self, territory: u32) -> bool {
        self.territories.contains(&territory)
    }

    /// Whether a camp may be placed in territory `territory`.
    pub fn can_place_camp(&self, territory: u32) -> bool {
        self.camp_territories.contains(&territory)
    }

    /// The rules of the given parts.
    pub fn build(
        landmarks: Vec<TravelLandmark>,
        discovered: HashMap<u32, String>,
        capitals: HashMap<u64, [f32; 3]>,
        encumbrance_max: Option<f64>,
        keys: TravelKeys,
    ) -> Self {
        Self {
            landmarks,
            discovered,
            capitals,
            encumbrance_max,
            channel_ms: millis(keys.fast_travel_channeling_seconds, OBSERVED_CHANNEL_MS),
            unstuck_delay_ms: millis(keys.unstuck_delay_seconds, OBSERVED_UNSTUCK_DELAY_MS),
            unstuck_cooldown_ms: millis(
                keys.unstuck_interval_seconds,
                OBSERVED_UNSTUCK_COOLDOWN_MS,
            ),
            max_breadcrumbs: keys
                .max_breadcrumbs
                .and_then(|count| usize::try_from(count).ok())
                .unwrap_or(OBSERVED_BREADCRUMBS),
            // A breadcrumb distance of a few metres fits f32.
            breadcrumb_spacing_m: keys
                .breadcrumb_distance_m
                .filter(|distance| distance.is_finite() && *distance > 0.0)
                .map_or(OBSERVED_BREADCRUMB_SPACING_M, |distance| distance as f32),
            house_types: Vec::new(),
            plots: Vec::new(),
            teleporters: HashMap::new(),
            furniture: HashMap::new(),
            camp_territories: HashSet::new(),
            territories: HashSet::new(),
            housing: HousingRules::default(),
            costs: FastTravelCosts::default(),
            inn_cooldown_ms: OBSERVED_INN_COOLDOWN_MS,
        }
    }

    /// The landmark of type `landmark_type` whose radius holds (`x`, `y`): the client sends the
    /// landmark's own position, so the nearest of that type within its radius.
    pub fn landmark(&self, landmark_type: u8, x: f32, y: f32) -> Option<&TravelLandmark> {
        self.landmarks
            .iter()
            .filter(|landmark| landmark.landmark_type == landmark_type)
            .map(|landmark| {
                let distance = (landmark.position[0] - x).hypot(landmark.position[1] - y);
                (landmark, distance)
            })
            .filter(|(landmark, distance)| *distance <= landmark.radius)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(landmark, _)| landmark)
    }

    /// The achievement that marks territory `territory` discovered; `None` for a territory that
    /// is not discoverable.
    pub fn discovered_achievement(&self, territory: u32) -> Option<&str> {
        self.discovered.get(&territory).map(String::as_str)
    }

    /// The position of the capital of entity id `id` (a respawn point, an inn, a respawn camp).
    pub fn spawn_point(&self, id: u64) -> Option<[f32; 3]> {
        self.capitals.get(&id).copied()
    }
}

impl TravelLandmark {
    /// The landmark `landmark` of the level, in the first of its territory ids that is one of
    /// `territories`.
    fn of(landmark: &Landmark, territories: &HashSet<u32>) -> Self {
        Self {
            landmark_type: landmark.landmark_type,
            position: landmark.position,
            radius: landmark.radius,
            territory: landmark
                .territory_ids
                .iter()
                .map(|&id| u32::from(id))
                .find(|id| territories.contains(id)),
        }
    }
}

/// The house types of `tables`.
fn house_types(tables: &Tables) -> Vec<HouseType> {
    tables
        .house_type_data
        .iter()
        .flat_map(|table| &table.rows)
        .map(|row| HouseType {
            id: row.house_type_id.clone(),
            crc: lowercase_crc32(&row.house_type_id),
            cost: u64::try_from(row.cost).unwrap_or_default(),
            cooldown_ms: u64::try_from(row.fast_travel_cooldown_time_minutes)
                .unwrap_or_default()
                .saturating_mul(MS_PER_MINUTE),
            max_items: usize::try_from(row.max_number_total_housing_items_without_perks)
                .unwrap_or_default(),
        })
        .collect()
}

/// The furniture of `tables` by client item index (the index the item catalog `kits` gives each
/// housing item, [`StarterKits::with_housing`]), with its `RankingPoints`.
fn furniture(tables: &Tables, kits: &StarterKits) -> HashMap<u32, Furniture> {
    tables
        .variation_data
        .iter()
        .flat_map(|table| &table.rows)
        .filter_map(|row| {
            let item_id = row.house_item_id.clone()?;
            let index = kits.item(&item_id)?.item_index;
            Some((
                index,
                Furniture {
                    item_id,
                    ranking_points: row
                        .ranking_points
                        .as_deref()
                        .and_then(|points| points.trim().parse().ok())
                        .unwrap_or_default(),
                },
            ))
        })
        .collect()
}

/// The position of each capital by its entity id; a capital whose GUID does not read is left
/// out.
fn capitals(capitals: &[Capital]) -> HashMap<u64, [f32; 3]> {
    capitals
        .iter()
        .filter_map(|capital| Some((capital_id(&capital.id)?, capital.position)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The MB watchtower capital of the GDE id note and its card id.
    const WATCHTOWER_GUID: &str = "f6593b66-abc3-7460-7a4d-5afaeb8b2620";
    const WATCHTOWER_ID: u64 = 0x8c14_619c_4048_5243;
    /// A shrine landmark (synthetic position; the retail shrine type of 3713).
    const SHRINE_TYPE: u8 = 27;
    const SHRINE: [f32; 3] = [6_061.9, 9_856.0, 320.0];
    const SHRINE_RADIUS_M: f32 = 6.0;

    fn rules() -> TravelRules {
        TravelRules::build(
            vec![
                TravelLandmark {
                    landmark_type: SHRINE_TYPE,
                    position: SHRINE,
                    radius: SHRINE_RADIUS_M,
                    territory: None,
                },
                TravelLandmark {
                    landmark_type: 1,
                    position: SHRINE,
                    radius: SHRINE_RADIUS_M,
                    territory: None,
                },
            ],
            HashMap::from([(20_422, "Discovered_Shrine".to_owned())]),
            capitals(&[Capital {
                id: WATCHTOWER_GUID.to_owned(),
                position: SHRINE,
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: None,
                slice_name: String::new(),
                variant_name: None,
                slice_asset_id: String::new(),
            }]),
            Some(1_000.0),
            TravelKeys::default(),
        )
    }

    #[test]
    fn a_capital_id_is_the_xor_of_its_guid_halves_tagged_3() {
        assert_eq!(capital_id(WATCHTOWER_GUID), Some(WATCHTOWER_ID));
        assert_eq!(capital_id("not-a-guid"), None);
        assert_eq!(rules().spawn_point(WATCHTOWER_ID), Some(SHRINE));
        assert_eq!(rules().spawn_point(1), None);
    }

    #[test]
    fn a_landmark_is_found_by_its_type_within_its_radius() {
        let rules = rules();
        let found = rules
            .landmark(SHRINE_TYPE, SHRINE[0] + 1.0, SHRINE[1])
            .expect("the shrine");
        assert_eq!(found.landmark_type, SHRINE_TYPE);
        assert!(
            rules
                .landmark(SHRINE_TYPE, SHRINE[0] + 7.0, SHRINE[1])
                .is_none()
        );
        assert!(rules.landmark(2, SHRINE[0], SHRINE[1]).is_none());
        assert_eq!(
            rules.discovered_achievement(20_422),
            Some("Discovered_Shrine")
        );
        assert_eq!(rules.discovered_achievement(1), None);
    }

    #[test]
    fn the_timings_are_the_keys_or_the_retail_values() {
        let observed = rules();
        assert_eq!(
            (
                observed.channel_ms,
                observed.unstuck_delay_ms,
                observed.unstuck_cooldown_ms,
                observed.max_breadcrumbs
            ),
            (10_000, 10_000, 120_000, OBSERVED_BREADCRUMBS)
        );
        let keyed = TravelRules::build(
            Vec::new(),
            HashMap::new(),
            HashMap::new(),
            None,
            TravelKeys {
                fast_travel_channeling_seconds: Some(5.0),
                unstuck_delay_seconds: Some(2.5),
                unstuck_interval_seconds: Some(60.0),
                max_breadcrumbs: Some(4),
                breadcrumb_distance_m: Some(10.0),
            },
        );
        assert_eq!(
            (
                keyed.channel_ms,
                keyed.unstuck_delay_ms,
                keyed.unstuck_cooldown_ms,
                keyed.max_breadcrumbs
            ),
            (5_000, 2_500, 60_000, 4)
        );
        assert!((keyed.breadcrumb_spacing_m - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_fast_travel_cost_is_the_client_formula_with_retails_values() {
        /// The retail costs (travel cost survey, 203 paid travels).
        const TO_ANOTHER_TERRITORY: u32 = 20;
        const WITHIN_THE_TERRITORY: u32 = 10;
        let retail = FastTravelCosts::default();
        assert!(!retail.disabled);
        for (distance_m, load) in [(256.0, 0.0), (6_944.0, 900.0)] {
            assert_eq!(retail.cost(distance_m, load, false), TO_ANOTHER_TERRITORY);
            assert_eq!(retail.cost(distance_m, load, true), WITHIN_THE_TERRITORY);
        }
        let keyed = FastTravelCosts::of(&FastTravelCostKeys {
            settlement_cost: Some(500),
            distance_cost: Some(5),
            encumbrance_cost: Some(5),
            ..FastTravelCostKeys::default()
        });
        // 500 base; 3,550 m is 2,050 m past the free 1,500 m: 21 steps of 5; a load of 95 is
        // 10 steps of 5.
        assert_eq!(keyed.cost(3_550.0, 95.0, false), 500 + 21 * 5 + 10 * 5);
        assert_eq!(keyed.cost(1_000.0, 0.0, false), 500);
        assert_eq!(
            keyed.cost(1_000.0, 0.0, true),
            250,
            "half within the territory"
        );
        let disabled = FastTravelCosts::of(&FastTravelCostKeys {
            disabled: Some(true),
            ..FastTravelCostKeys::default()
        });
        assert_eq!(disabled.cost(9_000.0, 500.0, false), 0);
    }

    #[test]
    fn a_level_landmark_is_in_its_first_territory_that_is_a_territory() {
        /// A shrine of Everfall (territory 4) in its area 40300 (synthetic ids order).
        const EVERFALL: u16 = 4;
        const AREA: u16 = 40_300;
        let landmark = Landmark {
            landmark_type: SHRINE_TYPE,
            landmark_data: String::new(),
            position: SHRINE,
            radius: SHRINE_RADIUS_M,
            territory_ids: vec![AREA, EVERFALL],
        };
        let territories = HashSet::from([u32::from(EVERFALL)]);
        assert_eq!(
            TravelLandmark::of(&landmark, &territories).territory,
            Some(u32::from(EVERFALL))
        );
        assert_eq!(
            TravelLandmark::of(&landmark, &HashSet::new()).territory,
            None
        );
    }

    #[test]
    fn the_housing_rules_give_cost_taxes_cooldown_reset_and_recompute() {
        let tier_2 = HouseType {
            id: "HouseTypeTier2".to_owned(),
            crc: lowercase_crc32("HouseTypeTier2"),
            cost: 1_000_000,
            cooldown_ms: 180 * MS_PER_MINUTE,
            max_items: 125,
        };
        assert_eq!(tier_2.crc, 0xd0e4_0188, "the retail housing state's CRC");
        let rules = HousingRules {
            level_reqs: vec![15, 35, 55],
            ..HousingRules::default()
        };
        assert_eq!(rules.cost(&tier_2, 0), 1_000_000);
        assert_eq!(rules.cost(&tier_2, 1), 1_000_000);
        assert_eq!((rules.level_for(1), rules.level_for(3)), (Some(35), None));
        assert_eq!(rules.tax(&tier_2), 6_900);
        let day_ms = 24 * 60 * MS_PER_MINUTE;
        assert_eq!(HousingRules::first_due(0), 7 * day_ms);
        assert_eq!(
            HousingRules::paid_until(7 * day_ms, day_ms),
            14 * day_ms,
            "early"
        );
        assert_eq!(
            HousingRules::paid_until(day_ms, 3 * day_ms),
            10 * day_ms,
            "late"
        );
        assert_eq!(
            rules.reset_cost(tier_2.cooldown_ms, tier_2.cooldown_ms),
            200
        );
        assert_eq!(
            rules.reset_cost(tier_2.cooldown_ms / 2, tier_2.cooldown_ms),
            100
        );
        assert_eq!(rules.reset_cost(1, tier_2.cooldown_ms), 10, "the least");
        let next = HousingRules::next_recompute_ns(1_000);
        assert_eq!(next, TOP_HOUSES_PERIOD_MS * NS_PER_MS);
        assert_eq!(HousingRules::house_score(1.53), 153);
    }

    #[test]
    fn plots_teleporters_and_camp_territories_are_found() {
        let house_type = HouseType {
            id: "HouseTypeTier4".to_owned(),
            crc: lowercase_crc32("HouseTypeTier4"),
            cost: 2_000_000,
            cooldown_ms: 120 * MS_PER_MINUTE,
            max_items: 200,
        };
        let plot = HousingPlot {
            id: 0x6a41_43a0_cd66_37c7,
            slice: "slices/housing/plots/test/cityn_house_address_002".to_owned(),
            position: SHRINE,
            house_type: "housetypetier4".to_owned(),
        };
        let rules = rules()
            .with_housing(vec![house_type], &[plot], HashMap::new())
            .with_teleporters(&[Teleporter {
                capital: WATCHTOWER_GUID.to_owned(),
                destination: SHRINE,
                yaw_rad: 0.0,
                channel: 0,
            }])
            .with_camp_territories([16])
            .with_territories([16]);
        assert_eq!(
            rules
                .plot(0x6a41_43a0_cd66_37c7, [0.0; 3])
                .map(|plot| plot.house_type),
            Some(0)
        );
        assert!(
            rules.plot(1, [SHRINE[0] + 10.0, SHRINE[1], 0.0]).is_some(),
            "near"
        );
        assert!(
            rules.plot(1, [SHRINE[0] + 100.0, SHRINE[1], 0.0]).is_none(),
            "far"
        );
        let teleporter = rules.teleporter(WATCHTOWER_ID).expect("teleporter");
        assert_eq!(
            teleporter.delay_ms, OBSERVED_CHANNEL_MS,
            "channel 0: the fast travel's"
        );
        assert!(rules.can_place_camp(16) && !rules.can_place_camp(17));
        assert!(rules.is_territory(16) && !rules.is_territory(20_422));
        assert_eq!(
            rules
                .house_type_of_crc(lowercase_crc32("HouseTypeTier4"))
                .map(|t| t.cost),
            Some(2_000_000)
        );
    }
}
