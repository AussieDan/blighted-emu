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
//! Death's door ([`nw_content::deaths_door`]): a lethal hit puts the player down at 0 health
//! first when the rules allow it; a revive brings a downed or a dead player back, a hit after
//! the door's delay kills a downed one ([`SimWorld::apply_hit`]), and so does the end of the
//! door's time ([`SimWorld::tick_deaths_door`]). A dead player stays dead until a revive or its
//! respawn request (retail sent the respawn request only after the dead block; Inferred: a
//! downed player cannot ask for it).

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
        if !(vitals.fallen() && vitals.health.is_dead()) {
            return Err(ReviveRefusal::NotDead);
        }
        vitals.health.heal(REVIVED_HEALTH);
        vitals.rise();
        self.mobs.forget(target);
        Ok(target)
    }
}

impl Vitals {
    /// It is back in life, from death or from death's door (a revive, a GM heal): it is not dead
    /// and its death's door ends, so the door's end kills it no more. Its health is the caller's;
    /// the next own state sends the alive vitals (health above 0 after 0).
    pub(super) const fn rise(&mut self) {
        self.dead_since_ms = None;
        self.deaths_door_since_ms = None;
    }

    /// Whether it is dead or down at death's door.
    pub(super) const fn fallen(&self) -> bool {
        self.dead_since_ms.is_some() || self.downed()
    }
}

impl SimWorld {
    /// Kills each downed player whose death's door time ended at `now_ms` (retail: `b` about
    /// 120,033 ms in the dead block after the full time).
    pub(super) fn tick_deaths_door(&mut self, now_ms: u64) {
        let time_ms = self.content.deaths_door.time_ms;
        let mut ended = Vec::new();
        let mut query = self.world.query::<(Entity, &Player, &mut Vitals)>();
        for (entity, player, mut vitals) in query.iter_mut(&mut self.world) {
            if let Some(since) = vitals.deaths_door_since_ms
                && now_ms.saturating_sub(since) >= time_ms
            {
                vitals.close_deaths_door(now_ms.saturating_sub(since), now_ms);
                ended.push((entity, player.number, player.connection.clone()));
            }
        }
        for (entity, number, connection) in ended {
            tracing::debug!(player = number, "death's door ended");
            self.player_died(entity, number, number, connection);
        }
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

    /// Retail's death's door: 120 s, 1 s of delay (`VitalsBaseData` `Player`).
    const DOOR_MS: u64 = 120_000;
    const DELAY_MS: u64 = 1_000;
    const NOW: u64 = 10_000;

    fn door_world() -> SimWorld {
        let mut world = world();
        std::sync::Arc::make_mut(&mut world.content).deaths_door =
            nw_content::deaths_door::DeathsDoorRules::new((DOOR_MS, DELAY_MS), &[], Some(10));
        join(&mut world, REVIVER, 0.0);
        join(&mut world, DEAD, 2.0);
        world
    }

    fn vitals_of(world: &mut SimWorld, number: u64) -> Vitals {
        let entity = world.player_by_number(number).expect("player");
        *world.world.get::<Vitals>(entity).expect("vitals")
    }

    #[test]
    fn a_lethal_hit_downs_the_player_and_a_hit_after_the_delay_kills_it() {
        let mut world = door_world();
        assert!(matches!(
            world.apply_hit(DEAD, REVIVER, LETHAL, 0, NOW),
            Some(crate::combat::AttackOutcome::Hit { .. })
        ));
        let downed = vitals_of(&mut world, DEAD);
        assert!(
            downed.downed() && downed.dead_since_ms.is_none(),
            "down, not dead"
        );
        assert_eq!(health(&mut world, DEAD), 0);
        assert_eq!(
            world.apply_hit(DEAD, REVIVER, 1, 0, NOW + DELAY_MS - 1),
            None,
            "the delay"
        );
        assert!(matches!(
            world.apply_hit(DEAD, REVIVER, 1, 0, NOW + 2_430),
            Some(crate::combat::AttackOutcome::Killed { .. })
        ));
        let dead = vitals_of(&mut world, DEAD);
        assert_eq!(
            (dead.downed(), dead.dead_since_ms, dead.deaths_door_ms),
            (false, Some(NOW + 2_430), Some(2_430)),
            "retail NH-MSQ-5.1: b 2430"
        );
    }

    #[test]
    fn a_downed_player_dies_at_the_end_of_the_door_or_is_revived() {
        let mut world = door_world();
        world.apply_hit(DEAD, REVIVER, LETHAL, 0, NOW);
        world.tick_deaths_door(NOW + DOOR_MS - 1);
        assert!(vitals_of(&mut world, DEAD).downed());
        world.tick_deaths_door(NOW + DOOR_MS);
        assert_eq!(vitals_of(&mut world, DEAD).deaths_door_ms, Some(DOOR_MS));
        // A revive brings a downed player back too.
        let mut world = door_world();
        world.apply_hit(DEAD, REVIVER, LETHAL, 0, NOW);
        let reviver = world.player_by_number(REVIVER).expect("reviver");
        assert_eq!(world.revive(reviver, &revive_of(DEAD)), Ok(DEAD));
        let revived = vitals_of(&mut world, DEAD);
        assert!(!revived.downed());
        assert_eq!(revived.health.current(), REVIVED_HEALTH);
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
