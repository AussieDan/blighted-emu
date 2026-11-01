// SPDX-License-Identifier: AGPL-3.0-or-later
//! GM commands that the admin tool sends to this sim (admin studio spec sections 8.1, 8.2).
//! `Kick` needs the server loop (it saves and closes the connection): see
//! [`SimWorld::connection_of`].

use crate::mobs::ring;
use bevy_ecs::component::Component;
use nw_bus::{AdminAction, AdminCommand, AdminReply, ConnectionRef, Point};
use nw_formats::objectstream::lowercase_crc32;
use nw_world::records::VitalsSpawn;

use super::attack::Vitals;
use super::gear::unix_ns;
use super::{Placement, Player, QuestRefusal, SimWorld};

/// Most GM mobs one command spawns.
pub const MAX_GM_SPAWN: u32 = 50;
const LOWEST_LEVEL: u32 = 1;
/// The answer for a player whose character has no bag (no starter kit row for it).
pub const NO_BAG: &str = "the player has no bag (its character has no starter kit)";
const HIGHEST_LEVEL: u32 = 65;

/// A player that takes no damage (GM god mode).
#[derive(Component, Debug, Clone, Copy)]
pub(super) struct GodMode;

impl SimWorld {
    /// Carries out `command` at `now_ms`. A target this sim does not own gives
    /// [`AdminReply::NotMine`]. `Kick` is the server's: here it is refused.
    pub fn admin(&mut self, command: &AdminCommand, now_ms: u64) -> AdminReply {
        self.admin_action(&command.action, now_ms)
    }

    /// Carries out `action` at `now_ms`, as [`SimWorld::admin`].
    pub(super) fn admin_action(&mut self, action: &AdminAction, now_ms: u64) -> AdminReply {
        match action {
            AdminAction::TeleportPlayer { entity, to } => {
                self.teleport_player(*entity, *to, now_ms)
            }
            AdminAction::Heal { entity } => self.with_vitals(*entity, |vitals| {
                vitals.health.refill();
                vitals.dead_since_ms = None;
            }),
            AdminAction::Kill { entity } => self.kill_player(*entity, now_ms),
            AdminAction::GodMode { entity, on } => self.god_mode(*entity, *on),
            AdminAction::GiveItem {
                entity,
                item,
                quantity,
            } => self.give_item(*entity, item, *quantity),
            AdminAction::TakeItem { entity, item } => self.admin_take_item(*entity, item),
            AdminAction::StartQuest { entity, objective } => {
                match self.start_quest(*entity, objective, now_ms) {
                    Ok(()) => AdminReply::Done,
                    Err(QuestRefusal::NoPlayer) => AdminReply::NotMine,
                    Err(refusal) => AdminReply::Failed {
                        reason: refusal.to_string(),
                    },
                }
            }
            AdminAction::SpawnMobs {
                vitals,
                level,
                at,
                count,
            } => self.spawn_gm_mobs(vitals, *level, *at, *count),
            AdminAction::DespawnMobs { entities } => self.despawn_gm_mobs(entities),
            AdminAction::StartEvent { event, at } => match self.start_event(event, *at, now_ms) {
                Ok(()) => AdminReply::Done,
                Err(reason) => AdminReply::Failed { reason },
            },
            AdminAction::StopEvent { event } => {
                if self.stop_event(event) {
                    AdminReply::Done
                } else {
                    AdminReply::NotMine
                }
            }
            AdminAction::ListEvents => AdminReply::Events {
                running: self.running_events(),
            },
            AdminAction::GiveExperience { entity, amount } => {
                self.give_experience(*entity, *amount)
            }
            AdminAction::SetLevel { entity, level } => self.set_level(*entity, *level),
            AdminAction::GiveMasteryExperience {
                entity,
                mastery,
                amount,
            } => self.give_mastery_experience(*entity, mastery, *amount),
            AdminAction::SetMasteryRank {
                entity,
                mastery,
                rank,
            } => self.set_mastery_rank(*entity, mastery.as_deref(), *rank),
            AdminAction::SetCoins { entity, coins } => self.set_coins(*entity, *coins),
            AdminAction::Kick { .. } => AdminReply::Failed {
                reason: "a kick is the server loop's".to_owned(),
            },
        }
    }

    /// The connection of the player with entity number `entity`, when it is here.
    pub fn connection_of(&mut self, entity: u64) -> Option<ConnectionRef> {
        let target = self.player_by_number(entity)?;
        self.world
            .get::<Player>(target)
            .map(|player| player.connection.clone())
    }

    /// True when the player with entity number `entity` takes no damage.
    pub(super) fn is_god(&self, entity: bevy_ecs::entity::Entity) -> bool {
        self.world.get::<GodMode>(entity).is_some()
    }

    /// Moves the player with entity number `entity` to `to`. The next own state of the player
    /// carries the teleport to its client.
    pub(super) fn teleport_player(&mut self, entity: u64, to: Point, now_ms: u64) -> AdminReply {
        let bounds = self.settings.bounds;
        let inside = (bounds.min_x..=bounds.max_x).contains(&to.x)
            && (bounds.min_y..=bounds.max_y).contains(&to.y)
            && to.z.is_finite();
        if !inside {
            return AdminReply::Failed {
                reason: format!("({}, {}) is off the level", to.x, to.y),
            };
        }
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        let position = nw_sim_core::Position {
            x: to.x,
            y: to.y,
            z: to.z,
        };
        self.place_player(target, position, None, Placement::Drop, now_ms);
        AdminReply::Done
    }

    /// Changes the vitals of the player `entity`; the next own state sends the health.
    fn with_vitals(&mut self, entity: u64, change: impl FnOnce(&mut Vitals)) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        match self.world.get_mut::<Vitals>(target) {
            Some(mut vitals) => {
                change(&mut vitals);
                AdminReply::Done
            }
            None => AdminReply::Failed {
                reason: "the player has no vitals".to_owned(),
            },
        }
    }

    /// Takes all of the player's health as a hit of itself, so its client sees the death.
    fn kill_player(&mut self, entity: u64, now_ms: u64) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        let Some(health) = self
            .world
            .get::<Vitals>(target)
            .map(|vitals| vitals.health.current())
        else {
            return AdminReply::Failed {
                reason: "the player has no vitals".to_owned(),
            };
        };
        self.world.entity_mut(target).remove::<GodMode>();
        match self.apply_hit(entity, entity, health, 0, now_ms) {
            Some(_) => AdminReply::Done,
            None => AdminReply::Failed {
                reason: "the player is dead or moving between sims".to_owned(),
            },
        }
    }

    fn god_mode(&mut self, entity: u64, on: bool) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        let mut player = self.world.entity_mut(target);
        if on {
            player.insert(GodMode);
        } else {
            player.remove::<GodMode>();
        }
        AdminReply::Done
    }

    fn give_item(&mut self, entity: u64, item_id: &str, quantity: u32) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        let has_bag = self
            .world
            .get::<Player>(target)
            .is_some_and(|player| player.gear.is_some());
        if !has_bag {
            return AdminReply::Failed {
                reason: NO_BAG.to_owned(),
            };
        }
        let Some(item) = self.content.kits.item(item_id).cloned() else {
            return AdminReply::Failed {
                reason: format!("no item {item_id}"),
            };
        };
        let given = self
            .world
            .get_mut::<Player>(target)
            .is_some_and(|mut player| {
                player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.add(&item, quantity))
            });
        if given {
            AdminReply::Done
        } else {
            AdminReply::Failed {
                reason: "the bag is full".to_owned(),
            }
        }
    }

    fn admin_take_item(&mut self, entity: u64, item_id: &str) -> AdminReply {
        let Some(target) = self.player_by_number(entity) else {
            return AdminReply::NotMine;
        };
        let time_ns = unix_ns(self.server_ms);
        let taken = self
            .world
            .get_mut::<Player>(target)
            .is_some_and(|mut player| {
                player
                    .gear
                    .as_mut()
                    .is_some_and(|gear| gear.remove(item_id, time_ns))
            });
        if taken {
            AdminReply::Done
        } else {
            AdminReply::Failed {
                reason: format!("the player has no {item_id}"),
            }
        }
    }

    /// Spawns `count` GM mobs of `vitals` on a ring around `at`. The admin sends this to the
    /// sim that owns the cell of `at`.
    fn spawn_gm_mobs(&mut self, vitals: &str, level: u32, at: Point, count: u32) -> AdminReply {
        match self.spawn_gm(vitals, level, at, count) {
            Ok(entities) => AdminReply::Spawned { entities },
            Err(reason) => AdminReply::Failed { reason },
        }
    }

    /// Spawns `count` GM mobs of `vitals` at `level` on a ring around `at` (GM commands, scripts
    /// and events); their entity numbers, or why none spawned.
    pub(super) fn spawn_gm(
        &mut self,
        vitals: &str,
        level: u32,
        at: Point,
        count: u32,
    ) -> Result<Vec<u64>, String> {
        if !(LOWEST_LEVEL..=HIGHEST_LEVEL).contains(&level) {
            return Err(format!(
                "level {level} is not {LOWEST_LEVEL} to {HIGHEST_LEVEL}"
            ));
        }
        if count == 0 || count > MAX_GM_SPAWN {
            return Err(format!("count {count} is not 1 to {MAX_GM_SPAWN}"));
        }
        let mut entities = Vec::new();
        for (x, y) in ring(at.x, at.y, count) {
            let spawn = VitalsSpawn {
                vitals_id: lowercase_crc32(vitals),
                level,
                position: [x, y, at.z],
                ..VitalsSpawn::default()
            };
            match self.mobs.spawn_gm(spawn) {
                Some(entity) => entities.push(entity),
                None => {
                    self.remove_gm(&entities);
                    return Err(format!("no mob type {vitals} at level {level}"));
                }
            }
        }
        Ok(entities)
    }

    /// Removes the GM mobs `entities`; how many were here.
    pub(super) fn remove_gm(&mut self, entities: &[u64]) -> usize {
        entities
            .iter()
            .filter(|entity| self.mobs.remove_gm(**entity))
            .count()
    }

    fn despawn_gm_mobs(&mut self, entities: &[u64]) -> AdminReply {
        let targets = if entities.is_empty() {
            self.mobs.gm_mobs()
        } else {
            entities.to_vec()
        };
        let removed: Vec<u64> = targets
            .into_iter()
            .filter(|entity| self.mobs.remove_gm(*entity))
            .collect();
        if removed.is_empty() && !entities.is_empty() {
            AdminReply::NotMine
        } else {
            AdminReply::Done
        }
    }
}

#[cfg(test)]
mod tests {
    use nw_bus::{AdminAction, AdminCommand, AdminReply, Point};
    use uuid::Uuid;

    use super::*;
    use crate::combat::test_support::{TEST_LEVEL, TEST_MOB};
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, world_with_spawns};

    const PLAYER: u64 = 41;
    const TARGET: Point = Point {
        x: 500.0,
        y: 600.0,
        z: 30.0,
    };

    fn command(action: AdminAction) -> AdminCommand {
        AdminCommand {
            request: Uuid::from_u128(1),
            actor: None,
            action,
        }
    }

    fn teleport(entity: u64, to: Point) -> AdminCommand {
        command(AdminAction::TeleportPlayer { entity, to })
    }

    fn admitted() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        world
    }

    fn health(world: &mut SimWorld) -> u32 {
        let target = world.player_by_number(PLAYER).expect("player");
        world
            .world
            .get::<Vitals>(target)
            .expect("vitals")
            .health
            .current()
    }

    #[test]
    fn a_teleport_moves_the_player() {
        let mut world = admitted();
        assert_eq!(
            world.admin(&teleport(PLAYER, TARGET), NOW),
            AdminReply::Done
        );
        let (players, _, _) = world.snapshot();
        let player = players
            .iter()
            .find(|player| player.entity == PLAYER)
            .expect("player");
        assert_eq!(
            (player.x, player.y, player.z),
            (TARGET.x, TARGET.y, TARGET.z)
        );
    }

    #[test]
    fn an_unknown_player_is_not_mine() {
        let mut world = world_with_spawns(Vec::new());
        assert_eq!(
            world.admin(&teleport(PLAYER, TARGET), NOW),
            AdminReply::NotMine
        );
        assert_eq!(
            world.admin(&command(AdminAction::Heal { entity: PLAYER }), NOW),
            AdminReply::NotMine
        );
    }

    #[test]
    fn a_target_off_the_level_fails() {
        let mut world = admitted();
        let off = Point { x: -10.0, ..TARGET };
        assert!(matches!(
            world.admin(&teleport(PLAYER, off), NOW),
            AdminReply::Failed { .. }
        ));
    }

    #[test]
    fn kill_then_heal_brings_the_player_back() {
        let mut world = admitted();
        let full = health(&mut world);
        assert!(full > 0);
        assert_eq!(
            world.admin(&command(AdminAction::Kill { entity: PLAYER }), NOW),
            AdminReply::Done
        );
        assert_eq!(health(&mut world), 0);
        assert_eq!(
            world.admin(&command(AdminAction::Heal { entity: PLAYER }), NOW),
            AdminReply::Done
        );
        assert_eq!(health(&mut world), full);
    }

    #[test]
    fn god_mode_stops_damage_until_it_is_turned_off() {
        let mut world = admitted();
        let full = health(&mut world);
        world.admin(
            &command(AdminAction::GodMode {
                entity: PLAYER,
                on: true,
            }),
            NOW,
        );
        assert_eq!(world.apply_hit(PLAYER, 1, 10, 0, NOW), None);
        assert_eq!(health(&mut world), full);
        world.admin(
            &command(AdminAction::GodMode {
                entity: PLAYER,
                on: false,
            }),
            NOW,
        );
        assert!(world.apply_hit(PLAYER, 1, 10, 0, NOW).is_some());
        assert!(health(&mut world) < full);
    }

    #[test]
    fn an_unknown_quest_fails_and_another_player_is_not_mine() {
        let mut world = admitted();
        let start = |entity| {
            command(AdminAction::StartQuest {
                entity,
                objective: "NoSuchQuest".to_owned(),
            })
        };
        assert_eq!(
            world.admin(&start(PLAYER), NOW),
            AdminReply::Failed {
                reason: QuestRefusal::NoQuest.to_string()
            }
        );
        assert_eq!(world.admin(&start(PLAYER + 1), NOW), AdminReply::NotMine);
    }

    #[test]
    fn an_unknown_item_fails() {
        let mut world = admitted();
        assert!(matches!(
            world.admin(
                &command(AdminAction::GiveItem {
                    entity: PLAYER,
                    item: "NoSuchItem".to_owned(),
                    quantity: 1,
                }),
                NOW
            ),
            AdminReply::Failed { .. }
        ));
    }

    #[test]
    fn a_player_without_a_bag_is_told_apart_from_a_full_bag() {
        let mut world = admitted();
        let reply = world.admin(
            &command(AdminAction::GiveItem {
                entity: PLAYER,
                item: "AnyItem".to_owned(),
                quantity: 1,
            }),
            NOW,
        );
        assert_eq!(
            reply,
            AdminReply::Failed {
                reason: NO_BAG.to_owned()
            }
        );
    }

    #[test]
    fn gm_mobs_spawn_on_a_ring_and_despawn() {
        let mut world = world_with_spawns(Vec::new());
        let spawned = world.admin(
            &command(AdminAction::SpawnMobs {
                vitals: TEST_MOB.to_owned(),
                level: TEST_LEVEL,
                at: TARGET,
                count: 3,
            }),
            NOW,
        );
        let AdminReply::Spawned { entities } = spawned else {
            panic!("spawned: {spawned:?}");
        };
        assert_eq!(entities.len(), 3);
        assert_eq!(world.mobs.gm_mobs(), entities);
        assert_eq!(
            world.admin(
                &command(AdminAction::DespawnMobs {
                    entities: Vec::new()
                }),
                NOW
            ),
            AdminReply::Done
        );
        assert!(world.mobs.gm_mobs().is_empty());
    }

    #[test]
    fn a_spawn_out_of_range_fails() {
        let mut world = world_with_spawns(Vec::new());
        for (level, count) in [(0, 1), (TEST_LEVEL, 0), (TEST_LEVEL, MAX_GM_SPAWN + 1)] {
            assert!(matches!(
                world.admin(
                    &command(AdminAction::SpawnMobs {
                        vitals: TEST_MOB.to_owned(),
                        level,
                        at: TARGET,
                        count,
                    }),
                    NOW
                ),
                AdminReply::Failed { .. }
            ));
        }
    }

    #[test]
    fn a_ring_keeps_its_spacing() {
        let points = ring(0.0, 0.0, 4);
        let (x0, y0) = points[0];
        let (x1, y1) = points[1];
        let apart = ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
        assert!(
            apart > crate::mobs::SPAWN_SPACING_M * 0.8
                && apart < crate::mobs::SPAWN_SPACING_M * 1.2
        );
        assert_eq!(ring(5.0, 6.0, 1), [(5.0, 6.0)]);
    }
}
