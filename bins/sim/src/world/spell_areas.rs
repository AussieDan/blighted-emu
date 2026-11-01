// SPDX-License-Identifier: AGPL-3.0-or-later
//! Spell areas of player abilities (abilities design note, 2026-10-31; the `SpellData` rows of
//! build 25810808). An area spell stays where it was cast while its `Duration` lasts (Sacred
//! Ground 12 s, Ice Storm 10 s, Meteor Storm 6 s); a `Duration` of -1 or 0 is one pass. Each tick
//! the targets in its radius hold its status effects: with `ClearOnExit` a target that leaves
//! loses them, with `ClearOnEnd` every target in it loses them when it ends. Its damage row hits
//! each enemy once, when it first stands in it (Inferred: a lasting area's repeated damage is its
//! status effects' own ticks, as `Status_FireStaff_MeteorShower_Damage` on Meteor Storm).

use bevy_ecs::entity::Entity;
use nw_bus::ConnectionRef;
use nw_content::player_abilities::SpellRow;
use nw_sim_core::Position;

use super::SimWorld;
use super::abilities::{Caster, ENEMY_TARGETS, FRIEND_TARGETS, SELF_TARGET};
use crate::combat::{AttackOutcome, AttackRefusal};

/// The radius of a spell row without one: its target only (Inferred: a point spell hits what is
/// on the point).
const POINT_RADIUS_M: f32 = 1.0;

/// A spell area of a player.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct SpellArea {
    caster: ConnectionRef,
    caster_entity: Entity,
    /// The caster's number, the source of the effects.
    source: u64,
    spell: SpellRow,
    centre: Position,
    /// When it opens: at the cast, or when the projectile that casts it gets there.
    starts_ms: u64,
    /// When it ends; its start for one pass.
    ends_ms: u64,
    /// The mobs its damage row hit.
    hit: Vec<u64>,
    /// The mobs that hold its effects.
    mobs_in: Vec<u64>,
    /// The players that hold its effects.
    players_in: Vec<Entity>,
}

impl SpellArea {
    /// The area of `spell` of `caster` at `centre`, from `starts_ms`.
    pub(super) fn new(caster: &Caster, spell: &SpellRow, centre: Position, starts_ms: u64) -> Self {
        Self {
            caster: caster.connection.clone(),
            caster_entity: caster.entity,
            source: caster.number,
            spell: spell.clone(),
            centre,
            starts_ms,
            ends_ms: starts_ms.saturating_add(spell.lasting_ms().unwrap_or_default()),
            hit: Vec::new(),
            mobs_in: Vec::new(),
            players_in: Vec::new(),
        }
    }
}

impl SimWorld {
    /// Opens `area`; it takes effect from its start ([`SimWorld::tick_spell_areas`]).
    pub(super) fn open_spell_area(&mut self, area: SpellArea) {
        tracing::debug!(
            source = area.source,
            spell = %area.spell.id,
            x = area.centre.x,
            y = area.centre.y,
            starts_ms = area.starts_ms,
            ends_ms = area.ends_ms,
            "ability spell area"
        );
        self.spell_areas.push(area);
    }

    /// Runs the spell areas open at `now_ms` and closes the ones that end; gives their hits.
    pub(super) fn tick_spell_areas(
        &mut self,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let mut results = Vec::new();
        let mut open = Vec::new();
        for mut area in std::mem::take(&mut self.spell_areas) {
            if now_ms < area.starts_ms {
                open.push(area);
                continue;
            }
            results.extend(self.area_pass(&mut area, now_ms));
            if now_ms < area.ends_ms {
                open.push(area);
            } else if area.spell.lasting_ms().is_some() && area.spell.clear_on_end {
                self.clear_area(&area, &area.mobs_in, &area.players_in);
            }
        }
        self.spell_areas = open;
        results
    }

    /// One pass of `area` at `now_ms`: the enemies first in it take its damage row; the targets
    /// in it hold its effects, and with `ClearOnExit` the ones that left lose them.
    fn area_pass(
        &mut self,
        area: &mut SpellArea,
        now_ms: u64,
    ) -> Vec<Result<AttackOutcome, AttackRefusal>> {
        let radius = area.spell.radius_m.unwrap_or(POINT_RADIUS_M);
        let targets = area.spell.target_types.clone().unwrap_or_default();
        let effects = area.spell.status_effects.clone();
        let mut results = Vec::new();
        if ENEMY_TARGETS.contains(&targets.as_str()) {
            let inside: Vec<u64> = self
                .mob_states()
                .filter(|(_, at, state)| {
                    state.health > 0 && at.horizontal_distance(area.centre) <= radius
                })
                .map(|(mob, _, _)| mob)
                .collect();
            for mob in &inside {
                let first = !area.hit.contains(mob);
                if first {
                    area.hit.push(*mob);
                    let hurt = match area.spell.damage_row.as_deref() {
                        Some(row) => {
                            let result =
                                self.hit_paced(&area.caster, *mob, row, None, false, now_ms);
                            let hurt = matches!(result, Ok(AttackOutcome::Hit { .. }));
                            results.push(result);
                            hurt
                        }
                        None => true,
                    };
                    if hurt && !effects.is_empty() {
                        area.mobs_in.push(*mob);
                    }
                }
                if area.mobs_in.contains(mob) {
                    self.spell_effects_on_mob(*mob, &effects, area.source, !first);
                }
            }
            if area.spell.clear_on_exit {
                let left: Vec<u64> = area
                    .mobs_in
                    .iter()
                    .copied()
                    .filter(|mob| !inside.contains(mob))
                    .collect();
                self.clear_area(area, &left, &[]);
                area.mobs_in.retain(|mob| inside.contains(mob));
            }
        }
        if !effects.is_empty() && FRIEND_TARGETS.contains(&targets.as_str()) {
            let inside = if targets == SELF_TARGET {
                vec![area.caster_entity]
            } else {
                self.players_within(area.centre, radius)
            };
            for player in &inside {
                let held = area.players_in.contains(player);
                if !held {
                    area.players_in.push(*player);
                }
                self.spell_effects_on_player(*player, &effects, area.source, held);
            }
            if area.spell.clear_on_exit {
                let left: Vec<Entity> = area
                    .players_in
                    .iter()
                    .copied()
                    .filter(|player| !inside.contains(player))
                    .collect();
                self.clear_area(area, &[], &left);
                area.players_in.retain(|player| inside.contains(player));
            }
        }
        results
    }

    /// Takes the status effects of `area` off `mobs` and `players`.
    fn clear_area(&mut self, area: &SpellArea, mobs: &[u64], players: &[Entity]) {
        for mob in mobs {
            self.clear_spell_effects_on_mob(*mob, &area.spell.status_effects);
        }
        for player in players {
            self.clear_spell_effects_on_player(*player, &area.spell.status_effects);
        }
    }
}
