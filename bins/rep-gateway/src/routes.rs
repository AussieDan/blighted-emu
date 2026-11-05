// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sim of each connection (M5 spec sections 3.3, 3.6 and 8), with no I/O.
//!
//! A join sets the route. A handoff changes it only with a higher sequence, so two route messages
//! that arrive out of order cannot leave an old sim. A connection whose sim and generation leave
//! the cell table is resumed through the director; until the answer it has no route: its
//! reliable inputs wait (up to [`MAX_HELD_INPUT_BYTES`]) and go to the new sim in order, its
//! unreliable ones are dropped. A resume raises the sequence by [`RESUME_SEQUENCE_STEP`], so a late route
//! of the lost sim loses.
//!
//! A sim that is not a connection's route and still sends messages for it after
//! [`STRAY_GRACE`] holds a copy that nobody routes to (a handoff that its lost sender never ended):
//! it is told that the connection left.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use nw_bus::{BusId, CellTable};
use nw_login::queue::EntryRecord;
use uuid::Uuid;

/// How far a resume raises the handoff sequence; no session hands off this often.
pub const RESUME_SEQUENCE_STEP: u64 = 1 << 32;
/// How long a sim that is not the route may send for a connection: longer than a handoff with
/// its route message.
pub const STRAY_GRACE: Duration = Duration::from_secs(2);
/// Bytes of reliable inputs one resuming connection may hold. Inferred: about 1 s of a busy
/// client's reliable messages, far below one input batch (`nw_bus::MAX_BATCH_BYTES`).
pub const MAX_HELD_INPUT_BYTES: usize = 64 * 1024;

/// What became of a reliable input of a connection with no route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// It waits for the end of the resume.
    Held,
    /// The connection holds [`MAX_HELD_INPUT_BYTES`] already: it must close.
    Full,
    /// The gateway does not know the connection (gone).
    Unknown,
}

/// What to do with messages from a sim for a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stray {
    /// The sim is the route, or the gateway has no route to compare: deliver.
    No,
    /// Another sim, inside the grace (a handoff in progress): deliver.
    Waiting,
    /// Another sim, past the grace: tell it the connection left, and drop the messages.
    Evict,
}

#[derive(Debug)]
struct Route {
    sim: BusId,
    generation: u64,
    sequence: u64,
    account: Uuid,
    character: Uuid,
    entity: u64,
    bundles_sent: u64,
    resuming: bool,
    /// The reliable inputs that came during a resume, in order, and their bytes.
    held: Vec<Vec<u8>>,
    held_bytes: usize,
}

/// A connection to resume through the director.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    /// The connection id inside the gateway.
    pub connection: u64,
    /// The account.
    pub account: Uuid,
    /// The character.
    pub character: Uuid,
    /// The entity number it keeps.
    pub entity: u64,
    /// The handoff sequence it continues from.
    pub handoffs: u64,
    /// Bundles sent to it so far.
    pub bundles_sent: u64,
}

/// The routes of one gateway.
#[derive(Debug, Default)]
pub struct Routes {
    routes: HashMap<u64, Route>,
    strays: HashMap<(u64, BusId), Instant>,
}

impl Routes {
    /// The director put the character of `record` on `sim` with `entity`.
    pub fn joined(
        &mut self,
        connection: u64,
        record: &EntryRecord,
        sim: BusId,
        generation: u64,
        entity: u64,
    ) {
        self.routes.insert(
            connection,
            Route {
                sim,
                generation,
                sequence: 0,
                account: record.account_id,
                character: record.character_id,
                entity,
                bundles_sent: 0,
                resuming: false,
                held: Vec::new(),
                held_bytes: 0,
            },
        );
    }

    /// The sim that gets the inputs of `connection`; `None` for an unknown or resuming one.
    pub fn target(&self, connection: u64) -> Option<&BusId> {
        self.routes
            .get(&connection)
            .filter(|route| !route.resuming)
            .map(|route| &route.sim)
    }

    /// Keeps the reliable input `envelope` of a resuming `connection` for its new sim.
    pub fn hold(&mut self, connection: u64, envelope: Vec<u8>) -> Hold {
        let Some(route) = self.routes.get_mut(&connection) else {
            return Hold::Unknown;
        };
        let bytes = route.held_bytes.saturating_add(envelope.len());
        if bytes > MAX_HELD_INPUT_BYTES {
            return Hold::Full;
        }
        route.held_bytes = bytes;
        route.held.push(envelope);
        Hold::Held
    }

    /// A handoff moved `connection` to `sim`. Returns whether the route changed: only a higher
    /// `sequence` of a connection that is not resuming does.
    pub fn apply_route(
        &mut self,
        connection: u64,
        sim: BusId,
        generation: u64,
        sequence: u64,
    ) -> bool {
        let Some(route) = self
            .routes
            .get_mut(&connection)
            .filter(|route| !route.resuming && sequence > route.sequence)
        else {
            return false;
        };
        self.strays.remove(&(connection, sim.clone()));
        route.sim = sim;
        route.generation = generation;
        route.sequence = sequence;
        true
    }

    /// The connections whose sim and generation are not in `table`; they are marked resuming.
    pub fn lost(&mut self, table: &CellTable) -> Vec<Resume> {
        let live = |route: &Route| {
            table
                .sims
                .iter()
                .any(|slot| slot.id == route.sim && slot.generation == route.generation)
        };
        let mut jobs: Vec<Resume> = self
            .routes
            .iter_mut()
            .filter(|(_, route)| !route.resuming && !live(route))
            .map(|(connection, route)| {
                route.resuming = true;
                Resume {
                    connection: *connection,
                    account: route.account,
                    character: route.character,
                    entity: route.entity,
                    handoffs: route.sequence.saturating_add(RESUME_SEQUENCE_STEP),
                    bundles_sent: route.bundles_sent,
                }
            })
            .collect();
        jobs.sort_by_key(|job| job.connection);
        jobs
    }

    /// The director put the player of `job` on `sim`. Returns the reliable inputs held during
    /// the resume, in order; `None` when the connection left during the resume.
    pub fn resumed(&mut self, job: &Resume, sim: BusId, generation: u64) -> Option<Vec<Vec<u8>>> {
        let route = self.routes.get_mut(&job.connection)?;
        self.strays.remove(&(job.connection, sim.clone()));
        route.sim = sim;
        route.generation = generation;
        route.sequence = job.handoffs;
        route.resuming = false;
        route.held_bytes = 0;
        Some(std::mem::take(&mut route.held))
    }

    /// One bundle (an unreliable server message) went to `connection`.
    pub fn bundle_sent(&mut self, connection: u64) {
        if let Some(route) = self.routes.get_mut(&connection) {
            route.bundles_sent = route.bundles_sent.saturating_add(1);
        }
    }

    /// Forgets `connection`; returns the sim it had.
    pub fn left(&mut self, connection: u64) -> Option<BusId> {
        self.strays.retain(|(stray, _), _| *stray != connection);
        self.routes.remove(&connection).map(|route| route.sim)
    }

    /// Whether messages of `sim` for `connection` at `now` come from a stray copy.
    pub fn stray(&mut self, connection: u64, sim: &BusId, now: Instant) -> Stray {
        let routed = self.routes.get(&connection).filter(|route| !route.resuming);
        if routed.is_none_or(|route| route.sim == *sim) {
            return Stray::No;
        }
        let key = (connection, sim.clone());
        let first = *self.strays.entry(key.clone()).or_insert(now);
        if now.saturating_duration_since(first) <= STRAY_GRACE {
            return Stray::Waiting;
        }
        self.strays.remove(&key);
        Stray::Evict
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use nw_bus::{CellTable, SimSlot};

    use super::*;

    const GENERATION: u64 = 5;
    const ENTITY: u64 = 40;
    const PAUSE: Duration = Duration::from_millis(1);

    fn sim(name: &str) -> BusId {
        BusId::try_from(name.to_owned()).expect("id")
    }

    fn record() -> EntryRecord {
        EntryRecord {
            ticket_id: "ticket".to_owned(),
            account_id: Uuid::new_v4(),
            world_id: Uuid::new_v4(),
            character_id: Uuid::new_v4(),
            expires_at: 0,
        }
    }

    fn table(slots: &[(&str, u64)]) -> CellTable {
        CellTable {
            version: 1,
            cell_size_m: 2048.0,
            columns: 0,
            rows: 0,
            sims: slots
                .iter()
                .map(|(name, generation)| SimSlot {
                    id: sim(name),
                    generation: *generation,
                })
                .collect(),
            owners: Vec::new(),
        }
    }

    #[test]
    fn an_older_route_is_ignored() {
        let mut routes = Routes::default();
        routes.joined(1, &record(), sim("sim-a"), GENERATION, ENTITY);
        assert_eq!(routes.target(1), Some(&sim("sim-a")));
        assert!(routes.apply_route(1, sim("sim-b"), GENERATION, 2));
        assert!(!routes.apply_route(1, sim("sim-a"), GENERATION, 1));
        assert!(!routes.apply_route(1, sim("sim-c"), GENERATION, 2));
        assert_eq!(routes.target(1), Some(&sim("sim-b")));
        assert!(!routes.apply_route(9, sim("sim-a"), GENERATION, 3));
    }

    #[test]
    fn connections_of_a_missing_sim_are_resumed_once() {
        let mut routes = Routes::default();
        let first = record();
        routes.joined(1, &first, sim("sim-a"), GENERATION, ENTITY);
        routes.joined(2, &record(), sim("sim-b"), GENERATION, ENTITY + 1);
        let jobs = routes.lost(&table(&[("sim-a", GENERATION + 1), ("sim-b", GENERATION)]));
        assert_eq!(
            jobs,
            vec![Resume {
                connection: 1,
                account: first.account_id,
                character: first.character_id,
                entity: ENTITY,
                handoffs: RESUME_SEQUENCE_STEP,
                bundles_sent: 0,
            }]
        );
        assert!(routes.lost(&table(&[("sim-b", GENERATION)])).is_empty());
        assert_eq!(routes.target(2), Some(&sim("sim-b")));
    }

    #[test]
    fn reliable_inputs_during_a_resume_wait_and_a_late_route_of_the_lost_sim_loses() {
        let mut routes = Routes::default();
        routes.joined(1, &record(), sim("sim-a"), GENERATION, ENTITY);
        routes.apply_route(1, sim("sim-a"), GENERATION, 3);
        let jobs = routes.lost(&table(&[]));
        assert_eq!(jobs[0].handoffs, 3 + RESUME_SEQUENCE_STEP);
        assert_eq!(routes.target(1), None);
        assert!(!routes.apply_route(1, sim("sim-b"), GENERATION, 4));
        assert_eq!(routes.hold(1, vec![1]), Hold::Held);
        assert_eq!(routes.hold(1, vec![2]), Hold::Held);
        assert_eq!(routes.hold(9, vec![3]), Hold::Unknown);
        assert_eq!(
            routes.resumed(&jobs[0], sim("sim-c"), GENERATION),
            Some(vec![vec![1], vec![2]])
        );
        assert_eq!(routes.target(1), Some(&sim("sim-c")));
        assert!(
            !routes.apply_route(1, sim("sim-b"), GENERATION, 4),
            "late route"
        );
        assert!(routes.apply_route(1, sim("sim-d"), GENERATION, jobs[0].handoffs + 1));
        assert_eq!(routes.left(1), Some(sim("sim-d")));
        assert_eq!(routes.left(1), None);
        assert_eq!(routes.resumed(&jobs[0], sim("sim-c"), GENERATION), None);
    }

    #[test]
    fn a_resuming_connection_holds_a_bounded_number_of_bytes() {
        let mut routes = Routes::default();
        routes.joined(1, &record(), sim("sim-a"), GENERATION, ENTITY);
        routes.lost(&table(&[]));
        assert_eq!(routes.hold(1, vec![0; MAX_HELD_INPUT_BYTES]), Hold::Held);
        assert_eq!(routes.hold(1, vec![0]), Hold::Full);
    }

    #[test]
    fn a_resume_carries_the_bundle_count() {
        const BUNDLES: u64 = 5;
        let mut routes = Routes::default();
        routes.joined(1, &record(), sim("sim-a"), GENERATION, ENTITY);
        for _ in 0..BUNDLES {
            routes.bundle_sent(1);
        }
        routes.bundle_sent(9);
        let jobs = routes.lost(&table(&[]));
        assert_eq!(jobs[0].bundles_sent, BUNDLES);
    }

    #[test]
    fn a_stray_sim_is_told_to_drop_the_player_after_the_grace() {
        let start = Instant::now();
        let mut routes = Routes::default();
        routes.joined(1, &record(), sim("sim-a"), GENERATION, ENTITY);
        assert_eq!(routes.stray(1, &sim("sim-a"), start), Stray::No);
        assert_eq!(routes.stray(9, &sim("sim-a"), start), Stray::No, "unknown");
        assert_eq!(routes.stray(1, &sim("sim-b"), start), Stray::Waiting);
        assert_eq!(
            routes.stray(1, &sim("sim-b"), start + STRAY_GRACE),
            Stray::Waiting
        );
        assert_eq!(
            routes.stray(1, &sim("sim-b"), start + STRAY_GRACE + PAUSE),
            Stray::Evict
        );
        assert_eq!(
            routes.stray(1, &sim("sim-b"), start + STRAY_GRACE + PAUSE),
            Stray::Waiting,
            "a new grace after the eviction"
        );
        assert!(routes.apply_route(1, sim("sim-b"), GENERATION, 1));
        assert_eq!(
            routes.stray(1, &sim("sim-b"), start + STRAY_GRACE * 3),
            Stray::No,
            "a handoff makes it the route"
        );
        assert_eq!(routes.stray(1, &sim("sim-a"), start), Stray::Waiting);
        routes.left(1);
        assert_eq!(routes.stray(1, &sim("sim-a"), start), Stray::No);
    }
}
