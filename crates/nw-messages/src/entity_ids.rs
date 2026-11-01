// SPDX-License-Identifier: AGPL-3.0-or-later
//! The raw ids (`GdeRef`) of the dynamic entities this server spawns: players, mobs and the
//! other entities it sends a spawn card with an asset for (GDE id note, "Dynamic entities").
//!
//! **Observed** (690 retail ledgers, 39,430 mob, 403 player and 51,899 other dynamic cards): a
//! retail dynamic id is 16 random bytes, made per spawn and per session, with bit 0 of the first
//! byte clear; no half is ever all zero, and the halves are independent. The client reads the
//! first 8 bytes as a little-endian `u64`: its entity id (the facet entity), and the offline
//! cache key. With the default `javelin.offline-id-version` 2 and
//! `javelin.enable-offline-capitals` on, `FUN_146167060` takes an id with bit 0 set as a managed
//! offline (level) object, so a dynamic id must keep bit 0 clear. A mob that respawns gets a new
//! id and a new card (GDE id note).
//!
//! The ids carry no entity number, so a registry maps them both ways. It is process-wide: the
//! sims of one process share it, and a sim that takes a player or shows another sim's entity
//! adopts the owner's id ([`adopt_raw_id`]) so the entity keeps one id. Each user of an entry
//! holds it ([`IdHolder`]); the entry goes when its last holder releases it
//! ([`release_raw_id`]), so the table holds only the entities in some sim or view now.

use std::collections::HashMap;

use rustc_hash::FxHashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{OnceLock, PoisonError, RwLock};

use thiserror::Error;

/// Bytes of a raw id field.
pub const RAW_ID_LEN: usize = 16;
/// Bytes of one half of a raw id; the first half is the facet entity.
const HALF_LEN: usize = RAW_ID_LEN / 2;
/// Bit 0 of the first byte: set on a level (offline) object, clear on a dynamic entity.
const OFFLINE_BIT: u8 = 1;
/// The first number [`number_of_seen_raw_id`] gives an id this process did not make. Far above
/// every server range (mobs `1 << 34`, assets `1 << 41`), so a client's numbers never meet them.
const SEEN_NUMBER_BASE: u64 = 1 << 62;

/// One user of registry entries: a sim world or a client view. It holds the entries of the
/// entities it has and releases each when the entity leaves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IdHolder(u64);

impl IdHolder {
    /// A holder no other one equals.
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for IdHolder {
    fn default() -> Self {
        Self::new()
    }
}

/// An id that cannot be adopted.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RawIdError {
    /// The id is not of the dynamic form (bit 0 of the first byte set, or a zero half).
    #[error("raw id is not of the dynamic form")]
    NotDynamic,
    /// Another entity already has this id or its facet entity.
    #[error("the raw id belongs to entity {other}")]
    IdTaken {
        /// The entity that has it.
        other: u64,
    },
}

/// What an adoption changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adopted {
    /// The entity had this id already.
    Same,
    /// The entity had no id here.
    New,
    /// The entity had another id (its owner gave it a new one at a respawn): the clients that
    /// have its old card need a new one.
    Renewed,
}

#[derive(Debug)]
struct Entry {
    id: [u8; RAW_ID_LEN],
    holders: Vec<IdHolder>,
}

#[derive(Debug, Default)]
struct Registry {
    /// Looked up for every shown entity on every tick, so with a fast hash.
    by_number: FxHashMap<u64, Entry>,
    by_id: HashMap<[u8; RAW_ID_LEN], u64>,
    by_facet: HashMap<u64, u64>,
    next_seen: u64,
}

impl Registry {
    fn insert(&mut self, number: u64, id: [u8; RAW_ID_LEN], holders: Vec<IdHolder>) {
        if let Some(old) = self.by_number.insert(number, Entry { id, holders }) {
            self.forget_id(&old.id);
        }
        self.by_id.insert(id, number);
        self.by_facet.insert(facet_of_raw_id(&id), number);
    }

    fn forget_id(&mut self, id: &[u8; RAW_ID_LEN]) {
        self.by_id.remove(id);
        self.by_facet.remove(&facet_of_raw_id(id));
    }

    fn is_free(&self, id: &[u8; RAW_ID_LEN]) -> bool {
        !self.by_id.contains_key(id) && !self.by_facet.contains_key(&facet_of_raw_id(id))
    }

    /// A new random id that no entity here has.
    fn fresh_id(&self) -> [u8; RAW_ID_LEN] {
        loop {
            let id = random_dynamic_id();
            if self.is_free(&id) {
                return id;
            }
        }
    }

    /// The id of `number`, made now when it has none, held by `holder` when given.
    fn id_of(&mut self, number: u64, holder: Option<IdHolder>) -> [u8; RAW_ID_LEN] {
        if !self.by_number.contains_key(&number) {
            let id = self.fresh_id();
            self.insert(number, id, Vec::new());
        }
        let entry = self
            .by_number
            .get_mut(&number)
            .expect("the entry was just made");
        if let Some(holder) = holder
            && !entry.holders.contains(&holder)
        {
            entry.holders.push(holder);
        }
        entry.id
    }
}

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(RwLock::default)
}

/// Whether `id` has the retail dynamic form: bit 0 of the first byte clear and no zero half.
pub fn is_dynamic_raw_id(id: &[u8; RAW_ID_LEN]) -> bool {
    let (first, last) = id.split_at(HALF_LEN);
    id[0] & OFFLINE_BIT == 0
        && first.iter().any(|byte| *byte != 0)
        && last.iter().any(|byte| *byte != 0)
}

/// `id` with bit 0 of its first byte cleared: a server-made id (a name-based UUID) in the dynamic
/// form, so the client spawns its prefab and does not take it as a level object. Its halves stay
/// as they are.
pub const fn as_dynamic_raw_id(mut id: [u8; RAW_ID_LEN]) -> [u8; RAW_ID_LEN] {
    id[0] &= !OFFLINE_BIT;
    id
}

/// The facet entity of the entity with raw id `id`: its first 8 bytes as a little-endian `u64`
/// (the client's entity id; 1589 row 3 and the 3743 station answers carry it).
pub fn facet_of_raw_id(id: &[u8; RAW_ID_LEN]) -> u64 {
    let mut first = [0; HALF_LEN];
    first.copy_from_slice(&id[..HALF_LEN]);
    u64::from_le_bytes(first)
}

/// A new id of the dynamic form, from the operating system's random source.
///
/// # Panics
///
/// Panics when the operating system has no random source.
fn random_dynamic_id() -> [u8; RAW_ID_LEN] {
    loop {
        let mut id = [0; RAW_ID_LEN];
        getrandom::fill(&mut id).expect("the operating system's random source");
        let id = as_dynamic_raw_id(id);
        if is_dynamic_raw_id(&id) {
            return id;
        }
    }
}

/// The raw id of entity `number`: the one it has in this process, else a new random id of the
/// dynamic form, unique here in itself and in its facet entity. The same id goes in the entity's
/// spawn card, `ClientAddEntryMsg` and every message that names the entity. The entity's holders
/// ([`hold_raw_id`]) keep the entry.
///
/// # Panics
///
/// Panics when the operating system has no random source.
pub fn raw_id_of_entity(number: u64) -> [u8; RAW_ID_LEN] {
    if let Some(entry) = registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_number
        .get(&number)
    {
        return entry.id;
    }
    registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .id_of(number, None)
}

/// The raw id of entity `number` ([`raw_id_of_entity`]), held by `holder` until it releases it.
///
/// # Panics
///
/// Panics when the operating system has no random source.
pub fn hold_raw_id(holder: IdHolder, number: u64) -> [u8; RAW_ID_LEN] {
    if let Some(entry) = registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_number
        .get(&number)
        .filter(|entry| entry.holders.contains(&holder))
    {
        return entry.id;
    }
    registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .id_of(number, Some(holder))
}

/// `holder` no longer has entity `number`; the entry goes when no holder is left.
pub fn release_raw_id(holder: IdHolder, number: u64) {
    let mut ids = registry().write().unwrap_or_else(PoisonError::into_inner);
    let Some(entry) = ids.by_number.get_mut(&number) else {
        return;
    };
    entry.holders.retain(|held| *held != holder);
    if entry.holders.is_empty()
        && let Some(entry) = ids.by_number.remove(&number)
    {
        ids.forget_id(&entry.id);
    }
}

/// Gives entity `number` a new id, as a respawn does (GDE id note: a respawned mob is a new
/// spawn with a new `GdeRef` and card); its holders keep it. Returns the new id.
///
/// # Panics
///
/// Panics when the operating system has no random source.
pub fn renew_raw_id(number: u64) -> [u8; RAW_ID_LEN] {
    let mut ids = registry().write().unwrap_or_else(PoisonError::into_inner);
    let holders = ids
        .by_number
        .get(&number)
        .map(|entry| entry.holders.clone())
        .unwrap_or_default();
    let id = ids.fresh_id();
    ids.insert(number, id, holders);
    id
}

/// Entries in the registry now.
pub fn raw_id_count() -> usize {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_number
        .len()
}

/// The entity number of raw id `id`; `None` for an id this process neither made nor adopted.
pub fn entity_of_raw_id(id: &[u8; RAW_ID_LEN]) -> Option<u64> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_id
        .get(id)
        .copied()
}

/// The entity number of facet entity `facet` (the first 8 bytes of its raw id, little-endian);
/// `None` for a facet entity of no id this process made or adopted.
pub fn entity_of_facet(facet: u64) -> Option<u64> {
    registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .by_facet
        .get(&facet)
        .copied()
}

/// Gives entity `number` the id `id` that its owner made (a handoff or another sim's ghost),
/// held by `holder`, so the entity keeps the owner's id. The owner's id wins over another one
/// the entity has here (a respawn renewed it).
///
/// # Errors
///
/// Returns [`RawIdError`] when `id` is not of the dynamic form, or when another entity has `id`
/// or its facet entity.
pub fn adopt_raw_id(
    holder: IdHolder,
    number: u64,
    id: [u8; RAW_ID_LEN],
) -> Result<Adopted, RawIdError> {
    if !is_dynamic_raw_id(&id) {
        return Err(RawIdError::NotDynamic);
    }
    let mut ids = registry().write().unwrap_or_else(PoisonError::into_inner);
    let taken = ids
        .by_id
        .get(&id)
        .or_else(|| ids.by_facet.get(&facet_of_raw_id(&id)))
        .copied();
    if let Some(other) = taken.filter(|other| *other != number) {
        return Err(RawIdError::IdTaken { other });
    }
    let (adopted, mut holders) = match ids.by_number.get(&number) {
        Some(entry) if entry.id == id => (Adopted::Same, entry.holders.clone()),
        Some(entry) => (Adopted::Renewed, entry.holders.clone()),
        None => (Adopted::New, Vec::new()),
    };
    if !holders.contains(&holder) {
        holders.push(holder);
    }
    ids.insert(number, id, holders);
    Ok(adopted)
}

/// The entity number of an id that a client process sees (a spawn card, its own entry id), held
/// by `holder`: the number the id has here, else a new number of this process from
/// [`SEEN_NUMBER_BASE`] up. A test client keys the entities it sees by these numbers; every
/// client of one process agrees on them.
pub fn number_of_seen_raw_id(holder: IdHolder, id: &[u8; RAW_ID_LEN]) -> u64 {
    let mut ids = registry().write().unwrap_or_else(PoisonError::into_inner);
    let number = match ids.by_id.get(id) {
        Some(number) => *number,
        None => {
            let number = SEEN_NUMBER_BASE + ids.next_seen;
            ids.next_seen += 1;
            ids.insert(number, *id, Vec::new());
            number
        }
    };
    ids.id_of(number, Some(holder));
    number
}

/// The entity number of facet entity `facet` for a test that copies retail bytes: the number the
/// facet has here, else a new one for an id whose first half is the facet (little-endian) and
/// whose second half is its bytes in reverse; the tests' holder keeps it.
#[cfg(any(test, feature = "test-util"))]
pub fn number_of_facet(facet: u64) -> u64 {
    static TESTS: OnceLock<IdHolder> = OnceLock::new();
    if let Some(number) = entity_of_facet(facet) {
        return number;
    }
    let mut id = [0; RAW_ID_LEN];
    id[..HALF_LEN].copy_from_slice(&facet.to_le_bytes());
    id[HALF_LEN..].copy_from_slice(&facet.to_be_bytes());
    number_of_seen_raw_id(*TESTS.get_or_init(IdHolder::new), &id)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// Spawns for the bit statistics: enough that a fair bit lands within the band below.
    const SPAWNS: u64 = 20_000;
    /// The share of ones every bit but the offline bit must have over [`SPAWNS`] ids.
    const FAIR_BAND: std::ops::RangeInclusive<f64> = 0.46..=0.54;
    /// Entity numbers of these tests, apart from those of other tests in this process.
    const TEST_BASE: u64 = 0x7e00_0000_0000;
    const BYTE_BITS: usize = 8;
    const BITS: usize = RAW_ID_LEN * BYTE_BITS;

    #[test]
    fn spawns_get_random_ids_of_the_retail_dynamic_form() {
        let ids: Vec<[u8; RAW_ID_LEN]> = (0..SPAWNS)
            .map(|offset| raw_id_of_entity(TEST_BASE + offset))
            .collect();
        assert!(ids.iter().all(is_dynamic_raw_id));
        assert!(ids.iter().all(|id| id[0] & OFFLINE_BIT == 0));
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
        let facets: HashSet<u64> = ids.iter().map(facet_of_raw_id).collect();
        assert_eq!(facets.len(), ids.len(), "unique facet entities");
        let halves_equal = ids
            .iter()
            .filter(|id| id[..HALF_LEN] == id[HALF_LEN..])
            .count();
        assert_eq!(halves_equal, 0, "independent halves");
        for bit in 1..BITS {
            let ones = ids
                .iter()
                .filter(|id| id[bit / BYTE_BITS] >> (bit % BYTE_BITS) & 1 == 1)
                .count();
            let share = ones as f64 / SPAWNS as f64;
            assert!(FAIR_BAND.contains(&share), "bit {bit}: {share}");
        }
    }

    #[test]
    fn an_entity_keeps_its_id_and_the_id_names_it() {
        let number = TEST_BASE + SPAWNS + 1;
        let id = raw_id_of_entity(number);
        assert_eq!(raw_id_of_entity(number), id);
        assert_eq!(entity_of_raw_id(&id), Some(number));
        assert_eq!(entity_of_facet(facet_of_raw_id(&id)), Some(number));
        assert_ne!(raw_id_of_entity(number + 1), id);
    }

    #[test]
    fn an_unknown_id_names_no_entity() {
        let mut id = raw_id_of_entity(TEST_BASE + SPAWNS + 2);
        id[RAW_ID_LEN - 1] ^= 1;
        assert_eq!(entity_of_raw_id(&id), None);
    }

    #[test]
    fn an_adopted_id_stays_with_its_entity() {
        let number = TEST_BASE + SPAWNS + 3;
        let holder = IdHolder::new();
        let id = random_dynamic_id();
        assert_eq!(adopt_raw_id(holder, number, id), Ok(Adopted::New));
        assert_eq!(adopt_raw_id(holder, number, id), Ok(Adopted::Same));
        assert_eq!(raw_id_of_entity(number), id);
        assert_eq!(entity_of_raw_id(&id), Some(number));
        assert_eq!(
            adopt_raw_id(holder, number + 1, id),
            Err(RawIdError::IdTaken { other: number })
        );
        let respawned = random_dynamic_id();
        assert_eq!(
            adopt_raw_id(holder, number, respawned),
            Ok(Adopted::Renewed),
            "the owner's new id wins"
        );
        assert_eq!(raw_id_of_entity(number), respawned);
        assert_eq!(entity_of_raw_id(&id), None, "the old id names no one");
    }

    #[test]
    fn a_level_object_id_is_not_adopted() {
        let mut odd = random_dynamic_id();
        odd[0] |= OFFLINE_BIT;
        assert_eq!(
            adopt_raw_id(IdHolder::new(), TEST_BASE + SPAWNS + 4, odd),
            Err(RawIdError::NotDynamic)
        );
        let mut zero_half = random_dynamic_id();
        zero_half[HALF_LEN..].fill(0);
        assert!(!is_dynamic_raw_id(&zero_half));
    }

    #[test]
    fn a_client_numbers_the_ids_it_sees_once() {
        let holder = IdHolder::new();
        let id = random_dynamic_id();
        let number = number_of_seen_raw_id(holder, &id);
        assert!(number >= SEEN_NUMBER_BASE);
        assert_eq!(number_of_seen_raw_id(holder, &id), number);
        assert_eq!(entity_of_raw_id(&id), Some(number));
        let made = raw_id_of_entity(TEST_BASE + SPAWNS + 5);
        assert_eq!(number_of_seen_raw_id(holder, &made), TEST_BASE + SPAWNS + 5);
        release_raw_id(holder, number);
        assert_eq!(entity_of_raw_id(&id), None);
    }

    #[test]
    fn an_entry_goes_with_its_last_holder() {
        let number = TEST_BASE + SPAWNS + 6;
        let (sim, other) = (IdHolder::new(), IdHolder::new());
        let id = hold_raw_id(sim, number);
        assert_eq!(hold_raw_id(other, number), id);
        release_raw_id(sim, number);
        assert_eq!(
            entity_of_raw_id(&id),
            Some(number),
            "the other holder keeps it"
        );
        release_raw_id(other, number);
        assert_eq!(entity_of_raw_id(&id), None);
        assert_eq!(entity_of_facet(facet_of_raw_id(&id)), None);
        assert_ne!(hold_raw_id(sim, number), id, "a new spawn gets a new id");
        release_raw_id(sim, number);
    }

    #[test]
    fn a_renewed_entity_keeps_its_holders_and_drops_its_old_id() {
        let number = TEST_BASE + SPAWNS + 7;
        let sim = IdHolder::new();
        let old = hold_raw_id(sim, number);
        let new = renew_raw_id(number);
        assert_ne!(new, old);
        assert!(is_dynamic_raw_id(&new));
        assert_eq!(raw_id_of_entity(number), new);
        assert_eq!(entity_of_raw_id(&old), None);
        assert_eq!(entity_of_raw_id(&new), Some(number));
        release_raw_id(sim, number);
        assert_eq!(entity_of_raw_id(&new), None, "the holder came along");
    }
}
