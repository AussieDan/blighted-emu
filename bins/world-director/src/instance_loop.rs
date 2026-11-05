// SPDX-License-Identifier: AGPL-3.0-or-later
//! The I/O of the instances in the director's loop: the requests of the sims, the group queries
//! to the social service, the openings and closings on the instance sims, and the instance sim
//! processes the director starts and stops ([`crate::instances`] decides).
//!
//! The loop owns the instances, so two requests never take the same free sim; every bus call
//! runs in its own task and reports back to the loop ([`InstanceEvent`]).

use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use nw_bus::{
    Bus, BusId, GroupAnswer, GroupQuery, Incoming, InstanceCommand, InstanceOpened,
    InstanceRefusal, InstanceReply, InstanceRequest, SimSlot, subject,
};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::instances::{
    Decision, InstanceSettings, InstanceTable, Instances, Launch, LaunchSettings, Launches, granted,
};
use crate::sims::SimRegistry;

/// Instance events that may wait for the loop.
pub const INSTANCE_QUEUE: usize = 256;
/// Metric: instance requests by `result`.
pub const METRIC_INSTANCE_REQUESTS: &str = "director_instance_requests_total";
/// Metric: open instances.
pub const METRIC_INSTANCES: &str = "director_instances";
/// Metric: instance sims the director started and runs.
pub const METRIC_LAUNCHED_SIMS: &str = "director_launched_sims";
const RESULT_LABEL: &str = "result";
const GRANTED: &str = "granted";
/// The environment variables of a started instance sim (`NW_SIM_` overrides of
/// `config/sim.toml`).
const SIM_ID_ENV: &str = "NW_SIM_SIM_ID";
const SIM_LEVEL_ENV: &str = "NW_SIM_LEVEL";
const SIM_ROLE_ENV: &str = "NW_SIM_ROLE";
const SIM_METRICS_ENV: &str = "NW_SIM_METRICS_LISTEN";
/// The role value of an instance sim.
const INSTANCE_ROLE: &str = "instance";
/// A started sim's metrics on a port the system picks, so many sims share one host.
const ANY_LOCAL_PORT: &str = "127.0.0.1:0";
/// The smallest phase id: a phase id must not be 0 (the client drops a 0-phase entry).
const MIN_PHASE: u64 = 1;

/// A result of a bus call of the instances, for the loop.
#[derive(Debug)]
pub enum InstanceEvent {
    /// The social service answered the group query of `request`.
    Grouped {
        /// The request.
        request: Incoming<InstanceRequest>,
        /// The group, or why there is none.
        group: Result<GroupAnswer, InstanceRefusal>,
    },
    /// The instance sim answered the opening of `instance`.
    Opened {
        /// The instance.
        instance: Uuid,
        /// Whether it opened.
        ok: bool,
    },
    /// The admission of `character` to `instance` did not reach its sim.
    AdmitFailed {
        /// The instance.
        instance: Uuid,
        /// The character.
        character: Uuid,
    },
}

/// A request that waits for the opening of its group's instance, with its group.
type Waiting = (Incoming<InstanceRequest>, GroupAnswer);

/// The instances of the loop with their bus and processes.
pub struct InstanceLoop {
    settings: Option<InstanceSettings>,
    world_id: Uuid,
    bus: Bus,
    instances: Instances,
    launches: Launches,
    children: HashMap<BusId, Child>,
    events: mpsc::Sender<InstanceEvent>,
    /// The requests that wait for an instance's opening, by instance.
    waiting: HashMap<Uuid, Vec<Waiting>>,
}

impl InstanceLoop {
    /// The instances of world `world_id`; `settings` is `None` for a world without instances.
    pub fn new(
        settings: Option<InstanceSettings>,
        world_id: Uuid,
        bus: Bus,
        events: mpsc::Sender<InstanceEvent>,
    ) -> Self {
        Self {
            settings,
            world_id,
            bus,
            instances: Instances::default(),
            launches: Launches::default(),
            children: HashMap::new(),
            events,
            waiting: HashMap::new(),
        }
    }

    /// The open instances, for a join.
    pub fn table(&self) -> InstanceTable {
        self.instances.table()
    }

    /// A sim's request: a solo game mode goes on at once, a group one after the group query.
    pub fn request(&mut self, request: Incoming<InstanceRequest>, sims: &SimRegistry) {
        let Some(timeout) = self
            .settings
            .as_ref()
            .map(|settings| settings.group_timeout)
        else {
            self.reply(
                request,
                InstanceReply::Refused {
                    reason: InstanceRefusal::NoInstanceSim,
                },
            );
            return;
        };
        self.launches.wanted(&request.message.level, Instant::now());
        if request.message.capacity <= 1 {
            self.decide(request, Ok(GroupAnswer::Alone), sims);
            return;
        }
        let (bus, events) = (self.bus.clone(), self.events.clone());
        let query = GroupQuery {
            character: request.message.requester,
        };
        let subject = subject::social_group(self.world_id);
        tokio::spawn(async move {
            let group = bus
                .request::<GroupQuery, GroupAnswer>(subject, &query, timeout)
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "group query failed");
                    InstanceRefusal::GroupTimeout
                });
            // The loop may have stopped; then the answer no longer matters.
            let _ = events.send(InstanceEvent::Grouped { request, group }).await;
        });
    }

    /// A result of a bus call.
    pub fn event(&mut self, event: InstanceEvent, sims: &SimRegistry) {
        match event {
            InstanceEvent::Grouped { request, group } => self.decide(request, group, sims),
            InstanceEvent::Opened { instance, ok } => {
                self.instances.opened(instance, ok);
                metrics::gauge!(METRIC_INSTANCES).set(self.instances.len() as f64);
                // The requests that waited get what the first one got: a decision on the
                // confirmed instance, or the same refusal.
                for (request, group) in self.waiting.remove(&instance).unwrap_or_default() {
                    if ok {
                        self.decide(request, Ok(group), sims);
                    } else {
                        self.reply(
                            request,
                            InstanceReply::Refused {
                                reason: InstanceRefusal::OpenFailed,
                            },
                        );
                    }
                }
            }
            InstanceEvent::AdmitFailed {
                instance,
                character,
            } => self.instances.remove_member(instance, character),
        }
    }

    fn decide(
        &mut self,
        request: Incoming<InstanceRequest>,
        group: Result<GroupAnswer, InstanceRefusal>,
        sims: &SimRegistry,
    ) {
        let group = match group {
            Ok(group) => group,
            Err(reason) => {
                self.reply(request, InstanceReply::Refused { reason });
                return;
            }
        };
        let free = sims.free_instance_sims(&request.message.level);
        let ids = (Uuid::new_v4(), random_phase());
        match self
            .instances
            .decide(&request.message, &group, &free, ids, unix_ms())
        {
            Decision::Reply(reply) => self.reply(request, reply),
            Decision::Wait { instance } => self
                .waiting
                .entry(instance)
                .or_default()
                .push((request, group)),
            Decision::AddMember {
                instance,
                character,
            } => {
                let open = self
                    .instances
                    .add_member(instance.id, character)
                    .unwrap_or(instance);
                let (bus, world_id, events) =
                    (self.bus.clone(), self.world_id, self.events.clone());
                tokio::spawn(async move {
                    let command = InstanceCommand::Admit {
                        instance: open.id,
                        character,
                    };
                    let reply = match bus
                        .publish(subject::sim_instance(world_id, &open.sim.id), &command)
                        .await
                    {
                        Ok(()) => granted(&open, false),
                        Err(error) => {
                            tracing::warn!(%error, instance = %open.id, %character, "instance member not sent");
                            // The loop may have stopped; then the member no longer matters.
                            let _ = events
                                .send(InstanceEvent::AdmitFailed {
                                    instance: open.id,
                                    character,
                                })
                                .await;
                            // Inferred: the sim never heard of the member, so the entrance
                            // answers as for a failed opening (`EntranceActivationFailure`).
                            InstanceReply::Refused {
                                reason: InstanceRefusal::OpenFailed,
                            }
                        }
                    };
                    answer(&bus, &request, &reply).await;
                });
            }
            Decision::Open(open) => {
                let Some(timeout) = self.settings.as_ref().map(|settings| settings.open_timeout)
                else {
                    self.instances.opened(open.id, false);
                    self.reply(
                        request,
                        InstanceReply::Refused {
                            reason: InstanceRefusal::NoInstanceSim,
                        },
                    );
                    return;
                };
                let (bus, world_id, events) =
                    (self.bus.clone(), self.world_id, self.events.clone());
                tokio::spawn(async move {
                    let command = InstanceCommand::Open {
                        instance: open.id,
                        game_mode: open.game_mode.clone(),
                        phase: open.phase,
                        members: open.members.clone(),
                        ends_at_ms: open.ends_at_ms,
                    };
                    let opened = bus
                        .request::<InstanceCommand, InstanceOpened>(
                            subject::sim_instance(world_id, &open.sim.id),
                            &command,
                            timeout,
                        )
                        .await;
                    let ok = matches!(opened, Ok(InstanceOpened::Opened));
                    if !ok {
                        tracing::warn!(
                            instance = %open.id,
                            sim = %open.sim.id,
                            ?opened,
                            "instance not opened"
                        );
                    }
                    let _ = events
                        .send(InstanceEvent::Opened {
                            instance: open.id,
                            ok,
                        })
                        .await;
                    let reply = if ok {
                        tracing::info!(
                            instance = %open.id,
                            game_mode = %open.game_mode,
                            sim = %open.sim.id,
                            "instance opened"
                        );
                        granted(&open, true)
                    } else {
                        InstanceReply::Refused {
                            reason: InstanceRefusal::OpenFailed,
                        }
                    };
                    answer(&bus, &request, &reply).await;
                });
            }
        }
    }

    fn reply(&self, request: Incoming<InstanceRequest>, reply: InstanceReply) {
        let bus = self.bus.clone();
        tokio::spawn(async move { answer(&bus, &request, &reply).await });
    }

    /// Each heartbeat interval: ends the instances that ran out of time or stayed empty, adopts
    /// the ones a restarted director does not know, and starts and stops instance sims.
    pub fn timer(&mut self, sims: &SimRegistry) {
        let Some(settings) = self.settings.clone() else {
            return;
        };
        let running = sims.running_instances();
        self.instances
            .adopt(&running, |slot| sims.level_of(&slot.id));
        for closing in self.instances.review(&running, &settings, unix_ms()) {
            self.close(closing.sim, closing.instance);
            self.instances.closed(closing.instance);
        }
        metrics::gauge!(METRIC_INSTANCES).set(self.instances.len() as f64);
        if let Some(launch) = &settings.launch {
            self.launch(launch, sims);
        }
    }

    fn close(&self, sim: SimSlot, instance: Uuid) {
        let (bus, world_id) = (self.bus.clone(), self.world_id);
        tokio::spawn(async move {
            let command = InstanceCommand::Close { instance };
            if let Err(error) = bus
                .publish(subject::sim_instance(world_id, &sim.id), &command)
                .await
            {
                tracing::warn!(%error, %instance, "instance close not sent");
            }
        });
    }

    fn launch(&mut self, settings: &LaunchSettings, sims: &SimRegistry) {
        self.children.retain(|id, child| match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                tracing::warn!(sim = %id, %status, "started instance sim exited");
                false
            }
            Err(error) => {
                tracing::warn!(sim = %id, %error, "started instance sim cannot be checked");
                false
            }
        });
        let exited: Vec<BusId> = self
            .launches
            .started()
            .filter(|id| !self.children.contains_key(*id))
            .cloned()
            .collect();
        for id in &exited {
            self.launches.exited(id);
        }
        let free =
            |level: &str| u32::try_from(sims.free_instance_sims(level).len()).unwrap_or(u32::MAX);
        let live = |id: &BusId| sims.level_of(id).is_some();
        for Launch { id, level } in self.launches.to_start(settings, free, live) {
            match start_sim(settings, &id, &level) {
                Ok(child) => {
                    tracing::info!(sim = %id, %level, "instance sim started");
                    self.children.insert(id, child);
                }
                Err(error) => {
                    tracing::warn!(sim = %id, %level, %error, "instance sim not started");
                    self.launches.exited(&id);
                }
            }
        }
        for id in self.launches.to_stop(
            settings,
            |level| sims.free_instance_sims(level),
            Instant::now(),
        ) {
            if let Some(mut child) = self.children.remove(&id) {
                tracing::info!(sim = %id, "free instance sim stopped");
                if let Err(error) = child.start_kill() {
                    tracing::warn!(sim = %id, %error, "instance sim not stopped");
                }
            }
        }
        metrics::gauge!(METRIC_LAUNCHED_SIMS).set(self.children.len() as f64);
    }
}

/// Starts one instance sim process of `level` with id `id`.
fn start_sim(settings: &LaunchSettings, id: &BusId, level: &str) -> std::io::Result<Child> {
    Command::new(&settings.program)
        .args(&settings.args)
        .env(SIM_ID_ENV, id.to_string())
        .env(SIM_LEVEL_ENV, level)
        .env(SIM_ROLE_ENV, INSTANCE_ROLE)
        .env(SIM_METRICS_ENV, ANY_LOCAL_PORT)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

async fn answer(bus: &Bus, request: &Incoming<InstanceRequest>, reply: &InstanceReply) {
    let result = match reply {
        InstanceReply::Granted { .. } => GRANTED,
        InstanceReply::Refused { reason } => refusal_label(*reason),
    };
    metrics::counter!(METRIC_INSTANCE_REQUESTS, RESULT_LABEL => result).increment(1);
    if let Err(error) = bus.reply(request, reply).await {
        tracing::warn!(%error, "instance reply failed");
    }
}

/// The metric label of a refusal.
const fn refusal_label(reason: InstanceRefusal) -> &'static str {
    match reason {
        InstanceRefusal::NotEnoughMembers => "not_enough_members",
        InstanceRefusal::TooManyMembers => "too_many_members",
        InstanceRefusal::GroupInOtherInstance => "group_in_other_instance",
        InstanceRefusal::NoInstanceSim => "no_instance_sim",
        InstanceRefusal::GroupTimeout => "group_timeout",
        InstanceRefusal::OpenFailed => "open_failed",
    }
}

/// A new random phase id, never 0.
fn random_phase() -> u64 {
    Uuid::new_v4().as_u64_pair().0.max(MIN_PHASE)
}

/// Unix milliseconds now.
fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_are_never_zero_and_refusals_have_labels() {
        assert!((0..64).all(|_| random_phase() >= MIN_PHASE));
        assert_eq!(
            refusal_label(InstanceRefusal::GroupTimeout),
            "group_timeout"
        );
        assert!(unix_ms() > 0);
    }
}
