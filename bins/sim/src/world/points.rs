// SPDX-License-Identifier: AGPL-3.0-or-later
//! A player's attribute points and the client's point calls (progression note, sections 6 and
//! 9): spending attribute points (1502) and refunding them (2409), assigning weapon points
//! (1112), applying them (2857), mapping an ability into a slot (2701) and resetting a weapon
//! tree (186, answered by 2858).
//!
//! The attribute points of a character are the `AttributePoints` of every level up to its own,
//! less the spent ones. The client keeps them in its attribute state (type 129): whole at login
//! and after a spend, the points alone at a level up (retail newchar 2026-09-18).

use nw_bus::ConnectionRef;
use nw_content::character_points::{ATTRIBUTE_COUNT, CONSTITUTION, CharacterPoints};
use nw_content::progression::Progression;
use nw_messages::items::FIRST_VERSION;
use nw_messages::{
    AttributeState, AttributeValue, MasteryTreeRespecResponseMsg, PersistentAttributes,
    PointsClientMsg, ServerMessage,
};
use nw_sim_core::player_max_health;

use super::attack::Vitals;
use super::{Player, SimWorld, send_now};

/// The `m_chosenMagnifyBehavior` retail sends (5 for the new character of newchar 2026-09-18
/// and the level 66 character of NH-MSQ-2.1; the client sets it with 6397, which this server
/// does not take; meaning Unknown).
const RETAIL_MAGNIFY_BEHAVIOR: u32 = 5;
/// The placing bonus of each attribute: retail sends 0 for all five in every capture (meaning
/// Unknown).
const RETAIL_PLACING_BONUS: u32 = 0;

/// Why a spend of attribute points (1502) is refused; the state stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpendRefusal {
    /// The two lists differ in length, or an attribute comes twice.
    BadLists,
    /// An attribute the client does not have, or the content has no attribute tables.
    UnknownAttribute,
    /// More points than the character has unspent.
    NotEnoughPoints,
    /// An attribute would go above its largest value.
    AboveMax,
}

/// Why a refund of the spent attribute points (2409) is refused; the state stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RefundRefusal {
    /// No point is spent.
    NothingSpent,
    /// The player has fewer coins than the level's `AttributeRespecCost`.
    NotEnoughCoins,
}

/// A player's attribute points.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Attributes {
    /// The points spent on each attribute, in the order first spent.
    spent: Vec<AttributeValue>,
    /// The version of the newest bonus lists the client has.
    version: u64,
    /// The level whose points the client has; `None` before the first card.
    level_sent: Option<u32>,
    /// Whether a spend changed the values since the last bundle.
    spent_changed: bool,
}

impl Attributes {
    /// The attributes of `spent` (attribute id, points, in the order first spent); the client's
    /// lists go on above `version` (0 for none).
    pub(super) fn of(spent: &[AttributeValue], version: u64) -> Self {
        Self {
            spent: spent.to_vec(),
            version,
            ..Self::default()
        }
    }

    /// The spent points, as a save or a handoff carries them.
    pub(super) fn carried(&self) -> Vec<AttributeValue> {
        self.spent.clone()
    }

    /// The version of the newest bonus lists the client has, for the next sim.
    pub(super) fn version(&self) -> u64 {
        self.version
    }

    /// Spends `points[i]` on `attributes[i]` for a character of `level`, as `rules` allow.
    pub(super) fn spend(
        &mut self,
        rules: &CharacterPoints,
        level: u32,
        attributes: &[u32],
        points: &[u32],
    ) -> Result<(), SpendRefusal> {
        let distinct = attributes
            .iter()
            .enumerate()
            .all(|(index, id)| !attributes[..index].contains(id));
        if attributes.len() != points.len() || !distinct {
            return Err(SpendRefusal::BadLists);
        }
        let total = points
            .iter()
            .try_fold(0_u32, |sum, points| sum.checked_add(*points))
            .ok_or(SpendRefusal::NotEnoughPoints)?;
        if total > self.unspent(rules, level) {
            return Err(SpendRefusal::NotEnoughPoints);
        }
        for (id, points) in attributes.iter().zip(points) {
            let range = rules.attribute(*id).ok_or(SpendRefusal::UnknownAttribute)?;
            let value = range
                .base
                .saturating_add(self.spent_on(*id))
                .saturating_add(*points);
            if value > range.max {
                return Err(SpendRefusal::AboveMax);
            }
        }
        // A zero entry changes nothing (retail's third spend sent Strength 0 with Constitution
        // 5; Strength stayed as it was).
        for (id, points) in attributes
            .iter()
            .zip(points)
            .filter(|(_, points)| **points > 0)
        {
            match self.spent.iter_mut().find(|(spent, _)| spent == id) {
                Some((_, spent)) => *spent += points,
                None => self.spent.push((*id, *points)),
            }
        }
        self.spent_changed = true;
        Ok(())
    }

    /// Refunds every spent point (2409): the next state goes out whole (retail: all values back
    /// to the base, no spent entry, every point unspent).
    pub(super) fn refund(&mut self) -> Result<(), RefundRefusal> {
        if self.spent.iter().all(|(_, points)| *points == 0) {
            return Err(RefundRefusal::NothingSpent);
        }
        self.spent.clear();
        self.spent_changed = true;
        Ok(())
    }

    /// The state for the client of a character of `level`: whole with the first card and
    /// after a spend, the points alone after a level change; `None` without a change.
    pub(super) fn take_state(
        &mut self,
        rules: &CharacterPoints,
        level: u32,
        first: bool,
    ) -> Option<AttributeState> {
        // Before the first card the client has no state to change: the card sends it whole.
        if !first && self.level_sent.is_none() {
            return None;
        }
        let whole = first || std::mem::take(&mut self.spent_changed);
        if !whole && self.level_sent == Some(level) {
            return None;
        }
        self.version = if first && self.version < FIRST_VERSION {
            FIRST_VERSION
        } else {
            self.version.saturating_add(1)
        };
        self.level_sent = Some(level);
        let placing = vec![RETAIL_PLACING_BONUS; ATTRIBUTE_COUNT];
        let persistent = PersistentAttributes {
            unspent: self.unspent(rules, level),
            ignore_backstory_points: false,
            has_respec: false,
            spent: self.spent.clone(),
            magnify_behavior: RETAIL_MAGNIFY_BEHAVIOR,
        };
        Some(if whole {
            AttributeState::full(self.version, self.values(rules), placing, persistent)
        } else {
            AttributeState::points(self.version, placing, persistent)
        })
    }

    /// The final value of attribute `id`: its base and its spent points; 0 for an id the rules
    /// do not have.
    pub(super) fn value(&self, rules: &CharacterPoints, id: u32) -> u32 {
        rules
            .attribute(id)
            .map_or(0, |range| range.base.saturating_add(self.spent_on(id)))
    }

    fn spent_on(&self, id: u32) -> u32 {
        self.spent
            .iter()
            .find(|(spent, _)| *spent == id)
            .map_or(0, |(_, points)| *points)
    }

    fn unspent(&self, rules: &CharacterPoints, level: u32) -> u32 {
        let spent = self
            .spent
            .iter()
            .fold(0_u32, |sum, (_, points)| sum.saturating_add(*points));
        rules.attribute_points_at(level).saturating_sub(spent)
    }

    /// The final value of each attribute, highest id first (retail order; a saved loadout keeps
    /// them so, retail `Hiveloadouts`).
    pub(super) fn values(&self, rules: &CharacterPoints) -> Vec<AttributeValue> {
        (0..ATTRIBUTE_COUNT)
            .rev()
            .filter_map(|index| {
                let id = u32::try_from(index).ok()?;
                rules.attribute(id)?;
                Some((id, self.value(rules, id)))
            })
            .collect()
    }
}

/// The max health of a player of `experience` with `attributes` and the base health `base`.
fn max_health(
    progression: &Progression,
    base: u32,
    experience: u32,
    attributes: &Attributes,
) -> u32 {
    let level = progression.level_of(experience);
    let constitution = attributes.value(&progression.points, CONSTITUTION);
    player_max_health(
        base,
        level,
        progression.points.constitution_health(constitution),
    )
}

impl SimWorld {
    /// The max health of a player of `experience` with the spent attribute points `spent`.
    pub(super) fn player_max_health(&self, experience: u32, spent: &[AttributeValue]) -> u32 {
        max_health(
            &self.content.progression,
            self.combat.data.player_base_health(),
            experience,
            &Attributes::of(spent, FIRST_VERSION),
        )
    }

    /// Moves each player's max health to its level and Constitution: after a level up, a spend
    /// and a refund (the vitals change goes with the next bundle).
    pub(super) fn tick_max_health(&mut self) {
        let content = std::sync::Arc::clone(&self.content);
        let base = self.combat.data.player_base_health();
        let mut query = self.world.query::<(&Player, &mut Vitals)>();
        for (player, mut vitals) in query.iter_mut(&mut self.world) {
            let max = max_health(
                &content.progression,
                base,
                player.experience,
                &player.attributes,
            );
            if vitals.health.max() != max {
                vitals.health.set_max(max);
            }
        }
    }

    /// Carries out the point call `call` of the client of `connection`.
    pub fn points_call(&mut self, connection: &ConnectionRef, call: PointsClientMsg) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let progression = &self.content.progression;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let player = &mut *player;
        let call_type = call.type_index();
        let refusal = match call {
            PointsClientMsg::SpendAttributePoints {
                attributes, points, ..
            } => {
                let level = progression.level_of(player.experience);
                player
                    .attributes
                    .spend(&progression.points, level, &attributes, &points)
                    .map_err(|refusal| format!("{refusal:?}"))
            }
            PointsClientMsg::AssignPoolPoints {
                pool,
                point,
                points,
                ..
            } => player
                .masteries
                .assign(progression, pool, point, points)
                .map_err(|refusal| format!("{refusal:?}")),
            PointsClientMsg::SpendTerritoryStandingPoint { pool, point, .. } => {
                player.masteries.spend_territory_point(pool, point);
                Ok(())
            }
            PointsClientMsg::ApplyAbilityPoints { .. } => {
                player.masteries.apply();
                Ok(())
            }
            PointsClientMsg::RefundSpentPoints { .. } => {
                let level = progression.level_of(player.experience);
                let cost = progression.points.attribute_respec_cost(level);
                if player.coins < cost {
                    Err(format!("{:?}", RefundRefusal::NotEnoughCoins))
                } else {
                    player.attributes.refund().map_or_else(
                        |refusal| Err(format!("{refusal:?}")),
                        |()| {
                            if cost > 0 {
                                player.coins -= cost;
                                player.coins_changed = true;
                            }
                            Ok(())
                        },
                    )
                }
            }
            PointsClientMsg::ChangeMappedAbility {
                table,
                slot,
                ability,
                ..
            } => player
                .masteries
                .change_mapped(progression, table, slot, ability)
                .map_err(|refusal| format!("{refusal:?}")),
            PointsClientMsg::MasteryTreeRespec { header, table, .. } => {
                let done = player.masteries.respec(progression, table);
                let answer = MasteryTreeRespecResponseMsg {
                    header,
                    done: done.is_ok(),
                    table: u32::from(table),
                };
                send_now(
                    &mut self.combat_messages,
                    connection,
                    &[ServerMessage::MasteryTreeRespecResponse(answer)],
                );
                done.map(|_| ()).map_err(|refusal| format!("{refusal:?}"))
            }
        };
        // The next save round finds the change: the save compares the whole progression.
        if let Err(reason) = refusal {
            tracing::info!(entity = player.number, call_type, %reason, "point call refused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nw_content::character_points::PointRow;

    /// Strength (client enum `CharacterAttributeType`).
    const STRENGTH: u32 = 2;
    /// Retail `AttributePoints` of levels 0 to 3.
    const LEVEL_POINTS: [i64; 4] = [0, 5, 5, 5];
    const BASE: i64 = 5;
    const MAX: i64 = 500;
    /// The retail first level up: level 0 to 2.
    const LEVEL_TWO: u32 = 2;

    fn rules() -> CharacterPoints {
        rules_with(&LEVEL_POINTS)
    }

    /// The rules with `level_points` as the `AttributePoints` of levels 0 up.
    fn rules_with(level_points: &[i64]) -> CharacterPoints {
        let attribute_levels: Vec<(&str, i64)> = nw_content::character_points::ATTRIBUTE_NAMES
            .iter()
            .flat_map(|name| [(*name, BASE), (*name, MAX)])
            .collect();
        CharacterPoints::build(
            (0..).zip(level_points.iter().copied()).collect::<Vec<_>>(),
            attribute_levels,
            [],
            [],
            [],
            std::iter::empty::<PointRow<'_>>(),
        )
    }

    #[test]
    fn the_login_and_a_level_up_send_the_retail_points() {
        let rules = rules();
        let mut attributes = Attributes::of(&[], 0);
        assert_eq!(
            attributes.take_state(&rules, LEVEL_TWO, false),
            None,
            "nothing before the card"
        );
        let login = attributes.take_state(&rules, 0, true).expect("login");
        assert_eq!(
            login,
            AttributeState::full(
                FIRST_VERSION,
                vec![(4, 5), (3, 5), (2, 5), (1, 5), (0, 5)],
                vec![0; ATTRIBUTE_COUNT],
                PersistentAttributes {
                    magnify_behavior: RETAIL_MAGNIFY_BEHAVIOR,
                    ..PersistentAttributes::default()
                },
            )
        );
        assert_eq!(attributes.take_state(&rules, 0, false), None);
        // Retail: 10 unspent points after the gain from level 0 to 2, children 1 to 3.
        let level_up = attributes
            .take_state(&rules, LEVEL_TWO, false)
            .expect("level up");
        assert_eq!(
            level_up,
            AttributeState::points(
                FIRST_VERSION + 1,
                vec![0; ATTRIBUTE_COUNT],
                PersistentAttributes {
                    unspent: 10,
                    magnify_behavior: RETAIL_MAGNIFY_BEHAVIOR,
                    ..PersistentAttributes::default()
                },
            )
        );
    }

    #[test]
    fn a_spend_sends_the_whole_state_and_keeps_the_first_spent_order() {
        let rules = rules();
        let mut attributes = Attributes::of(&[], 0);
        attributes.take_state(&rules, 3, true);
        // Retail: Strength 10, then Constitution 5 with Strength 0.
        attributes
            .spend(&rules, 3, &[STRENGTH], &[10])
            .expect("strength");
        attributes
            .spend(&rules, 3, &[CONSTITUTION, STRENGTH], &[5, 0])
            .expect("constitution");
        let state = attributes.take_state(&rules, 3, false).expect("spent");
        assert_eq!(
            state.attributes,
            Some(vec![(4, 5), (3, 5), (2, 15), (1, 5), (0, 10)])
        );
        let persistent = state.persistent.expect("points");
        assert_eq!(persistent.unspent, 0);
        assert_eq!(persistent.spent, vec![(STRENGTH, 10), (CONSTITUTION, 5)]);
        assert_eq!(
            attributes.carried(),
            vec![(STRENGTH, 10), (CONSTITUTION, 5)]
        );
        assert_eq!(attributes.version(), FIRST_VERSION + 1);
    }

    #[test]
    fn bad_spends_change_nothing() {
        let rules = rules();
        let mut attributes = Attributes::of(&[(STRENGTH, 10)], 7);
        assert_eq!(
            attributes.spend(&rules, 3, &[STRENGTH], &[6]),
            Err(SpendRefusal::NotEnoughPoints)
        );
        assert_eq!(
            attributes.spend(&rules, 3, &[STRENGTH, STRENGTH], &[1, 1]),
            Err(SpendRefusal::BadLists)
        );
        assert_eq!(
            attributes.spend(&rules, 3, &[STRENGTH], &[1, 1]),
            Err(SpendRefusal::BadLists)
        );
        assert_eq!(
            attributes.spend(&rules, 3, &[9], &[1]),
            Err(SpendRefusal::UnknownAttribute)
        );
        assert_eq!(
            attributes.spend(&rules, 3, &[STRENGTH], &[u32::MAX]),
            Err(SpendRefusal::NotEnoughPoints)
        );
        assert_eq!(
            attributes.spend(&CharacterPoints::default(), 3, &[STRENGTH], &[0]),
            Err(SpendRefusal::UnknownAttribute)
        );
        // Strength 5 + 10 + 486 would pass the largest value, 500.
        let rich = rules_with(&[0, i64::from(u16::MAX)]);
        assert_eq!(
            attributes.spend(&rich, 1, &[STRENGTH], &[486]),
            Err(SpendRefusal::AboveMax)
        );
        assert_eq!(attributes.spend(&rich, 1, &[STRENGTH], &[485]), Ok(()));
        let mut attributes = Attributes::of(&[(STRENGTH, 10)], 7);
        assert_eq!(attributes.carried(), vec![(STRENGTH, 10)]);
        // A handed-off player's lists go on above the version it had.
        let state = attributes.take_state(&rules, 3, true).expect("first");
        assert_eq!(attributes.version(), 8);
        assert_eq!(state.persistent.expect("points").unspent, 5);
    }

    #[test]
    fn a_refund_returns_every_spent_point_and_sends_the_whole_state() {
        let rules = rules();
        let mut attributes = Attributes::of(&[], 0);
        attributes.take_state(&rules, 3, true);
        assert_eq!(attributes.refund(), Err(RefundRefusal::NothingSpent));
        attributes
            .spend(&rules, 3, &[STRENGTH, CONSTITUTION], &[10, 5])
            .expect("spend");
        attributes.take_state(&rules, 3, false);
        assert_eq!(attributes.refund(), Ok(()));
        // Retail 2026-08-26: every attribute back to the base, no spent entry.
        let state = attributes.take_state(&rules, 3, false).expect("refunded");
        assert_eq!(
            state.attributes,
            Some(vec![(4, 5), (3, 5), (2, 5), (1, 5), (0, 5)])
        );
        let persistent = state.persistent.expect("points");
        assert_eq!(persistent.unspent, 15);
        assert!(persistent.spent.is_empty());
        assert!(attributes.carried().is_empty());
    }

    mod world {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use crate::world::tests::messages_to;

        use nw_content::progression::Progression;
        use nw_formats::objectstream::lowercase_crc32;
        use nw_messages::{
            AbilityState, FacetHeader, LOCAL_PLAYER_INTEREST, MemberState, ServerMessage, SlotRead,
        };
        use nw_reflect::Envelope;

        use super::super::*;
        use super::{BASE, CONSTITUTION, LEVEL_POINTS, MAX, PointRow, STRENGTH};
        use crate::player_content::PlayerContent;
        use crate::world::REPLICATION_CHANNEL;
        use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

        const PLAYER: u64 = 41;
        const SWORD: &str = "GreatSwordAbilityTable";
        /// The ability table of the sword pool (retail id case).
        const SWORD_TABLE: &str = "GreatswordAbilityTable";
        const COMBO: &str = "Ability_Greatsword_Combo";
        const UPGRADE: &str = "Upgrade_Greatsword_Combo_1";
        /// Retail `VersionNumber` of the greatsword pool.
        const SWORD_VERSION: i64 = 1;
        const MAX_LEVEL: i64 = 10;
        /// Retail Constitution `Health` of rows 5 and 10.
        const CONSTITUTION_ROWS: [(i64, i64); 2] = [(5, 0), (10, 200)];
        /// The health 1.75 x level 2 squared adds.
        const LEVEL_TWO_HEALTH: u32 = 7;
        /// Level 2 starts at 480 experience; weapon rank 1 at 10.
        const LEVEL_TWO_XP: u32 = 480;
        const RANK_ONE_XP: u32 = 10;
        const TICK_MS: u64 = 33;
        /// Ticks long enough for the own card to go out.
        const CARD_TICKS: u64 = 100;
        /// Ticks long enough for an own state to go out.
        const STATE_TICKS: u64 = 10;
        const HEADER: FacetHeader = FacetHeader {
            target: 0,
            entity: 0,
        };

        fn progression() -> Progression {
            let mut progression = Progression::build(
                [(0, 0), (1, 150), (2, i64::from(LEVEL_TWO_XP))],
                [(0, i64::from(RANK_ONE_XP)), (1, 50)],
                [(SWORD, 2)],
            );
            let attribute_levels: Vec<(&str, i64)> = nw_content::character_points::ATTRIBUTE_NAMES
                .iter()
                .flat_map(|name| [(*name, BASE), (*name, MAX)])
                .collect();
            let point = |id, requires| PointRow {
                id,
                pool: SWORD,
                max_level: MAX_LEVEL,
                requires,
            };
            progression.points = CharacterPoints::build(
                (0..).zip(LEVEL_POINTS).collect::<Vec<_>>(),
                attribute_levels,
                [SWORD_TABLE],
                [COMBO],
                [(SWORD, SWORD_VERSION)],
                [point(COMBO, None), point(UPGRADE, Some((COMBO, 1)))],
            )
            .with_constitution_health(CONSTITUTION_ROWS);
            progression
        }

        fn world() -> SimWorld {
            let mut world = world_with_spawns(Vec::new());
            world.content = Arc::new(PlayerContent {
                progression: progression(),
                ..PlayerContent::default()
            });
            let admit = nw_bus::Admit {
                progression: nw_bus::PlayerProgression {
                    experience: LEVEL_TWO_XP,
                    masteries: BTreeMap::from([(SWORD.to_owned(), RANK_ONE_XP)]),
                    ..nw_bus::PlayerProgression::default()
                },
                ..admit_at(PLAYER, 0.0, true)
            };
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            // In the world, as after the client's first movement.
            let entity = world.player_by_number(PLAYER).expect("player");
            world
                .world
                .get_mut::<crate::world::Vitals>(entity)
                .expect("vitals")
                .spawned = true;
            world
        }

        /// The attribute and ability states the own groups of `ticks` ticks from `from` sent.
        fn point_states(world: &mut SimWorld, from: u64, ticks: u64) -> Vec<MemberState> {
            (0..ticks)
                .flat_map(|tick| {
                    world
                        .tick(from + tick * TICK_MS, SERVER_TIME)
                        .expect("tick")
                        .messages
                })
                .filter(|out| {
                    out.connection == connection(PLAYER) && out.channel == REPLICATION_CHANNEL
                })
                .flat_map(|out| out.envelopes)
                .filter_map(|bytes| {
                    let envelope = Envelope::decode(&bytes).expect("envelope");
                    match ServerMessage::from_envelope(&envelope) {
                        Some(Ok(ServerMessage::Bundle(bundle))) => Some(bundle),
                        _ => None,
                    }
                })
                .flat_map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .filter(|update| update.interest == LOCAL_PLAYER_INTEREST)
                .flat_map(|update| update.members)
                .map(|member| member.state)
                .filter(|state| {
                    matches!(
                        state,
                        MemberState::Attributes(AttributeState {
                            persistent: Some(_),
                            ..
                        }) | MemberState::Abilities(AbilityState {
                            persistent: Some(_)
                        })
                    )
                })
                .collect()
        }

        fn progress(world: &mut SimWorld) -> Option<nw_bus::PlayerProgression> {
            world
                .positions()
                .into_iter()
                .find(|departure| departure.connection == connection(PLAYER))
                .and_then(|departure| departure.progression)
        }

        /// The first slot of the sword table in `state`.
        fn first_slot(state: &MemberState) -> Option<u32> {
            let MemberState::Abilities(AbilityState {
                persistent: Some(abilities),
            }) = state
            else {
                return None;
            };
            abilities
                .tables
                .iter()
                .find(|table| table.table == lowercase_crc32(SWORD_TABLE))
                .and_then(|table| table.slots.first())
                .map(|slot| slot.ability)
        }

        fn unspent(state: &MemberState) -> Option<u32> {
            match state {
                MemberState::Attributes(AttributeState {
                    persistent: Some(points),
                    ..
                }) => Some(points.unspent),
                _ => None,
            }
        }

        #[test]
        fn the_retail_point_calls_spend_send_and_save_the_points() {
            let mut world = world();
            let first = point_states(&mut world, NOW, CARD_TICKS);
            assert_eq!(
                first.len(),
                2,
                "the card's attributes and abilities: {first:?}"
            );
            assert_eq!(first.iter().find_map(unspent), Some(10));
            assert_eq!(first.iter().find_map(first_slot), Some(0));
            let later = NOW + CARD_TICKS * TICK_MS;
            assert_eq!(
                point_states(&mut world, later, STATE_TICKS),
                [],
                "no change"
            );
            let later = later + STATE_TICKS * TICK_MS;

            for call in [
                PointsClientMsg::SpendAttributePoints {
                    header: HEADER,
                    attributes: vec![STRENGTH],
                    points: vec![10],
                },
                // The upgrade needs the combo first.
                PointsClientMsg::AssignPoolPoints {
                    header: HEADER,
                    pool: lowercase_crc32(SWORD),
                    point: lowercase_crc32(UPGRADE),
                    points: 1,
                },
                PointsClientMsg::AssignPoolPoints {
                    header: HEADER,
                    pool: lowercase_crc32(SWORD),
                    point: lowercase_crc32(COMBO),
                    points: 1,
                },
                PointsClientMsg::ApplyAbilityPoints { header: HEADER },
                // Rank 1 gave one point only.
                PointsClientMsg::AssignPoolPoints {
                    header: HEADER,
                    pool: lowercase_crc32(SWORD),
                    point: lowercase_crc32(UPGRADE),
                    points: 1,
                },
            ] {
                world.points_call(&connection(PLAYER), call);
            }
            let changed = point_states(&mut world, later, STATE_TICKS);
            assert_eq!(
                changed.len(),
                2,
                "one attribute and one ability state: {changed:?}"
            );
            let later = later + STATE_TICKS * TICK_MS;
            assert_eq!(changed.iter().find_map(unspent), Some(0));
            assert_eq!(
                changed.iter().find_map(first_slot),
                Some(lowercase_crc32(COMBO)),
                "the applied combo goes into the first slot"
            );
            let saved = progress(&mut world).expect("the progress changed");
            assert_eq!(saved.attributes, [(STRENGTH, 10)]);
            assert_eq!(saved.weapon_points, [(COMBO.to_owned(), 1)]);
            // An apply that changes nothing sends nothing.
            world.points_call(
                &connection(PLAYER),
                PointsClientMsg::ApplyAbilityPoints { header: HEADER },
            );
            assert_eq!(point_states(&mut world, later, STATE_TICKS), []);
        }

        /// Retail `AttributeRespecCost` of a level 59 character, here charged at level 2.
        const REFUND_COST: i64 = 5_000;
        /// The sword table's position in the test ability state.
        const SWORD_TABLE_INDEX: u16 = 0;

        fn coins(world: &mut SimWorld) -> u64 {
            let entity = world.player_by_number(PLAYER).expect("player");
            world.world.get::<Player>(entity).expect("player").coins
        }

        #[test]
        fn a_refund_costs_the_level_coins_and_a_respec_is_answered() {
            let mut world = world();
            let mut content = (*world.content).clone();
            content.progression.points = content
                .progression
                .points
                .clone()
                .with_respec_costs([(2, REFUND_COST)]);
            world.content = Arc::new(content);
            point_states(&mut world, NOW, CARD_TICKS);
            let later = NOW + CARD_TICKS * TICK_MS;
            let refund = || PointsClientMsg::RefundSpentPoints { header: HEADER };
            world.points_call(
                &connection(PLAYER),
                PointsClientMsg::SpendAttributePoints {
                    header: HEADER,
                    attributes: vec![STRENGTH],
                    points: vec![10],
                },
            );
            world.points_call(&connection(PLAYER), refund());
            let spent = point_states(&mut world, later, STATE_TICKS);
            assert_eq!(
                spent.iter().find_map(unspent),
                Some(0),
                "too poor to refund"
            );
            let later = later + STATE_TICKS * TICK_MS;
            {
                let entity = world.player_by_number(PLAYER).expect("player");
                let mut player = world.world.get_mut::<Player>(entity).expect("player");
                player.coins = 7_000;
            }
            world.points_call(&connection(PLAYER), refund());
            let refunded = point_states(&mut world, later, STATE_TICKS);
            assert_eq!(refunded.iter().find_map(unspent), Some(10));
            assert_eq!(coins(&mut world), 2_000);
            let saved = progress(&mut world).expect("the progress changed");
            assert!(saved.attributes.is_empty());
            assert_eq!(saved.coins, 2_000);
            // A respec of a tree with no rank costs nothing and is answered with its table.
            world.points_call(
                &connection(PLAYER),
                PointsClientMsg::MasteryTreeRespec {
                    header: HEADER,
                    table: SWORD_TABLE_INDEX,
                    flag: 0,
                },
            );
            world.points_call(
                &connection(PLAYER),
                PointsClientMsg::MasteryTreeRespec {
                    header: HEADER,
                    table: 9,
                    flag: 0,
                },
            );
            let sent = world.take_messages();
            let answers: Vec<_> = messages_to(&sent, PLAYER)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::MasteryTreeRespecResponse(answer) => {
                        Some((answer.done, answer.table))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(answers, [(true, 0), (false, 9)]);
        }

        /// The max healths of the own vitals changes of `ticks` ticks from `from`.
        fn max_changes(world: &mut SimWorld, from: u64, ticks: u64) -> Vec<f32> {
            (0..ticks)
                .flat_map(|tick| {
                    world
                        .tick(from + tick * TICK_MS, SERVER_TIME)
                        .expect("tick")
                        .messages
                })
                .filter(|out| {
                    out.connection == connection(PLAYER) && out.channel == REPLICATION_CHANNEL
                })
                .flat_map(|out| out.envelopes)
                .filter_map(|bytes| {
                    let envelope = Envelope::decode(&bytes).expect("envelope");
                    match ServerMessage::from_envelope(&envelope) {
                        Some(Ok(ServerMessage::Bundle(bundle))) => Some(bundle),
                        _ => None,
                    }
                })
                .flat_map(|bundle| SlotRead::of(&bundle).expect("read").updates)
                .filter(|update| update.interest == LOCAL_PLAYER_INTEREST)
                .flat_map(|update| update.members)
                .filter_map(|member| match member.state {
                    MemberState::Vitals(vitals) => vitals.slot_0?.health_max,
                    _ => None,
                })
                .collect()
        }

        fn health(world: &mut SimWorld) -> (u32, u32) {
            let entity = world.player_by_number(PLAYER).expect("player");
            let vitals = world
                .world
                .get::<crate::world::Vitals>(entity)
                .expect("vitals");
            (vitals.health.current(), vitals.health.max())
        }

        #[test]
        fn the_level_and_constitution_set_the_max_health_and_a_change_is_sent() {
            let mut world = world();
            let base = world.combat.data.player_base_health();
            let at_level = base + LEVEL_TWO_HEALTH;
            assert_eq!(
                health(&mut world),
                (at_level, at_level),
                "the level at login"
            );
            let first = max_changes(&mut world, NOW, CARD_TICKS);
            assert_eq!(first, [at_level as f32], "the card's vitals");
            let later = NOW + CARD_TICKS * TICK_MS;
            world.points_call(
                &connection(PLAYER),
                PointsClientMsg::SpendAttributePoints {
                    header: HEADER,
                    attributes: vec![CONSTITUTION],
                    points: vec![5],
                },
            );
            let with_constitution = at_level + 200;
            assert_eq!(
                max_changes(&mut world, later, STATE_TICKS),
                [with_constitution as f32],
                "the new max alone"
            );
            assert_eq!(health(&mut world), (with_constitution, with_constitution));
            let later = later + STATE_TICKS * TICK_MS;
            assert!(
                max_changes(&mut world, later, STATE_TICKS).is_empty(),
                "no change"
            );
        }
    }
}
