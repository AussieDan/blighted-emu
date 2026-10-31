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

use bevy_ecs::entity::Entity;
use nw_content::factions::{NO_FACTION, joinable};
use nw_messages::faction_state::FactionChange;

use super::gear::unix_ns;
use super::{Player, SimWorld};
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

/// Queues the faction state change of `player`: its faction, and `switched` (the change's time
/// and the change count) for a change from one faction to another.
fn push_faction(player: &mut Player, switched: Option<(u64, u16)>) {
    let change = FactionChange {
        faction: player.faction.faction,
        switched,
    };
    match change.member() {
        Ok(member) => player.extra_members.push(member),
        Err(error) => tracing::warn!(%error, "faction state not encoded"),
    }
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
}
