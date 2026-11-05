// SPDX-License-Identifier: AGPL-3.0-or-later
//! The instances of the world (expeditions, raids, trials; architecture spec sections 4.4, 4.5
//! and 8), with no I/O: which group has which instance on which instance sim, the decision of a
//! request, the end of an instance, and how many instance sims each level needs.
//!
//! A sim with the instance role runs one instance at a time (spec 4.4). The director gives a
//! group a free instance sim of the level, keeps the group's instance for re-entry (members who
//! come later, players who come back after a disconnect), and ends it when its game mode's time
//! (`OverrideDungeonTimeout`) is over or it stayed empty for the configured time. An instance
//! whose sim goes silent ends with it (spec 8: its players go back to the open map at their next
//! join). With a launch command, the director starts instance sim processes when a level has
//! fewer free ones than its spare count, and stops the ones it started that stayed free beyond
//! the spare count: 10 players need none, 10,000 players start what the requests ask for, on any
//! host that runs the command (no cloud service).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use nw_bus::{
    BusId, GroupAnswer, InstanceRefusal, InstanceReply, InstanceRequest, RunningInstance, SimSlot,
};
use uuid::Uuid;

/// The rules of the instances from the configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSettings {
    /// An instance with no player for this long ends.
    pub empty_timeout: Duration,
    /// Longest wait for an instance sim to open an instance.
    pub open_timeout: Duration,
    /// Longest wait for the social service's group answer.
    pub group_timeout: Duration,
    /// How to start instance sims; `None` when the operator starts them.
    pub launch: Option<LaunchSettings>,
}

/// How the director starts instance sim processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSettings {
    /// The program (the `sim` binary).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Free instance sims each level keeps ready once a player asked for it.
    pub spare_per_level: u32,
    /// Most instance sims the director starts in all.
    pub max_sims: u32,
    /// Prefix of the sim ids it gives the started sims.
    pub id_prefix: String,
    /// A level nobody asked for in this long keeps no spare sim.
    pub forget_after: Duration,
}

/// One open instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenInstance {
    /// The instance.
    pub id: Uuid,
    /// The game mode.
    pub game_mode: String,
    /// The level folder.
    pub level: String,
    /// The instance sim.
    pub sim: SimSlot,
    /// The phase id.
    pub phase: u64,
    /// The characters that may enter.
    pub members: Vec<Uuid>,
    /// Most players.
    pub capacity: u32,
    /// The group (or the solo player) it belongs to.
    pub owner: Uuid,
    /// Unix milliseconds when it ends, when its game mode limits it.
    pub ends_at_ms: Option<u64>,
    /// Whether the sim confirmed the opening.
    pub confirmed: bool,
}

/// What to do with a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Answer at once.
    Reply(InstanceReply),
    /// Give the existing instance to a member who joined the group later: tell its sim, then
    /// answer.
    AddMember {
        /// The instance.
        instance: OpenInstance,
        /// The new member.
        character: Uuid,
    },
    /// Open `instance` on its sim, then answer.
    Open(OpenInstance),
    /// The group's instance is still opening: decide again when its sim answers the opening
    /// (a grant before then names an instance the join cannot find yet).
    Wait {
        /// The instance.
        instance: Uuid,
    },
}

/// An instance to end: its sim and id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Closing {
    /// The instance sim.
    pub sim: SimSlot,
    /// The instance.
    pub instance: Uuid,
}

/// The open instances.
#[derive(Debug, Default)]
pub struct Instances {
    open: BTreeMap<Uuid, OpenInstance>,
    /// The instance of each owner (group or solo player).
    by_owner: HashMap<Uuid, Uuid>,
}

impl Instances {
    /// The open instances, for the joins.
    pub fn table(&self) -> InstanceTable {
        InstanceTable {
            open: self.open.clone(),
        }
    }

    /// The instance `id`.
    pub fn get(&self, id: &Uuid) -> Option<&OpenInstance> {
        self.open.get(id)
    }

    /// Number of open instances.
    pub fn len(&self) -> usize {
        self.open.len()
    }

    /// Whether no instance is open.
    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// The decision for `request` with the requester's group `group` and the free instance sims
    /// of the level `free` (by id), at Unix millisecond `now_ms`. `phase` and `id` are the new
    /// instance's when one opens.
    pub fn decide(
        &mut self,
        request: &InstanceRequest,
        group: &GroupAnswer,
        free: &[SimSlot],
        ids: (Uuid, u64),
        now_ms: u64,
    ) -> Decision {
        let (owner, members) = match group {
            GroupAnswer::Group { group, members, .. } => (*group, members.clone()),
            GroupAnswer::Alone => (request.requester, vec![request.requester]),
        };
        let count = u32::try_from(members.len()).unwrap_or(u32::MAX);
        // The owner's instance, or one the requester is a member of, or an adopted one whose
        // owner is in the group (an instance adopted after a restart knows its members, not its
        // group: its owner is its first member).
        let existing = self
            .by_owner
            .get(&owner)
            .and_then(|instance| self.open.get(instance))
            .or_else(|| {
                self.open.values().find(|open| {
                    open.members.contains(&request.requester) || members.contains(&open.owner)
                })
            });
        if let Some(open) = existing {
            if open.game_mode != request.game_mode {
                return refuse(InstanceRefusal::GroupInOtherInstance);
            }
            if !open.confirmed {
                return Decision::Wait { instance: open.id };
            }
            if open.members.contains(&request.requester) {
                return Decision::Reply(granted(open, false));
            }
            if u32::try_from(open.members.len()).unwrap_or(u32::MAX) >= open.capacity {
                return refuse(InstanceRefusal::TooManyMembers);
            }
            return Decision::AddMember {
                instance: open.clone(),
                character: request.requester,
            };
        }
        if count < request.min_group {
            return refuse(InstanceRefusal::NotEnoughMembers);
        }
        if count > request.capacity {
            return refuse(InstanceRefusal::TooManyMembers);
        }
        let taken: Vec<&SimSlot> = self.open.values().map(|open| &open.sim).collect();
        let Some(sim) = free.iter().find(|sim| !taken.contains(sim)) else {
            return refuse(InstanceRefusal::NoInstanceSim);
        };
        let (id, phase) = ids;
        let instance = OpenInstance {
            id,
            game_mode: request.game_mode.clone(),
            level: request.level.clone(),
            sim: sim.clone(),
            phase,
            members,
            capacity: request.capacity,
            owner,
            ends_at_ms: request
                .timeout_seconds
                .map(|seconds| now_ms.saturating_add(seconds.saturating_mul(MS_PER_SECOND))),
            confirmed: false,
        };
        self.by_owner.insert(owner, id);
        self.open.insert(id, instance.clone());
        Decision::Open(instance)
    }

    /// The answer of the sim to the opening of `instance`: kept when it opened, else dropped.
    pub fn opened(&mut self, instance: Uuid, ok: bool) -> Option<OpenInstance> {
        if ok {
            let open = self.open.get_mut(&instance)?;
            open.confirmed = true;
            return Some(open.clone());
        }
        self.remove(instance);
        None
    }

    /// `character` may enter `instance` now.
    pub fn add_member(&mut self, instance: Uuid, character: Uuid) -> Option<OpenInstance> {
        let open = self.open.get_mut(&instance)?;
        if !open.members.contains(&character) {
            open.members.push(character);
        }
        Some(open.clone())
    }

    /// `character` may no longer enter `instance` (its sim did not get the admission).
    pub fn remove_member(&mut self, instance: Uuid, character: Uuid) {
        if let Some(open) = self.open.get_mut(&instance) {
            open.members.retain(|member| *member != character);
        }
    }

    /// The instances to end from the instance sims' reports `running` (each live instance sim
    /// with what it runs) at Unix millisecond `now_ms`; an instance whose sim is gone, or that
    /// its sim no longer runs, is forgotten.
    pub fn review(
        &mut self,
        running: &[(SimSlot, Option<RunningInstance>)],
        settings: &InstanceSettings,
        now_ms: u64,
    ) -> Vec<Closing> {
        let empty_ms = u64::try_from(settings.empty_timeout.as_millis()).unwrap_or(u64::MAX);
        let mut closing = Vec::new();
        let mut gone = Vec::new();
        for open in self.open.values() {
            let report = running
                .iter()
                .find(|(sim, _)| *sim == open.sim)
                .map(|(_, running)| running.as_ref());
            match report {
                // The sim went silent: the instance ended with it.
                None => gone.push(open.id),
                // The sim runs no instance although it confirmed this one: it ended there.
                Some(None) if open.confirmed => gone.push(open.id),
                Some(Some(running)) if running.instance != open.id => gone.push(open.id),
                Some(Some(running)) => {
                    let timed_out = open.ends_at_ms.is_some_and(|end| now_ms >= end);
                    let empty = running.empty_for_ms.is_some_and(|ms| ms >= empty_ms);
                    if timed_out || empty {
                        closing.push(Closing {
                            sim: open.sim.clone(),
                            instance: open.id,
                        });
                    }
                }
                Some(None) => {}
            }
        }
        for id in gone {
            self.remove(id);
        }
        closing
    }

    /// Adopts the instances that live sims run but this director does not know (a restarted
    /// director), so their groups keep them: with the phase, capacity and end time the sims
    /// report.
    pub fn adopt(
        &mut self,
        running: &[(SimSlot, Option<RunningInstance>)],
        level_of: impl Fn(&SimSlot) -> Option<String>,
    ) {
        for (sim, report) in running {
            let Some(report) = report else {
                continue;
            };
            if self.open.contains_key(&report.instance) {
                continue;
            }
            let Some(level) = level_of(sim) else {
                continue;
            };
            let owner = report.members.first().copied().unwrap_or(report.instance);
            let capacity = if report.capacity == UNKNOWN_CAPACITY {
                // Inferred (safe choice): a sim that does not know the capacity takes no player
                // beyond the members it reports, so the instance never goes above its real one.
                u32::try_from(report.members.len()).unwrap_or(u32::MAX)
            } else {
                report.capacity
            };
            self.by_owner.insert(owner, report.instance);
            self.open.insert(
                report.instance,
                OpenInstance {
                    id: report.instance,
                    game_mode: report.game_mode.clone(),
                    level,
                    sim: sim.clone(),
                    phase: report.phase,
                    members: report.members.clone(),
                    capacity,
                    owner,
                    ends_at_ms: report.ends_at_ms,
                    confirmed: true,
                },
            );
        }
    }

    fn remove(&mut self, id: Uuid) {
        if let Some(open) = self.open.remove(&id) {
            self.by_owner.retain(|_, instance| *instance != id);
            tracing::info!(
                instance = %id,
                game_mode = %open.game_mode,
                sim = %open.sim.id,
                "instance ended"
            );
        }
    }

    /// The instance `id` ends now (its close went out).
    pub fn closed(&mut self, id: Uuid) {
        self.remove(id);
    }
}

/// The capacity a sim reports when it does not know its game mode's `TeamCapacity`.
const UNKNOWN_CAPACITY: u32 = 0;
/// Milliseconds in a second.
const MS_PER_SECOND: u64 = 1000;

fn refuse(reason: InstanceRefusal) -> Decision {
    Decision::Reply(InstanceReply::Refused { reason })
}

/// The grant of `open`.
pub fn granted(open: &OpenInstance, opened: bool) -> InstanceReply {
    InstanceReply::Granted {
        instance: open.id,
        sim: open.sim.clone(),
        phase: open.phase,
        members: open.members.clone(),
        opened,
    }
}

/// A copy of the open instances for the join tasks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceTable {
    open: BTreeMap<Uuid, OpenInstance>,
}

impl InstanceTable {
    /// The sim of instance `instance` when it is open and `character` may enter it.
    pub fn sim_for(&self, instance: &Uuid, character: &Uuid) -> Option<&OpenInstance> {
        self.open
            .get(instance)
            .filter(|open| open.confirmed && open.members.contains(character))
    }
}

/// The instance sims to start and to stop for each level.
#[derive(Debug, Default)]
pub struct Launches {
    /// The sims this director started: id and level.
    started: BTreeMap<BusId, String>,
    /// The levels players asked for, with the time of the last ask.
    wanted: BTreeMap<String, Instant>,
    next: u64,
}

/// One instance sim to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// Its id.
    pub id: BusId,
    /// Its level.
    pub level: String,
}

impl Launches {
    /// A player asked for an instance of `level` at `now`.
    pub fn wanted(&mut self, level: &str, now: Instant) {
        self.wanted.insert(level.to_owned(), now);
    }

    /// The sims to start: for each wanted level, as many as it lacks of its spare count, within
    /// the total limit. `free` gives the number of a level's free instance sims, and `live`
    /// whether a sim beats: a started sim that does not beat yet counts as free.
    pub fn to_start(
        &mut self,
        settings: &LaunchSettings,
        free: impl Fn(&str) -> u32,
        live: impl Fn(&BusId) -> bool,
    ) -> Vec<Launch> {
        let mut launches = Vec::new();
        let levels: Vec<String> = self.wanted.keys().cloned().collect();
        for level in levels {
            let starting = self
                .started
                .iter()
                .filter(|(id, started)| **started == level && !live(id))
                .count();
            let ready = free(&level).saturating_add(u32::try_from(starting).unwrap_or(u32::MAX));
            for _ in ready..settings.spare_per_level {
                if u32::try_from(self.started.len()).unwrap_or(u32::MAX) >= settings.max_sims {
                    return launches;
                }
                self.next += 1;
                let name = format!("{}-{}", settings.id_prefix, self.next);
                let Ok(id) = BusId::try_from(name) else {
                    return launches;
                };
                self.started.insert(id.clone(), level.clone());
                launches.push(Launch {
                    id,
                    level: level.clone(),
                });
            }
        }
        launches
    }

    /// The started sims to stop: free ones of a level beyond its spare count, and every free one
    /// of a level nobody asked for in the settings' `forget_after`.
    pub fn to_stop(
        &mut self,
        settings: &LaunchSettings,
        free_sims: impl Fn(&str) -> Vec<SimSlot>,
        now: Instant,
    ) -> Vec<BusId> {
        let forget_after = settings.forget_after;
        self.wanted
            .retain(|_, asked| now.saturating_duration_since(*asked) < forget_after);
        let mut stop = Vec::new();
        let levels: Vec<String> = self.started.values().cloned().collect();
        for level in dedup(levels) {
            let keep = if self.wanted.contains_key(&level) {
                settings.spare_per_level
            } else {
                0
            };
            let mine: Vec<BusId> = free_sims(&level)
                .into_iter()
                .map(|slot| slot.id)
                .filter(|id| self.started.contains_key(id))
                .collect();
            let keep = usize::try_from(keep).unwrap_or(usize::MAX);
            let surplus = mine.len().saturating_sub(keep);
            stop.extend(mine.into_iter().take(surplus));
        }
        for id in &stop {
            self.started.remove(id);
        }
        stop
    }

    /// A started sim exited.
    pub fn exited(&mut self, id: &BusId) {
        self.started.remove(id);
    }

    /// The sims this director started that did not exit.
    pub fn started(&self) -> impl Iterator<Item = &BusId> {
        self.started.keys()
    }
}

fn dedup(mut levels: Vec<String>) -> Vec<String> {
    levels.sort();
    levels.dedup();
    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEVEL: &str = "nw_dungeon_windsward_00";
    const MODE: &str = "DungeonAmrine";
    const NOW_MS: u64 = 1_000_000;
    const PHASE: u64 = 0xbd6e_b5a2_6a1f_2b2f;
    const TRIAL_SECONDS: u64 = 1800;

    fn slot(name: &str) -> SimSlot {
        SimSlot {
            id: BusId::try_from(name.to_owned()).expect("id"),
            generation: 1,
        }
    }

    fn character(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn request(requester: u128) -> InstanceRequest {
        InstanceRequest {
            game_mode: MODE.to_owned(),
            level: LEVEL.to_owned(),
            requester: character(requester),
            capacity: 5,
            min_group: 3,
            timeout_seconds: None,
        }
    }

    fn group(members: &[u128]) -> GroupAnswer {
        GroupAnswer::Group {
            group: Uuid::from_u128(100),
            leader: character(members[0]),
            members: members.iter().map(|n| character(*n)).collect(),
        }
    }

    fn settings() -> InstanceSettings {
        InstanceSettings {
            empty_timeout: Duration::from_secs(60),
            open_timeout: Duration::from_millis(500),
            group_timeout: Duration::from_millis(300),
            launch: None,
        }
    }

    #[test]
    fn a_group_gets_a_free_sim_and_keeps_its_instance_for_re_entry() {
        let mut instances = Instances::default();
        let free = [slot("inst-1"), slot("inst-2")];
        let Decision::Open(open) = instances.decide(
            &request(1),
            &group(&[1, 2, 3]),
            &free,
            (Uuid::from_u128(7), PHASE),
            NOW_MS,
        ) else {
            panic!("opens");
        };
        assert_eq!(open.sim, slot("inst-1"));
        assert_eq!(open.members.len(), 3);
        assert!(instances.table().sim_for(&open.id, &character(1)).is_none());
        assert!(instances.opened(open.id, true).is_some());
        assert!(instances.table().sim_for(&open.id, &character(2)).is_some());
        // Another member asks: the same instance, no new one.
        let again = instances.decide(
            &request(2),
            &group(&[1, 2, 3]),
            &free,
            (Uuid::from_u128(8), 1),
            NOW_MS,
        );
        assert_eq!(
            again,
            Decision::Reply(granted(&open_confirmed(&open), false))
        );
        // A member who joined the group later is added.
        let Decision::AddMember {
            character: added, ..
        } = instances.decide(
            &request(4),
            &group(&[1, 2, 3, 4]),
            &free,
            (Uuid::from_u128(9), 1),
            NOW_MS,
        )
        else {
            panic!("adds");
        };
        assert_eq!(added, character(4));
        let members = instances.add_member(open.id, added).expect("open").members;
        assert_eq!(members.len(), 4);
        // Another game mode for the same group is refused.
        let other = InstanceRequest {
            game_mode: "DungeonShatteredObelisk".to_owned(),
            ..request(1)
        };
        let other_mode = instances.decide(
            &other,
            &group(&[1, 2, 3]),
            &free,
            (Uuid::from_u128(10), 1),
            NOW_MS,
        );
        assert_eq!(other_mode, refuse(InstanceRefusal::GroupInOtherInstance));
    }

    fn open_confirmed(open: &OpenInstance) -> OpenInstance {
        OpenInstance {
            confirmed: true,
            ..open.clone()
        }
    }

    #[test]
    fn group_size_and_free_sims_are_checked() {
        let mut instances = Instances::default();
        let free = [slot("inst-1")];
        let ids = (Uuid::from_u128(7), PHASE);
        assert_eq!(
            instances.decide(&request(1), &group(&[1, 2]), &free, ids, NOW_MS),
            refuse(InstanceRefusal::NotEnoughMembers)
        );
        let six = group(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(
            instances.decide(&request(1), &six, &free, ids, NOW_MS),
            refuse(InstanceRefusal::TooManyMembers)
        );
        assert_eq!(
            instances.decide(&request(1), &GroupAnswer::Alone, &free, ids, NOW_MS),
            refuse(InstanceRefusal::NotEnoughMembers)
        );
        assert_eq!(
            instances.decide(&request(1), &group(&[1, 2, 3]), &[], ids, NOW_MS),
            refuse(InstanceRefusal::NoInstanceSim)
        );
        // The one free sim goes to one group only.
        assert!(matches!(
            instances.decide(&request(1), &group(&[1, 2, 3]), &free, ids, NOW_MS),
            Decision::Open(_)
        ));
        let other_group = GroupAnswer::Group {
            group: Uuid::from_u128(200),
            leader: character(11),
            members: vec![character(11), character(12), character(13)],
        };
        let second = (Uuid::from_u128(8), 1);
        assert_eq!(
            instances.decide(&request(11), &other_group, &free, second, NOW_MS),
            refuse(InstanceRefusal::NoInstanceSim)
        );
        // A failed opening frees the sim.
        assert_eq!(instances.opened(Uuid::from_u128(7), false), None);
        assert!(instances.is_empty());
    }

    #[test]
    fn a_solo_trial_ends_on_time_or_when_empty_and_a_lost_sim_ends_it() {
        let mut instances = Instances::default();
        let solo = InstanceRequest {
            game_mode: "TrialSeason04Q06".to_owned(),
            capacity: 1,
            min_group: 0,
            timeout_seconds: Some(TRIAL_SECONDS),
            ..request(1)
        };
        let Decision::Open(open) = instances.decide(
            &solo,
            &GroupAnswer::Alone,
            &[slot("inst-1")],
            (Uuid::from_u128(7), PHASE),
            NOW_MS,
        ) else {
            panic!("opens");
        };
        assert_eq!(
            open.ends_at_ms,
            Some(NOW_MS + TRIAL_SECONDS * MS_PER_SECOND)
        );
        instances.opened(open.id, true);
        let report = |empty_for_ms: Option<u64>| {
            vec![(
                slot("inst-1"),
                Some(RunningInstance {
                    instance: open.id,
                    game_mode: open.game_mode.clone(),
                    members: open.members.clone(),
                    players: 1,
                    empty_for_ms,
                    open_for_ms: 0,
                    phase: open.phase,
                    capacity: open.capacity,
                    ends_at_ms: open.ends_at_ms,
                }),
            )]
        };
        assert!(
            instances
                .review(&report(None), &settings(), NOW_MS)
                .is_empty()
        );
        let close = Closing {
            sim: slot("inst-1"),
            instance: open.id,
        };
        assert_eq!(
            instances.review(&report(Some(60_000)), &settings(), NOW_MS),
            vec![close.clone()]
        );
        let end_ms = NOW_MS + TRIAL_SECONDS * MS_PER_SECOND;
        assert_eq!(
            instances.review(&report(None), &settings(), end_ms),
            vec![close]
        );
        // The sim went silent: forgotten.
        assert!(instances.review(&[], &settings(), NOW_MS).is_empty());
        assert!(instances.is_empty());
    }

    fn adopted_report(capacity: u32) -> Vec<(SimSlot, Option<RunningInstance>)> {
        vec![(
            slot("inst-1"),
            Some(RunningInstance {
                instance: Uuid::from_u128(7),
                game_mode: MODE.to_owned(),
                members: vec![character(1), character(2), character(3)],
                players: 3,
                empty_for_ms: None,
                open_for_ms: 5,
                phase: PHASE,
                capacity,
                ends_at_ms: Some(NOW_MS + TRIAL_SECONDS * MS_PER_SECOND),
            }),
        )]
    }

    #[test]
    fn a_restarted_director_adopts_what_the_sims_run() {
        let mut instances = Instances::default();
        instances.adopt(&adopted_report(5), |_| Some(LEVEL.to_owned()));
        assert_eq!(instances.len(), 1);
        let table = instances.table();
        assert!(table.sim_for(&Uuid::from_u128(7), &character(2)).is_some());
        let adopted = instances.get(&Uuid::from_u128(7)).expect("adopted");
        assert_eq!(adopted.phase, PHASE);
        assert_eq!(adopted.capacity, 5);
        assert_eq!(
            adopted.ends_at_ms,
            Some(NOW_MS + TRIAL_SECONDS * MS_PER_SECOND)
        );
        // A member who joined the group later still gets in (the capacity is the real one).
        assert!(matches!(
            instances.decide(
                &request(4),
                &group(&[1, 2, 3, 4]),
                &[],
                (Uuid::from_u128(8), 1),
                NOW_MS,
            ),
            Decision::AddMember { .. }
        ));
        // The end time ends it.
        let end_ms = NOW_MS + TRIAL_SECONDS * MS_PER_SECOND;
        assert_eq!(
            instances.review(&adopted_report(5), &settings(), end_ms),
            vec![Closing {
                sim: slot("inst-1"),
                instance: Uuid::from_u128(7),
            }]
        );
    }

    #[test]
    fn an_adopted_instance_of_unknown_capacity_takes_no_new_member() {
        let mut instances = Instances::default();
        instances.adopt(&adopted_report(UNKNOWN_CAPACITY), |_| {
            Some(LEVEL.to_owned())
        });
        assert_eq!(
            instances.decide(
                &request(4),
                &group(&[1, 2, 3, 4]),
                &[],
                (Uuid::from_u128(8), 1),
                NOW_MS,
            ),
            refuse(InstanceRefusal::TooManyMembers)
        );
    }

    #[test]
    fn a_repeat_ask_waits_for_the_opening_and_a_failed_admission_takes_the_member_back() {
        let mut instances = Instances::default();
        let free = [slot("inst-1")];
        let Decision::Open(open) = instances.decide(
            &request(1),
            &group(&[1, 2, 3]),
            &free,
            (Uuid::from_u128(7), PHASE),
            NOW_MS,
        ) else {
            panic!("opens");
        };
        // Not confirmed yet: a member and a later member both wait.
        let ids = (Uuid::from_u128(8), 1);
        assert_eq!(
            instances.decide(&request(2), &group(&[1, 2, 3]), &free, ids, NOW_MS),
            Decision::Wait { instance: open.id }
        );
        assert_eq!(
            instances.decide(&request(4), &group(&[1, 2, 3, 4]), &free, ids, NOW_MS),
            Decision::Wait { instance: open.id }
        );
        instances.opened(open.id, true);
        assert!(matches!(
            instances.decide(&request(2), &group(&[1, 2, 3]), &free, ids, NOW_MS),
            Decision::Reply(InstanceReply::Granted { .. })
        ));
        instances.add_member(open.id, character(4));
        instances.remove_member(open.id, character(4));
        assert!(instances.table().sim_for(&open.id, &character(4)).is_none());
    }

    fn launch_settings() -> LaunchSettings {
        LaunchSettings {
            program: "target/release/sim".to_owned(),
            args: Vec::new(),
            spare_per_level: 2,
            max_sims: 3,
            id_prefix: "inst".to_owned(),
            forget_after: FORGET_AFTER,
        }
    }

    /// The forget time of the launch tests.
    const FORGET_AFTER: Duration = Duration::from_secs(600);

    #[test]
    fn wanted_levels_get_their_spare_sims_within_the_limit_and_lose_the_rest() {
        let mut launches = Launches::default();
        let start = Instant::now();
        assert!(
            launches
                .to_start(&launch_settings(), |_| 0, |_| false)
                .is_empty()
        );
        launches.wanted(LEVEL, start);
        let started = launches.to_start(&launch_settings(), |_| 0, |_| false);
        assert_eq!(started.len(), 2);
        assert!(started.iter().all(|launch| launch.level == LEVEL));
        // Not beating yet: they count as ready, so nothing more starts.
        assert!(
            launches
                .to_start(&launch_settings(), |_| 0, |_| false)
                .is_empty()
        );
        launches.wanted("nw_dungeon_everfall_00", start);
        assert_eq!(
            launches
                .to_start(&launch_settings(), |_| 0, |_| false)
                .len(),
            1,
            "the total limit"
        );
        // Nobody asks any more: the free started sims stop.
        let free = |level: &str| {
            if level == LEVEL {
                started
                    .iter()
                    .map(|launch| SimSlot {
                        id: launch.id.clone(),
                        generation: 1,
                    })
                    .collect()
            } else {
                Vec::new()
            }
        };
        assert!(launches.to_stop(&launch_settings(), free, start).is_empty());
        let stop = launches.to_stop(&launch_settings(), free, start + FORGET_AFTER);
        assert_eq!(stop.len(), 2);
        launches.exited(&stop[0]);
    }
}
