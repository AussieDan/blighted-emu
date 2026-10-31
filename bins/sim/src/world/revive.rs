// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player revives a dead player (interaction kind 30, `RevivePlayer`; run-15 revive study,
//! `local-data/scratch-work/run15/revive/`).
//!
//! Retail (the 2026-10-02 expedition run, 43 revives; two gorgon raid runs): the reviver's client
//! holds the revive itself for about 3 s and only then asks for the interaction; the server
//! accepts it (3743, status 1) and ends it 17 ms later (3746); the client cancels (2473). The
//! dead player comes back where it lies, at 1 health (4 of 4 revived vitals: `hp` 1.0 with the
//! alive `vitalsData` head), and its health regenerates from there.
//!
//! Death's door is not built: the expedition run had 2,659 dead `vitalsData` heads against 6
//! death's door heads, and none of the 682 deaths of a war and two free-for-all runs went
//! through it, but 107 of 281 own-player deaths of 75 community ledgers (older builds) did (head
//! 2 before head 1; open-world quests and catacombs, never an outpost rush, a capture-the-flag, a
//! 3v3 or an invasion; status effects study 2026-10-31). Which deaths take it is Unknown. A dead
//! player stays dead until a revive or its respawn request.

use bevy_ecs::entity::Entity;
use nw_messages::{InteractionEndedMsg, InteractionRequest, ServerMessage};

use super::attack::Vitals;
use super::{Body, Player, SimWorld, gather_answer, send_now};

/// The health a revived player comes back with.
pub(super) const REVIVED_HEALTH: u32 = 1;
/// How far the reviver may stand from the dead player, m (the player slice's revive interaction
/// option `RevivePlayerOption`: its interactable carries 25.0; best guess: that is its reach).
const REVIVE_REACH_M: f32 = 25.0;
/// The 3743 status of a taken revive.
const REVIVE_ACCEPTED: u8 = 1;
/// The 3743 status of a refused one (the refusal of the other interactions).
const REVIVE_REFUSED: u8 = 2;

/// Why a revive is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ReviveRefusal {
    /// The target is no player of this sim.
    NoTarget,
    /// The target is not dead.
    NotDead,
    /// The reviver is dead.
    ReviverDead,
    /// The target is out of the reach.
    TooFar,
}

impl SimWorld {
    /// Player `number` (`entity`) revives the dead player its interaction `request` names: the
    /// accept and the end of the interaction to the reviver, and the dead player alive at
    /// [`REVIVED_HEALTH`]; a refusal gets the refused status alone.
    pub(super) fn revive_player(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
    ) {
        let Some(connection) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.connection.clone())
        else {
            return;
        };
        let answers = match self.revive(entity, &request) {
            Ok(target) => {
                tracing::debug!(player = number, target, "player revived");
                vec![
                    gather_answer(number, request, REVIVE_ACCEPTED, name),
                    ServerMessage::InteractionEnded(InteractionEndedMsg {
                        header: nw_messages::FacetHeader::of(
                            nw_messages::facet::component::INTERACTOR,
                            number,
                        ),
                    }),
                ]
            }
            Err(refusal) => {
                tracing::debug!(player = number, ?refusal, "revive refused");
                vec![gather_answer(number, request, REVIVE_REFUSED, name)]
            }
        };
        send_now(&mut self.combat_messages, &connection, &answers);
    }

    /// Brings the dead player `request` names back to life at [`REVIVED_HEALTH`] for the reviver
    /// `entity`: its number, or why not.
    fn revive(
        &mut self,
        entity: Entity,
        request: &InteractionRequest,
    ) -> Result<u64, ReviveRefusal> {
        let target = nw_messages::entity_of_raw_id(&request.target)
            .filter(|target| self.player_by_number(*target).is_some())
            .ok_or(ReviveRefusal::NoTarget)?;
        let target_entity = self
            .player_by_number(target)
            .ok_or(ReviveRefusal::NoTarget)?;
        if target_entity == entity {
            return Err(ReviveRefusal::NoTarget);
        }
        if self
            .world
            .get::<Vitals>(entity)
            .is_none_or(|vitals| vitals.health.is_dead())
        {
            return Err(ReviveRefusal::ReviverDead);
        }
        let place = |entity| {
            self.world
                .get::<Body>(entity)
                .map(|body| body.motion.position())
        };
        let (Some(from), Some(at)) = (place(entity), place(target_entity)) else {
            return Err(ReviveRefusal::NoTarget);
        };
        if from.horizontal_distance(at) > REVIVE_REACH_M {
            return Err(ReviveRefusal::TooFar);
        }
        let mut vitals = self
            .world
            .get_mut::<Vitals>(target_entity)
            .ok_or(ReviveRefusal::NoTarget)?;
        if vitals.dead_since_ms.is_none() || !vitals.health.is_dead() {
            return Err(ReviveRefusal::NotDead);
        }
        vitals.health.heal(REVIVED_HEALTH);
        vitals.dead_since_ms = None;
        self.mobs.forget(target);
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::client_input::REVIVE_PLAYER_KIND;

    use super::*;
    use crate::world::tests::{connection, join, world};

    /// A lethal hit.
    const LETHAL: u32 = 100_000;
    /// The reviver and the dead player.
    const REVIVER: u64 = 1;
    const DEAD: u64 = 2;
    /// The interaction id of the revive.
    const INTERACTION: u64 = 7;

    fn revive_of(target: u64) -> InteractionRequest {
        InteractionRequest {
            target: nw_messages::raw_id_of_entity(target),
            target_entity: target,
            interaction: INTERACTION,
            kind: REVIVE_PLAYER_KIND,
            gatherable: None,
        }
    }

    fn health(world: &mut SimWorld, number: u64) -> u32 {
        let entity = world.player_by_number(number).expect("player");
        world
            .world
            .get::<Vitals>(entity)
            .expect("vitals")
            .health
            .current()
    }

    #[test]
    fn a_dead_player_comes_back_at_one_health_and_the_reviver_gets_the_accept_and_the_end() {
        let mut world = world();
        join(&mut world, REVIVER, 0.0);
        join(&mut world, DEAD, 2.0);
        let reviver = world.player_by_number(REVIVER).expect("reviver");
        assert_eq!(
            world.revive(reviver, &revive_of(DEAD)),
            Err(ReviveRefusal::NotDead)
        );
        world.apply_hit(DEAD, REVIVER, LETHAL, 0, 0);
        assert_eq!(health(&mut world, DEAD), 0);
        let _ = world.take_messages();
        world.revive_player(reviver, REVIVER, revive_of(DEAD), &[]);
        assert_eq!(health(&mut world, DEAD), REVIVED_HEALTH);
        let to_reviver = world
            .take_messages()
            .into_iter()
            .filter(|out| out.connection == connection(REVIVER))
            .flat_map(|out| out.envelopes)
            .count();
        assert_eq!(to_reviver, 2, "the accept and the end");
        assert_eq!(
            world.revive(reviver, &revive_of(DEAD)),
            Err(ReviveRefusal::NotDead)
        );
    }

    #[test]
    fn a_far_or_unknown_target_is_refused() {
        let mut world = world();
        join(&mut world, REVIVER, 0.0);
        join(&mut world, DEAD, REVIVE_REACH_M + 5.0);
        let reviver = world.player_by_number(REVIVER).expect("reviver");
        world.apply_hit(DEAD, REVIVER, LETHAL, 0, 0);
        assert_eq!(
            world.revive(reviver, &revive_of(DEAD)),
            Err(ReviveRefusal::TooFar)
        );
        assert_eq!(
            world.revive(reviver, &revive_of(REVIVER)),
            Err(ReviveRefusal::NoTarget)
        );
        assert_eq!(health(&mut world, DEAD), 0);
    }
}
