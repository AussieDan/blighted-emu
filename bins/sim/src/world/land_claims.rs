// SPDX-License-Identifier: AGPL-3.0-or-later
//! The slot-2 world entity every client gets once it spawned: its spawn card, its position and
//! the territory claims ([`nw_messages::land_claim`]).
//!
//! Observed (client trace 2026-11-06): without the claims the map's shrine and town flyout finds
//! no governance data, stops with a script error and shows no name. Observed (retail ledgers
//! newchar 2026-09-18, general-other-01, travel-shrines-settlements-mount): the first slot-2
//! group is a spawn card: the mutations (component 6), the position (4), the card (0), the claims
//! (2), the global map data (1) and the war data (5), with the same asset, GDE ref and position in
//! all three ([`WORLD_ENTITY_ASSET`]); members sent to slot 2 without the card reach no entity
//! (live 2026-11-06: the claims alone changed nothing). The retail state has the 13
//! claims, their positions and one tax rate set for all ([`CLAIM_KEYS`], [`CLAIM_POSITIONS`],
//! [`RETAIL_GOVERNANCE`]); companies own every claim there. This server runs no company claims
//! or wars, so every claim is sent without an owner, upgrades, conflict or darkness (Inferred:
//! the zero of each value).

use nw_messages::gde::AssetId;
use nw_messages::items::FullList;
use nw_messages::position::{PositionInTheWorldReplicatedState, WorldPosition};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BundleGroup, BundleMember, CLAIM_KEYS, ClaimOwner, ClaimProgression,
    GdeMetadataReplicatedState, GovernanceTaxData, LandClaimManagerState, MemberState,
    MessageError,
};

use super::crafting::CARD_COMPONENT;

/// The slice of the slot-2 world entity (Observed: the same in the three retail ledgers).
const WORLD_ENTITY_ASSET: AssetId = AssetId {
    id: [
        241, 133, 95, 148, 136, 148, 85, 215, 166, 176, 208, 3, 236, 39, 129, 38,
    ],
    sub_id: 2,
};
/// Its GDE ref (Observed: the same in the three retail ledgers).
const WORLD_ENTITY_GDE_REF: [u8; RAW_ID_LEN] = [
    76, 64, 180, 139, 78, 222, 12, 4, 15, 243, 158, 244, 233, 34, 253, 253,
];
/// Its position, no rotation or scale (Observed: the same in the three retail ledgers).
const WORLD_ENTITY_POSITION: WorldPosition = WorldPosition {
    x: 7168.0,
    y: 7168.0,
    z_raw: 4119,
};
/// The component of its position (Observed).
const POSITION_COMPONENT: u64 = 4;

/// The position of each claim, by claim index (Observed: the same in the three retail ledgers).
const CLAIM_POSITIONS: [[f32; 3]; CLAIM_KEYS.len()] = [
    [6851.21, 1330.6776, 62.924_44],
    [6305.3696, 3352.3557, 53.653_294],
    [8928.186, 3289.11, 70.321_95],
    [10986.032, 2357.098, 46.180_897],
    [4344.5234, 4873.731, 120.192_15],
    [5902.4795, 5428.118, 57.516_47],
    [8913.23, 5783.515, 168.272_63],
    [8461.229, 4877.2725, 180.431_1],
    [11017.301, 5906.705, 146.002_23],
    [14046.733, 4820.699, 57.624_416],
    [5805.467, 7387.4556, 71.078],
    [13694.403, 6314.9653, 45.882_137],
    [11507.5625, 8964.927, 418.562_65],
];
/// The taxes and fees of every claim (Observed: all 13 claims in the three retail ledgers).
const RETAIL_GOVERNANCE: GovernanceTaxData = GovernanceTaxData {
    house_fee: 0.2,
    crafting_fee: 0.5,
    refining_fee: 0.5,
    trading_fee: 1.0,
    is_valid: true,
    failed_to_pay_upkeep: false,
};
/// A fort lock timer that runs no lock (Observed: on every claim without a lock).
const NO_FCP_LOCK: u16 = u16::MAX;
/// Entries of `replicatedFactionControlPointData` (Observed: 29 in the three retail ledgers).
const FACTION_CONTROL_POINT_ENTRIES: usize = 29;
/// The faction byte of no faction.
const NO_FACTION: u8 = 0;
/// The zero time of a conflict, darkness cycle or influence race that does not run.
const NO_TIME: u64 = 0;
/// The darkness threshold of a claim without darkness.
const NO_DARKNESS: u32 = 0;

/// One value per claim.
fn per_claim<T: Clone>(value: T) -> Option<FullList<T>> {
    Some(FullList::first(vec![value; CLAIM_KEYS.len()]))
}

/// The manager state of this server's claims.
fn manager_state() -> LandClaimManagerState {
    LandClaimManagerState {
        fcp_lock_timers: per_claim(NO_FCP_LOCK),
        // Observed: one retail ledger leaves the anchor out.
        fcp_lock_anchor: None,
        claim_keys: Some(FullList::first(CLAIM_KEYS.to_vec())),
        conflict_faction: per_claim(NO_FACTION),
        conflict_lottery_end_time: per_claim(NO_TIME),
        conflict_start_time: per_claim(NO_TIME),
        darkness_threshold: per_claim(NO_DARKNESS),
        darkness_cycle_end_time: per_claim(NO_TIME),
        faction_control_point_data: Some(FullList::first(vec![
            NO_FACTION;
            FACTION_CONTROL_POINT_ENTRIES
        ])),
        faction_influence: [
            per_claim(NO_FACTION),
            per_claim(NO_FACTION),
            per_claim(NO_FACTION),
        ],
        governance: per_claim(RETAIL_GOVERNANCE),
        influence_race_start_time: per_claim(NO_TIME),
        positions: Some(FullList::first(CLAIM_POSITIONS.to_vec())),
        progression: per_claim(ClaimProgression::default()),
        owners: per_claim(ClaimOwner::default()),
        war_dec_threshold_met_faction: per_claim(NO_FACTION),
    }
}

/// The spawn members of the slot-2 world entity, in retail order (after the mutations): the
/// position, the card and the claims, for a client that does not have the entity yet (`spawned`
/// false); sets `spawned`.
///
/// # Errors
///
/// Returns [`MessageError`] when the claims cannot be encoded.
pub(super) fn spawn_members_once(
    spawned: &mut bool,
) -> Result<Option<Vec<BundleMember>>, MessageError> {
    if std::mem::replace(spawned, true) {
        return Ok(None);
    }
    Ok(Some(vec![
        BundleMember {
            component: POSITION_COMPONENT,
            state: MemberState::Position(PositionInTheWorldReplicatedState {
                position: Some(WORLD_ENTITY_POSITION),
                rotation: None,
                scale: None,
            }),
        },
        BundleMember {
            component: CARD_COMPONENT,
            state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
                asset: Some(WORLD_ENTITY_ASSET),
                gde_ref: Some(WORLD_ENTITY_GDE_REF),
            }),
        },
        manager_state().member()?,
    ]))
}

/// Adds `members` to the slot-2 group `group` of this tick, or makes the group (retail sends the
/// mutations and the spawn members in one slot-2 group).
pub(super) fn with_slot_2_members(
    group: Option<BundleGroup>,
    members: Vec<BundleMember>,
) -> BundleGroup {
    match group {
        Some(mut group) => {
            group.members.extend(members);
            group
        }
        None => BundleGroup {
            interest: nw_messages::LAND_CLAIM_MANAGER_INTEREST,
            members,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nw_messages::{LAND_CLAIM_MANAGER_COMPONENT, LAND_CLAIM_MANAGER_INTEREST};

    #[test]
    fn the_entity_spawns_once_with_its_card_and_every_claim() {
        let mut spawned = false;
        let members = spawn_members_once(&mut spawned)
            .expect("encode")
            .expect("first time");
        assert!(spawned);
        let components: Vec<u64> = members.iter().map(|member| member.component).collect();
        assert_eq!(
            components,
            [
                POSITION_COMPONENT,
                CARD_COMPONENT,
                LAND_CLAIM_MANAGER_COMPONENT
            ]
        );
        let MemberState::GdeMetadata(card) = &members[1].state else {
            panic!("card");
        };
        assert_eq!(card.asset, Some(WORLD_ENTITY_ASSET));
        assert_eq!(card.gde_ref, Some(WORLD_ENTITY_GDE_REF));
        let MemberState::Recorded { body, .. } = &members[2].state else {
            panic!("recorded state");
        };
        let state = LandClaimManagerState::decode(body).expect("decode");
        let claims = CLAIM_KEYS.len();
        let governance = state.governance.expect("governance");
        assert_eq!(governance.elements.len(), claims);
        assert!(
            governance
                .elements
                .iter()
                .all(|entry| entry.is_valid && !entry.failed_to_pay_upkeep)
        );
        let owners = state.owners.expect("owners");
        assert_eq!(owners.elements.len(), claims);
        assert!(
            owners
                .elements
                .iter()
                .all(|owner| owner.faction == NO_FACTION)
        );
        assert_eq!(state.positions.expect("positions").elements.len(), claims);
        assert_eq!(spawn_members_once(&mut spawned), Ok(None));
    }

    #[test]
    fn the_spawn_members_join_the_mutations_group_or_make_their_own() {
        let members = spawn_members_once(&mut false)
            .expect("encode")
            .expect("first time");
        let alone = with_slot_2_members(None, members.clone());
        assert_eq!(alone.interest, LAND_CLAIM_MANAGER_INTEREST);
        assert_eq!(alone.members, members);
        let joined = with_slot_2_members(Some(alone), members.clone());
        assert_eq!(joined.members.len(), 2 * members.len());
    }
}
