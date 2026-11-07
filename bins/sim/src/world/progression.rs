// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's weapon masteries as its client keeps them (docs/re/2026-09-18-progression-and-gm-
//! mode.md sections 2 and 4): the categorical progression list (type 911) and the weapon point
//! pools (type 3681). The sim keeps the total experience of each mastery; the rank and the
//! points inside it come from the rank table. Each rank gives one point of the weapon's pool;
//! the client assigns them to the progression points of the weapon's tree (1112), and the
//! applied active abilities go into the weapon table's mapped slots (2857, type 185;
//! docs/re/2026-09-18-progression-and-gm-mode.md section 6). The client moves an ability into a
//! slot (2701) and resets a weapon tree for azoth (186, answered by 2858; progression note,
//! section 9). The list holds every other categorical progression the player has too: the
//! tradeskills, the ranked ones and the currencies (`Azoth_Currency`, whose points are the
//! azoth balance).

use std::collections::{BTreeMap, BTreeSet};

use nw_bus::AdminReply;
use nw_content::progression::{AZOTH_PROGRESSION, Mastery, Progression};
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::items::FIRST_VERSION;
use nw_messages::{
    ABILITY_KEYS, AbilityState, CategoricalProgressionState, MAX_MAPPED_SLOTS, MappedSlot,
    MappedTable, MasteryChange, MasteryEntry, MasteryFields, PersistentAbilities, PointEntry,
    PointPool, PoolCategory, ProgressionPointState,
};

use super::{Player, SimWorld};

/// The most coins a save keeps (the database column is a signed 64-bit number).
const MAX_COINS: u64 = i64::MAX.unsigned_abs();
/// The answer for a level or mastery command when the content has no level table.
const NO_LEVELS: &str = "the content has no level table";
/// The slot key of each mapped slot of an ability table: retail sends 402, 115 and 139 for the
/// three slots of all 29 tables (newchar 2026-09-18 and NH-MSQ-2.1 logins; meaning Unknown).
/// The most points of one territory standing point (3681 note: territory standing entries
/// hold up to 14).
const TERRITORY_POINT_MAX: u16 = 14;

/// Why a territory standing point was not spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerritoryRefusal {
    /// The bonus is not one of the pool's, or the pool is no territory's.
    NoSuchBonus,
    /// Every point the pool holds is spent.
    NoPointLeft,
    /// The bonus holds its most points.
    AtMaxLevel,
}

/// One weapon progression point a player assigned points to.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WeaponPoint {
    /// Its `ProgressionPointId`.
    id: String,
    /// Its wire id.
    crc: u32,
    /// Its pool's wire id.
    pool: u32,
    /// The points in it.
    level: u16,
    /// Whether it is an active ability.
    active: bool,
}

/// One mapped slot as a save carries it: the weapon mastery (`CategoricalProgressionId`) of the
/// ability table, the slot (0 to 2) and the ability's `ProgressionPointId`.
pub(super) type SavedSlot = (String, u8, String);

/// Why a change of a mapped slot (2701) is refused; the slots stay as they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MapRefusal {
    /// The table is not in the player's ability state, or holds no weapon tree.
    UnknownTable,
    /// The slot is not 0 to 2.
    UnknownSlot,
    /// The ability is no active ability of the table's tree with a point in it.
    NotUnlocked,
}

/// Why a weapon tree respec (186) is refused; the tree stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RespecRefusal {
    /// The table is not in the player's ability state, or holds no weapon tree.
    UnknownTable,
    /// The player has less azoth than the respec costs at the weapon's rank.
    NotEnoughAzoth,
}

/// Why an assignment of weapon points (1112) is refused; the state stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AssignRefusal {
    /// The point is no ability-tree point of a weapon pool, or not of the pool sent.
    UnknownPoint,
    /// No points, or more than the point's `MaxLevel`.
    BadLevel,
    /// Its `RequiredProgressionPointId` has fewer points than `RequiredProgressionPointLevel`.
    Required,
    /// The pool has fewer unspent points.
    NotEnoughPoints,
}

/// The weapon masteries of one player, in the order its client lists them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Masteries {
    /// Each mastery id and its total experience, in list order.
    entries: Vec<(String, u32)>,
    /// Saved masteries the content does not know: kept as they are, so a content change never
    /// deletes them at the next save.
    kept: BTreeMap<String, u32>,
    /// The list positions that changed since the last bundle, and the fields that did.
    changed: BTreeMap<usize, MasteryFields>,
    /// The list positions whose rank changed since the last bundle.
    ranked: BTreeSet<usize>,
    /// The version of the newest 911 list the client has.
    version: u64,
    /// The version of the newest 3681 pool list the client has.
    pools_version: u64,
    /// The weapon progression points with assigned points, in the order first assigned.
    points: Vec<WeaponPoint>,
    /// The territory standing points spent with a 4040, in the order first spent.
    territory_points: Vec<PointEntry>,
    /// A territory point changed since the last bundle.
    territory_changed: bool,
    /// Saved points the content does not know: kept as they are for the save.
    kept_points: Vec<(String, u16)>,
    /// The pools whose unspent count an assignment changed since the last bundle.
    assigned: BTreeSet<u32>,
    /// The abilities in the mapped slots of each weapon tree, by pool (the CRC of its ability
    /// table); 0 for an empty slot.
    slots: BTreeMap<u32, [u32; MAX_MAPPED_SLOTS]>,
    /// Saved slots the content does not know: kept as they are for the save.
    kept_slots: Vec<SavedSlot>,
    /// The mapped abilities its client has.
    mapping_sent: Option<PersistentAbilities>,
}

impl Masteries {
    /// The ids of the weapon progression points with points in them: each is the `AbilityID` of
    /// the ability (or upgrade) it unlocks.
    pub(super) fn spent_points(&self) -> impl Iterator<Item = &str> + '_ {
        self.points
            .iter()
            .filter(|point| point.level > 0)
            .map(|point| point.id.as_str())
    }

    /// The abilities mapped to ability key `key` (0, 1 or 2) over every weapon tree, as CRC-32
    /// of their lowercase ids; empty slots left out.
    pub(super) fn mapped_to(&self, key: usize) -> impl Iterator<Item = u32> + '_ {
        self.slots
            .values()
            .filter_map(move |slots| slots.get(key).copied())
            .filter(|ability| *ability != 0)
    }

    /// The masteries of `saved` (id to total experience): the ones `progression` knows go to
    /// the client; another id is kept unsent, with a log line. The client's lists go on above
    /// `versions` (the 911 and 3681 versions it got from the sim before; 0 for none): retail's
    /// list versions only rise.
    pub(super) fn of(
        saved: &BTreeMap<String, u32>,
        progression: &Progression,
        versions: (u64, u64),
    ) -> Self {
        let mut entries = Vec::new();
        let mut kept = BTreeMap::new();
        for (id, experience) in saved {
            match progression.categorical(id) {
                Some(mastery) => {
                    entries.push((
                        mastery.id.clone(),
                        (*experience).min(mastery.max_experience()),
                    ));
                }
                None => {
                    tracing::warn!(mastery = %id, "unknown weapon mastery kept unsent");
                    kept.insert(id.clone(), *experience);
                }
            }
        }
        Self {
            entries,
            kept,
            version: versions.0,
            pools_version: versions.1,
            ..Self::default()
        }
    }

    /// These masteries with the saved weapon points `saved` (point id, level, in assignment
    /// order): the points `progression` knows count; another is kept unsent, with a log line.
    pub(super) fn with_points(
        mut self,
        saved: &[(String, u16)],
        progression: &Progression,
    ) -> Self {
        for (id, level) in saved {
            match progression.points.point_by_id(id) {
                Some(rule) => self.points.push(WeaponPoint {
                    id: rule.id.clone(),
                    crc: rule.crc,
                    pool: rule.pool,
                    level: *level,
                    active: rule.active,
                }),
                None => {
                    tracing::warn!(point = %id, "unknown weapon point kept unsent");
                    self.kept_points.push((id.clone(), *level));
                }
            }
        }
        self
    }

    /// These masteries with the saved mapped slots `saved`: the slots of a tree and an ability
    /// `progression` knows count; another is kept unsent, with a log line. A save without slots
    /// (one made before the slots were saved) maps the assigned active abilities as an apply
    /// does.
    pub(super) fn with_slots(mut self, saved: &[SavedSlot], progression: &Progression) -> Self {
        if saved.is_empty() {
            self.fill_free_slots();
            return self;
        }
        for (pool_id, slot, ability_id) in saved {
            let known = progression.mastery(pool_id).zip(
                progression
                    .points
                    .point_by_id(ability_id)
                    .filter(|rule| rule.active),
            );
            match known {
                Some((mastery, rule))
                    if rule.pool == mastery.crc && usize::from(*slot) < MAX_MAPPED_SLOTS =>
                {
                    self.slots.entry(mastery.crc).or_default()[usize::from(*slot)] = rule.crc;
                }
                _ => {
                    tracing::warn!(pool = %pool_id, slot, ability = %ability_id, "unknown mapped slot kept unsent");
                    self.kept_slots
                        .push((pool_id.clone(), *slot, ability_id.clone()));
                }
            }
        }
        self
    }

    /// Each mapped slot, as a save or a handoff carries them: the tree by the id of its
    /// mastery entry (a slot holds only an assigned ability, whose mastery has a rank).
    pub(super) fn carried_slots(&self) -> Vec<SavedSlot> {
        self.slots
            .iter()
            .filter_map(|(pool, abilities)| {
                let (mastery, _) = self
                    .entries
                    .iter()
                    .find(|(id, _)| lowercase_crc32(id) == *pool)?;
                Some((mastery, abilities))
            })
            .flat_map(|(mastery, abilities)| {
                abilities
                    .iter()
                    .zip(0_u8..)
                    .filter(|(ability, _)| **ability != 0)
                    .filter_map(|(ability, slot)| {
                        let point = self.points.iter().find(|point| point.crc == *ability)?;
                        Some((mastery.clone(), slot, point.id.clone()))
                    })
            })
            .chain(self.kept_slots.iter().cloned())
            .collect()
    }

    /// Each weapon point and its level, as a save or a handoff carries them.
    pub(super) fn carried_points(&self) -> Vec<(String, u16)> {
        self.points
            .iter()
            .map(|point| (point.id.clone(), point.level))
            .chain(self.kept_points.iter().cloned())
            .collect()
    }

    /// Assigns `points` of pool `pool` to the progression point `point` (1112), as the content's
    /// `ProgressionPointData` allows; the next bundle sends the pool's count and the point list.
    pub(super) fn assign(
        &mut self,
        progression: &Progression,
        pool: u32,
        point: u32,
        points: u32,
    ) -> Result<(), AssignRefusal> {
        let rule = progression
            .points
            .point(point)
            .filter(|rule| rule.pool == pool)
            .ok_or(AssignRefusal::UnknownPoint)?;
        let points = u16::try_from(points)
            .ok()
            .filter(|points| *points > 0)
            .ok_or(AssignRefusal::BadLevel)?;
        let level = self.level_of(point);
        if level
            .checked_add(points)
            .is_none_or(|level| level > rule.max_level)
        {
            return Err(AssignRefusal::BadLevel);
        }
        if let Some((required, required_level)) = rule.requires
            && self.level_of(required) < required_level
        {
            return Err(AssignRefusal::Required);
        }
        if self.unspent(progression, pool) < points {
            return Err(AssignRefusal::NotEnoughPoints);
        }
        match self.points.iter_mut().find(|known| known.crc == point) {
            Some(known) => known.level += points,
            None => self.points.push(WeaponPoint {
                id: rule.id.clone(),
                crc: rule.crc,
                pool,
                level: points,
                active: rule.active,
            }),
        }
        self.assigned.insert(pool);
        Ok(())
    }

    /// The client applied its assigned points (2857): each assigned active ability that is in
    /// no slot goes into the first empty slot of its tree (retail mapped
    /// `Ability_Greatsword_Combo` and `Ability_Blunderbuss_AzothShrapnelBlast` into the first slot
    /// at their 2857, and no upgrade or passive); the next bundle sends the mapped abilities when
    /// they changed.
    pub(super) fn apply(&mut self) {
        self.fill_free_slots();
    }

    /// Puts each assigned active ability that is in no slot of its tree into the tree's first
    /// empty slot, in assignment order.
    fn fill_free_slots(&mut self) {
        for point in self
            .points
            .iter()
            .filter(|point| point.active && point.level > 0)
        {
            let slots = self.slots.entry(point.pool).or_default();
            if slots.contains(&point.crc) {
                continue;
            }
            if let Some(empty) = slots.iter_mut().find(|slot| **slot == 0) {
                *empty = point.crc;
            }
        }
    }

    /// Puts `ability` (0 empties the slot) into slot `slot` of the table at position `table` of
    /// the player's ability state (2701). An ability already in another slot of the table moves:
    /// the two slots swap (Inferred: the window drags one ability onto another's slot).
    pub(super) fn change_mapped(
        &mut self,
        progression: &Progression,
        table: u16,
        slot: u32,
        ability: u32,
    ) -> Result<(), MapRefusal> {
        let pool = Self::weapon_table(progression, table).ok_or(MapRefusal::UnknownTable)?;
        let slot = usize::try_from(slot)
            .ok()
            .filter(|slot| *slot < MAX_MAPPED_SLOTS)
            .ok_or(MapRefusal::UnknownSlot)?;
        let unlocked = ability == 0
            || self.points.iter().any(|point| {
                point.crc == ability && point.pool == pool && point.active && point.level > 0
            });
        if !unlocked {
            return Err(MapRefusal::NotUnlocked);
        }
        let slots = self.slots.entry(pool).or_default();
        if ability != 0
            && let Some(other) = slots.iter().position(|mapped| *mapped == ability)
        {
            slots.swap(other, slot);
        } else {
            slots[slot] = ability;
        }
        Ok(())
    }

    /// Resets the weapon tree of the table at position `table` of the player's ability state
    /// (186): every point of its pool goes back to 0 (retail kept the entries with level 0),
    /// its slots empty, and the respec's `AzothRespecCost` at the weapon's rank leaves the
    /// azoth. Returns the table's id (its pool).
    pub(super) fn respec(
        &mut self,
        progression: &Progression,
        table: u16,
    ) -> Result<u32, RespecRefusal> {
        let pool = Self::weapon_table(progression, table).ok_or(RespecRefusal::UnknownTable)?;
        let rank = progression
            .mastery_by_crc(pool)
            .and_then(|mastery| self.position(mastery))
            .and_then(|index| self.wire_entry(progression, index))
            .map_or(0, |entry| entry.rank);
        let cost = progression.respec_cost(rank);
        if cost > 0 {
            let azoth = progression
                .categorical(AZOTH_PROGRESSION)
                .ok_or(RespecRefusal::NotEnoughAzoth)?
                .clone();
            let balance = self.experience(&azoth);
            if balance < cost {
                return Err(RespecRefusal::NotEnoughAzoth);
            }
            self.set(&azoth, balance - cost);
        }
        for point in self.points.iter_mut().filter(|point| point.pool == pool) {
            point.level = 0;
        }
        self.slots.remove(&pool);
        self.assigned.insert(pool);
        Ok(pool)
    }

    /// The pool of the weapon tree at position `table` of the ability state the client has.
    fn weapon_table(progression: &Progression, table: u16) -> Option<u32> {
        let crc = *progression
            .points
            .ability_tables()
            .get(usize::from(table))?;
        progression.mastery_by_crc(crc).map(|mastery| mastery.crc)
    }

    /// Adds `amount` to the total experience of `mastery` (any categorical progression), at
    /// most its top.
    pub(super) fn gain(&mut self, mastery: &Mastery, amount: u32) {
        let experience = self.experience(mastery).saturating_add(amount);
        self.set(mastery, experience);
    }

    /// The rank (zero-based) of categorical progression `id`; 0 for one it does not have.
    pub(super) fn rank(&self, progression: &Progression, id: &str) -> u16 {
        progression
            .categorical(id)
            .and_then(|mastery| self.position(mastery))
            .and_then(|index| self.wire_entry(progression, index))
            .map_or(0, |entry| entry.rank)
    }

    /// The highest rank among the progressions it has whose id `on_table` takes.
    pub(super) fn highest_rank(
        &self,
        progression: &Progression,
        on_table: impl Fn(&str) -> bool,
    ) -> u16 {
        (0..self.entries.len())
            .filter(|index| on_table(&self.entries[*index].0))
            .filter_map(|index| self.wire_entry(progression, index))
            .map(|entry| entry.rank)
            .max()
            .unwrap_or_default()
    }

    /// The mapped abilities: whole with the first card, then after an apply, a slot change or
    /// a respec that changed them.
    pub(super) fn take_abilities(
        &mut self,
        progression: &Progression,
        first: bool,
    ) -> Option<AbilityState> {
        // Before the first card the client has no state to change: the card sends it whole.
        if !first && self.mapping_sent.is_none() {
            return None;
        }
        let mapping = self.mapping(progression);
        if !first && self.mapping_sent.as_ref() == Some(&mapping) {
            return None;
        }
        self.mapping_sent = Some(mapping.clone());
        Some(AbilityState {
            persistent: Some(mapping),
        })
    }

    /// The points in progression point `point`.
    fn level_of(&self, point: u32) -> u16 {
        self.points
            .iter()
            .find(|known| known.crc == point)
            .map_or(0, |known| known.level)
    }

    /// The unspent points of weapon pool `pool`: one per rank of its mastery, less the
    /// assigned ones.
    fn unspent(&self, progression: &Progression, pool: u32) -> u16 {
        let rank = progression
            .mastery_by_crc(pool)
            .and_then(|mastery| self.position(mastery))
            .and_then(|index| self.wire_entry(progression, index))
            .map_or(0, |entry| entry.rank);
        rank.saturating_sub(self.assigned_in(pool))
    }

    fn assigned_in(&self, pool: u32) -> u16 {
        self.points
            .iter()
            .filter(|point| point.pool == pool)
            .fold(0, |sum, point| sum.saturating_add(point.level))
    }

    /// The point list as the client keeps it.
    fn point_entries(&self) -> Vec<PointEntry> {
        self.points
            .iter()
            .map(|point| PointEntry {
                pool: point.pool,
                point: point.crc,
                level: point.level,
            })
            .chain(self.territory_points.iter().copied())
            .collect()
    }

    /// Spends one territory standing point (4040) on bonus `point` of `pool`, whose `MaxLevel`
    /// is `max_level`, while the pool holds `available` points in all: the bonus holds at most
    /// `max_level` and [`TERRITORY_POINT_MAX`] points (3681 note), and the points spent in the
    /// pool stay at most `available`. A spent point's state goes out with the next bundle's
    /// whole point list.
    pub(super) fn spend_territory_point(
        &mut self,
        pool: u32,
        point: u32,
        max_level: u16,
        available: u32,
    ) -> Result<(), TerritoryRefusal> {
        let spent = self
            .territory_points
            .iter()
            .filter(|entry| entry.pool == pool)
            .fold(0_u32, |sum, entry| {
                sum.saturating_add(u32::from(entry.level))
            });
        if spent >= available {
            return Err(TerritoryRefusal::NoPointLeft);
        }
        let most = max_level.min(TERRITORY_POINT_MAX);
        match self
            .territory_points
            .iter_mut()
            .find(|entry| entry.pool == pool && entry.point == point)
        {
            Some(entry) if entry.level >= most => return Err(TerritoryRefusal::AtMaxLevel),
            Some(entry) => entry.level += 1,
            None if most == 0 => return Err(TerritoryRefusal::AtMaxLevel),
            None => self.territory_points.push(PointEntry {
                pool,
                point,
                level: 1,
            }),
        }
        self.territory_changed = true;
        Ok(())
    }

    /// These masteries with the saved territory points `saved` (pool, point, level, in
    /// first-spent order), as a join or a handoff carries them.
    pub(super) fn with_territory_points(mut self, saved: &[(u32, u32, u16)]) -> Self {
        self.territory_points = saved
            .iter()
            .map(|(pool, point, level)| PointEntry {
                pool: *pool,
                point: *point,
                level: (*level).min(TERRITORY_POINT_MAX),
            })
            .collect();
        self
    }

    /// The territory points as a save or a handoff carries them.
    pub(super) fn carried_territory_points(&self) -> Vec<(u32, u32, u16)> {
        self.territory_points
            .iter()
            .map(|entry| (entry.pool, entry.point, entry.level))
            .collect()
    }

    /// Every ability table with its mapped slots, and the weapon pool versions.
    fn mapping(&self, progression: &Progression) -> PersistentAbilities {
        let rules = &progression.points;
        PersistentAbilities {
            tables: rules
                .ability_tables()
                .iter()
                .map(|table| {
                    let abilities = self.slots.get(table).copied().unwrap_or_default();
                    MappedTable {
                        table: *table,
                        slots: ABILITY_KEYS
                            .iter()
                            .zip(abilities)
                            .map(|(key, ability)| MappedSlot { ability, key: *key })
                            .collect(),
                    }
                })
                .collect(),
            pool_versions: rules.pool_versions().to_vec(),
        }
    }

    /// Each mastery id and its total experience, as a save or a handoff carries them.
    pub(super) fn carried(&self) -> BTreeMap<String, u32> {
        self.kept
            .iter()
            .map(|(id, experience)| (id.clone(), *experience))
            .chain(self.entries.iter().cloned())
            .collect()
    }

    /// The versions of the newest 911 and 3681 lists the client has, for the next sim.
    pub(super) fn versions(&self) -> (u64, u64) {
        (self.version, self.pools_version)
    }

    /// The total experience of `mastery`.
    pub(super) fn experience(&self, mastery: &Mastery) -> u32 {
        self.position(mastery)
            .map_or(0, |index| self.entries[index].1)
    }

    /// Sets the total experience of `mastery` to `experience`, at most its top rank's.
    pub(super) fn set(&mut self, mastery: &Mastery, experience: u32) {
        let experience = experience.min(mastery.max_experience());
        let index = match self.position(mastery) {
            Some(index) => index,
            None => {
                self.entries.push((mastery.id.clone(), 0));
                self.changed
                    .insert(self.entries.len() - 1, MasteryFields::ALL);
                self.entries.len() - 1
            }
        };
        let before = self.entries[index].1;
        if before == experience {
            return;
        }
        self.entries[index].1 = experience;
        let rank_changed = mastery.rank_of(before).0 != mastery.rank_of(experience).0;
        if rank_changed {
            self.ranked.insert(index);
        }
        let fields = if rank_changed {
            MasteryFields::RANK_AND_POINTS
        } else {
            MasteryFields::POINTS
        };
        self.changed
            .entry(index)
            .and_modify(|known| {
                known.rank |= fields.rank;
                known.points |= fields.points;
            })
            .or_insert(fields);
    }

    /// The whole lists for the first card: the masteries and their point pools.
    pub(super) fn first_states(
        &mut self,
        progression: &Progression,
    ) -> (CategoricalProgressionState, ProgressionPointState) {
        self.changed.clear();
        self.ranked.clear();
        self.version = next_full_version(self.version);
        self.pools_version = next_full_version(self.pools_version);
        let entries: Vec<MasteryEntry> = self.wire_entries(progression).collect();
        (
            CategoricalProgressionState::full(self.version, &entries),
            ProgressionPointState::full(
                self.pools_version,
                &self.pools(progression),
                &self.point_entries(),
            ),
        )
    }

    /// The changes since the last bundle: the 911 changes (`None` without one), and the point
    /// lists: the whole pool lists when a rank changed, else the counts of the pools an
    /// assignment changed, with the whole point list after an assignment (retail answer to a
    /// 1112, newchar 2026-09-18); `None` without a change.
    pub(super) fn take_change(
        &mut self,
        progression: &Progression,
    ) -> (
        Option<CategoricalProgressionState>,
        Option<ProgressionPointState>,
    ) {
        let assigned = std::mem::take(&mut self.assigned);
        let ranked = !std::mem::take(&mut self.ranked).is_empty();
        let territory = std::mem::take(&mut self.territory_changed);
        let points = (ranked || !assigned.is_empty() || territory).then(|| {
            self.pools_version += 1;
            let pools = self.pools(progression);
            let state = if ranked {
                ProgressionPointState::pools(self.pools_version, &pools)
            } else if !assigned.is_empty() {
                let counts: Vec<(u32, u16)> = pools
                    .iter()
                    .enumerate()
                    .filter(|(_, pool)| assigned.contains(&pool.pool))
                    .filter_map(|(index, pool)| Some((u32::try_from(index).ok()?, pool.count)))
                    .collect();
                ProgressionPointState::pool_counts(self.pools_version, &counts)
            } else {
                // Only a territory point changed: the whole point list alone.
                ProgressionPointState::default()
            };
            if assigned.is_empty() && !territory {
                state
            } else {
                state.with_points(self.pools_version, &self.point_entries())
            }
        });
        (self.take_mastery_change(progression), points)
    }

    /// The 911 changes since the last bundle; `None` without a change.
    fn take_mastery_change(
        &mut self,
        progression: &Progression,
    ) -> Option<CategoricalProgressionState> {
        if self.changed.is_empty() {
            return None;
        }
        self.version += 1;
        let changes: Vec<MasteryChange> = std::mem::take(&mut self.changed)
            .into_iter()
            .filter_map(|(index, fields)| {
                Some(MasteryChange {
                    index: u32::try_from(index).ok()?,
                    entry: self.wire_entry(progression, index)?,
                    fields,
                })
            })
            .collect();
        Some(CategoricalProgressionState::changes(self.version, &changes))
    }

    fn position(&self, mastery: &Mastery) -> Option<usize> {
        self.entries
            .iter()
            .position(|(id, _)| id.eq_ignore_ascii_case(&mastery.id))
    }

    fn wire_entry(&self, progression: &Progression, index: usize) -> Option<MasteryEntry> {
        let (id, experience) = self.entries.get(index)?;
        let mastery = progression.categorical(id)?;
        let (rank, points) = mastery.rank_of(*experience);
        Some(MasteryEntry {
            progression_id: mastery.crc,
            rank,
            points,
        })
    }

    fn wire_entries<'a>(
        &'a self,
        progression: &'a Progression,
    ) -> impl Iterator<Item = MasteryEntry> + 'a {
        (0..self.entries.len()).filter_map(|index| self.wire_entry(progression, index))
    }

    /// One weapon point pool per weapon mastery with a rank: one point per rank, less the
    /// assigned ones. A tradeskill has no pool. The territory pools with a spent point
    /// follow: the content holds no territory pools, so their unspent count is Unknown
    /// and 0 is sent.
    fn pools(&self, progression: &Progression) -> Vec<PointPool> {
        let mut pools: Vec<PointPool> = self
            .wire_entries(progression)
            .filter(|entry| entry.rank > 0)
            .filter(|entry| progression.mastery_by_crc(entry.progression_id).is_some())
            .map(|entry| PointPool {
                pool: entry.progression_id,
                count: entry
                    .rank
                    .saturating_sub(self.assigned_in(entry.progression_id)),
                category: PoolCategory::Weapon,
            })
            .collect();
        for entry in &self.territory_points {
            if !pools.iter().any(|pool| pool.pool == entry.pool) {
                pools.push(PointPool {
                    pool: entry.pool,
                    count: 0,
                    category: PoolCategory::Territory,
                });
            }
        }
        pools
    }
}

/// The version of a full list after `last`: the first one, or the one above `last`.
fn next_full_version(last: u64) -> u64 {
    if last < FIRST_VERSION {
        FIRST_VERSION
    } else {
        last.saturating_add(1)
    }
}

impl SimWorld {
    /// Adds `amount` experience points to the player `entity`, at most the top level's start;
    /// the next own state sends them and a new level.
    pub(super) fn give_experience(&mut self, entity: u64, amount: u32) -> AdminReply {
        let top = self
            .content
            .progression
            .experience_for_level(self.content.progression.max_level())
            .unwrap_or(u32::MAX);
        self.with_player(entity, |player| {
            player.experience = player.experience.saturating_add(amount).min(top);
            player.experience_changed = true;
            AdminReply::Done
        })
    }

    /// Sets the player `entity` at the start of `level` (zero-based), at most the top level.
    pub(super) fn set_level(&mut self, entity: u64, level: u32) -> AdminReply {
        let progression = &self.content.progression;
        let Some(experience) = progression.experience_for_level(level.min(progression.max_level()))
        else {
            return AdminReply::Failed {
                reason: NO_LEVELS.to_owned(),
            };
        };
        self.with_player(entity, |player| {
            player.experience = experience;
            player.experience_changed = true;
            AdminReply::Done
        })
    }

    /// Adds `amount` experience to the weapon mastery `id` of the player `entity`.
    pub(super) fn give_mastery_experience(
        &mut self,
        entity: u64,
        id: &str,
        amount: u32,
    ) -> AdminReply {
        let Some(mastery) = self.content.progression.categorical(id).cloned() else {
            return unknown_mastery(id);
        };
        self.with_player(entity, |player| {
            player.masteries.gain(&mastery, amount);
            AdminReply::Done
        })
    }

    /// Sets the weapon mastery `id` of the player `entity` (every one for `None`) at the start
    /// of `rank` (zero-based), at most the top rank.
    pub(super) fn set_mastery_rank(
        &mut self,
        entity: u64,
        id: Option<&str>,
        rank: u16,
    ) -> AdminReply {
        let masteries: Vec<Mastery> = match id {
            Some(id) => match self.content.progression.categorical(id) {
                Some(mastery) => vec![mastery.clone()],
                None => return unknown_mastery(id),
            },
            None => self.content.progression.masteries().to_vec(),
        };
        if masteries.is_empty() {
            return AdminReply::Failed {
                reason: "the content has no weapon masteries".to_owned(),
            };
        }
        self.with_player(entity, |player| {
            for mastery in &masteries {
                player
                    .masteries
                    .set(mastery, mastery.experience_for_rank(rank));
            }
            AdminReply::Done
        })
    }

    /// Sets the coins of the player `entity`, at most what a save keeps.
    pub(super) fn set_coins(&mut self, entity: u64, coins: u64) -> AdminReply {
        self.with_player(entity, |player| {
            player.coins = coins.min(MAX_COINS);
            player.coins_changed = true;
            AdminReply::Done
        })
    }

    /// Sets the azoth of the player `entity`, at most the azoth cap (the top of the
    /// `Azoth_Currency` progression).
    pub(super) fn set_azoth(&mut self, entity: u64, azoth: u32) -> AdminReply {
        let Some(currency) = self
            .content
            .progression
            .categorical(AZOTH_PROGRESSION)
            .cloned()
        else {
            return unknown_mastery(AZOTH_PROGRESSION);
        };
        self.with_player(entity, |player| {
            player.masteries.set(&currency, azoth);
            AdminReply::Done
        })
    }

    /// Runs `change` on the player `entity`; [`AdminReply::NotMine`] when it is not here.
    fn with_player(
        &mut self,
        entity: u64,
        change: impl FnOnce(&mut Player) -> AdminReply,
    ) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        match self.world.get_mut::<Player>(target) {
            Some(mut player) => change(&mut player),
            None => AdminReply::NotMine,
        }
    }
}

fn unknown_mastery(id: &str) -> AdminReply {
    AdminReply::Failed {
        reason: format!("no categorical progression {id}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_bus::{AdminAction, AdminCommand};
    use uuid::Uuid;

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, world_with_spawns};

    const PLAYER: u64 = 41;

    const SWORD: &str = "GreatSwordAbilityTable";
    const BUSS: &str = "BlunderbussAbilityTable";
    const RANK_CAPS: [i64; 3] = [10, 50, 70];
    const TOP_RANK: i64 = 3;

    fn progression() -> Progression {
        Progression::build(
            [(0, 0), (1, 150)],
            (0..).zip(RANK_CAPS),
            [(SWORD, TOP_RANK), (BUSS, TOP_RANK)],
        )
    }

    #[test]
    fn territory_points_spend_to_the_cap_and_are_carried() {
        const POOL: u32 = 0xd65a_1577;
        const POINT: u32 = 0x1234_5678;
        let progression = progression();
        const AVAILABLE: u32 = 30;
        let mut masteries = Masteries::default();
        assert_eq!(
            masteries.spend_territory_point(POOL, POINT, TERRITORY_POINT_MAX, AVAILABLE),
            Ok(())
        );
        let (_, pools) = masteries.take_change(&progression);
        let pools = pools.expect("a point change");
        assert_eq!(
            pools.point_entries,
            ProgressionPointState::full(
                1,
                &[],
                &[PointEntry {
                    pool: POOL,
                    point: POINT,
                    level: 1,
                }]
            )
            .point_entries
        );
        assert_eq!(masteries.take_change(&progression), (None, None));
        for _ in 1..TERRITORY_POINT_MAX {
            assert_eq!(
                masteries.spend_territory_point(POOL, POINT, TERRITORY_POINT_MAX, AVAILABLE),
                Ok(())
            );
        }
        assert_eq!(
            masteries.spend_territory_point(POOL, POINT, TERRITORY_POINT_MAX, AVAILABLE),
            Err(TerritoryRefusal::AtMaxLevel),
            "at the cap"
        );
        assert!(masteries.take_change(&progression).1.is_some());
        assert_eq!(
            masteries.carried_territory_points(),
            [(POOL, POINT, TERRITORY_POINT_MAX)]
        );
        let mut restored =
            Masteries::default().with_territory_points(&masteries.carried_territory_points());
        assert_eq!(
            restored.carried_territory_points(),
            [(POOL, POINT, TERRITORY_POINT_MAX)]
        );
        let (_, first) = restored.first_states(&progression);
        assert_eq!(
            first.point_entries,
            ProgressionPointState::full(
                FIRST_VERSION,
                &[PointPool {
                    pool: POOL,
                    count: 0,
                    category: PoolCategory::Territory,
                }],
                &[PointEntry {
                    pool: POOL,
                    point: POINT,
                    level: TERRITORY_POINT_MAX,
                }],
            )
            .point_entries
        );
    }

    #[test]
    fn a_territory_point_needs_an_unspent_point_of_its_pool() {
        const POOL: u32 = 0xd65a_1577;
        const OTHER_POOL: u32 = 0x0bad_cafe;
        const POINT: u32 = 0x1234_5678;
        const OTHER_POINT: u32 = 0x8765_4321;
        const MAX_LEVEL: u16 = 5;
        let mut masteries = Masteries::default();
        assert_eq!(
            masteries.spend_territory_point(POOL, POINT, MAX_LEVEL, 1),
            Ok(())
        );
        assert_eq!(
            masteries.spend_territory_point(POOL, OTHER_POINT, MAX_LEVEL, 1),
            Err(TerritoryRefusal::NoPointLeft),
            "the pool's one point is spent"
        );
        assert_eq!(
            masteries.spend_territory_point(OTHER_POOL, POINT, MAX_LEVEL, 0),
            Err(TerritoryRefusal::NoPointLeft),
            "a pool without standing holds none"
        );
        assert_eq!(masteries.carried_territory_points(), [(POOL, POINT, 1)]);
    }

    #[test]
    fn a_first_kill_adds_entries_and_a_rank_up_sends_the_pools() {
        let progression = progression();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let buss = progression.mastery(BUSS).expect("buss").clone();
        let mut masteries = Masteries::of(&BTreeMap::new(), &progression, (0, 0));
        let (first, pools) = masteries.first_states(&progression);
        assert_eq!(first, CategoricalProgressionState::full(FIRST_VERSION, &[]));
        assert_eq!(pools, ProgressionPointState::full(FIRST_VERSION, &[], &[]));
        assert_eq!(masteries.take_change(&progression), (None, None));
        // Retail first kill: greatsword +20 (rank 1, 10 points), blunderbuss +4.
        masteries.set(&sword, 20);
        masteries.set(&buss, 4);
        let (change, pools) = masteries.take_change(&progression);
        let entry = |crc, rank, points| MasteryEntry {
            progression_id: crc,
            rank,
            points,
        };
        assert_eq!(
            change,
            Some(CategoricalProgressionState::changes(
                FIRST_VERSION + 1,
                &[
                    MasteryChange {
                        index: 0,
                        entry: entry(sword.crc, 1, 10),
                        fields: MasteryFields::ALL,
                    },
                    MasteryChange {
                        index: 1,
                        entry: entry(buss.crc, 0, 4),
                        fields: MasteryFields::ALL,
                    },
                ]
            ))
        );
        let pool = PointPool {
            pool: sword.crc,
            count: 1,
            category: PoolCategory::Weapon,
        };
        assert_eq!(
            pools,
            Some(ProgressionPointState::pools(FIRST_VERSION + 1, &[pool]))
        );
        // Points alone: no pool list.
        masteries.set(&buss, 5);
        let (change, pools) = masteries.take_change(&progression);
        assert_eq!(
            change,
            Some(CategoricalProgressionState::changes(
                FIRST_VERSION + 2,
                &[MasteryChange {
                    index: 1,
                    entry: entry(buss.crc, 0, 5),
                    fields: MasteryFields::POINTS,
                }]
            ))
        );
        assert_eq!(pools, None);
    }

    #[test]
    fn a_handed_off_player_gets_lists_above_the_versions_it_had() {
        let progression = progression();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let carried = BTreeMap::from([(SWORD.to_owned(), 20)]);
        let mut masteries = Masteries::of(&carried, &progression, (12, 7));
        let (first, pools) = masteries.first_states(&progression);
        let entry = MasteryEntry {
            progression_id: sword.crc,
            rank: 1,
            points: 10,
        };
        assert_eq!(first, CategoricalProgressionState::full(13, &[entry]));
        let pool = PointPool {
            pool: sword.crc,
            count: 1,
            category: PoolCategory::Weapon,
        };
        assert_eq!(pools, ProgressionPointState::full(8, &[pool], &[]));
        assert_eq!(masteries.versions(), (13, 8));
    }

    #[test]
    fn experience_stops_at_the_top_rank_and_unknown_ids_are_kept() {
        let progression = progression();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let saved = BTreeMap::from([(SWORD.to_owned(), u32::MAX), ("Cooking".to_owned(), 7)]);
        let mut masteries = Masteries::of(&saved, &progression, (0, 0));
        assert_eq!(masteries.experience(&sword), sword.max_experience());
        assert_eq!(
            masteries.carried(),
            BTreeMap::from([
                (SWORD.to_owned(), sword.max_experience()),
                ("Cooking".to_owned(), 7),
            ]),
            "an id the content does not know is kept for the save"
        );
        masteries.set(&sword, u32::MAX);
        assert_eq!(
            masteries.take_change(&progression),
            (None, None),
            "no change at the top"
        );
        let (first, _) = masteries.first_states(&progression);
        assert_eq!(
            first,
            CategoricalProgressionState::full(
                FIRST_VERSION,
                &[MasteryEntry {
                    progression_id: sword.crc,
                    rank: 3,
                    points: 0,
                }]
            )
        );
    }

    /// The rules of the sword tree's first two points (retail rows).
    fn with_points() -> Progression {
        use nw_content::character_points::{CharacterPoints, PointRow};
        let mut progression = progression();
        let point = |id, requires| PointRow {
            id,
            pool: SWORD,
            max_level: MAX_POINT_LEVEL,
            requires,
        };
        progression.points = CharacterPoints::build(
            Vec::new(),
            Vec::new(),
            [SWORD_TABLE],
            [COMBO],
            [(SWORD, SWORD_VERSION)],
            [point(COMBO, None), point(UPGRADE, Some((COMBO, 1)))],
        );
        progression
    }

    const SWORD_TABLE: &str = "GreatswordAbilityTable";
    const COMBO: &str = "Ability_Greatsword_Combo";
    const UPGRADE: &str = "Upgrade_Greatsword_Combo_1";
    const SWORD_VERSION: i64 = 1;
    const MAX_POINT_LEVEL: i64 = 10;
    /// Rank 2 of the test caps: two points.
    const RANK_TWO_XP: u32 = 60;

    #[test]
    fn assigned_points_leave_the_pool_and_go_out_as_retail_answers() {
        let progression = with_points();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let combo = progression.points.point_by_id(COMBO).expect("combo").crc;
        let upgrade = progression
            .points
            .point_by_id(UPGRADE)
            .expect("upgrade")
            .crc;
        let saved = BTreeMap::from([(SWORD.to_owned(), RANK_TWO_XP)]);
        let mut masteries = Masteries::of(&saved, &progression, (0, 0))
            .with_points(&[("Retired_Point".to_owned(), 1)], &progression);
        masteries.first_states(&progression);
        assert_eq!(
            masteries.assign(&progression, sword.crc, upgrade, 1),
            Err(AssignRefusal::Required)
        );
        assert_eq!(
            masteries.assign(&progression, sword.crc, 0, 1),
            Err(AssignRefusal::UnknownPoint)
        );
        assert_eq!(
            masteries.assign(&progression, 0, combo, 1),
            Err(AssignRefusal::UnknownPoint)
        );
        assert_eq!(
            masteries.assign(&progression, sword.crc, combo, 0),
            Err(AssignRefusal::BadLevel)
        );
        assert_eq!(
            masteries.assign(&progression, sword.crc, combo, 3),
            Err(AssignRefusal::NotEnoughPoints)
        );
        assert_eq!(masteries.assign(&progression, sword.crc, combo, 1), Ok(()));
        let entry = |point, level| PointEntry {
            pool: sword.crc,
            point,
            level,
        };
        // Retail answer: the pool's count and the whole point list.
        assert_eq!(
            masteries.take_change(&progression),
            (
                None,
                Some(
                    ProgressionPointState::pool_counts(FIRST_VERSION + 1, &[(0, 1)])
                        .with_points(FIRST_VERSION + 1, &[entry(combo, 1)])
                )
            )
        );
        assert_eq!(
            masteries.assign(&progression, sword.crc, upgrade, 1),
            Ok(())
        );
        assert_eq!(
            masteries.assign(&progression, sword.crc, upgrade, 1),
            Err(AssignRefusal::NotEnoughPoints)
        );
        assert_eq!(
            masteries.carried_points(),
            [
                (COMBO.to_owned(), 1),
                (UPGRADE.to_owned(), 1),
                ("Retired_Point".to_owned(), 1)
            ],
            "an unknown saved point is kept for the save"
        );
        // A later login lists the points and an empty pool.
        let (_, pools) = masteries.first_states(&progression);
        let pool = PointPool {
            pool: sword.crc,
            count: 0,
            category: PoolCategory::Weapon,
        };
        assert_eq!(
            pools,
            ProgressionPointState::full(
                FIRST_VERSION + 2,
                &[pool],
                &[entry(combo, 1), entry(upgrade, 1)]
            )
        );
    }

    #[test]
    fn applied_active_abilities_fill_the_first_free_slot() {
        let progression = with_points();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let combo = progression.points.point_by_id(COMBO).expect("combo").crc;
        let upgrade = progression
            .points
            .point_by_id(UPGRADE)
            .expect("upgrade")
            .crc;
        let saved = BTreeMap::from([(SWORD.to_owned(), RANK_TWO_XP)]);
        let mut masteries = Masteries::of(&saved, &progression, (0, 0));
        masteries.apply();
        assert_eq!(
            masteries.take_abilities(&progression, false),
            None,
            "nothing before the card"
        );
        let slots = |state: Option<AbilityState>| -> Vec<u32> {
            state
                .and_then(|state| state.persistent)
                .map(|abilities| {
                    abilities.tables[0]
                        .slots
                        .iter()
                        .map(|slot| slot.ability)
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            slots(masteries.take_abilities(&progression, true)),
            [0, 0, 0]
        );
        masteries
            .assign(&progression, sword.crc, combo, 1)
            .expect("combo");
        assert_eq!(
            masteries.take_abilities(&progression, false),
            None,
            "not applied"
        );
        masteries.apply();
        assert_eq!(
            slots(masteries.take_abilities(&progression, false)),
            [combo, 0, 0]
        );
        // An upgrade is no active ability: the mapping stays, so nothing goes out.
        masteries
            .assign(&progression, sword.crc, upgrade, 1)
            .expect("upgrade");
        masteries.apply();
        assert_eq!(masteries.take_abilities(&progression, false), None);
    }

    /// A second active ability of the sword tree.
    const SLAM: &str = "Ability_Greatsword_Slam";
    /// The sword tree's position in the ability state.
    const SWORD_TABLE_INDEX: u16 = 0;
    /// `AzothCurrency` rank 0 cap, and the retail respec cost of the top weapon rank.
    const AZOTH_CAP: i64 = 2_500;
    const TOP_RESPEC: i64 = 50;
    /// Rank 3 of the test caps (the top): three points.
    const RANK_THREE_XP: u32 = 130;

    /// The sword tree with two active abilities and the upgrade, the azoth currency, and the
    /// respec cost of the top rank.
    fn with_tree() -> Progression {
        use nw_content::character_points::{CharacterPoints, PointRow};
        use nw_content::progression::ProgressionSource;
        let mut progression = progression()
            .with_progressions([ProgressionSource {
                id: AZOTH_PROGRESSION.to_owned(),
                max_level: 0,
                auto_rank_up: false,
                ranks: vec![(0, AZOTH_CAP)],
            }])
            .with_respec_costs([(TOP_RANK, TOP_RESPEC)]);
        let point = |id, requires| PointRow {
            id,
            pool: SWORD,
            max_level: MAX_POINT_LEVEL,
            requires,
        };
        progression.points = CharacterPoints::build(
            Vec::new(),
            Vec::new(),
            [SWORD_TABLE],
            [COMBO, SLAM],
            [(SWORD, SWORD_VERSION)],
            [
                point(COMBO, None),
                point(SLAM, None),
                point(UPGRADE, Some((COMBO, 1))),
            ],
        );
        progression
    }

    fn sword_slots(masteries: &mut Masteries, progression: &Progression) -> Vec<u32> {
        masteries
            .take_abilities(progression, true)
            .and_then(|state| state.persistent)
            .map(|abilities| {
                abilities.tables[usize::from(SWORD_TABLE_INDEX)]
                    .slots
                    .iter()
                    .map(|slot| slot.ability)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_client_moves_unlocked_abilities_between_slots() {
        let progression = with_tree();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let crc = |id| progression.points.point_by_id(id).expect("point").crc;
        let saved = BTreeMap::from([(SWORD.to_owned(), RANK_THREE_XP)]);
        let mut masteries = Masteries::of(&saved, &progression, (0, 0));
        for point in [COMBO, SLAM, UPGRADE] {
            masteries
                .assign(&progression, sword.crc, crc(point), 1)
                .expect("assign");
        }
        masteries.apply();
        assert_eq!(
            sword_slots(&mut masteries, &progression),
            [crc(COMBO), crc(SLAM), 0],
            "an apply fills the first empty slots in assignment order"
        );
        assert_eq!(
            masteries.change_mapped(&progression, SWORD_TABLE_INDEX, 2, crc(COMBO)),
            Ok(())
        );
        assert_eq!(
            sword_slots(&mut masteries, &progression),
            [0, crc(SLAM), crc(COMBO)],
            "a mapped ability moves"
        );
        assert_eq!(
            masteries.change_mapped(&progression, SWORD_TABLE_INDEX, 0, crc(SLAM)),
            Ok(())
        );
        assert_eq!(
            sword_slots(&mut masteries, &progression),
            [crc(SLAM), 0, crc(COMBO)],
            "two slots swap"
        );
        assert_eq!(
            masteries.change_mapped(&progression, SWORD_TABLE_INDEX, 1, crc(UPGRADE)),
            Err(MapRefusal::NotUnlocked),
            "an upgrade is no active ability"
        );
        assert_eq!(
            masteries.change_mapped(&progression, SWORD_TABLE_INDEX, 3, crc(SLAM)),
            Err(MapRefusal::UnknownSlot)
        );
        assert_eq!(
            masteries.change_mapped(&progression, 1, 0, crc(SLAM)),
            Err(MapRefusal::UnknownTable)
        );
        // The slots go to the save, and a later login keeps them.
        let carried = masteries.carried_slots();
        assert_eq!(
            carried,
            [
                (SWORD.to_owned(), 0, SLAM.to_owned()),
                (SWORD.to_owned(), 2, COMBO.to_owned()),
            ]
        );
        let mut later = Masteries::of(&saved, &progression, (0, 0))
            .with_points(&masteries.carried_points(), &progression)
            .with_slots(&carried, &progression);
        assert_eq!(
            sword_slots(&mut later, &progression),
            [crc(SLAM), 0, crc(COMBO)]
        );
        // A save from before the slots maps the active abilities as an apply does.
        let mut legacy = Masteries::of(&saved, &progression, (0, 0))
            .with_points(&masteries.carried_points(), &progression)
            .with_slots(&[], &progression);
        assert_eq!(
            sword_slots(&mut legacy, &progression),
            [crc(COMBO), crc(SLAM), 0]
        );
    }

    #[test]
    fn a_respec_costs_azoth_by_rank_and_returns_the_points() {
        let progression = with_tree();
        let sword = progression.mastery(SWORD).expect("sword").clone();
        let azoth = progression
            .categorical(AZOTH_PROGRESSION)
            .expect("azoth")
            .clone();
        let crc = |id| progression.points.point_by_id(id).expect("point").crc;
        let saved = BTreeMap::from([(SWORD.to_owned(), sword.max_experience())]);
        let mut masteries = Masteries::of(&saved, &progression, (0, 0));
        masteries.first_states(&progression);
        masteries.take_abilities(&progression, true);
        for point in [COMBO, UPGRADE] {
            masteries
                .assign(&progression, sword.crc, crc(point), 1)
                .expect("assign");
        }
        masteries.apply();
        masteries.take_change(&progression);
        // The top rank costs 50 azoth.
        assert_eq!(
            masteries.respec(&progression, SWORD_TABLE_INDEX),
            Err(RespecRefusal::NotEnoughAzoth)
        );
        masteries.gain(&azoth, u32::MAX);
        assert_eq!(
            masteries.experience(&azoth),
            2_500,
            "the balance stops at the cap"
        );
        masteries.take_change(&progression);
        assert_eq!(
            masteries.respec(&progression, 1),
            Err(RespecRefusal::UnknownTable)
        );
        assert_eq!(
            masteries.respec(&progression, SWORD_TABLE_INDEX),
            Ok(sword.crc)
        );
        assert_eq!(masteries.experience(&azoth), 2_450);
        assert_eq!(
            masteries.carried_points(),
            [(COMBO.to_owned(), 0), (UPGRADE.to_owned(), 0)],
            "retail keeps the points with level 0"
        );
        let (azoth_change, pools) = masteries.take_change(&progression);
        assert!(azoth_change.is_some(), "the azoth goes out");
        let entry = |point| PointEntry {
            pool: sword.crc,
            point: crc(point),
            level: 0,
        };
        let version = FIRST_VERSION + 2;
        assert_eq!(
            pools,
            Some(
                ProgressionPointState::pool_counts(version, &[(0, 3)])
                    .with_points(version, &[entry(COMBO), entry(UPGRADE)])
            ),
            "the pool gets its three points back"
        );
        assert_eq!(sword_slots(&mut masteries, &progression), [0, 0, 0]);
        assert_eq!(masteries.rank(&progression, SWORD), 3);
        assert_eq!(masteries.rank(&progression, "Cooking"), 0);
    }

    fn command(action: AdminAction) -> AdminCommand {
        AdminCommand {
            request: Uuid::from_u128(1),
            actor: None,
            action,
        }
    }

    /// A world with the test progression and one player, outside the tutorial.
    fn admitted() -> SimWorld {
        admitted_with(progression())
    }

    fn admitted_with(progression: Progression) -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(PlayerContent {
            progression,
            ..PlayerContent::default()
        });
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        world
    }

    fn saved_progress(world: &mut SimWorld) -> Option<nw_bus::PlayerProgression> {
        world
            .positions()
            .into_iter()
            .find_map(|departure| departure.progression)
    }

    #[test]
    fn gm_commands_change_the_progress_and_it_is_saved_once() {
        let mut world = admitted();
        assert_eq!(saved_progress(&mut world), None, "nothing changed");
        let run = |world: &mut SimWorld, action| world.admin(&command(action), NOW);
        assert_eq!(
            run(
                &mut world,
                AdminAction::SetLevel {
                    entity: PLAYER,
                    level: 9
                }
            ),
            AdminReply::Done
        );
        assert_eq!(
            run(
                &mut world,
                AdminAction::GiveMasteryExperience {
                    entity: PLAYER,
                    mastery: SWORD.to_lowercase(),
                    amount: 20,
                }
            ),
            AdminReply::Done
        );
        assert_eq!(
            run(
                &mut world,
                AdminAction::SetCoins {
                    entity: PLAYER,
                    coins: 77
                }
            ),
            AdminReply::Done
        );
        let progress = saved_progress(&mut world).expect("a save");
        // Level 9 is above the top level (1): the top level's start.
        assert_eq!(progress.experience, 150);
        assert_eq!(progress.coins, 77);
        assert_eq!(progress.masteries, BTreeMap::from([(SWORD.to_owned(), 20)]));
        assert_eq!(saved_progress(&mut world), None, "saved once");
        assert_eq!(
            run(
                &mut world,
                AdminAction::SetMasteryRank {
                    entity: PLAYER,
                    mastery: None,
                    rank: u16::MAX,
                }
            ),
            AdminReply::Done
        );
        let top = progression()
            .mastery(SWORD)
            .expect("sword")
            .max_experience();
        let progress = saved_progress(&mut world).expect("a save");
        assert_eq!(
            progress.masteries,
            BTreeMap::from([(SWORD.to_owned(), top), (BUSS.to_owned(), top)])
        );
        assert_eq!(
            run(
                &mut world,
                AdminAction::GiveExperience {
                    entity: PLAYER,
                    amount: u32::MAX
                }
            ),
            AdminReply::Done
        );
        assert_eq!(
            saved_progress(&mut world),
            None,
            "experience stops at the top level's start"
        );
    }

    #[test]
    fn set_azoth_sets_the_balance_up_to_the_cap() {
        const AZOTH: u32 = 77;
        let progression = with_tree();
        let cap = progression
            .categorical(AZOTH_PROGRESSION)
            .expect("azoth")
            .max_experience();
        let mut world = admitted_with(progression);
        let set = |world: &mut SimWorld, azoth| {
            world.admin(
                &command(AdminAction::SetAzoth {
                    entity: PLAYER,
                    azoth,
                }),
                NOW,
            )
        };
        assert_eq!(set(&mut world, AZOTH), AdminReply::Done);
        let progress = saved_progress(&mut world).expect("a save");
        assert_eq!(progress.masteries.get(AZOTH_PROGRESSION), Some(&AZOTH));
        assert_eq!(set(&mut world, u32::MAX), AdminReply::Done);
        let progress = saved_progress(&mut world).expect("a save");
        assert_eq!(progress.masteries.get(AZOTH_PROGRESSION), Some(&cap));
        assert!(matches!(
            set(&mut admitted(), AZOTH),
            AdminReply::Failed { .. }
        ));
    }

    #[test]
    fn an_unknown_mastery_fails_and_an_absent_player_is_not_mine() {
        let mut world = admitted();
        assert!(matches!(
            world.admin(
                &command(AdminAction::GiveMasteryExperience {
                    entity: PLAYER,
                    mastery: "Cooking".to_owned(),
                    amount: 1,
                }),
                NOW
            ),
            AdminReply::Failed { .. }
        ));
        assert_eq!(
            world.admin(
                &command(AdminAction::GiveExperience {
                    entity: PLAYER + 1,
                    amount: 1,
                }),
                NOW
            ),
            AdminReply::NotMine
        );
    }
}
