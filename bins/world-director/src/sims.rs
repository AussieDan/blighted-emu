// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live `sim` processes and the cell assignment (M5 spec sections 3.6, 3.7, 4.2 and 9), with
//! no I/O.
//!
//! The assignment depends only on the live sims, sorted by id, so a restarted director that has
//! heard every sim builds the same table again.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use nw_bus::{
    BusId, CellTable, InstanceBeat, RunningInstance, SimHeartbeat, SimSlot, heartbeat_silence,
};
use nw_sim_core::{CellGrid, CellOwners, Position};

struct Live {
    generation: u64,
    /// The level it runs.
    level: String,
    players: u32,
    max_players: u32,
    /// Admits since the last heartbeat, which its player count does not hold yet.
    pending: u32,
    last_seen: Instant,
    /// `Some` for a sim with the instance role, with the instance it runs.
    instance: Option<InstanceBeat>,
}

/// The sims that beat in time.
///
/// When every sim is silent at once, the director most likely lost its own NATS link: it keeps
/// every sim, and after the beats come back it waits one more silence window before it drops
/// the sims that are still silent (M5 spec section 12, "Director down").
pub struct SimRegistry {
    silence: Duration,
    sims: BTreeMap<BusId, Live>,
    hold_until: Option<Instant>,
}

impl SimRegistry {
    /// A sim is gone after `misses` missed heartbeats of `interval`.
    pub fn new(interval: Duration, misses: u32) -> Self {
        Self {
            silence: heartbeat_silence(interval, misses),
            sims: BTreeMap::new(),
            hold_until: None,
        }
    }

    /// Whether at least one sim is known and none beat inside the silence window at `now`.
    pub fn all_silent(&self, now: Instant) -> bool {
        !self.sims.is_empty() && self.sims.values().all(|live| self.is_silent(live, now))
    }

    fn is_silent(&self, live: &Live, now: Instant) -> bool {
        now.saturating_duration_since(live.last_seen) > self.silence
    }

    /// A heartbeat at `now`. Returns `true` when the set of live sims changed: a new sim or a
    /// new generation of a known one.
    pub fn beat(&mut self, beat: &SimHeartbeat, now: Instant) -> bool {
        let live = Live {
            generation: beat.started_at_ms,
            level: beat.level.clone(),
            players: beat.players,
            max_players: beat.max_players,
            pending: 0,
            last_seen: now,
            instance: beat.instance.clone(),
        };
        let before = self.sims.insert(beat.id.clone(), live);
        before.is_none_or(|known| known.generation != beat.started_at_ms)
    }

    /// Removes the sims silent for longer than the limit at `now`, and returns them. While every
    /// sim is silent, and for one window after that, nobody is removed.
    pub fn drop_silent(&mut self, now: Instant) -> Vec<BusId> {
        if self.all_silent(now) {
            self.hold_until = Some(now + self.silence);
        }
        if self.hold_until.is_some_and(|until| now <= until) {
            return Vec::new();
        }
        self.hold_until = None;
        let silent: Vec<BusId> = self
            .sims
            .iter()
            .filter(|(_, live)| self.is_silent(live, now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &silent {
            self.sims.remove(id);
        }
        silent
    }

    /// The live open-map sims of `level`, by id: the instance sims of an instanced open-world
    /// area run the same level and own no cells.
    pub fn slots(&self, level: &str) -> Vec<SimSlot> {
        self.slots_where(|live| live.level == level && live.instance.is_none())
    }

    /// The live sims that own no cells of `level` (the tutorial and instance sims), by id.
    pub fn other_slots(&self, level: &str) -> Vec<SimSlot> {
        self.slots_where(|live| live.level != level || live.instance.is_some())
    }

    /// The live instance sims of `level` that run no instance, by id.
    pub fn free_instance_sims(&self, level: &str) -> Vec<SimSlot> {
        self.slots_where(|live| {
            live.level == level
                && live
                    .instance
                    .as_ref()
                    .is_some_and(|instance| instance.running.is_none())
        })
    }

    /// The live instance sims of `level` (free or running), by id.
    pub fn instance_sims(&self, level: &str) -> Vec<SimSlot> {
        self.slots_where(|live| live.level == level && live.instance.is_some())
    }

    /// Every live instance sim with the instance it runs, by id.
    pub fn running_instances(&self) -> Vec<(SimSlot, Option<RunningInstance>)> {
        self.sims
            .iter()
            .filter_map(|(id, live)| {
                let beat = live.instance.as_ref()?;
                Some((slot_of(id, live), beat.running.clone()))
            })
            .collect()
    }

    /// The level of the live sim `id`.
    pub fn level_of(&self, id: &BusId) -> Option<String> {
        self.sims.get(id).map(|live| live.level.clone())
    }

    /// The live sim `id` of generation `generation`.
    pub fn is_live(&self, slot: &SimSlot) -> bool {
        self.sims
            .get(&slot.id)
            .is_some_and(|live| live.generation == slot.generation)
    }

    fn slots_where(&self, keep: impl Fn(&Live) -> bool) -> Vec<SimSlot> {
        self.sims
            .iter()
            .filter(|(_, live)| keep(live))
            .map(|(id, live)| slot_of(id, live))
            .collect()
    }

    /// The live sim of `level` with the most free player slots (admits in flight counted), the
    /// first by id on a tie; `None` when no sim of `level` has a free slot.
    pub fn least_loaded(&self, level: &str) -> Option<SimSlot> {
        self.sims
            .iter()
            .filter(|(_, live)| live.level == level && free_of(live) > 0)
            .min_by_key(|(_, live)| std::cmp::Reverse(free_of(live)))
            .map(|(id, live)| slot_of(id, live))
    }

    /// Live sims of every level.
    pub fn count(&self) -> usize {
        self.sims.len()
    }

    /// Players in the world.
    pub fn players(&self) -> u32 {
        self.sims
            .values()
            .fold(0, |sum, live| sum.saturating_add(live.players))
    }

    /// `sim` took a player after its last heartbeat (M5 load spec section 8 item 3).
    pub fn admitted(&mut self, sim: &BusId) {
        if let Some(live) = self.sims.get_mut(sim) {
            live.pending = live.pending.saturating_add(1);
        }
    }

    /// Players the live sims can still take, admits in flight counted; a sim above its own limit
    /// counts as full. Instance sims take no logins, so their slots do not count.
    pub fn free_slots(&self) -> u32 {
        self.sims
            .values()
            .filter(|live| live.instance.is_none())
            .fold(0, |sum, live| sum.saturating_add(free_of(live)))
    }
}

fn slot_of(id: &BusId, live: &Live) -> SimSlot {
    SimSlot {
        id: id.clone(),
        generation: live.generation,
    }
}

/// Players `live` can still take, admits in flight counted; above its own limit it is full.
fn free_of(live: &Live) -> u32 {
    let taken = live.players.saturating_add(live.pending);
    live.max_players.saturating_sub(taken)
}

/// The cells of the open map shared over the live sims.
#[derive(Debug, Clone, PartialEq)]
pub struct Assignment {
    version: u64,
    sims: Vec<SimSlot>,
    owners: CellOwners<u16>,
}

impl Assignment {
    /// The column stripes of `grid` over `sims`, in their order. More sims than a cell table
    /// can name (65,536) are left out. The sims of other levels (`other_levels`) own no cells;
    /// they follow in the table, so that gateways count their players as routed.
    pub fn new(
        version: u64,
        grid: CellGrid,
        mut sims: Vec<SimSlot>,
        other_levels: Vec<SimSlot>,
    ) -> Self {
        sims.truncate(usize::from(u16::MAX) + 1);
        let indexes: Vec<u16> = (0..=u16::MAX).take(sims.len()).collect();
        sims.extend(other_levels);
        Self {
            version,
            owners: CellOwners::columns(grid, &indexes),
            sims,
        }
    }

    /// The sim that owns the cell at `position`.
    pub fn owner_at(&self, position: Position) -> Option<&SimSlot> {
        let index = self.owners.owner_at(position)?;
        self.sims.get(usize::from(index))
    }

    /// Whether `position` is on the map: inside the cell grid of the level.
    pub fn covers(&self, position: Position) -> bool {
        self.owners.grid().cell_of(position.x, position.y).is_some()
    }

    /// The table that sims and gateways get.
    pub fn table(&self) -> CellTable {
        let grid = self.owners.grid();
        CellTable {
            version: self.version,
            cell_size_m: grid.cell_size_m(),
            columns: grid.columns(),
            rows: grid.rows(),
            sims: self.sims.clone(),
            owners: self.owners.cells().to_vec(),
        }
    }

    /// Each sim with the x range of its columns, for the log.
    pub fn x_ranges(&self) -> Vec<(SimSlot, f32, f32)> {
        let grid = self.owners.grid();
        let width = grid.bounds().max_x;
        let first_row = &self.owners.cells()[..grid.columns() as usize];
        self.sims
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                let mine = |owner: &Option<u16>| *owner == u16::try_from(index).ok();
                let first = first_row.iter().position(mine)?;
                let last = first_row.iter().rposition(mine)?;
                let edge = |column: usize| (column as f32 * grid.cell_size_m()).min(width);
                Some((slot.clone(), edge(first), edge(last + 1)))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use nw_bus::{BusId, SimHeartbeat, SimSlot};
    use nw_sim_core::{CellGrid, Position};

    use super::*;

    const INTERVAL: Duration = Duration::from_secs(1);
    const MISSES: u32 = 3;
    const STARTED_AT_MS: u64 = 100;
    const MAX_PLAYERS: u32 = 2000;
    const LEVEL_M: f32 = 14_336.0;
    const LEVEL: &str = "newworld_vitaeeterna";
    const CELL_M: f32 = 2048.0;
    const VERSION: u64 = 4;

    fn id(name: &str) -> BusId {
        BusId::try_from(name.to_owned()).expect("id")
    }

    fn beat(name: &str, started_at_ms: u64, players: u32) -> SimHeartbeat {
        SimHeartbeat {
            id: id(name),
            started_at_ms,
            players,
            max_players: MAX_PLAYERS,
            level: LEVEL.to_owned(),
            instance: None,
        }
    }

    fn slot(name: &str) -> SimSlot {
        SimSlot {
            id: id(name),
            generation: STARTED_AT_MS,
        }
    }

    fn grid() -> CellGrid {
        CellGrid::new(LEVEL_M, LEVEL_M, CELL_M).expect("grid")
    }

    #[test]
    fn a_new_or_restarted_sim_changes_the_set() {
        let now = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        assert!(sims.beat(&beat("sim-2", STARTED_AT_MS, 0), now));
        assert!(!sims.beat(&beat("sim-2", STARTED_AT_MS, 5), now));
        assert!(sims.beat(&beat("sim-1", STARTED_AT_MS, 0), now));
        assert_eq!(sims.slots(LEVEL), vec![slot("sim-1"), slot("sim-2")]);
        assert!(sims.beat(&beat("sim-1", STARTED_AT_MS + 1, 0), now));
        assert_eq!(sims.slots(LEVEL)[0].generation, STARTED_AT_MS + 1);
    }

    #[test]
    fn a_silent_sim_is_dropped() {
        let start = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), start);
        sims.beat(&beat("sim-2", STARTED_AT_MS, 0), start);
        let limit = INTERVAL * MISSES;
        sims.beat(&beat("sim-2", STARTED_AT_MS, 0), start + limit);
        assert!(sims.drop_silent(start + limit).is_empty());
        assert_eq!(
            sims.drop_silent(start + limit + INTERVAL),
            vec![id("sim-1")]
        );
        assert_eq!(sims.slots(LEVEL), vec![slot("sim-2")]);
    }

    #[test]
    fn all_sims_silent_at_once_keeps_them() {
        let start = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), start);
        sims.beat(&beat("sim-2", STARTED_AT_MS, 0), start);
        let limit = INTERVAL * MISSES;
        let quiet = start + limit + INTERVAL;
        assert!(sims.all_silent(quiet));
        assert!(
            sims.drop_silent(quiet).is_empty(),
            "the director lost its link"
        );
        assert_eq!(sims.slots(LEVEL).len(), 2);
        // Beats come back from one sim only; the other is dropped one window later.
        let back = quiet + INTERVAL;
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), back);
        assert!(!sims.all_silent(back));
        assert!(
            sims.drop_silent(back).is_empty(),
            "a window to hear every sim"
        );
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), quiet + limit);
        assert_eq!(
            sims.drop_silent(quiet + limit + INTERVAL),
            vec![id("sim-2")]
        );
        assert!(!SimRegistry::new(INTERVAL, MISSES).all_silent(start));
    }

    #[test]
    fn free_slots_are_capacity_minus_players() {
        let now = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        assert_eq!(sims.free_slots(), 0);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 10), now);
        sims.beat(&beat("sim-2", STARTED_AT_MS, 30), now);
        assert_eq!(sims.players(), 40);
        assert_eq!(sims.free_slots(), 2 * MAX_PLAYERS - 40);
        let mut over = beat("sim-1", STARTED_AT_MS, MAX_PLAYERS + 10);
        over.max_players = MAX_PLAYERS;
        sims.beat(&over, now);
        assert_eq!(sims.free_slots(), MAX_PLAYERS - 30);
    }

    #[test]
    fn free_slots_count_admits_in_flight() {
        let now = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 10), now);
        sims.admitted(&id("sim-1"));
        sims.admitted(&id("sim-1"));
        sims.admitted(&id("sim-9"));
        assert_eq!(sims.free_slots(), MAX_PLAYERS - 12);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 12), now);
        assert_eq!(
            sims.free_slots(),
            MAX_PLAYERS - 12,
            "the heartbeat counts them now"
        );
    }

    #[test]
    fn the_assignment_routes_by_column_and_becomes_a_table() {
        let assignment = Assignment::new(
            VERSION,
            grid(),
            vec![slot("sim-1"), slot("sim-2")],
            Vec::new(),
        );
        let west = Position {
            x: 10.0,
            y: 9000.0,
            z: 0.0,
        };
        let east = Position {
            x: LEVEL_M - 10.0,
            ..west
        };
        assert_eq!(assignment.owner_at(west), Some(&slot("sim-1")));
        assert_eq!(assignment.owner_at(east), Some(&slot("sim-2")));
        assert_eq!(assignment.owner_at(Position { x: -1.0, ..west }), None);
        assert!(assignment.covers(west));
        assert!(!assignment.covers(Position { x: -1.0, ..west }));
        assert!(
            !Assignment::new(VERSION, grid(), Vec::new(), Vec::new()).covers(Position {
                x: LEVEL_M + 1.0,
                ..west
            })
        );
        let table = assignment.table();
        assert_eq!(table.version, VERSION);
        assert_eq!((table.columns, table.rows), (7, 7));
        assert_eq!(table.cell_size_m, CELL_M);
        assert_eq!(table.sims, vec![slot("sim-1"), slot("sim-2")]);
        assert_eq!(table.owners.len(), 49);
        assert_eq!(table.owners[0], Some(0));
        assert_eq!(table.owners[6], Some(1));
        assert_eq!(
            assignment.x_ranges(),
            vec![
                (slot("sim-1"), 0.0, 3.0 * CELL_M),
                (slot("sim-2"), 3.0 * CELL_M, LEVEL_M)
            ]
        );
        let empty = Assignment::new(VERSION, grid(), Vec::new(), Vec::new());
        assert_eq!(empty.owner_at(west), None);
        assert!(empty.x_ranges().is_empty());
    }

    #[test]
    fn the_map_goes_to_open_sims_and_new_players_to_the_freest_tutorial_sim() {
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        let now = Instant::now();
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), now);
        let tutorial = |name: &str, players: u32| SimHeartbeat {
            level: nw_sim_core::TUTORIAL_FOLDER.to_owned(),
            ..beat(name, STARTED_AT_MS, players)
        };
        sims.beat(&tutorial("tut-1", 5), now);
        sims.beat(&tutorial("tut-2", 2), now);
        assert_eq!(sims.slots(LEVEL), vec![slot("sim-1")]);
        assert_eq!(sims.count(), 3);
        assert_eq!(
            sims.least_loaded(nw_sim_core::TUTORIAL_FOLDER),
            Some(slot("tut-2")),
            "the tutorial sim with the most free slots"
        );
        assert_eq!(sims.least_loaded("no_such_level"), None);
        assert_eq!(sims.other_slots(LEVEL), vec![slot("tut-1"), slot("tut-2")]);
    }

    #[test]
    fn sims_of_other_levels_are_in_the_table_without_cells() {
        let assignment = Assignment::new(VERSION, grid(), vec![slot("sim-1")], vec![slot("tut-1")]);
        let table = assignment.table();
        assert_eq!(table.sims, vec![slot("sim-1"), slot("tut-1")]);
        assert!(table.owners.iter().all(|owner| *owner == Some(0)));
        assert_eq!(
            assignment.x_ranges(),
            vec![(slot("sim-1"), 0.0, LEVEL_M)],
            "the tutorial sim has no columns"
        );
    }

    const DUNGEON: &str = "nw_dungeon_windsward_00";

    fn instance_beat(name: &str, running: Option<RunningInstance>) -> SimHeartbeat {
        SimHeartbeat {
            level: DUNGEON.to_owned(),
            instance: Some(InstanceBeat { running }),
            ..beat(name, STARTED_AT_MS, 0)
        }
    }

    fn running(instance: u128) -> RunningInstance {
        RunningInstance {
            instance: uuid::Uuid::from_u128(instance),
            game_mode: "DungeonAmrine".to_owned(),
            members: Vec::new(),
            players: 1,
            empty_for_ms: None,
            open_for_ms: 0,
            phase: 1,
            capacity: 5,
            ends_at_ms: None,
        }
    }

    #[test]
    fn instance_sims_own_no_cells_take_no_logins_and_report_their_instance() {
        let now = Instant::now();
        let mut sims = SimRegistry::new(INTERVAL, MISSES);
        sims.beat(&beat("sim-1", STARTED_AT_MS, 0), now);
        sims.beat(&instance_beat("inst-1", None), now);
        sims.beat(&instance_beat("inst-2", Some(running(7))), now);
        // An instanced open-world area runs the open map's level on an instance sim.
        sims.beat(
            &SimHeartbeat {
                level: LEVEL.to_owned(),
                ..instance_beat("inst-3", None)
            },
            now,
        );
        assert_eq!(sims.slots(LEVEL), vec![slot("sim-1")]);
        assert_eq!(
            sims.other_slots(LEVEL),
            vec![slot("inst-1"), slot("inst-2"), slot("inst-3")]
        );
        assert_eq!(sims.free_instance_sims(DUNGEON), vec![slot("inst-1")]);
        assert_eq!(sims.free_instance_sims(LEVEL), vec![slot("inst-3")]);
        assert_eq!(sims.instance_sims(DUNGEON).len(), 2);
        assert_eq!(
            sims.free_slots(),
            MAX_PLAYERS,
            "only the open-map sim takes logins"
        );
        let reported = sims.running_instances();
        assert_eq!(reported.len(), 3);
        assert!(reported.contains(&(slot("inst-2"), Some(running(7)))));
        assert!(sims.is_live(&slot("inst-1")));
        assert!(!sims.is_live(&SimSlot {
            generation: STARTED_AT_MS + 1,
            ..slot("inst-1")
        }));
    }
}
