// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player's faction (faction note 2026-10-30, `local-data/findings`): the client's
//! `RequestSetFaction` (3161) after a faction representative's `Faction_Selection` service sets
//! it; the faction state change goes with the next bundle (retail sends no answer), and the
//! quests take it (`TaskChooseFaction`).
//!
//! **Observed** (retail `Hive_join_yellow_faction_quest`, `Change-Faction`): a first join changes
//! the faction alone; a change from one faction to another also sets the change's time and adds
//! one to the change count. **Unknown**: whether retail refuses a change in the faction change
//! cooldown (the representative's conversation states `InCooldownSingleDay` and
//! `InCooldownMultipleDays` keep the client from asking then); this server takes every pick of a
//! joinable faction other than the player's.
//!
//! The PvP flag (`TogglePvpFlag` 2525, retail `Hive_flag_pvp_on_and_off`, faction state bodies in
//! `nw_messages::faction_state`): a toggle on makes the flag pending; in a sanctuary it waits;
//! out of one its countdown of [`PVP_FLAG_DELAY_MS`] runs (it starts when the player leaves the
//! sanctuary); at its end the flag is on, and only then does combat count the player as
//! flagged. Entering a sanctuary with the flag on takes it off and makes it pending again; a
//! toggle while pending cancels it. Inferred: a sanctuary is the reach of a settlement or outpost
//! capital ([`crate::trading_post::SETTLEMENT_REACH_M`]: the level data loaded here has no
//! sanctuary volumes); a toggle outside a sanctuary starts the countdown at once, a toggle with
//! the flag on takes it off (`pvpFlag` 0, `notifyPending` 1), and entering a sanctuary during a
//! countdown stops it (`pvpFlagPendingEndTime` 0): the captured run toggled in a sanctuary only.
//! A countdown does not survive a handoff to another sim (the handoff carries the flag alone).

use bevy_ecs::entity::Entity;
use nw_content::factions::{NO_FACTION, joinable};
use nw_messages::faction_state::{FactionChange, FactionStateChange};

use super::gear::unix_ns;
use super::{Body, Player, SimWorld};

/// The PvP flag countdown after the player leaves a sanctuary. Observed: in
/// `Hive_flag_pvp_on_and_off` the flag went on 30.0 s after the bundle that left the sanctuary
/// (1782821022777 to 1782821052796).
pub(super) const PVP_FLAG_DELAY_MS: u64 = 30_000;

/// A player's PvP flag on its way on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PvpFlagWait {
    /// The flag is pending (`pvpFlagPending`).
    pending: bool,
    /// Sim ms when the pending flag goes on; `None` while it waits in a sanctuary.
    ends_ms: Option<u64>,
    /// Whether the player is in a sanctuary (`hasSanctuary`); `None` before the first check.
    in_sanctuary: Option<bool>,
}
use crate::objectives::Event;

impl SimWorld {
    /// Player `number` (`entity`) picks faction `faction` at `now_ms` (3161).
    pub(super) fn set_faction(&mut self, entity: Entity, number: u64, faction: u8, now_ms: u64) {
        let changed_ns = unix_ns(self.server_ms);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let before = player.faction.faction;
        if !joinable(faction) || faction == before {
            tracing::debug!(player = number, faction, before, "faction pick refused");
            return;
        }
        let switched = (before != NO_FACTION).then(|| {
            player.faction.changes = player.faction.changes.saturating_add(1);
            player.faction.changed_ns = changed_ns;
            (changed_ns, player.faction.changes)
        });
        player.faction.faction = faction;
        push_faction(&mut player, switched);
        tracing::info!(player = number, faction, before, "faction set");
        self.quest_event(number, Event::FactionChosen(faction), now_ms);
    }

    /// The client of player `number`, which just spawned, gets its faction when it has one
    /// (retail logs a character with a faction in with it; the card's first body has none).
    pub(super) fn send_faction(&mut self, number: u64) {
        if let Some(mut player) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
            && player.faction.faction != NO_FACTION
        {
            push_faction(&mut player, None);
        }
    }
}

impl SimWorld {
    /// 2525: player `entity` toggles its PvP flag at sim ms `now_ms`.
    pub(super) fn toggle_pvp_flag(&mut self, entity: Entity, now_ms: u64) {
        let end_ns = unix_ns(self.server_ms.saturating_add(PVP_FLAG_DELAY_MS));
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let wait = player.pvp_wait;
        let change = if player.pvp_flagged {
            player.pvp_flagged = false;
            FactionStateChange {
                notify_pending: Some(true),
                pvp_flag: Some(false),
                ..FactionStateChange::default()
            }
        } else if wait.pending {
            player.pvp_wait.pending = false;
            player.pvp_wait.ends_ms = None;
            FactionStateChange {
                pvp_flag_pending: Some(false),
                notify_pending: Some(true),
                pvp_flag_pending_end_ns: wait.ends_ms.map(|_| 0),
                ..FactionStateChange::default()
            }
        } else {
            player.pvp_wait.pending = true;
            let outside = wait.in_sanctuary == Some(false);
            if outside {
                player.pvp_wait.ends_ms = Some(now_ms.saturating_add(PVP_FLAG_DELAY_MS));
            }
            FactionStateChange {
                pvp_flag_pending: Some(true),
                notify_pending: Some(true),
                pvp_flag_pending_end_ns: outside.then_some(end_ns),
                ..FactionStateChange::default()
            }
        };
        push_state(&mut player, &change);
    }

    /// Follows each player's sanctuary and PvP flag countdown at sim ms `now_ms`.
    pub(super) fn tick_pvp_flags(&mut self, now_ms: u64) {
        let end_ns = unix_ns(self.server_ms.saturating_add(PVP_FLAG_DELAY_MS));
        let settlements = &self.content.settlements;
        let mut query = self.world.query::<(&mut Player, &Body)>();
        for (mut player, body) in query.iter_mut(&mut self.world) {
            let at = body.motion.position();
            let inside = settlements.nearest(at.x, at.y).is_some();
            let mut change = FactionStateChange::default();
            let was = player.pvp_wait.in_sanctuary;
            if was != Some(inside) {
                player.pvp_wait.in_sanctuary = Some(inside);
                // A first check outside changes nothing the client shows (its default is 0).
                if was.is_some() || inside {
                    change.has_sanctuary = Some(inside);
                }
                if inside && player.pvp_flagged {
                    player.pvp_flagged = false;
                    player.pvp_wait.pending = true;
                    change.pvp_flag = Some(false);
                    change.pvp_flag_pending = Some(true);
                } else if inside && player.pvp_wait.ends_ms.take().is_some() {
                    change.pvp_flag_pending_end_ns = Some(0);
                } else if !inside && player.pvp_wait.pending {
                    player.pvp_wait.ends_ms = Some(now_ms.saturating_add(PVP_FLAG_DELAY_MS));
                    change.pvp_flag_pending_end_ns = Some(end_ns);
                }
            }
            if player.pvp_wait.pending && player.pvp_wait.ends_ms.is_some_and(|ends| now_ms >= ends)
            {
                player.pvp_wait.pending = false;
                player.pvp_wait.ends_ms = None;
                player.pvp_flagged = true;
                change.pvp_flag_pending = Some(false);
                change.notify_pending = Some(false);
                change.pvp_flag_pending_end_ns = Some(0);
                change.pvp_flag = Some(true);
            }
            if change != FactionStateChange::default() {
                push_state(&mut player, &change);
            }
        }
    }
}

/// Queues faction state change `change` of `player`.
fn push_state(player: &mut Player, change: &FactionStateChange) {
    push_member(player, change.member());
}

/// Queues the encoded faction state member `member` of `player`.
fn push_member(
    player: &mut Player,
    member: Result<nw_messages::BundleMember, nw_messages::wire::MessageError>,
) {
    match member {
        Ok(member) => player.extra_members.push(member),
        Err(error) => tracing::warn!(%error, "faction state not encoded"),
    }
}

/// Queues the faction state change of `player`: its faction, and `switched` (the change's time
/// and the change count) for a change from one faction to another.
fn push_faction(player: &mut Player, switched: Option<(u64, u16)>) {
    let change = FactionChange {
        faction: player.faction.faction,
        switched,
    };
    push_member(player, change.member());
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_content::factions::{COVENANT, NO_FACTION, SYNDICATE};
    use nw_messages::MemberState;
    use nw_messages::faction_state::FACTION_TYPE_INDEX;

    use super::*;
    use crate::objectives::{TaskBook, TaskRow};
    use crate::player_content::{ObjectiveRow, QuestBook};
    use crate::trading_post::{Settlement, Settlements};
    use crate::world::QuestRefusal;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    const PLAYER: u64 = 1;
    /// The Covenant recruitment of build 25810808 (cut): its join task, and a Covenant-only
    /// quest.
    const RECRUITMENT: &str = "FactionIntro_Covenant_Recruitment";
    const COVENANT_ONLY: &str = "Covenant_Story";

    fn world() -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        let task = |id: &str, kind: &str, children: &[&str]| TaskRow {
            task_id: id.to_owned(),
            kind: kind.to_owned(),
            sub_tasks: children.iter().map(|child| (*child).to_owned()).collect(),
            ..TaskRow::default()
        };
        let quests = QuestBook::build(
            [
                ObjectiveRow {
                    id: RECRUITMENT.to_owned(),
                    root: "root".to_owned(),
                    required_faction: Some("!Covenant".to_owned()),
                    ..ObjectiveRow::default()
                },
                ObjectiveRow {
                    id: COVENANT_ONLY.to_owned(),
                    root: "root".to_owned(),
                    required_faction: Some("Covenant".to_owned()),
                    ..ObjectiveRow::default()
                },
            ],
            TaskBook::new(vec![
                task("root", "ConsecutiveTaskContainer", &["join"]),
                TaskRow {
                    item_name: Some("9814_Beyza".to_owned()),
                    ..task("join", "TaskChooseFaction", &[])
                },
            ]),
        );
        Arc::make_mut(&mut world.content).quests = quests;
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.by_connection[&connection(PLAYER)];
        (world, entity)
    }

    /// The faction state bodies queued for the player, and the queue emptied.
    fn faction_bodies(world: &mut SimWorld, entity: Entity) -> Vec<Vec<u8>> {
        let mut player = world.world.get_mut::<Player>(entity).expect("player");
        let bodies = player
            .extra_members
            .iter()
            .filter_map(|member| match &member.state {
                MemberState::Recorded { class, body } if *class == FACTION_TYPE_INDEX => {
                    Some(body.clone())
                }
                _ => None,
            })
            .collect();
        player.extra_members.clear();
        bodies
    }

    #[test]
    fn a_pick_joins_the_faction_ends_the_join_task_and_a_switch_counts() {
        let (mut world, entity) = world();
        assert_eq!(
            world.accept_quest(PLAYER, COVENANT_ONLY, None, NOW),
            Err(QuestRefusal::Faction),
            "not of the Covenant yet"
        );
        world
            .accept_quest(PLAYER, RECRUITMENT, None, NOW)
            .expect("the recruitment");
        faction_bodies(&mut world, entity);
        world.set_faction(entity, PLAYER, NO_FACTION, NOW);
        assert!(
            faction_bodies(&mut world, entity).is_empty(),
            "no faction is not a pick"
        );
        world.set_faction(entity, PLAYER, COVENANT, NOW);
        assert_eq!(
            faction_bodies(&mut world, entity),
            [vec![0x04, 0x01, COVENANT]]
        );
        let recruitment = world
            .quests_of_player(PLAYER)
            .iter()
            .find(|quest| quest.def.id == RECRUITMENT)
            .map(|quest| quest.run.finished());
        assert_ne!(recruitment, Some(false), "the join task is done");
        world
            .accept_quest(PLAYER, COVENANT_ONLY, None, NOW)
            .expect("of the Covenant now");
        world.set_faction(entity, PLAYER, SYNDICATE, NOW);
        let faction = world.world.get::<Player>(entity).expect("player").faction;
        assert_eq!((faction.faction, faction.changes), (SYNDICATE, 1));
        assert_eq!(faction.changed_ns, unix_ns(world.server_ms));
        let bodies = faction_bodies(&mut world, entity);
        assert_eq!(bodies.len(), 1);
        assert_eq!(
            &bodies[0][..2],
            [0x06, 0x18],
            "the time and the count with it"
        );
        assert_eq!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .progression()
                .faction,
            faction,
            "it is saved"
        );
    }

    /// Puts the test player at `x` metres east (the settlement's capital is at the origin).
    fn move_to(world: &mut SimWorld, entity: Entity, x: f32) {
        let limits = world.settings.limits;
        world.world.get_mut::<Body>(entity).expect("body").motion =
            nw_sim_core::Motion::new(nw_sim_core::Position { x, y: 0.0, z: 50.0 }, NOW, &limits);
    }

    fn flagged(world: &SimWorld, entity: Entity) -> bool {
        world
            .world
            .get::<Player>(entity)
            .expect("player")
            .pvp_flagged
    }

    /// Retail `Hive_flag_pvp_on_and_off`: a toggle in a sanctuary waits; leaving it starts the
    /// countdown; the flag (and combat's view of it) is on only at its end; entering a
    /// sanctuary takes it off and makes it pending; a toggle then cancels it.
    #[test]
    fn the_pvp_flag_waits_out_the_sanctuary_and_its_countdown() {
        const OUTSIDE_M: f32 = 1_000.0;
        let (mut world, entity) = world();
        Arc::make_mut(&mut world.content).settlements = Settlements::of(vec![Settlement {
            id: 1,
            x: 0.0,
            y: 0.0,
        }]);
        move_to(&mut world, entity, 0.0);
        faction_bodies(&mut world, entity);
        world.tick_pvp_flags(NOW);
        assert_eq!(faction_bodies(&mut world, entity), [vec![0x04, 0x04, 0x01]]);
        world.toggle_pvp_flag(entity, NOW);
        assert_eq!(
            faction_bodies(&mut world, entity),
            [vec![0x02, 0x03, 0x01, 0x01]]
        );
        world.tick_pvp_flags(NOW + PVP_FLAG_DELAY_MS * 2);
        assert!(!flagged(&world, entity), "it waits in the sanctuary");
        move_to(&mut world, entity, OUTSIDE_M);
        let left_ms = NOW + PVP_FLAG_DELAY_MS * 2;
        world.tick_pvp_flags(left_ms);
        let end_ns = unix_ns(world.server_ms + PVP_FLAG_DELAY_MS);
        let mut left = vec![0x06, 0x04];
        left.extend(end_ns.to_be_bytes());
        left.extend([0x04, 0x00]);
        assert_eq!(faction_bodies(&mut world, entity), [left]);
        world.tick_pvp_flags(left_ms + PVP_FLAG_DELAY_MS - 1);
        assert!(!flagged(&world, entity), "the countdown runs");
        world.tick_pvp_flags(left_ms + PVP_FLAG_DELAY_MS);
        assert!(flagged(&world, entity), "on at the end");
        assert_eq!(
            faction_bodies(&mut world, entity),
            [vec![0x06, 0x07, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x02, 0x01]]
        );
        move_to(&mut world, entity, 0.0);
        world.tick_pvp_flags(left_ms + PVP_FLAG_DELAY_MS + 1);
        assert!(!flagged(&world, entity), "a sanctuary takes it off");
        assert_eq!(
            faction_bodies(&mut world, entity),
            [vec![0x06, 0x01, 0x01, 0x06, 0x00, 0x01]]
        );
        world.toggle_pvp_flag(entity, NOW);
        assert_eq!(
            faction_bodies(&mut world, entity),
            [vec![0x02, 0x03, 0x00, 0x01]]
        );
        move_to(&mut world, entity, OUTSIDE_M);
        world.tick_pvp_flags(left_ms * 2);
        world.tick_pvp_flags(left_ms * 4);
        assert!(!flagged(&world, entity), "a cancelled flag stays off");
    }
}
