// SPDX-License-Identifier: AGPL-3.0-or-later
//! The camp a player places (camp study 2026-10-30): its entity in the world, a crafting station
//! of its own spawn group ([`crafting::Stations`]).
//!
//! Retail `Hivecamp`: the 2236 at 1781367544.661 was answered by 441 at 544.698, and at 544.743
//! the camp's spawn group came: the card of its slice asset with a dynamic `GdeRef`, the built
//! buildable controller (state 5, the committed costs of its blueprint, the station type
//! `camp<tier>`, the tier buildable `CampT<tier>`, and for a PvP camp `isPvPCamp`), its position
//! and rotation (the 2236's), the territory interface, the delayed events, the placement
//! obstruction, the interact (disabled), for a PvP camp the vitals and the damage receiver, and
//! the ownership (the owner's character id and name). The members' component numbers are its
//! slice's ([`nw_world::camps`]). The player's camping state (1292) names the camp from then on,
//! and its home points (3652) list the camp's respawn point.
//!
//! Retail `Hive_camping` (a camp without PvP) spawned the group in state 3 (the slice's
//! `m_initialState`), sent state 4 with the committed resources 69 ms later and state 5 4.2 s
//! later: that build stage is not run here; the camp spawns built, as the `Hivecamp` group did.
//!
//! The camp is an entity of its owner's sim while the owner is in it (Inferred: no capture shows
//! a camp after its owner left). A save keeps its place and tier, not its rotation or blueprint:
//! a camp that comes back with its owner stands unturned, of the owner's camp skin or the default
//! camp of its tier (Inferred).

use bevy_ecs::entity::Entity;
use nw_content::camps::{camp_station_type, default_camp};
use nw_formats::objectstream::lowercase_crc32;
use nw_formats::rigid::{IDENTITY_QUAT, quat_normalize, rotate};
use nw_messages::buildables::NO_OWNER_KIND;
use nw_messages::gde::AssetId;
use nw_messages::position::{PositionInTheWorldReplicatedState, Rotation, WorldPosition};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BuildableControllerState, BundleMember, DamageReceiverState, DelayedEventState,
    GdeMetadataReplicatedState, GuildCrest, InteractState, MemberState, OwnerIdentification,
    OwnershipState, PlacementObstructionState, TerritoryInterfaceState, VitalsData, VitalsSlot0,
    VitalsState,
};
use nw_sim_core::Position;
use nw_world::camps::CampSlice;
use uuid::Uuid;

use super::crafting::{
    CARD_COMPONENT, CHECK_FOUNDATION_EVENTS, NO_FACTION, NO_GUILD_ID, NO_RAID, RUIN_COMPLETED,
    RUIN_DELAY, Station, TERRITORY_REFS_SET, committed, list_at,
};
use super::{Player, SimWorld};

/// Names the camp ids this server makes.
const CAMP_ID_NAMESPACE: u64 = 0x6361_6d70_7369_7465;
/// `currentState` of a built camp (both retail camps once built).
const BUILT: u8 = 5;
/// `Enabled` of a built camp's first interact (both retail camps).
const INTERACT_ENABLED: bool = false;
/// `permissionOptions` of a player's camp (both retail camps).
const CAMP_PERMISSION_OPTIONS: u8 = 3;
/// `ownerFFAFlag` (both retail camps).
const OWNER_FFA: bool = false;
/// The vitals of a PvP camp's group (retail `Hivecamp` 1781367544743: no health of its own in
/// the vitals, health 0 of 1, stamina 0 of 1, no ticks, no change flags).
const CAMP_HEALTH: f32 = 0.0;
const CAMP_HEALTH_MAX: f32 = 1.0;
const CAMP_MAX_HEALTH: u16 = 1;
const CAMP_TICK_RATE: u16 = 0;
const CAMP_HEALTH_CHANGE_FLAGS: u8 = 0;
/// The damage receiver of a PvP camp's group (retail `Hivecamp`: not blocking, slot alias 52;
/// the alias's meaning is Unknown).
const CAMP_BLOCK_ACTIVE: bool = false;
const CAMP_BLOCK_WEAPON_SLOT_ALIAS: u32 = 52;

/// A player's camp entity: what its camping state, home points and respawn name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct CampEntity {
    /// Its slot key ([`crafting::Stations`]).
    pub(super) number: u64,
    /// Its `GdeRef`.
    pub(super) gde_ref: [u8; RAW_ID_LEN],
    /// Where its owner respawns at it.
    pub(super) spawn_point: Position,
}

impl CampEntity {
    /// Its facet entity: the first 8 bytes of its `GdeRef`, little-endian (Observed: retail
    /// `Hivecamp` camp `6e5ac4808181beaf…` was facet 12663701595423529582 in 1292, 3652 and the
    /// respawn request).
    pub(super) fn facet(&self) -> u64 {
        let mut bytes = [0; size_of::<u64>()];
        bytes.copy_from_slice(&self.gde_ref[..size_of::<u64>()]);
        u64::from_le_bytes(bytes)
    }
}

/// What a camp's group holds beyond its slice.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CampSpawn<'a> {
    /// Its `GdeRef`.
    pub(super) gde_ref: [u8; RAW_ID_LEN],
    /// Where it stands.
    pub(super) position: Position,
    /// Its rotation (x, y, z, w).
    pub(super) rotation: [f32; 4],
    /// Its tier: the owner's `Camping` rank.
    pub(super) tier: u32,
    /// What its blueprint costs (item id, quantity).
    pub(super) costs: &'a [(String, u32)],
    /// The owner's character id and name.
    pub(super) character: Uuid,
    pub(super) name: &'a str,
    /// The owner's PvP flag.
    pub(super) owner_pvp: bool,
    /// The server time of the placement (ms since 2000-01-01).
    pub(super) server_ms: u64,
}

impl CampSpawn<'_> {
    /// The world point of the slice point `local` of the camp.
    fn world_point(&self, local: [f32; 3]) -> Position {
        let [x, y, z] = rotate(quat_normalize(self.rotation), local);
        Position {
            x: self.position.x + x,
            y: self.position.y + y,
            z: self.position.z + z,
        }
    }
}

/// The spawn group members of the camp of `slice` placed as `spawn`, with replicated lists at
/// `version`, by component number.
pub(super) fn camp_members(
    slice: &CampSlice,
    spawn: &CampSpawn<'_>,
    version: u64,
) -> Vec<BundleMember> {
    let station = &slice.station;
    let member = |component: u8, state| BundleMember {
        component: u64::from(component),
        state,
    };
    let mut members = vec![BundleMember {
        component: CARD_COMPONENT,
        state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
            asset: Some(AssetId {
                id: slice.asset,
                sub_id: slice.asset_sub_id,
            }),
            gde_ref: Some(spawn.gde_ref),
        }),
    }];
    let pvp = slice.vitals.is_some();
    if let Some((component, _)) = station.buildable {
        members.push(member(
            component,
            MemberState::BuildableController(BuildableControllerState {
                current_state: Some(BUILT),
                check_foundation_events: Some(CHECK_FOUNDATION_EVENTS),
                committed_resources: Some(list_at(version, committed(spawn.costs))),
                crafting_station_type_crc: Some(lowercase_crc32(&camp_station_type(spawn.tier))),
                camp_tier_buildable_id: Some(lowercase_crc32(&default_camp(spawn.tier))),
                is_pvp_camp: pvp.then_some(true),
                ..BuildableControllerState::default()
            }),
        ));
    }
    members.push(member(
        slice.position,
        MemberState::Position(PositionInTheWorldReplicatedState {
            position: WorldPosition::from_metres(
                spawn.position.x,
                spawn.position.y,
                spawn.position.z,
            ),
            rotation: Some(Rotation::of_quaternion(spawn.rotation)),
            scale: None,
        }),
    ));
    if let Some(component) = station.territory {
        members.push(member(
            component,
            MemberState::TerritoryInterface(TerritoryInterfaceState {
                functionality_down: None,
                refs_set: Some(TERRITORY_REFS_SET),
            }),
        ));
    }
    for component in &slice.delayed_events {
        members.push(member(
            *component,
            MemberState::DelayedEvent(DelayedEventState {
                total_delay: Some(RUIN_DELAY),
                completed: Some(RUIN_COMPLETED),
            }),
        ));
    }
    if let Some((component, completion)) = station.obstruction {
        members.push(member(
            component,
            MemberState::PlacementObstruction(PlacementObstructionState {
                has_completion_obstruction: Some(u8::from(completion)),
            }),
        ));
    }
    if let Some(component) = slice.interact {
        members.push(member(
            component,
            MemberState::Interact(InteractState {
                enabled: Some(INTERACT_ENABLED),
                has_interactors: None,
            }),
        ));
    }
    if let Some(vitals) = &slice.vitals {
        members.push(member(
            vitals.component,
            MemberState::Vitals(VitalsState {
                slot_0: Some(VitalsSlot0 {
                    health_amount: Some(CAMP_HEALTH),
                    stamina_amount: Some(CAMP_HEALTH),
                    vitals_data: Some(VitalsData::alive_since(spawn.server_ms)),
                    health_change_flags: Some(CAMP_HEALTH_CHANGE_FLAGS),
                    health_max: Some(CAMP_HEALTH_MAX),
                    health_tick_rate: Some(CAMP_TICK_RATE),
                    stamina_max: Some(CAMP_HEALTH_MAX),
                    stamina_tick_rate: Some(CAMP_TICK_RATE),
                    vitals_id: Some(lowercase_crc32(&vitals.row)),
                    max_health: Some(CAMP_MAX_HEALTH),
                    ..VitalsSlot0::default()
                }),
                slot_1: None,
            }),
        ));
    }
    if let Some(component) = slice.damage_receiver {
        members.push(member(
            component,
            MemberState::DamageReceiver(DamageReceiverState {
                is_block_active: Some(CAMP_BLOCK_ACTIVE),
                block_weapon_slot_alias: Some(CAMP_BLOCK_WEAPON_SLOT_ALIAS),
                debug_crit_chance: None,
            }),
        ));
    }
    if let Some((component, rule)) = station.ownership {
        // The owner's guild, crest and faction are the social service's; this sim has none of
        // them, so the camp is of no guild and faction.
        members.push(member(
            component,
            MemberState::Ownership(OwnershipState {
                guild_id: Some(NO_GUILD_ID),
                guild_owner: Some(Vec::new()),
                guild_crest: Some(GuildCrest::NONE),
                owner: Some(OwnerIdentification {
                    first: spawn.character.hyphenated().to_string().into_bytes(),
                    second: spawn.name.as_bytes().to_vec(),
                    // Retail sends the constructor's kind with an owner too (`Hivecamp`).
                    kind: NO_OWNER_KIND,
                }),
                permission_rule: Some(rule),
                structure_name: Some(station.structure_name.as_bytes().to_vec()),
                raid_id: Some(NO_RAID),
                faction_type: Some(NO_FACTION),
                owner_pvp: Some(spawn.owner_pvp),
                owner_ffa: Some(OWNER_FFA),
                permission_options: Some(CAMP_PERMISSION_OPTIONS),
            }),
        ));
    }
    members.sort_by_key(|member| member.component);
    members
}

/// The `GdeRef` of the camp of slot key `number` of the sim started at `generation` (Unix ms):
/// a dynamic id (retail camps have an even first byte, as every dynamic spawn).
fn camp_id(generation: u64, number: u64) -> [u8; RAW_ID_LEN] {
    let namespace = Uuid::from_u64_pair(CAMP_ID_NAMESPACE, generation);
    nw_messages::as_dynamic_raw_id(*Uuid::new_v5(&namespace, &number.to_be_bytes()).as_bytes())
}

/// Where a camp goes and what it is.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct CampPlacement {
    /// The camp blueprint placed (`CampT5`, `CampSkin_MetalFest02_A_PVP`).
    pub(super) blueprint: String,
    /// Where it stands.
    pub(super) position: Position,
    /// Its rotation (x, y, z, w).
    pub(super) rotation: [f32; 4],
    /// Its tier.
    pub(super) tier: u32,
}

impl SimWorld {
    /// Spawns the camp `placement` of player `entity` in the world: its group goes to every
    /// viewer near it and it is a crafting station of its tier. `None` when the content has no
    /// slice of the blueprint or the player is not here.
    pub(super) fn spawn_camp(
        &mut self,
        entity: Entity,
        placement: &CampPlacement,
    ) -> Option<CampEntity> {
        let slice = self.content.camp_slices.get(&placement.blueprint)?;
        let player = self.world.get::<Player>(entity)?;
        let generation = self.me.generation;
        let server_ms = self.server_ms;
        let costs = self.content.structure_costs.of(&placement.blueprint);
        let mut camp = None;
        self.stations.place_camp(|number| {
            let spawn = CampSpawn {
                gde_ref: camp_id(generation, number),
                position: placement.position,
                rotation: placement.rotation,
                tier: placement.tier,
                costs,
                character: player.character,
                name: &player.name,
                owner_pvp: player.pvp_flagged,
                server_ms,
            };
            camp = Some(CampEntity {
                number,
                gde_ref: spawn.gde_ref,
                spawn_point: spawn.world_point(slice.spawn_point),
            });
            Station {
                gde_ref: spawn.gde_ref,
                position: placement.position,
                station_type: camp_station_type(placement.tier),
                name: slice.station.name.as_bytes().to_vec(),
                members: camp_members(slice, &spawn, nw_messages::items::FIRST_VERSION),
            }
        });
        tracing::info!(
            player = player.number,
            blueprint = %placement.blueprint,
            tier = placement.tier,
            "camp spawned"
        );
        camp
    }

    /// Takes the camp entity of player `entity` out of the world, when it has one.
    pub(super) fn despawn_camp(&mut self, entity: Entity) {
        let camp = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| player.travel.camp.take());
        if let Some(camp) = camp {
            self.stations.remove_camp(camp.number);
        }
    }

    /// The camp of player `entity` comes with it into this sim: a saved camp stands again, of
    /// its saved blueprint and rotation; a camp saved before they were kept (world migration
    /// 0027) is of the player's camp skin or the default camp of its tier, unturned.
    pub(super) fn camp_arrived(&mut self, entity: Entity) {
        if self.is_tutorial() {
            return;
        }
        let Some((saved, skin)) = self.world.get::<Player>(entity).and_then(|player| {
            let travel = player.travel.carried();
            Some((travel.placed_camp?, travel.camp_skin))
        }) else {
            return;
        };
        let tier = u32::from(saved.tier);
        let camps = &self.content.camps;
        let blueprint = camps
            .blueprint_of_crc(saved.blueprint)
            .or_else(|| {
                skin.and_then(|skin| camps.skin_of_crc(skin))
                    .and_then(|skin| camps.blueprint(&skin.id))
            })
            .map_or_else(|| default_camp(tier), |blueprint| blueprint.id.clone());
        let nw_bus::Point { x, y, z } = saved.position.point();
        let rotation = if saved.rotation == [0; 4] {
            IDENTITY_QUAT
        } else {
            saved.rotation.map(f32::from_bits)
        };
        let placement = CampPlacement {
            blueprint,
            position: Position { x, y, z },
            rotation,
            tier,
        };
        let camp = self.spawn_camp(entity, &placement);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.travel.camp = camp;
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use nw_world::camps::CampVitals;
    use nw_world::stations::StationSlice;

    use super::*;

    /// Retail `Hivecamp` 1781367544743: the camp group's members (component, class and body
    /// bytes) in wire order, and its owner.
    const RETAIL_GROUP: [&str; 12] = [
        "01ba1801c705000001020206576f6f6454318a91442700000000000500ffffffff0000003300000000000007466c696e7454310c9280c800000000000100ffffffff00000033000000000000a2a98f40031badbdf501",
        "0d83310103000000",
        "070f01ff0000000000000000000b956388ac0fc1090000000000000000ff003f80000000003f8000004300001eaefb730001",
        "0483310103000000",
        "000a0103a0ba3a561e7e5ba0b3439694e4c33217000000026e5ac4808181beaff1f846dcd11b879d",
        "08b81701030000000034",
        "0e83310103000000",
        "03921b010201",
        "058b22010101",
        "06b22d010100",
        "0b913201ff33a71b99a90145c881c6a40d3e2c8d69094e57666f726576657200f83f09898a3ebebebf3d50d0d23f80000000c13e78f8fa3da0a0a13de0e0e23f8000002436666562363336372d393732662d343963642d623166622d3333396335626465626362310a48697665736861646f7702050000000000000000000f03010003",
        "020d0103458d535345e2779a1f9443efed",
    ];
    const RETAIL_OWNER: &str = "6feb6367-972f-49cd-b1fb-339c5bdebcb1";
    const RETAIL_NAME: &str = "Hiveshadow";
    /// The retail camp's `GdeRef`, list version and placement (2236 at 1781367544.661).
    pub(in crate::world) const RETAIL_CAMP: [u8; RAW_ID_LEN] = [
        0x6e, 0x5a, 0xc4, 0x80, 0x81, 0x81, 0xbe, 0xaf, 0xf1, 0xf8, 0x46, 0xdc, 0xd1, 0x1b, 0x87,
        0x9d,
    ];
    const RETAIL_LIST_VERSION: u64 = 2;
    pub(in crate::world) const RETAIL_AT: Position = Position {
        x: 4_522.415_5,
        y: 7_246.95,
        z: 159.065_1,
    };
    pub(in crate::world) const RETAIL_ROTATION: [u32; 4] = [0, 0, 0x3f5f_dd61, 0xbef8_5bf1];
    /// The server time (ms since 2000) of the camp's vitals block `0b956388ac0fc109`.
    const RETAIL_SERVER_MS: u64 = 834_682_744_600;
    const RETAIL_TIER: u32 = 5;
    /// The retail respawn place in the camp's 3652 element (`458d47c9 45e27ecd 431f43df`).
    pub(in crate::world) const RETAIL_SPAWN_POINT: [u32; 3] =
        [0x458d_47c9, 0x45e2_7ecd, 0x431f_43df];
    /// The largest difference from a retail float of the respawn point, metres.
    pub(in crate::world) const SPAWN_POINT_TOLERANCE_M: f32 = 0.01;

    /// The camp slice `campskin_metalfest02_a_pvp` (`camp-slices-25810808.json`).
    pub(in crate::world) fn pvp_slice() -> CampSlice {
        CampSlice {
            slice: "slices/structures/camps/campvariants/campskin_metalfest02_a_pvp.dynamicslice"
                .to_owned(),
            asset: [
                0xa0, 0xba, 0x3a, 0x56, 0x1e, 0x7e, 0x5b, 0xa0, 0xb3, 0x43, 0x96, 0x94, 0xe4, 0xc3,
                0x32, 0x17,
            ],
            asset_sub_id: 2,
            station: StationSlice {
                station_type: "Camp1".to_owned(),
                name: String::new(),
                buildable: Some((1, 3)),
                structure: "CampSkin_MetalFest02_A_PVP".to_owned(),
                ownership: Some((11, 5)),
                structure_name: String::new(),
                obstruction: Some((5, true)),
                territory: Some(3),
                grid: None,
                delayed_event: Some(4),
            },
            position: 2,
            interact: Some(6),
            vitals: Some(CampVitals {
                component: 7,
                row: "SiegeWeaponSmall".to_owned(),
            }),
            damage_receiver: Some(8),
            delayed_events: vec![4, 13, 14],
            spawn_point: [0.0, -1.7, 0.2],
        }
    }

    pub(in crate::world) fn costs() -> Vec<(String, u32)> {
        vec![("WoodT1".to_owned(), 5), ("FlintT1".to_owned(), 1)]
    }

    pub(in crate::world) fn retail_rotation() -> [f32; 4] {
        RETAIL_ROTATION.map(f32::from_bits)
    }

    fn retail_members() -> Vec<BundleMember> {
        let mut members: Vec<BundleMember> = RETAIL_GROUP
            .iter()
            .map(|bytes| {
                let bytes = hex::decode(bytes).expect("hex");
                let (member, read) = BundleMember::read_from(&bytes).expect("retail member");
                assert_eq!(read, bytes.len(), "the whole member reads");
                member
            })
            .collect();
        members.sort_by_key(|member| member.component);
        members
    }

    #[test]
    fn the_camp_group_holds_the_retail_members() {
        let costs = costs();
        let spawn = CampSpawn {
            gde_ref: RETAIL_CAMP,
            position: RETAIL_AT,
            rotation: retail_rotation(),
            tier: RETAIL_TIER,
            costs: &costs,
            character: Uuid::parse_str(RETAIL_OWNER).expect("uuid"),
            name: RETAIL_NAME,
            owner_pvp: true,
            server_ms: RETAIL_SERVER_MS,
        };
        let members = camp_members(&pvp_slice(), &spawn, RETAIL_LIST_VERSION);
        let retail = retail_members();
        let classes = |members: &[BundleMember]| -> Vec<(u64, u64)> {
            members
                .iter()
                .map(|member| (member.component, member.state.type_index()))
                .collect()
        };
        assert_eq!(classes(&members), classes(&retail));
        for (ours, theirs) in members.iter().zip(&retail) {
            match (&ours.state, &theirs.state) {
                (MemberState::Position(ours), MemberState::Position(theirs)) => {
                    let (ours_at, theirs_at) = (
                        ours.position.expect("ours"),
                        theirs.position.expect("retail"),
                    );
                    assert_eq!((ours_at.x, ours_at.y), (theirs_at.x, theirs_at.y));
                    // Whether retail rounds the height down or puts the camp on the ground is
                    // Unknown: its raw height is one step below the 2236 height's.
                    assert!(ours_at.z_raw.abs_diff(theirs_at.z_raw) <= 1);
                    assert_eq!(ours.rotation, theirs.rotation);
                }
                (MemberState::Ownership(ours), MemberState::Ownership(theirs)) => {
                    // The retail owner's guild, crest and faction are not this sim's.
                    assert_eq!(ours.owner, theirs.owner);
                    assert_eq!(
                        (
                            ours.permission_rule,
                            &ours.structure_name,
                            ours.raid_id,
                            ours.owner_pvp,
                            ours.owner_ffa,
                            ours.permission_options,
                        ),
                        (
                            theirs.permission_rule,
                            &theirs.structure_name,
                            theirs.raid_id,
                            theirs.owner_pvp,
                            theirs.owner_ffa,
                            theirs.permission_options,
                        )
                    );
                }
                (MemberState::Vitals(ours), MemberState::Vitals(theirs)) => {
                    // The retail block holds the placement's server time in ns; this server's
                    // clock counts ms.
                    let time = |state: &VitalsState| {
                        let mut slot = state.slot_0.expect("slot 0");
                        let data = slot.vitals_data.as_mut().expect("data");
                        data.a /= nw_messages::NS_PER_MS;
                        slot
                    };
                    assert_eq!(time(ours), time(theirs));
                }
                (ours, theirs) => assert_eq!(ours, theirs),
            }
        }
    }

    #[test]
    fn the_respawn_point_is_the_slice_point_turned_by_the_camp() {
        let costs = costs();
        let spawn = CampSpawn {
            gde_ref: RETAIL_CAMP,
            position: RETAIL_AT,
            rotation: retail_rotation(),
            tier: RETAIL_TIER,
            costs: &costs,
            character: Uuid::nil(),
            name: "",
            owner_pvp: false,
            server_ms: 0,
        };
        let at = spawn.world_point(pvp_slice().spawn_point);
        let retail = RETAIL_SPAWN_POINT.map(f32::from_bits);
        for (ours, theirs) in [at.x, at.y, at.z].into_iter().zip(retail) {
            assert!(
                (ours - theirs).abs() <= SPAWN_POINT_TOLERANCE_M,
                "{ours} vs {theirs}"
            );
        }
        let camp = CampEntity {
            number: 0,
            gde_ref: RETAIL_CAMP,
            spawn_point: at,
        };
        assert_eq!(camp.facet(), 12_663_701_595_423_529_582);
    }

    #[test]
    fn a_camp_id_has_the_dynamic_form() {
        let id = camp_id(1, 2);
        assert!(nw_messages::is_dynamic_raw_id(&id));
        assert_ne!(id, camp_id(1, 3));
    }
}

/// The camp rows and slices of the content of `local-data/content` (run `content-tool build` and
/// `content-tool camp-slices` first).
#[cfg(all(test, feature = "local-data"))]
mod retail_tests {
    use std::path::{Path, PathBuf};

    use nw_content::ContentBundle;
    use nw_content::bundle::bundle_path;
    use nw_content::camps::CampRules;
    use nw_content::client_build::load_client_build;
    use nw_content::crafting::StructureCosts;
    use nw_world::camps::{CampSlices, camp_slices_path};

    use super::tests::{RETAIL_AT, RETAIL_SPAWN_POINT, SPAWN_POINT_TOLERANCE_M, retail_rotation};
    use super::*;

    const CLIENT_BUILD_CONFIG: &str = "config/client-build.toml";
    /// `campt1.dynamicslice` and `campskin_metalfest02_a_pvp.dynamicslice` in the asset catalog
    /// (the second is the card asset of the retail `Hivecamp` camp).
    const CAMP_T1_ASSET: &str = "1b66134908d75b12963ab8e320c3342a";
    const PVP_SKIN_ASSET: &str = "a0ba3a561e7e5ba0b3439694e4c33217";
    /// `HP` of `CampT1` and of `CampT1_PVP` (`BlueprintItemDefinitions`).
    const CAMP_T1_HP: u32 = 300;
    const CAMP_T1_PVP_HP: u32 = 2_000;

    fn content_dir() -> (PathBuf, String) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let build = load_client_build(&repo.join(CLIENT_BUILD_CONFIG)).expect("client build");
        (repo.join("local-data/content"), build)
    }

    #[test]
    fn the_retail_camp_t1_row_and_slices_load() {
        let (dir, build) = content_dir();
        let bundle = ContentBundle::load(&bundle_path(&dir, &build), &build).expect("bundle");
        let camps = CampRules::from_tables(&bundle.tables);
        let camp = camps.blueprint("CampT1").expect("CampT1");
        assert_eq!(
            (camp.hp, camp.pvp, camp.pvp_variant.as_deref()),
            (CAMP_T1_HP, false, Some("CampT1_PVP"))
        );
        assert_eq!(camps.placed(camp, true).hp, CAMP_T1_PVP_HP);
        let costs = StructureCosts::from_tables(&bundle.tables);
        assert_eq!(
            costs.of("CampT1"),
            [("WoodT1".to_owned(), 5), ("FlintT1".to_owned(), 1)]
        );
        let pvp = camps
            .blueprint_of_crc(0x89a7_af2d)
            .expect("the Hivecamp blueprint");
        assert_eq!(
            camps
                .skin_of(pvp)
                .map(|skin| (skin.id.as_str(), skin.enabled)),
            Some(("CampSkin_MetalFest02_A", true))
        );
        let slices = CampSlices::load(&camp_slices_path(&dir, &build)).expect("camp slices");
        let t1 = slices.get("CampT1").expect("CampT1 slice");
        assert_eq!(hex::encode(t1.asset), CAMP_T1_ASSET);
        assert_eq!(t1.station.ownership, Some((7, 1)));
        let slice = slices.get(&pvp.id).expect("PvP skin slice");
        assert_eq!(hex::encode(slice.asset), PVP_SKIN_ASSET);
        assert_eq!(slice.delayed_events, [4, 13, 14]);
        let spawn = CampSpawn {
            gde_ref: [0; RAW_ID_LEN],
            position: RETAIL_AT,
            rotation: retail_rotation(),
            tier: 5,
            costs: &[],
            character: Uuid::nil(),
            name: "",
            owner_pvp: true,
            server_ms: 0,
        };
        let at = spawn.world_point(slice.spawn_point);
        for (ours, theirs) in [at.x, at.y, at.z]
            .into_iter()
            .zip(RETAIL_SPAWN_POINT.map(f32::from_bits))
        {
            assert!((ours - theirs).abs() <= SPAWN_POINT_TOLERANCE_M);
        }
    }
}
