// SPDX-License-Identifier: AGPL-3.0-or-later
//! Travel (run 15 travel study, scripts in `local-data/scratch-work/run15/travel`): fast travel to
//! a landmark or a home, unstuck, the map marker, the respawn points a player binds (an inn, a
//! respawn camp), territory discovery, door passes, and the house plot and furniture calls.
//!
//! Retail order and timing, from the captures:
//! - A fast travel (3713 to a landmark, 3712 to a home) channels [`TravelRules::channel_ms`]; the
//!   server then sends the loading screen (3951, flag 1) and the end of the channel (3696) and
//!   moves the player. A movement key during the channel cancels it with 3042 (7 of 8 cancels of
//!   the cancel capture came 25 to 30 ms after the first record with the forward key; no travel
//!   that ended had one). A travel to a settlement or a shrine takes its azoth at the end of the
//!   channel, in the ms of the loading screen (travel cost survey of 2026-10-31); an inn recall
//!   and a home travel take none. A destination in another sim's cells hands the player over once
//!   the move's own states are out.
//! - An unstuck (388) is accepted at once (3980, the wait in nanoseconds) and moves the player
//!   after the wait (2190) to the oldest of its breadcrumbs (Inferred: 31 retail landings were on
//!   the player's own trail, 39 to 264 m back); it is denied (2774) while dead, while one runs, on
//!   cooldown, or without breadcrumbs, with the cooldown left. A failed move ends the cooldown
//!   (retail asked again 54 s after one and was accepted).
//! - A map marker (2728) comes back as the waypoints state (4321) in the next bundle.
//! - Binding a respawn camp (interaction 73) demolishes the player's own camp (6760, every one of
//!   8 retail binds; its entity goes and the save forgets it) and is accepted (3743); checking in
//!   at an inn (70, then 18) is accepted.
//!   The bound ids are the ids a respawn request (1938) names.
//! - A door (67) is accepted and its client moves through it on its own, 4.7 to 5.6 m, 134 to
//!   267 ms after the answer (3 retail doors): that one move is taken.
//!
//! The run 15 travel2 study (`local-data/scratch-work/run15/travel2`) adds:
//! - The player's travel states after its card, and again after each change: the player time
//!   (3290), the housing state (2768) with its owned houses, the home points (3652) its respawn
//!   screen lists (the bound inn, the houses, the placed camp) and the camping state (1292).
//! - An inn recall is a 3713 of landmark type `InnRespawn` (20; 61 retail recalls): it goes to
//!   the bound inn, at most once per the inn home point's cooldown (900 s in every retail one).
//! - The fast travel azoth cost is the client's formula ([`crate::travel_rules::FastTravelCosts`])
//!   with the values retail charges: 20 azoth, half within the player's territory (203 paid
//!   retail travels of 256 ledgers). 63 retail travels were free; why is Unknown.
//! - A house plot's type comes from the level (`HousingPlotComponent.m_houseTypeString`): its
//!   cost, the level rule of `javelin.housing.house-level-reqs`, one house per territory, the
//!   weekly tax (due 7 days after the purchase or the last payment), the home fast travel
//!   cooldown of its type and its reset for azoth (1998: `max(10, 200 * left / cooldown)`, the
//!   cooldown ends at once, no answer but the housing state). Placed furniture leaves the bag
//!   (the 3277 item is the client item index: the housing rows after the other items) and comes
//!   back when picked up.
//! - A teleporter (interaction 82) is a capital whose slice holds its destination; retail answered
//!   and moved the player 1 s later (or after the fast travel channel for `m_channelDuration`
//!   0) with 3951 and 3696.
//! - A camp (2236 from build mode, answered by 441; a former camp goes with 6760, as retail's
//!   second placement) needs the `Camping` rank (its tier), a territory with `CanPlaceCamp`, the
//!   blueprint the 2236 names (or the default camp of its tier; a PvP-flagged player's is the
//!   blueprint's `PvPCampVariantID`), an enabled skin whose achievement the player has, and its
//!   `StructureCost` items in the bag, which it takes. It stands in the world as an entity
//!   ([`super::camps`]); the camping state (1292) names it and its bind cooldown end (placement
//!   plus [`CAMP_BIND_COOLDOWN_MS`]), the home points (3652) list its respawn point.

use std::collections::VecDeque;
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_bus::{
    ConnectionRef, HomeRecord, HousingItem, OwnedHome, PlacedCamp, PlayerTravel, Point, StoredPoint,
};
use nw_content::camps::default_camp;
use nw_content::progression::AZOTH_PROGRESSION;
use nw_messages::facet::component as player_component;
use nw_messages::items::{FIRST_VERSION, FullList};
use nw_messages::travel::{
    BUILDER_COMPONENT, ItemPlacement, PlotRef, StructurePlacement, TerritoryResidentData,
    UnstuckDenial, WAYPOINTS_COMPONENT, WAYPOINTS_TYPE_INDEX, WaypointsState, component,
};
use nw_messages::travel_states::{
    CAMPING_COMPONENT, CAMPING_TYPE_INDEX, CampRef, CampingState, HomePoint, HouseDataRef,
    NO_PHASED_ENTITY, OwnedHouse, PLAYER_HOME_COMPONENT, PLAYER_HOME_TYPE_INDEX,
    PLAYER_HOUSING_COMPONENT, PLAYER_HOUSING_TYPE_INDEX, PLAYER_TIME_COMPONENT,
    PLAYER_TIME_TYPE_INDEX, PlayerHomeState, PlayerHousingState, PlayerTimeState, home_point_id,
    home_point_kind,
};
use nw_messages::{
    BundleMember, FacetHeader, InputRecord, InteractionRequest, MemberState, NS_PER_MS, RAW_ID_LEN,
    ServerMessage, TeleportWithLoadingScreenMsg, TravelClientMsg, TravelServerMsg,
};
use nw_sim_core::Position;

use crate::charting::CHART_EVENT;
use crate::objectives::Event;
use crate::travel_rules::{HousingRules, TravelRules};

use super::camps::{CampEntity, CampPlacement};
use super::quest_objects::gain_experience;
use super::{
    Body, Placement, Player, REFUSED_RESYNC_MS, SimWorld, Vitals, gather_answer, send_now, unix_ns,
};

/// Interaction kinds of the client's `InteractionUIActions` that travel answers (survey of 721
/// ledgers, `GAPS.md` section 2.0).
pub mod kind {
    /// `SetSpawnPoint`: check in at an inn.
    pub const SET_SPAWN_POINT: u8 = 18;
    /// `HousingPurchase`: the purchase screen of a house plot.
    pub const HOUSING_PURCHASE: u8 = 57;
    /// `HousingEnter`: enter a house.
    pub const HOUSING_ENTER: u8 = 58;
    /// `HousingQuickEnter`: enter one's own house.
    pub const HOUSING_QUICK_ENTER: u8 = 59;
    /// `HousingExit`: leave a house.
    pub const HOUSING_EXIT: u8 = 60;
    /// `TeleportToOtherSideOfDoor`.
    pub const DOOR: u8 = 67;
    /// `OpenInnScreen`.
    pub const OPEN_INN: u8 = 70;
    /// `BindToCamp`: bind the respawn to a respawn camp.
    pub const BIND_TO_CAMP: u8 = 73;
    /// `InteractTeleporter`.
    pub const TELEPORTER: u8 = 82;
}

/// The 3743 status of an accepted interaction.
const ACCEPTED: u8 = 1;
/// The 3743 status of a house exit (retail: two answers, the first without the name).
const EXITED: u8 = 2;
/// The flag of the loading screen of a fast travel (3951).
const LOADING_SCREEN: bool = true;
/// The radius of the interaction sphere of the retail teleport door slice
/// (`Master_TeleportDoor_1m_Locked`, `SphereShapeComponent` `Radius` 3.25): a door pass is a move
/// longer than it and at most twice it (Inferred: the 3 retail passes were 4.7 to 5.6 m).
const DOOR_SPHERE_RADIUS_M: f32 = 3.25;
/// How many door sphere radii a pass may cross at most.
const DOOR_PASS_RADII: f32 = 2.0;
/// The decoration result of a done change (`eHousingDecorationResult_Success`, retail 0).
const DECORATION_SUCCESS: u8 = 0;
/// `eHousingDecorationResult_NotOwner` (the client's enum order, Inferred from success 0).
const DECORATION_NOT_OWNER: u8 = 1;
/// `eHousingDecorationResult_InvalidHousingItemId`.
const DECORATION_INVALID_ITEM_ID: u8 = 5;
/// `eHousingDecorationResult_HousingItemIdExceeded`.
const DECORATION_ITEM_ID_EXCEEDED: u8 = 6;
/// `eTerritoryLandmarkType_InnRespawn` (the client's enum registration `FUN_14465e8e0`).
const INN_RESPAWN_LANDMARK: u8 = 20;
/// The categorical progression whose rank is the tier of the camp a player places (`Camping`,
/// `MaxLevel` 5, ranks `@ui_camping_reward_tier_N`); rank 0 places none.
const CAMPING_PROGRESSION: &str = "Camping";
/// The game event of a placed camp.
const BIND_CAMP_EVENT: &str = "BindCamp";
/// The 441 result of a placed structure (0 in all 5 retail answers).
const PLACEMENT_PLACED: u8 = 0;
/// The 441 result of a refused structure (best guess: the first value after success; the
/// client's result enum is Unknown).
const PLACEMENT_REFUSED: u8 = 1;
/// The second number of 441 (0 in all 5 retail answers).
const PLACEMENT_VALUE: u32 = 0;
/// The status of a refused interaction (the crafting station's refusal).
const INTERACTION_REFUSED: u8 = 2;
/// How long after a camp's placement its owner may not bind to it (Observed: retail `Hivecamp`
/// 1292 with the camp at 1781367544.712, `campBindCooldownEndTime` 60 s after the placement).
const CAMP_BIND_COOLDOWN_MS: u64 = 60_000;
/// The cooldown of a camp home point (1 s in all 14 retail camp elements).
const CAMP_COOLDOWN_NS: u64 = 1_000_000_000;
/// The name of a house home point (all 1,042 retail ones).
const HOUSE_POINT_NAME: &[u8] = b"House";
/// The index of a single home point of an entity (every inn, house and camp of retail).
const SINGLE_POINT: u32 = 0;
/// `lastHouseRequestResponse` (0 in every retail body).
const NO_HOUSE_RESPONSE: u8 = 0;
/// The house flags retail sets on 756 of 804 owned houses.
const HOUSE_HANDSHAKE: bool = true;
/// The decoration score and trophies of a house: the trophy items (house buffs) are not run
/// here.
const NO_DECORATION_SCORE: f32 = 0.0;

/// Why a camp placement is refused (the 441 result of each is Unknown: all answer
/// [`PLACEMENT_REFUSED`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CampRefusal {
    /// The player is dead or not here.
    Dead,
    /// Its `Camping` rank is 0.
    Rank,
    /// None of its territories allows camps.
    Territory,
    /// The content has neither the named blueprint nor the default camp of its tier.
    Blueprint,
    /// The camp's skin is disabled, or the player lacks its achievement.
    Skin,
    /// The content has no slice of the blueprint.
    Slice,
    /// The bag lacks a cost item.
    Cost,
}

/// What a channel is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// A fast travel to a landmark, for `azoth`.
    Landmark {
        /// The cost.
        azoth: u32,
    },
    /// A recall to the bound inn.
    Inn,
    /// A fast travel to the home on plot `plot`.
    Home {
        /// The plot's raw id.
        plot: [u8; RAW_ID_LEN],
    },
    /// A teleporter.
    Teleporter,
}

/// Where a fast travel goes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Channel {
    /// The destination.
    to: Position,
    /// The facing there, when the destination has one.
    facing: Option<f32>,
    /// When the channel ends (sim milliseconds).
    due_ms: u64,
    /// What it is for.
    purpose: Purpose,
}

/// The travel states its client does not have yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct StatesDue {
    time: bool,
    housing: bool,
    homes: bool,
    camping: bool,
}

impl StatesDue {
    const ALL: Self = Self {
        time: true,
        housing: true,
        homes: true,
        camping: true,
    };
}

/// A player's travel state: what a save keeps ([`PlayerTravel`]) and what runs now.
#[derive(Debug, Default)]
pub(super) struct TravelState {
    saved: PlayerTravel,
    /// The fast travel that channels.
    channel: Option<Channel>,
    /// When the accepted unstuck moves the player (sim milliseconds).
    unstuck_due_ms: Option<u64>,
    /// Places the player stood, oldest first.
    breadcrumbs: VecDeque<Position>,
    /// The marker is not sent yet.
    waypoint_due: bool,
    /// Until when (sim milliseconds) a door pass may come.
    door_pass_until_ms: Option<u64>,
    /// The house plot of the player's last housing interaction.
    plot: Option<[u8; RAW_ID_LEN]>,
    /// The plot of the house the player is in.
    inside: Option<[u8; RAW_ID_LEN]>,
    /// The states its client does not have yet.
    due: StatesDue,
    /// Its placed camp's entity in this sim.
    pub(super) camp: Option<CampEntity>,
    /// The versions of the owned house and home point lists its client has.
    houses_version: u64,
    homes_version: u64,
    /// The session's start (server ms since 2000) and the played time then (ms).
    session: Option<(u64, u64)>,
}

impl TravelState {
    /// The state of a player with the saved state `saved`: its marker goes out after its card.
    pub(super) fn of(saved: PlayerTravel) -> Self {
        Self {
            waypoint_due: saved.waypoint.is_some(),
            saved,
            due: StatesDue::ALL,
            houses_version: FIRST_VERSION,
            homes_version: FIRST_VERSION,
            ..Self::default()
        }
    }

    /// What a save or a handoff carries.
    pub(super) fn carried(&self) -> PlayerTravel {
        self.saved.clone()
    }

    /// How many houses the player owns.
    pub(super) fn houses(&self) -> u32 {
        u32::try_from(self.saved.homes.len()).unwrap_or(u32::MAX)
    }

    fn home(&self, plot: &[u8; RAW_ID_LEN]) -> Option<&OwnedHome> {
        self.saved.homes.iter().find(|home| &home.plot == plot)
    }

    fn home_mut(&mut self, plot: &[u8; RAW_ID_LEN]) -> Option<&mut OwnedHome> {
        self.saved.homes.iter_mut().find(|home| &home.plot == plot)
    }

    /// The owned home a call names by `plot`: it learns the reference's other ids, which only
    /// the client knows (the third is of unknown meaning).
    fn named_home(&mut self, plot: &PlotRef) -> Option<&mut OwnedHome> {
        let home = self.home_mut(&plot.id)?;
        home.record.plot_facet = plot.facet;
        home.record.plot_extra = plot.extra;
        Some(home)
    }

    /// Keeps `at` as a breadcrumb when it is `spacing_m` or more from the newest one; at most
    /// `max` are kept, the oldest go first.
    fn drop_crumb(&mut self, at: Position, spacing_m: f32, max: usize) {
        let far = self
            .breadcrumbs
            .back()
            .is_none_or(|last| last.horizontal_distance(at) >= spacing_m);
        if far {
            self.breadcrumbs.push_back(at);
        }
        while self.breadcrumbs.len() > max {
            self.breadcrumbs.pop_front();
        }
    }
}

/// The position of a stored point.
fn position_of(point: StoredPoint) -> Position {
    let Point { x, y, z } = point.point();
    Position { x, y, z }
}

/// The stored form of a position.
fn stored(at: Position) -> StoredPoint {
    StoredPoint::of(Point {
        x: at.x,
        y: at.y,
        z: at.z,
    })
}

/// A `f32` rotation as bit patterns.
fn rotation_bits(placement: &ItemPlacement) -> [u32; 4] {
    placement.rotation.map(f32::to_bits)
}

/// The level id of a raw id (its first 8 bytes, little-endian).
fn id_of_raw(raw: &[u8; RAW_ID_LEN]) -> u64 {
    let mut bytes = [0; size_of::<u64>()];
    bytes.copy_from_slice(&raw[..size_of::<u64>()]);
    u64::from_le_bytes(bytes)
}

/// The reference of level entity `id` (the open world's zero phase; the third id, which only a
/// client names, is 0).
fn entity_ref(id: u64) -> PlotRef {
    let mut raw = [0; RAW_ID_LEN];
    raw[..size_of::<u64>()].copy_from_slice(&id.to_le_bytes());
    PlotRef {
        id: raw,
        facet: id,
        extra: 0,
    }
}

/// The plot reference of `home`, with the ids its client named.
fn home_ref(home: &OwnedHome) -> PlotRef {
    PlotRef {
        id: home.plot,
        facet: if home.record.plot_facet == 0 {
            id_of_raw(&home.plot)
        } else {
            home.record.plot_facet
        },
        extra: home.record.plot_extra,
    }
}

/// Unix nanoseconds of Unix milliseconds `ms`; 0 stays 0 (none).
const fn unix_ms_ns(ms: u64) -> u64 {
    ms.saturating_mul(NS_PER_MS)
}

/// The header of a call to the travel component `component` of player `number`.
fn header(component: u64, number: u64) -> FacetHeader {
    FacetHeader::of(component, number)
}

fn housing(number: u64, answer: fn(FacetHeader) -> TravelServerMsg) -> ServerMessage {
    ServerMessage::Travel(answer(header(component::PLAYER_HOUSING, number)))
}

impl SimWorld {
    /// The Unix milliseconds of the last tick's server time.
    fn unix_ms(&self) -> u64 {
        unix_ns(self.server_ms) / NS_PER_MS
    }

    /// Sends `messages` to the client of `entity` now.
    fn send_travel(&mut self, entity: Entity, messages: &[ServerMessage]) {
        let Some(connection) = self
            .world
            .get::<Player>(entity)
            .map(|player| player.connection.clone())
        else {
            return;
        };
        send_now(&mut self.combat_messages, &connection, messages);
    }

    /// A travel call of the client of `connection` at `now_ms`.
    pub fn travel_call(&mut self, connection: &ConnectionRef, call: TravelClientMsg, now_ms: u64) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let Some(number) = self.world.get::<Player>(entity).map(|player| player.number) else {
            return;
        };
        match call {
            TravelClientMsg::FastTravelLandmark { landmark, .. }
                if landmark.landmark_type == INN_RESPAWN_LANDMARK =>
            {
                self.inn_recall(entity, number, now_ms);
            }
            TravelClientMsg::FastTravelLandmark { landmark, .. } => {
                let found = self
                    .content
                    .travel
                    .landmark(
                        landmark.landmark_type,
                        landmark.position[0],
                        landmark.position[1],
                    )
                    .copied();
                let discovered = self
                    .content
                    .travel
                    .discovered_achievement(u32::from(landmark.territory))
                    .is_none_or(|achievement| {
                        self.world.get::<Player>(entity).is_some_and(|player| {
                            player
                                .flow
                                .has_achievement(&self.achievement_rules, achievement)
                        })
                    });
                match found.filter(|_| discovered) {
                    Some(found) => {
                        let [x, y, z] = found.position;
                        let to = Position { x, y, z };
                        let azoth = self.fast_travel_cost(entity, to, found.territory);
                        self.start_channel(entity, number, to, now_ms, Purpose::Landmark { azoth });
                    }
                    None => {
                        tracing::info!(
                            player = number,
                            territory = landmark.territory,
                            landmark_type = landmark.landmark_type,
                            discovered,
                            "fast travel to an unknown or undiscovered landmark refused"
                        );
                        self.send_travel(entity, &[housing(number, fast_travel_failed)]);
                    }
                }
            }
            TravelClientMsg::FastTravelHome { plot, .. } => {
                let unix_ms = self.unix_ms();
                let home = self.world.get_mut::<Player>(entity).and_then(|mut player| {
                    let home = player.travel.named_home(&plot)?;
                    let ready = home.record.fast_travel_ready_ms <= unix_ms;
                    Some((position_of(home.position), ready))
                });
                match home {
                    Some((to, true)) => {
                        self.start_channel(
                            entity,
                            number,
                            to,
                            now_ms,
                            Purpose::Home { plot: plot.id },
                        );
                    }
                    found => {
                        tracing::info!(
                            player = number,
                            owned = found.is_some(),
                            "fast travel home refused (no home or its cooldown runs)"
                        );
                        self.send_travel(entity, &[housing(number, fast_travel_failed)]);
                    }
                }
            }
            TravelClientMsg::CancelFastTravel { .. } => self.cancel_channel(entity, number),
            TravelClientMsg::FastTravelTimeout { .. } => {
                tracing::info!(player = number, "client fast travel timed out");
            }
            TravelClientMsg::SetWaypoint { position, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    let [x, y, z] = position;
                    player.travel.saved.waypoint = Some(StoredPoint::of(Point { x, y, z }));
                    player.travel.waypoint_due = true;
                }
            }
            TravelClientMsg::Unstuck { .. } => self.unstuck_request(entity, number, now_ms),
            TravelClientMsg::SetCampSkin { skin, .. } => self.set_camp_skin(entity, number, skin),
            TravelClientMsg::EnterPlot { id, facet, .. } => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity)
                    && let Some(plot) = player.travel.plot
                    && let Some(home) = player.travel.home_mut(&plot)
                {
                    home.record.house_data = id;
                    home.record.house_data_facet = facet;
                }
                self.enter_plot(entity, number);
            }
            TravelClientMsg::ExitPlot { plot, .. } => self.exit_plot(entity, number, &plot),
            TravelClientMsg::BuyHome { .. } => self.buy_home(entity, number),
            TravelClientMsg::AbandonHome { .. } => self.abandon_home(entity, number),
            TravelClientMsg::ResetCooldown { plot, .. } => {
                self.reset_cooldown(entity, number, &plot)
            }
            TravelClientMsg::PayTaxes { plot, .. } => self.pay_taxes(entity, number, &plot),
            TravelClientMsg::TaxesDue { plot, .. } => self.taxes_due(entity, number, &plot),
            TravelClientMsg::PlotInfo { .. } => self.plot_info(entity, number),
            TravelClientMsg::ResidentData { .. } => self.resident_data(entity, number),
            TravelClientMsg::PlaceItem {
                item, placement, ..
            } => self.place_item(entity, number, item, &placement),
            TravelClientMsg::MoveItem {
                index, placement, ..
            } => self.move_item(entity, number, index, &placement),
            TravelClientMsg::RemoveItem { index, .. } => {
                self.remove_item(entity, number, index, now_ms);
            }
            TravelClientMsg::RemoveAllFurniture { .. } => {
                self.remove_all_furniture(entity, number, now_ms);
            }
            TravelClientMsg::PlaceStructure { placement, .. } => {
                self.place_camp(entity, number, &placement, now_ms);
            }
            call @ (TravelClientMsg::CancelPlacingStructure { .. }
            | TravelClientMsg::InteractHousingItem { .. }
            | TravelClientMsg::HousingItemInteractionEnded { .. }) => {
                // Retail answers none of these (the build mode cancel, the use of a piece of
                // furniture and its end): the client plays them on its own.
                tracing::debug!(
                    player = number,
                    type_index = call.type_index(),
                    "travel call"
                );
            }
        }
    }

    /// The azoth of a fast travel of player `entity` to `to` in territory `territory` (the
    /// `IsTerritory` territory of the landmark): half within the territory the player stands in
    /// (its client's last 2297 of that territory; retail survey: 111 of 111 such travels).
    fn fast_travel_cost(&self, entity: Entity, to: Position, territory: Option<u32>) -> u32 {
        let costs = &self.content.travel.costs;
        if costs.disabled {
            return 0;
        }
        let distance = self
            .world
            .get::<Body>(entity)
            .map_or(0.0, |body| body.motion.position().horizontal_distance(to));
        let in_territory = territory.is_some_and(|territory| {
            self.world.get::<Player>(entity).is_some_and(|player| {
                player
                    .territories
                    .iter()
                    .any(|&inside| u32::from(inside) == territory)
            })
        });
        costs.cost(f64::from(distance), self.bag_load(entity), in_territory)
    }

    /// A recall of player `number` (`entity`) to the inn it checked in at: refused without one or
    /// while the inn's cooldown runs.
    fn inn_recall(&mut self, entity: Entity, number: u64, now_ms: u64) {
        let unix_ms = self.unix_ms();
        let cooldown_ms = self.content.travel.inn_cooldown_ms;
        let inn = self.world.get::<Player>(entity).and_then(|player| {
            let saved = &player.travel.saved;
            let ready = saved.inn_recalled_ms.saturating_add(cooldown_ms) <= unix_ms
                || saved.inn_recalled_ms == 0;
            saved.inn.filter(|_| ready)
        });
        match inn.and_then(|inn| self.content.travel.spawn_point(inn)) {
            Some([x, y, z]) => {
                self.start_channel(entity, number, Position { x, y, z }, now_ms, Purpose::Inn);
            }
            None => {
                tracing::info!(
                    player = number,
                    "inn recall refused (no inn or its cooldown runs)"
                );
                self.send_travel(entity, &[housing(number, fast_travel_failed)]);
            }
        }
    }

    /// Starts the fast travel of player `number` (`entity`) to `to` for `purpose`: refused while
    /// dead, while another travel channels, or while it carries more than its encumbrance limit.
    fn start_channel(
        &mut self,
        entity: Entity,
        number: u64,
        to: Position,
        now_ms: u64,
        purpose: Purpose,
    ) {
        let delay_ms = self.content.travel.channel_ms;
        self.start_channel_after(
            entity,
            number,
            to,
            None,
            now_ms.saturating_add(delay_ms),
            purpose,
        );
    }

    /// [`Self::start_channel`] facing `facing` at the end, due at `due_ms`.
    fn start_channel_after(
        &mut self,
        entity: Entity,
        number: u64,
        to: Position,
        facing: Option<f32>,
        due_ms: u64,
        purpose: Purpose,
    ) {
        let dead = self
            .world
            .get::<Vitals>(entity)
            .is_none_or(|vitals| vitals.health.is_dead());
        let encumbered = self.encumbered(entity);
        let channel = Channel {
            to,
            facing,
            due_ms,
            purpose,
        };
        let started = self
            .world
            .get_mut::<Player>(entity)
            .is_some_and(|mut player| {
                let free =
                    !dead && !encumbered && !player.frozen && player.travel.channel.is_none();
                if free {
                    player.travel.channel = Some(channel);
                    player.travel.due.housing = true;
                }
                free
            });
        if started {
            tracing::info!(
                player = number,
                x = to.x,
                y = to.y,
                ?purpose,
                "fast travel channels"
            );
        } else {
            tracing::info!(player = number, dead, encumbered, "fast travel refused");
            let failed = match purpose {
                Purpose::Teleporter => {
                    ServerMessage::Travel(TravelServerMsg::TeleportFailedOrCancelled {
                        header: header(player_component::PLAYER, number),
                    })
                }
                _ => housing(number, fast_travel_failed),
            };
            self.send_travel(entity, &[failed]);
        }
    }

    /// The weight of the bag of `entity`, in the units of the item tables.
    fn bag_load(&self, entity: Entity) -> f64 {
        let Some(gear) = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
        else {
            return 0.0;
        };
        let weights = &self.content.equip_loads;
        gear.carried()
            .iter()
            .filter(|item| matches!(item.place, nw_bus::ItemPlace::Bag(_)))
            .map(|item| weights.weight(&item.item_id) * f64::from(item.quantity))
            .sum()
    }

    /// Whether the bag of `entity` weighs more than the player's encumbrance limit
    /// (`eCanFastTravelToSettlementResults_IsEncumbered`).
    fn encumbered(&self, entity: Entity) -> bool {
        self.content
            .travel
            .encumbrance_max
            .is_some_and(|limit| self.bag_load(entity) > limit)
    }

    /// Ends the channel of player `number` (`entity`), when it has one, with 3042.
    fn cancel_channel(&mut self, entity: Entity, number: u64) {
        let cancelled = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| {
                player.travel.due.housing = true;
                player.travel.channel.take()
            })
            .is_some();
        if cancelled {
            tracing::info!(player = number, "fast travel cancelled");
            self.send_travel(entity, &[housing(number, fast_travel_failed)]);
        }
    }

    /// The client of `connection` sent the input records `records` at `now_ms`, before they move
    /// its player: a movement key cancels a fast travel's channel, and an open door pass takes
    /// the newest position when it crosses the door.
    pub fn travel_input(
        &mut self,
        connection: &ConnectionRef,
        records: &[InputRecord],
        now_ms: u64,
    ) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let Some((number, channeling, pass_until)) =
            self.world.get::<Player>(entity).map(|player| {
                (
                    player.number,
                    player.travel.channel.is_some(),
                    player.travel.door_pass_until_ms,
                )
            })
        else {
            return;
        };
        let moving = records.iter().any(|record| {
            let keys = record.move_keys();
            keys.forward || keys.back || keys.left || keys.right || keys.jump
        });
        if channeling && moving {
            self.cancel_channel(entity, number);
        }
        let Some(until) = pass_until else {
            return;
        };
        if now_ms > until {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.travel.door_pass_until_ms = None;
            }
            return;
        }
        let Some(newest) = records.iter().find_map(|record| record.bit_7) else {
            return;
        };
        let Some(at) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return;
        };
        let to = Position {
            x: newest.x,
            y: newest.y,
            z: newest.z(),
        };
        let step = at.horizontal_distance(to);
        if step > DOOR_SPHERE_RADIUS_M && step <= DOOR_SPHERE_RADIUS_M * DOOR_PASS_RADII {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.travel.door_pass_until_ms = None;
            }
            tracing::info!(player = number, step, "door pass");
            self.place_player(entity, to, None, Placement::Stand, now_ms);
        }
    }

    /// An unstuck request of player `number` (`entity`) at `now_ms`.
    fn unstuck_request(&mut self, entity: Entity, number: u64, now_ms: u64) {
        let unix_ms = self.unix_ms();
        let dead = self
            .world
            .get::<Vitals>(entity)
            .is_none_or(|vitals| vitals.health.is_dead());
        let rules = &self.content.travel;
        let (delay_ms, cooldown_ms) = (rules.unstuck_delay_ms, rules.unstuck_cooldown_ms);
        let unstuck = header(component::UNSTUCK, number);
        let Some(answer) = self.world.get_mut::<Player>(entity).map(|mut player| {
            let travel = &mut player.travel;
            let ready_ms = travel.saved.unstuck_ready_ms;
            let denial = if dead {
                Some(UnstuckDenial::PlayerDead)
            } else if travel.unstuck_due_ms.is_some() || travel.channel.is_some() {
                Some(UnstuckDenial::RequestAlreadyActive)
            } else if unix_ms < ready_ms {
                Some(UnstuckDenial::OnCooldown)
            } else if travel.breadcrumbs.is_empty() {
                Some(UnstuckDenial::NoBreadcrumbs)
            } else {
                None
            };
            match denial {
                Some(reason) => {
                    tracing::info!(player = number, ?reason, "unstuck denied");
                    TravelServerMsg::UnstuckDenied {
                        header: unstuck,
                        reason,
                        cooldown_ns: cooldown_left_ns(ready_ms, unix_ms),
                    }
                }
                None => {
                    travel.unstuck_due_ms = Some(now_ms.saturating_add(delay_ms));
                    travel.saved.unstuck_ready_ms = unix_ms.saturating_add(cooldown_ms);
                    tracing::info!(player = number, "unstuck accepted");
                    TravelServerMsg::UnstuckAccepted {
                        header: unstuck,
                        delay_ns: delay_ms.saturating_mul(NS_PER_MS),
                    }
                }
            }
        }) else {
            return;
        };
        self.send_travel(entity, &[ServerMessage::Travel(answer)]);
    }

    /// The channels, unstuck moves and markers due at `now_ms`, the travel states due, and the
    /// breadcrumbs of the players who stand somewhere new.
    pub(super) fn tick_travel(&mut self, now_ms: u64) {
        self.send_travel_states();
        if self.is_tutorial() {
            return;
        }
        let spacing_m = self.content.travel.breadcrumb_spacing_m;
        let max_crumbs = self.content.travel.max_breadcrumbs;
        let mut channels = Vec::new();
        let mut unstucks = Vec::new();
        let mut query = self.world.query::<(Entity, &mut Player, &Vitals)>();
        for (entity, mut player, vitals) in query.iter_mut(&mut self.world) {
            let number = player.number;
            if !vitals.health.is_dead()
                && let Some(at) = player.last_grounded
            {
                player.travel.drop_crumb(at, spacing_m, max_crumbs);
            }
            if player
                .travel
                .channel
                .is_some_and(|channel| channel.due_ms <= now_ms)
                && let Some(channel) = player.travel.channel.take()
            {
                player.travel.due.housing = true;
                channels.push((entity, number, channel));
            }
            if player
                .travel
                .unstuck_due_ms
                .is_some_and(|due_ms| due_ms <= now_ms)
            {
                player.travel.unstuck_due_ms = None;
                let target = player.travel.breadcrumbs.front().copied();
                unstucks.push((entity, number, target.filter(|_| !vitals.health.is_dead())));
            }
            if player.travel.waypoint_due && player.own_card_sent {
                player.travel.waypoint_due = false;
                let body = WaypointsState {
                    waypoint: player
                        .travel
                        .saved
                        .waypoint
                        .map(|point| point.point())
                        .map(|point| [point.x, point.y, point.z]),
                }
                .body();
                player.extra_members.push(BundleMember {
                    component: WAYPOINTS_COMPONENT,
                    state: MemberState::Recorded {
                        class: WAYPOINTS_TYPE_INDEX,
                        body,
                    },
                });
            }
        }
        for (entity, number, channel) in channels {
            self.complete_channel(entity, number, channel, now_ms);
        }
        for (entity, number, target) in unstucks {
            self.complete_unstuck(entity, number, target, now_ms);
        }
    }

    /// The played time of every player, and the travel states each client does not have yet
    /// (after its card).
    fn send_travel_states(&mut self) {
        let server_ms = self.server_ms;
        let rules = &self.content.travel;
        let mut query = self.world.query::<&mut Player>();
        for mut player in query.iter_mut(&mut self.world) {
            let number = player.number;
            let travel = &mut player.travel;
            let (start_ms, played_at_start_ms) = *travel
                .session
                .get_or_insert((server_ms, travel.saved.played_ms));
            travel.saved.played_ms =
                played_at_start_ms.saturating_add(server_ms.saturating_sub(start_ms));
            if !player.own_card_sent || player.travel.due == StatesDue::default() {
                continue;
            }
            let due = std::mem::take(&mut player.travel.due);
            let mut members = Vec::new();
            if due.time {
                let played_ns = played_at_start_ms.saturating_mul(NS_PER_MS);
                let body = PlayerTimeState {
                    start_time_point: Some(start_ms.saturating_mul(NS_PER_MS)),
                    duration_at_start_ns: Some(played_ns),
                    paid_duration_at_start_ns: Some(played_ns),
                }
                .body();
                members.push((PLAYER_TIME_COMPONENT, PLAYER_TIME_TYPE_INDEX, Ok(body)));
            }
            if due.housing {
                player.travel.houses_version = player.travel.houses_version.saturating_add(1);
                let body = housing_state(&player.travel).body();
                members.push((PLAYER_HOUSING_COMPONENT, PLAYER_HOUSING_TYPE_INDEX, body));
            }
            if due.homes {
                player.travel.homes_version = player.travel.homes_version.saturating_add(1);
                let body = home_state(&player.travel, rules).body();
                members.push((PLAYER_HOME_COMPONENT, PLAYER_HOME_TYPE_INDEX, body));
            }
            if due.camping {
                let travel = &player.travel;
                let body = CampingState {
                    current_camp: travel.camp.map(|camp| CampRef {
                        id: camp.gde_ref,
                        facet: camp.facet(),
                    }),
                    camp_skin: travel.saved.camp_skin,
                    under_attack: None,
                    bind_cooldown_end_ns: travel.saved.placed_camp.map(|camp| {
                        unix_ms_ns(camp.placed_ms.saturating_add(CAMP_BIND_COOLDOWN_MS))
                    }),
                }
                .body();
                members.push((CAMPING_COMPONENT, CAMPING_TYPE_INDEX, Ok(body)));
            }
            for (component, class, body) in members {
                match body {
                    Ok(body) => player.extra_members.push(BundleMember {
                        component,
                        state: MemberState::Recorded { class, body },
                    }),
                    Err(error) => {
                        tracing::warn!(player = number, class, %error, "travel state not written");
                    }
                }
            }
        }
    }

    /// The end of a fast travel's channel: what it was for (the cost, the inn and home
    /// cooldowns), the loading screen, the end of the channel, the move.
    fn complete_channel(&mut self, entity: Entity, number: u64, channel: Channel, now_ms: u64) {
        let unix_ms = self.unix_ms();
        let paid = match channel.purpose {
            Purpose::Landmark { azoth } => self.spend_azoth(entity, azoth),
            Purpose::Inn => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.travel.saved.inn_recalled_ms = unix_ms;
                    player.travel.due.homes = true;
                }
                true
            }
            Purpose::Home { plot } => {
                let cooldowns = &self.content.travel;
                if let Some(mut player) = self.world.get_mut::<Player>(entity)
                    && let Some(home) = player.travel.home_mut(&plot)
                {
                    let cooldown_ms = cooldowns
                        .house_type_of_crc(home.record.house_type)
                        .map_or(0, |house_type| house_type.cooldown_ms);
                    home.record.fast_travel_ready_ms = unix_ms.saturating_add(cooldown_ms);
                    player.travel.due.housing = true;
                    player.travel.due.homes = true;
                }
                true
            }
            Purpose::Teleporter => true,
        };
        if !paid {
            tracing::info!(player = number, "fast travel refused: not enough azoth");
            self.send_travel(entity, &[housing(number, fast_travel_failed)]);
            return;
        }
        let loading = ServerMessage::TeleportWithLoadingScreen(TeleportWithLoadingScreenMsg {
            header: header(player_component::PLAYER, number),
            flag: LOADING_SCREEN,
        });
        self.send_travel(entity, &[loading, housing(number, fast_travel_completed)]);
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            // The places behind the player are far away now.
            player.travel.breadcrumbs.clear();
        }
        let to = channel.to;
        tracing::info!(player = number, x = to.x, y = to.y, "fast travel arrives");
        self.mount_teleported(entity, now_ms);
        self.place_player(entity, to, channel.facing, Placement::Drop, now_ms);
    }

    /// Takes `azoth` from the azoth balance of `entity`: `false` (nothing taken) when it has
    /// less.
    fn spend_azoth(&mut self, entity: Entity, azoth: u32) -> bool {
        if azoth == 0 {
            return true;
        }
        let Some(currency) = self
            .content
            .progression
            .categorical(AZOTH_PROGRESSION)
            .cloned()
        else {
            return false;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return false;
        };
        let balance = player.masteries.experience(&currency);
        if balance < azoth {
            return false;
        }
        player.masteries.set(&currency, balance - azoth);
        true
    }

    /// The move of an accepted unstuck to `target`; `None` fails it (2774 teleport failed, and
    /// the cooldown ends).
    fn complete_unstuck(
        &mut self,
        entity: Entity,
        number: u64,
        target: Option<Position>,
        now_ms: u64,
    ) {
        let unstuck = header(component::UNSTUCK, number);
        let Some(to) = target else {
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.travel.saved.unstuck_ready_ms = 0;
            }
            tracing::info!(player = number, "unstuck move failed");
            self.send_travel(
                entity,
                &[ServerMessage::Travel(TravelServerMsg::UnstuckDenied {
                    header: unstuck,
                    reason: UnstuckDenial::TeleportFailed,
                    cooldown_ns: 0,
                })],
            );
            return;
        };
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.travel.breadcrumbs.retain(|crumb| *crumb == to);
        }
        tracing::info!(player = number, x = to.x, y = to.y, "unstuck");
        self.mount_teleported(entity, now_ms);
        self.place_player(entity, to, None, Placement::Stand, now_ms);
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::UnstuckCompleted {
                header: unstuck,
            })],
        );
    }

    /// Player `entity` entered territory `territory`: a discoverable one is discovered (its
    /// `DiscoveredAchievement`); `true` when this entry discovered it.
    pub(super) fn discover_territory(&mut self, entity: Entity, territory: u16) -> bool {
        let Some(achievement) = self
            .content
            .travel
            .discovered_achievement(u32::from(territory))
        else {
            return false;
        };
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && !player
                .flow
                .has_achievement(&self.achievement_rules, achievement)
        {
            player
                .flow
                .set_achievement(&self.achievement_rules, achievement);
            tracing::info!(player = player.number, territory, achievement, "discovered");
            return true;
        }
        false
    }

    /// Player `number` (`entity`) came into area `area` at `now_ms`: a chartable one it has not
    /// charted is charted ([`crate::charting`]): its `ChartedAchievement` is set, and the
    /// `Chart_Location` game event gives its experience (the event's `UniversalExpAmount`
    /// times the territory's `POIDiscoveryXPModifier`) and its standing to the territory.
    pub(super) fn chart_area(&mut self, entity: Entity, number: u64, area: u16, now_ms: u64) {
        let content = Arc::clone(&self.content);
        let charting = &content.charting;
        let Some(achievement) = charting.charted_achievement(u32::from(area)) else {
            return;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        if player
            .flow
            .has_achievement(&self.achievement_rules, achievement)
        {
            return;
        }
        player
            .flow
            .set_achievement(&self.achievement_rules, achievement);
        let amount = charting
            .territory_of(u32::from(area))
            .and_then(|territory| charting.experience(territory))
            .map_or(0, |territory| territory.chart_amount);
        let per_amount = content.rewards.experience(CHART_EVENT);
        gain_experience(
            &mut player,
            per_amount.saturating_mul(amount),
            &content.gameplay.xp,
        );
        tracing::info!(player = number, area, achievement, amount, "charted");
        self.grant_event_standing(number, CHART_EVENT);
        self.mark_quests_unsaved(number);
        self.quest_event(number, Event::AchievementSet(achievement), now_ms);
        self.quest_event(number, Event::GameEvent(CHART_EVENT), now_ms);
    }

    /// A travel interaction (`kind`) of player `number` (`entity`): `false` for another kind.
    pub(super) fn travel_interaction(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
        name: &[u8],
        now_ms: u64,
    ) -> bool {
        let accepted = gather_answer(number, request, ACCEPTED, name);
        match request.kind {
            kind::DOOR => {
                let until = now_ms.saturating_add(REFUSED_RESYNC_MS);
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.travel.door_pass_until_ms = Some(until);
                }
                self.send_travel(entity, &[accepted]);
            }
            kind::BIND_TO_CAMP => {
                self.despawn_camp(entity);
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.travel.saved.camp = Some(request.target_entity);
                    player.travel.saved.placed_camp = None;
                    player.travel.due.camping = true;
                    player.travel.due.homes = true;
                }
                let demolish = ServerMessage::Travel(TravelServerMsg::DemolishCamp {
                    header: header(component::CAMPING, number),
                });
                self.send_travel(entity, &[demolish, accepted]);
            }
            kind::OPEN_INN => self.send_travel(entity, &[accepted]),
            kind::SET_SPAWN_POINT => {
                if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                    player.travel.saved.inn = Some(request.target_entity);
                    player.travel.due.homes = true;
                }
                self.send_travel(entity, &[accepted]);
            }
            kind::TELEPORTER => {
                match self
                    .content
                    .travel
                    .teleporter(request.target_entity)
                    .copied()
                {
                    Some(teleporter) => {
                        self.send_travel(entity, &[accepted]);
                        let [x, y, z] = teleporter.destination;
                        self.start_channel_after(
                            entity,
                            number,
                            Position { x, y, z },
                            Some(teleporter.yaw_rad),
                            now_ms.saturating_add(teleporter.delay_ms),
                            Purpose::Teleporter,
                        );
                    }
                    None => {
                        tracing::info!(
                            player = number,
                            teleporter = request.target_entity,
                            "interaction with an unknown teleporter refused"
                        );
                        let refused = gather_answer(number, request, INTERACTION_REFUSED, name);
                        self.send_travel(entity, &[refused]);
                    }
                }
            }
            kind::HOUSING_PURCHASE | kind::HOUSING_ENTER => {
                self.note_plot(entity, request.target);
                self.send_travel(entity, &[accepted]);
            }
            kind::HOUSING_QUICK_ENTER => {
                self.note_plot(entity, request.target);
                self.send_travel(entity, &[accepted]);
                self.enter_plot(entity, number);
            }
            kind::HOUSING_EXIT => {
                let unnamed = gather_answer(number, request, EXITED, &[]);
                let named = gather_answer(number, request, EXITED, name);
                self.send_travel(entity, &[unnamed, named]);
            }
            _ => return false,
        }
        true
    }

    /// Notes the plot of a housing interaction of `entity`.
    fn note_plot(&mut self, entity: Entity, plot: [u8; RAW_ID_LEN]) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.travel.plot = Some(plot);
        }
    }

    /// Player `number` enters the house of its last housing interaction: the loading screen
    /// and the end of the channel (retail 3951 and 3696, 30 to 90 ms after the call).
    fn enter_plot(&mut self, entity: Entity, number: u64) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.travel.inside = player.travel.plot;
            player.travel.due.housing = true;
        }
        self.send_plot_transition(entity, number);
    }

    /// Player `number` leaves the house it is in; an empty plot (outside one) changes nothing.
    fn exit_plot(&mut self, entity: Entity, number: u64, plot: &PlotRef) {
        if plot.is_none() {
            return;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity) {
            player.travel.named_home(plot);
            player.travel.inside = None;
            player.travel.due.housing = true;
        }
        self.send_plot_transition(entity, number);
    }

    fn send_plot_transition(&mut self, entity: Entity, number: u64) {
        let loading = ServerMessage::TeleportWithLoadingScreen(TeleportWithLoadingScreenMsg {
            header: header(player_component::PLAYER, number),
            flag: LOADING_SCREEN,
        });
        self.send_travel(entity, &[loading, housing(number, fast_travel_completed)]);
    }

    /// Player `number` buys the home on the plot of its last housing interaction (the client's
    /// `CanPurchasePlot` order): refused on a plot it owns, with the most houses, with a house in
    /// the plot's territory, below the level `house-level-reqs` names, or without the coins.
    /// The house's taxes are due 7 days later; the house info follows (retail 3692 3.8 s after
    /// the 3700).
    fn buy_home(&mut self, entity: Entity, number: u64) {
        if self.purchase_home(entity, number) {
            self.plot_info(entity, number);
        }
    }

    /// The purchase of [`Self::buy_home`]: whether it went through.
    fn purchase_home(&mut self, entity: Entity, number: u64) -> bool {
        let unix_ms = self.unix_ms();
        let Some(at) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return false;
        };
        let rules = &self.content.travel;
        let progression = &self.content.progression;
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return false;
        };
        let Some(plot_raw) = player.travel.plot else {
            tracing::info!(player = number, "home bought without a plot interaction");
            return false;
        };
        let Some(plot) = rules.plot(id_of_raw(&plot_raw), [at.x, at.y, at.z]) else {
            tracing::info!(
                player = number,
                "home bought on a plot the level does not have"
            );
            return false;
        };
        let Some(house_type) = rules.house_type(plot.house_type) else {
            return false;
        };
        let territory = player
            .territories
            .iter()
            .copied()
            .find(|territory| rules.is_territory(u32::from(*territory)))
            .unwrap_or_default();
        let owned = player.travel.saved.homes.len();
        let cost = rules.housing.cost(house_type, owned);
        let level = progression.level_of(player.experience);
        let refusal = if player.travel.home(&plot_raw).is_some() {
            Some("it owns a house on the plot")
        } else if rules.housing.level_for(owned).is_none() {
            Some("it owns the most houses")
        } else if player
            .travel
            .saved
            .homes
            .iter()
            .any(|home| home.record.territory == territory)
        {
            Some("it owns a house in the territory")
        } else if rules
            .housing
            .level_for(owned)
            .is_some_and(|needed| level.saturating_add(1) < needed)
        {
            Some("its level is too low")
        } else if player.coins < cost {
            Some("it has too few coins")
        } else {
            None
        };
        if let Some(reason) = refusal {
            tracing::info!(player = number, reason, "home purchase refused");
            return false;
        }
        let [x, y, z] = plot.position;
        player.coins -= cost;
        player.coins_changed = true;
        player.travel.saved.homes.push(OwnedHome {
            plot: plot_raw,
            position: stored(Position { x, y, z }),
            purchased_ms: unix_ms,
            items: Vec::new(),
            record: HomeRecord {
                plot_facet: id_of_raw(&plot_raw),
                house_type: house_type.crc,
                territory,
                taxes_due_ms: HousingRules::first_due(unix_ms),
                ..HomeRecord::default()
            },
        });
        player.travel.due.housing = true;
        player.travel.due.homes = true;
        tracing::info!(player = number, cost, house_type = %house_type.id, "home bought");
        true
    }

    /// Player `number` gives up the home on the plot of its last housing interaction; its
    /// furniture goes back to the bag, and the house info follows (retail 3692 7.4 s after the
    /// 3701, with score 0).
    fn abandon_home(&mut self, entity: Entity, number: u64) {
        let Some(home) = self.world.get_mut::<Player>(entity).and_then(|mut player| {
            let plot = player.travel.plot?;
            let at = player
                .travel
                .saved
                .homes
                .iter()
                .position(|home| home.plot == plot)?;
            if player.travel.inside == Some(plot) {
                player.travel.inside = None;
            }
            player.travel.due.housing = true;
            player.travel.due.homes = true;
            Some(player.travel.saved.homes.remove(at))
        }) else {
            return;
        };
        tracing::info!(player = number, "home abandoned");
        self.refund_furniture(number, &home.items, self.server_ms);
        self.plot_info(entity, number);
    }

    /// Player `number` pays the taxes of its home on `plot` (952: paid or not).
    /// Player `number` (`entity`) picks camp skin `skin` (CRC-32 of the lowercase `CampSkinID`):
    /// it is kept for its next camp when the skin is enabled and the player holds its
    /// `RequiredAchievementId`; another skin changes nothing (Inferred: retail's refusal is
    /// Unknown, no capture picks a skin the player lacks).
    fn set_camp_skin(&mut self, entity: Entity, number: u64, skin: u32) {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let owned = self.content.camps.skin_of_crc(skin).is_some_and(|row| {
            row.enabled
                && row
                    .required_achievement
                    .as_deref()
                    .is_none_or(|achievement| {
                        player
                            .flow
                            .has_achievement(&self.achievement_rules, achievement)
                    })
        });
        if !owned {
            tracing::debug!(player = number, skin, "camp skin the player does not own");
            return;
        }
        player.travel.saved.camp_skin = Some(skin);
        player.travel.due.camping = true;
    }

    fn pay_taxes(&mut self, entity: Entity, number: u64, plot: &PlotRef) {
        let unix_ms = self.unix_ms();
        let rules = &self.content.travel;
        let paid = self
            .world
            .get_mut::<Player>(entity)
            .is_some_and(|mut player| {
                let Some(house_type) = player
                    .travel
                    .home(&plot.id)
                    .and_then(|home| rules.house_type_of_crc(home.record.house_type))
                else {
                    return false;
                };
                let tax = u64::from(rules.housing.tax(house_type));
                if player.coins < tax {
                    return false;
                }
                player.coins -= tax;
                player.coins_changed = true;
                player.travel.due.housing = true;
                player.travel.named_home(plot).is_some_and(|home| {
                    home.record.taxes_due_ms =
                        HousingRules::paid_until(home.record.taxes_due_ms, unix_ms);
                    true
                })
            });
        tracing::info!(player = number, paid, "taxes paid");
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::PayTaxFinished {
                header: header(component::PLAYER_HOUSING, number),
                paid,
            })],
        );
    }

    /// Player `number` asks for the taxes due on its home on `plot` (3694; retail 31 ms later).
    fn taxes_due(&mut self, entity: Entity, number: u64, plot: &PlotRef) {
        let rules = &self.content.travel;
        let amount = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| {
                let house_type = player.travel.named_home(plot)?.record.house_type;
                rules.house_type_of_crc(house_type)
            })
            .map_or(0, |house_type| rules.housing.tax(house_type));
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::TaxesDue {
                header: header(component::PLAYER_HOUSING, number),
                amount,
            })],
        );
    }

    /// The information of the plot of player `number`'s last housing interaction (3692; retail
    /// 42 ms after the 3717): the next top-house evaluation and the house's score.
    fn plot_info(&mut self, entity: Entity, number: u64) {
        let unix_ms = self.unix_ms();
        let rules = &self.content.travel;
        let Some((plot, points)) = self.world.get::<Player>(entity).and_then(|player| {
            let raw = player.travel.plot?;
            Some(match player.travel.home(&raw) {
                Some(home) => (
                    home_ref(home),
                    home.items
                        .iter()
                        .filter_map(|placed| rules.furniture(placed.item))
                        .map(|furniture| furniture.ranking_points)
                        .sum(),
                ),
                None => (entity_ref(id_of_raw(&raw)), 0.0),
            })
        }) else {
            return;
        };
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::HouseInfo {
                header: header(component::PLAYER_HOUSING, number),
                next_recompute_ns: HousingRules::next_recompute_ns(unix_ms),
                house_score: HousingRules::house_score(points),
                plot,
            })],
        );
    }

    /// The data of the territories player `number` resides in (3695): one per territory of its
    /// houses, with its territory's status effects, which this server does not run (none).
    fn resident_data(&mut self, entity: Entity, number: u64) {
        let Some(mut territories) = self.world.get::<Player>(entity).map(|player| {
            player
                .travel
                .saved
                .homes
                .iter()
                .map(|home| home.record.territory)
                .collect::<Vec<_>>()
        }) else {
            return;
        };
        territories.sort_unstable();
        territories.dedup();
        let territories = territories
            .into_iter()
            .map(|territory| {
                (
                    territory,
                    TerritoryResidentData {
                        territory_actor: [0; RAW_ID_LEN],
                        status_effects: Vec::new(),
                    },
                )
            })
            .collect();
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::ResidentData {
                header: header(component::PLAYER_HOUSING, number),
                territories,
            })],
        );
    }

    /// Player `number` resets the fast travel cooldown of its home on `plot` (1998) for its
    /// azoth: the cooldown ends now (retail sends no answer but the housing state).
    fn reset_cooldown(&mut self, entity: Entity, number: u64, plot: &PlotRef) {
        let unix_ms = self.unix_ms();
        let rules = &self.content.travel;
        let Some((left_ms, cooldown_ms)) =
            self.world.get_mut::<Player>(entity).and_then(|mut player| {
                let home = player.travel.named_home(plot)?;
                let cooldown_ms = rules.house_type_of_crc(home.record.house_type)?.cooldown_ms;
                Some((
                    home.record.fast_travel_ready_ms.saturating_sub(unix_ms),
                    cooldown_ms,
                ))
            })
        else {
            return;
        };
        if left_ms == 0 {
            return;
        }
        let azoth = rules.housing.reset_cost(left_ms, cooldown_ms);
        if !self.spend_azoth(entity, azoth) {
            tracing::info!(
                player = number,
                azoth,
                "home cooldown reset refused: too little azoth"
            );
            return;
        }
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(home) = player.travel.home_mut(&plot.id)
        {
            home.record.fast_travel_ready_ms = unix_ms;
            player.travel.due.housing = true;
            player.travel.due.homes = true;
        }
        tracing::info!(player = number, azoth, "home cooldown reset");
    }

    /// The home of the house `entity` is in, when it owns it.
    fn owned_home_inside(&self, entity: Entity) -> Option<[u8; RAW_ID_LEN]> {
        let player = self.world.get::<Player>(entity)?;
        let plot = player.travel.inside?;
        player.travel.home(&plot).map(|_| plot)
    }

    /// Player `number` places `item` (a client item index) in the home it is in: the item leaves
    /// its bag, at the next index above the home's items (retail: 24, then 25). Refused for an
    /// item that is not furniture or not in the bag, and in a full house.
    fn place_item(&mut self, entity: Entity, number: u64, item: u32, placement: &ItemPlacement) {
        let plot = self.owned_home_inside(entity);
        let rules = &self.content.travel;
        let furniture = rules.furniture(item);
        let (result, index) = match plot.and_then(|plot| {
            let mut player = self.world.get_mut::<Player>(entity)?;
            let max_items = player
                .travel
                .home(&plot)
                .and_then(|home| rules.house_type_of_crc(home.record.house_type))
                .map_or(usize::MAX, |house_type| house_type.max_items);
            let Some(furniture) = furniture else {
                return Some((DECORATION_INVALID_ITEM_ID, u16::MAX));
            };
            let home = player.travel.home(&plot)?;
            let index = match home.items.iter().map(|placed| placed.index).max() {
                Some(top) => top.checked_add(1),
                None => Some(0),
            };
            let (Some(index), true) = (index, home.items.len() < max_items) else {
                return Some((DECORATION_ITEM_ID_EXCEEDED, u16::MAX));
            };
            let taken = player
                .gear
                .as_mut()
                .is_some_and(|gear| gear.take(&furniture.item_id, 1));
            if !taken {
                return Some((DECORATION_INVALID_ITEM_ID, u16::MAX));
            }
            let [x, y, z] = placement.position;
            let home = player.travel.home_mut(&plot)?;
            home.items.push(HousingItem {
                index,
                item,
                position: StoredPoint::of(Point { x, y, z }),
                rotation: rotation_bits(placement),
            });
            Some((DECORATION_SUCCESS, index))
        }) {
            Some(done) => done,
            None => (DECORATION_NOT_OWNER, u16::MAX),
        };
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::PlaceItemResult {
                header: header(component::PLAYER_HOUSING, number),
                result,
                index,
            })],
        );
    }

    /// Gives the furniture `items` back to the bag of player `number`.
    fn refund_furniture(&mut self, number: u64, items: &[HousingItem], now_ms: u64) {
        let refund: Vec<(String, u32)> = items
            .iter()
            .filter_map(|placed| self.content.travel.furniture(placed.item))
            .map(|furniture| (furniture.item_id.clone(), 1))
            .collect();
        if !refund.is_empty() {
            self.give_loot(number, refund, now_ms);
        }
    }

    /// Player `number` picks every placed piece of furniture of the home it is in up (5490):
    /// they go back to its bag.
    fn remove_all_furniture(&mut self, entity: Entity, number: u64, now_ms: u64) {
        let Some(plot) = self.owned_home_inside(entity) else {
            return;
        };
        let items = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| {
                let home = player.travel.home_mut(&plot)?;
                Some(std::mem::take(&mut home.items))
            })
            .unwrap_or_default();
        self.refund_furniture(number, &items, now_ms);
    }

    /// What player `entity` places with `placement` (2236): the camp of its `Camping` rank's
    /// tier, of the blueprint the placement names (the default camp of the tier when it names
    /// none), in a territory that allows camps, of a skin it may use, with a slice and its costs
    /// in the bag.
    fn camp_placement(
        &self,
        entity: Entity,
        placement: &StructurePlacement,
    ) -> Result<CampPlacement, CampRefusal> {
        let player = self.world.get::<Player>(entity).ok_or(CampRefusal::Dead)?;
        if self
            .world
            .get::<Vitals>(entity)
            .is_none_or(|vitals| vitals.health.is_dead())
        {
            return Err(CampRefusal::Dead);
        }
        let tier = u32::from(
            player
                .masteries
                .rank(&self.content.progression, CAMPING_PROGRESSION),
        );
        if tier == 0 {
            return Err(CampRefusal::Rank);
        }
        if !self.camp_allowed_where(player) {
            return Err(CampRefusal::Territory);
        }
        let camps = &self.content.camps;
        let named = camps
            .blueprint_of_crc(placement.skin)
            .or_else(|| camps.blueprint(&default_camp(tier)))
            .ok_or(CampRefusal::Blueprint)?;
        let blueprint = camps.placed(named, player.pvp_flagged);
        if let Some(skin) = camps.skin_of(blueprint) {
            let achieved = skin
                .required_achievement
                .as_deref()
                .is_none_or(|achievement| {
                    player
                        .flow
                        .has_achievement(&self.achievement_rules, achievement)
                });
            if !skin.enabled || !achieved {
                return Err(CampRefusal::Skin);
            }
        }
        if self.content.camp_slices.get(&blueprint.id).is_none() {
            return Err(CampRefusal::Slice);
        }
        let in_bag = player.gear.as_ref().is_some_and(|gear| {
            self.content
                .structure_costs
                .of(&blueprint.id)
                .iter()
                .all(|(item, quantity)| gear.bag_count(item) >= *quantity)
        });
        if !in_bag {
            return Err(CampRefusal::Cost);
        }
        let [x, y, z] = placement.position;
        Ok(CampPlacement {
            blueprint: blueprint.id.clone(),
            position: Position { x, y, z },
            rotation: placement.rotation,
            tier,
        })
    }

    /// Whether `player` stands where a camp may go: in no area that forbids camps (the points of
    /// interest: `CanPlaceCamp` false), and in an area or a zone that allows them, the zones of
    /// its areas and its last known zone among them.
    fn camp_allowed_where(&self, player: &Player) -> bool {
        let rules = &self.content.travel;
        let charting = &self.content.charting;
        let areas: Vec<u32> = player
            .territories
            .iter()
            .map(|area| u32::from(*area))
            .collect();
        let in_forbidden_area = areas
            .iter()
            .any(|area| !rules.is_territory(*area) && !rules.can_place_camp(*area));
        let zones = areas
            .iter()
            .filter_map(|area| charting.territory_of(*area))
            .chain(player.last_zone);
        !in_forbidden_area
            && areas
                .iter()
                .copied()
                .chain(zones)
                .any(|territory| rules.can_place_camp(territory))
    }

    /// Player `number` places its camp (2236, [`SimWorld::camp_placement`]): the costs leave its
    /// bag, a former camp goes (6760), the camp stands in the world and the `BindCamp` game event
    /// fires. Answered by 441; a refusal changes nothing.
    fn place_camp(
        &mut self,
        entity: Entity,
        number: u64,
        placement: &StructurePlacement,
        now_ms: u64,
    ) {
        let unix_ms = self.unix_ms();
        let placed = self.camp_placement(entity, placement);
        tracing::info!(player = number, ?placed, "camp placement");
        let had_camp = self
            .world
            .get::<Player>(entity)
            .is_some_and(|player| player.travel.saved.placed_camp.is_some());
        let mut answers = Vec::new();
        if let Ok(camp) = &placed {
            let costs = self.content.structure_costs.of(&camp.blueprint).to_vec();
            if let Some(mut player) = self.world.get_mut::<Player>(entity)
                && let Some(gear) = player.gear.as_mut()
            {
                for (item, quantity) in &costs {
                    gear.take(item, *quantity);
                }
            }
            self.despawn_camp(entity);
            if had_camp {
                answers.push(ServerMessage::Travel(TravelServerMsg::DemolishCamp {
                    header: header(component::CAMPING, number),
                }));
            }
            let spawned = self.spawn_camp(entity, camp);
            if let Some(mut player) = self.world.get_mut::<Player>(entity) {
                player.travel.camp = spawned;
                player.travel.saved.placed_camp = Some(PlacedCamp {
                    position: stored(camp.position),
                    tier: u8::try_from(camp.tier).unwrap_or(u8::MAX),
                    placed_ms: unix_ms,
                    rotation: camp.rotation.map(f32::to_bits),
                    blueprint: nw_formats::objectstream::lowercase_crc32(&camp.blueprint),
                });
                player.travel.due.camping = true;
                player.travel.due.homes = true;
            }
            // Inferred: a placed camp is the player's bind point, the `BindCamp` game event that
            // the camp tutorial's task counts (`12A_MSQ_12_CampTutorial`, `TaskGameEvent`).
            self.fire_game_event(number, BIND_CAMP_EVENT, now_ms);
        }
        answers.push(ServerMessage::Travel(TravelServerMsg::PlacementResults {
            header: header(BUILDER_COMPONENT, number),
            result: if placed.is_ok() {
                PLACEMENT_PLACED
            } else {
                PLACEMENT_REFUSED
            },
            value: PLACEMENT_VALUE,
        }));
        self.send_travel(entity, &answers);
    }

    /// Where player `entity` respawns at its own home point `point`: its camp's respawn point or
    /// one of its houses.
    pub(super) fn own_spawn_point(&self, entity: Entity, point: u64) -> Option<[f32; 3]> {
        let travel = &self.world.get::<Player>(entity)?.travel;
        if let Some(camp) = travel.camp.filter(|camp| camp.facet() == point) {
            let Position { x, y, z } = camp.spawn_point;
            return Some([x, y, z]);
        }
        travel
            .saved
            .homes
            .iter()
            .find(|home| home_ref(home).facet == point)
            .map(|home| {
                let Point { x, y, z } = home.position.point();
                [x, y, z]
            })
    }

    /// Player `number` moves item `index` of the home it is in.
    fn move_item(&mut self, entity: Entity, number: u64, index: u16, placement: &ItemPlacement) {
        let result = self.change_item(entity, index, |items| {
            let item = items.iter_mut().find(|placed| placed.index == index)?;
            let [x, y, z] = placement.position;
            item.position = StoredPoint::of(Point { x, y, z });
            item.rotation = rotation_bits(placement);
            Some(())
        });
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::MoveItemResult {
                header: header(component::PLAYER_HOUSING, number),
                result,
                index,
            })],
        );
    }

    /// Player `number` picks item `index` of the home it is in up: it goes back to the bag.
    fn remove_item(&mut self, entity: Entity, number: u64, index: u16, now_ms: u64) {
        let mut removed = None;
        let result = self.change_item(entity, index, |items| {
            let at = items.iter().position(|placed| placed.index == index)?;
            removed = Some(items.remove(at));
            Some(())
        });
        if let Some(placed) = removed {
            self.refund_furniture(number, &[placed], now_ms);
        }
        self.send_travel(
            entity,
            &[ServerMessage::Travel(TravelServerMsg::RemoveItemResult {
                header: header(component::PLAYER_HOUSING, number),
                result,
                index,
            })],
        );
    }

    /// Changes the items of the home `entity` is in with `change`: the decoration result.
    fn change_item(
        &mut self,
        entity: Entity,
        index: u16,
        change: impl FnOnce(&mut Vec<HousingItem>) -> Option<()>,
    ) -> u8 {
        let Some(plot) = self.owned_home_inside(entity) else {
            return DECORATION_NOT_OWNER;
        };
        let changed = self
            .world
            .get_mut::<Player>(entity)
            .and_then(|mut player| {
                let home = player.travel.home_mut(&plot)?;
                change(&mut home.items)
            })
            .is_some();
        if changed {
            DECORATION_SUCCESS
        } else {
            tracing::info!(index, "housing item not found");
            DECORATION_INVALID_ITEM_ID
        }
    }
}

/// The housing state of `travel`: retail's channeling flag alone without a house; with houses,
/// the purchase flag and the owned houses; inside a house, the plot and the house entity.
fn housing_state(travel: &TravelState) -> PlayerHousingState {
    let homes = &travel.saved.homes;
    let houses = homes
        .iter()
        .map(|home| OwnedHouse {
            plot: home_ref(home),
            house_data: HouseDataRef {
                id: home.record.house_data,
                facet: home.record.house_data_facet,
            },
            position: {
                let Point { x, y, z } = home.position.point();
                [x, y, z]
            },
            taxes_due_ns: unix_ms_ns(home.record.taxes_due_ms),
            purchased_ns: unix_ms_ns(home.purchased_ms),
            territory: home.record.territory,
            buff_items_active: HOUSE_HANDSHAKE,
            trophies: Vec::new(),
            decoration_score: NO_DECORATION_SCORE,
            flag: HOUSE_HANDSHAKE,
            handshake: HOUSE_HANDSHAKE,
            fast_travel_ready_ns: unix_ms_ns(home.record.fast_travel_ready_ms),
            house_type: home.record.house_type,
        })
        .collect::<Vec<_>>();
    let owned = !houses.is_empty();
    let inside = travel
        .inside
        .and_then(|plot| travel.home(&plot))
        .map(|home| (home_ref(home).facet, home.record.house_data_facet));
    PlayerHousingState {
        has_purchased_house: owned.then_some(true),
        owned_houses: owned.then_some(FullList {
            version: Some(travel.houses_version),
            elements: houses,
        }),
        last_house_request_response: owned.then_some(NO_HOUSE_RESPONSE),
        is_within_a_plot: owned.then_some(inside.is_some()),
        is_fast_travel_channeling: Some(travel.channel.is_some()),
        phased_plot: owned.then_some(inside.map_or(NO_PHASED_ENTITY, |(plot, _)| plot)),
        phased_house_data: owned.then_some(inside.map_or(NO_PHASED_ENTITY, |(_, data)| {
            if data == 0 { NO_PHASED_ENTITY } else { data }
        })),
        phased_owner: None,
        debug_data: None,
    }
}

/// The home points of `travel`: its bound inn, its houses and its camp's respawn point, and
/// the bound inn's id as the chosen point.
fn home_state(travel: &TravelState, rules: &TravelRules) -> PlayerHomeState {
    let saved = &travel.saved;
    let point =
        |entity: u64, name: Vec<u8>, position: [f32; 3], cooldown_ns, used_ns, kind| HomePoint {
            entity: entity_ref(entity),
            name,
            position,
            cooldown_ns,
            used_ns,
            kind,
            flag: false,
            extra: 0,
            id: home_point_id(entity, SINGLE_POINT),
            index: SINGLE_POINT,
        };
    let inn = saved.inn.and_then(|inn| {
        // An inn's name (`@NN_InnName`) is its slice's, which the level data does not carry
        // here; retail sends shrines with an empty name too.
        Some(point(
            inn,
            Vec::new(),
            rules.spawn_point(inn)?,
            rules.inn_cooldown_ms.saturating_mul(NS_PER_MS),
            unix_ms_ns(saved.inn_recalled_ms),
            home_point_kind::INN,
        ))
    });
    let houses = saved.homes.iter().map(|home| {
        let Point { x, y, z } = home.position.point();
        point(
            home_ref(home).facet,
            HOUSE_POINT_NAME.to_vec(),
            [x, y, z],
            0,
            unix_ms_ns(home.record.fast_travel_ready_ms),
            home_point_kind::HOUSE,
        )
    });
    let camp = travel.camp.zip(saved.placed_camp).map(|(camp, placed)| {
        let Position { x, y, z } = camp.spawn_point;
        HomePoint {
            entity: PlotRef {
                id: camp.gde_ref,
                // The third id of retail's camp element (`40124f2ca3b07621` in `Hivecamp`) is of
                // unknown meaning.
                ..entity_ref(camp.facet())
            },
            ..point(
                camp.facet(),
                format!("@CampT{}_MasterName", placed.tier).into_bytes(),
                [x, y, z],
                CAMP_COOLDOWN_NS,
                0,
                home_point_kind::CAMP,
            )
        }
    });
    PlayerHomeState {
        home_points: Some(FullList {
            version: Some(travel.homes_version),
            elements: inn.into_iter().chain(houses).chain(camp).collect(),
        }),
        home_point_id: saved.inn.map(|inn| home_point_id(inn, SINGLE_POINT)),
    }
}

/// Whether `player` waits for the own states of a server move (a fast travel's or an unstuck's
/// arrival): its handoff waits until they are out, so the sim that moved it sends them.
pub(super) fn moved_by_server(player: &Player) -> bool {
    player.moved || player.teleport_step.is_some()
}

fn fast_travel_failed(header: FacetHeader) -> TravelServerMsg {
    TravelServerMsg::FastTravelFailed { header }
}

fn fast_travel_completed(header: FacetHeader) -> TravelServerMsg {
    TravelServerMsg::FastTravelCompleted { header }
}

/// The cooldown left at `unix_ms` of a cooldown that ends at `ready_ms`, in nanoseconds:
/// negative once it ended, as retail sends it.
fn cooldown_left_ns(ready_ms: u64, unix_ms: u64) -> i64 {
    let ready_ns = i128::from(ready_ms) * i128::from(NS_PER_MS);
    let now_ns = i128::from(unix_ms) * i128::from(NS_PER_MS);
    i64::try_from(ready_ns - now_ns).unwrap_or(if ready_ns > now_ns {
        i64::MAX
    } else {
        i64::MIN
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use nw_content::achievements::AchievementIndexes;
    use nw_content::gameplay_config::TravelKeys;
    use nw_content::starter::StarterKits;
    use nw_formats::objectstream::lowercase_crc32;
    use nw_messages::travel::LandmarkRef;
    use nw_messages::{InputPosition, InputSlots, InteractionValidatedMsg};
    use nw_world::travel_slices::{HousingPlot, Teleporter};

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::travel_rules::{Furniture, HouseType, TravelLandmark, TravelRules};
    use crate::world::level_flow::AchievementRules;
    use crate::world::tests::{NOW, connection, join, messages_to, tick, world_with_spawns};
    use nw_content::progression::{Mastery, Progression, ProgressionSource};

    const PLAYER: u64 = 7;
    const START_X: f32 = 100.0;
    /// The retail shrine type of 3713 and a shrine of the test level.
    const SHRINE_TYPE: u8 = 27;
    const SHRINE_TERRITORY: u16 = 20_422;
    const SHRINE: [f32; 3] = [500.0, 10.0, 60.0];
    const SHRINE_RADIUS_M: f32 = 6.0;
    /// A discoverable territory of the test level and its achievement.
    const HIDDEN_TERRITORY: u16 = 40_406;
    const DISCOVERED: &str = "Discovered_40406";
    const DISCOVERED_BIT: u32 = 3;
    /// The retail channel and unstuck timings (rules without keys).
    const CHANNEL_MS: u64 = 10_000;
    const UNSTUCK_DELAY_MS: u64 = 10_000;
    const UNSTUCK_COOLDOWN_NS: i64 = 120_000_000_000;
    /// Slots of a client record and the forward key's slot value and bit (movement study).
    const SLOT_VALUES: usize = 10;
    const FORWARD_VALUE: usize = 6;
    const FORWARD_BIT: u32 = 32;
    const NEUTRAL_SLOT_BYTE: u8 = 128;
    const ACTION_SLOTS: u8 = 19;
    /// A plot's raw id (retail form: 8 id bytes, 8 zero bytes).
    const PLOT: [u8; RAW_ID_LEN] = [
        0xc7, 0x37, 0x66, 0xcd, 0xa0, 0x43, 0x41, 0x6a, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    /// A respawn camp's facet entity (retail NH-MSQ-1 bind, a respawn point of the captures).
    const CAMP: u64 = 16_695_474_225_659_806_323;
    /// An inn spawn point's facet entity (retail S8 check-in) and its place in the test level.
    const INN: u64 = 17_680_737_194_352_937_251;
    const INN_AT: [f32; 3] = [700.0, 10.0, 60.0];
    /// The retail inn recall cooldown.
    const INN_COOLDOWN_MS: u64 = 900_000;
    /// Where the test player stands, and the plot there.
    const HERE: [f32; 3] = [START_X, 10.0, 50.0];
    /// A territory that allows camps and houses (the Hive house plot's).
    const TERRITORY: u16 = 16;
    /// `HouseTypeTier2`: its cost and its fast travel cooldown.
    const TIER_2: &str = "HouseTypeTier2";
    const TIER_2_COST: u64 = 1_000_000;
    const TIER_2_COOLDOWN_MS: u64 = 180 * 60_000;
    /// A bed (retail 3277 item 45982), its catalog index in the test content and its points.
    const BED: &str = "House_HousingItem_Bed_Settler_Bed02_C";
    const BED_INDEX: u32 = 1;
    const BED_POINTS: f64 = 0.12;
    /// A teleporter capital (the MB watchtower GUID) and where it goes.
    const TELEPORTER_GUID: &str = "f6593b66-abc3-7460-7a4d-5afaeb8b2620";
    const TELEPORTER_ID: u64 = 0x8c14_619c_4048_5243;
    const TELEPORTER_TO: [f32; 3] = [3_473.92, 6_723.95, 86.13];
    const TELEPORTER_YAW: f32 = -1.571;
    const TELEPORTER_DELAY_MS: u64 = 1_000;
    /// A retail camp placement's skin (`campskin_egyptian02_a`).
    const CAMP_SKIN: u32 = 0xbff6_8c5c;
    /// The azoth cap (`Azoth_Currency` rank 0 `MaximumInfluence`, content bundle 25810808), the
    /// test player's balance (less than a home cooldown reset costs) and the retail fast travel
    /// costs to another territory and within the player's.
    const AZOTH_CAP: i64 = 2_500;
    const START_AZOTH: u32 = 50;
    const AZOTH_TO_ANOTHER_TERRITORY: u32 = 20;
    const AZOTH_WITHIN_THE_TERRITORY: u32 = 10;

    fn world() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        let mut travel = TravelRules::build(
            vec![TravelLandmark {
                landmark_type: SHRINE_TYPE,
                position: SHRINE,
                radius: SHRINE_RADIUS_M,
                territory: Some(u32::from(TERRITORY)),
            }],
            HashMap::from([(u32::from(HIDDEN_TERRITORY), DISCOVERED.to_owned())]),
            HashMap::from([(INN, INN_AT)]),
            None,
            TravelKeys::default(),
        )
        .with_housing(
            vec![HouseType {
                id: TIER_2.to_owned(),
                crc: lowercase_crc32(TIER_2),
                cost: TIER_2_COST,
                cooldown_ms: TIER_2_COOLDOWN_MS,
                max_items: 2,
            }],
            &[HousingPlot {
                id: id_of_raw(&PLOT),
                slice: "slices/housing/plots/test/city_house_address_001".to_owned(),
                position: HERE,
                house_type: TIER_2.to_owned(),
            }],
            HashMap::from([(
                BED_INDEX,
                Furniture {
                    item_id: BED.to_owned(),
                    ranking_points: BED_POINTS,
                },
            )]),
        )
        .with_teleporters(&[Teleporter {
            capital: TELEPORTER_GUID.to_owned(),
            destination: TELEPORTER_TO,
            yaw_rad: TELEPORTER_YAW,
            channel: 1,
        }])
        .with_camp_territories([u32::from(TERRITORY)])
        .with_territories([u32::from(TERRITORY)]);
        travel.housing.level_reqs = vec![1, 1];
        world.content = Arc::new(PlayerContent {
            travel,
            kits: StarterKits::default().with_housing([(BED.to_owned(), Some(1))]),
            progression: Progression::default().with_progressions([ProgressionSource {
                id: AZOTH_PROGRESSION.to_owned(),
                max_level: 0,
                auto_rank_up: false,
                ranks: vec![(0, AZOTH_CAP)],
            }]),
            ..PlayerContent::default()
        });
        world.achievement_rules = AchievementRules::of(
            &AchievementIndexes::build([(DISCOVERED.to_owned(), DISCOVERED_BIT)]),
            &[],
        );
        join(&mut world, PLAYER, START_X);
        set_azoth(&mut world, START_AZOTH);
        world
    }

    fn azoth_currency(world: &SimWorld) -> Mastery {
        world
            .content
            .progression
            .categorical(AZOTH_PROGRESSION)
            .cloned()
            .expect("azoth")
    }

    fn set_azoth(world: &mut SimWorld, azoth: u32) {
        let currency = azoth_currency(world);
        let entity = entity(world);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .masteries
            .set(&currency, azoth);
    }

    fn azoth(world: &mut SimWorld) -> u32 {
        let currency = azoth_currency(world);
        let entity = entity(world);
        world
            .world
            .get::<Player>(entity)
            .expect("player")
            .masteries
            .experience(&currency)
    }

    fn entity(world: &mut SimWorld) -> Entity {
        world.player_by_number(PLAYER).expect("player")
    }

    fn body(world: &mut SimWorld) -> Position {
        let entity = entity(world);
        world
            .world
            .get::<Body>(entity)
            .expect("body")
            .motion
            .position()
    }

    fn call(world: &mut SimWorld, call: TravelClientMsg, now_ms: u64) {
        world.travel_call(&connection(PLAYER), call, now_ms);
    }

    fn landmark(territory: u16, x: f32) -> TravelClientMsg {
        TravelClientMsg::FastTravelLandmark {
            header: header(component::PLAYER_HOUSING, PLAYER),
            landmark: LandmarkRef {
                territory,
                landmark_type: SHRINE_TYPE,
                position: [x, SHRINE[1], 0.0],
            },
        }
    }

    /// The travel messages of one tick at `now_ms`: the travel answers, the loading screens and
    /// the interaction answers.
    fn answers(world: &mut SimWorld, now_ms: u64) -> Vec<ServerMessage> {
        messages_to(&tick(world, now_ms).messages, PLAYER)
            .into_iter()
            .filter(|message| {
                matches!(
                    message,
                    ServerMessage::Travel(_)
                        | ServerMessage::TeleportWithLoadingScreen(_)
                        | ServerMessage::InteractionValidated(_)
                )
            })
            .collect()
    }

    fn travel_answers(world: &mut SimWorld, now_ms: u64) -> Vec<TravelServerMsg> {
        answers(world, now_ms)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::Travel(answer) => Some(answer),
                _ => None,
            })
            .collect()
    }

    fn forward_record() -> InputRecord {
        let mut values = vec![0; SLOT_VALUES];
        values[FORWARD_VALUE] = 1 << FORWARD_BIT;
        InputRecord {
            slots: InputSlots {
                count: ACTION_SLOTS,
                values,
                bytes: [NEUTRAL_SLOT_BYTE; 4],
            },
            ..InputRecord::default()
        }
    }

    fn position_record(x: f32) -> InputRecord {
        InputRecord {
            bit_7: InputPosition::from_metres(x, 10.0, 50.0),
            ..InputRecord::default()
        }
    }

    fn request(kind: u8, target_entity: u64) -> InteractionRequest {
        InteractionRequest {
            target: PLOT,
            target_entity,
            interaction: 1,
            kind,
            gatherable: None,
        }
    }

    fn validated(messages: &[ServerMessage]) -> Vec<(u8, u8)> {
        messages
            .iter()
            .filter_map(|message| match message {
                ServerMessage::InteractionValidated(InteractionValidatedMsg {
                    kind,
                    status,
                    ..
                }) => Some((*kind, *status)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_fast_travel_channels_then_moves_the_player_after_the_loading_screen() {
        let mut world = world();
        call(&mut world, landmark(SHRINE_TERRITORY, SHRINE[0] + 1.0), NOW);
        assert!(travel_answers(&mut world, NOW + CHANNEL_MS - 1).is_empty());
        assert!(
            (body(&mut world).x - START_X).abs() < f32::EPSILON,
            "still channels"
        );
        let messages = answers(&mut world, NOW + CHANNEL_MS);
        assert!(matches!(
            messages.as_slice(),
            [
                ServerMessage::TeleportWithLoadingScreen(TeleportWithLoadingScreenMsg {
                    flag: true,
                    ..
                }),
                ServerMessage::Travel(TravelServerMsg::FastTravelCompleted { .. })
            ]
        ));
        let at = body(&mut world);
        assert_eq!([at.x, at.y], [SHRINE[0], SHRINE[1]], "at the shrine");
        let entity = entity(&mut world);
        assert!(moved_by_server(
            world.world.get::<Player>(entity).expect("player")
        ));
    }

    #[test]
    fn a_movement_key_or_a_cancel_ends_the_channel_with_3042() {
        let mut world = world();
        call(&mut world, landmark(SHRINE_TERRITORY, SHRINE[0]), NOW);
        world.travel_input(&connection(PLAYER), &[forward_record()], NOW + 1);
        assert!(matches!(
            travel_answers(&mut world, NOW + 2).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
        assert!(travel_answers(&mut world, NOW + CHANNEL_MS).is_empty());
        call(
            &mut world,
            landmark(SHRINE_TERRITORY, SHRINE[0]),
            NOW + CHANNEL_MS,
        );
        call(
            &mut world,
            TravelClientMsg::CancelFastTravel {
                header: header(component::PLAYER_HOUSING, PLAYER),
            },
            NOW + CHANNEL_MS + 1,
        );
        assert!(matches!(
            travel_answers(&mut world, NOW + CHANNEL_MS + 2).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
    }

    #[test]
    fn a_landmark_travel_takes_retails_20_azoth_or_10_within_the_territory() {
        let mut world = world();
        let travel = |world: &mut SimWorld, now_ms: u64| {
            call(world, landmark(SHRINE_TERRITORY, SHRINE[0]), now_ms);
            answers(world, now_ms + CHANNEL_MS)
        };
        assert!(matches!(
            travel(&mut world, NOW).as_slice(),
            [
                ServerMessage::TeleportWithLoadingScreen(_),
                ServerMessage::Travel(TravelServerMsg::FastTravelCompleted { .. })
            ]
        ));
        assert_eq!(azoth(&mut world), START_AZOTH - AZOTH_TO_ANOTHER_TERRITORY);
        let entity = entity(&mut world);
        world.territory_changed(entity, TERRITORY, true);
        travel(&mut world, NOW + 2 * CHANNEL_MS);
        assert_eq!(
            azoth(&mut world),
            START_AZOTH - AZOTH_TO_ANOTHER_TERRITORY - AZOTH_WITHIN_THE_TERRITORY,
            "half within the player's territory"
        );
        // Less azoth than the cost: refused at the end of the channel, nothing taken.
        let short = AZOTH_WITHIN_THE_TERRITORY - 1;
        set_azoth(&mut world, short);
        world.place_player(
            entity,
            Position {
                x: START_X,
                y: HERE[1],
                z: HERE[2],
            },
            None,
            Placement::Stand,
            NOW + 4 * CHANNEL_MS,
        );
        assert!(matches!(
            travel(&mut world, NOW + 4 * CHANNEL_MS).as_slice(),
            [ServerMessage::Travel(
                TravelServerMsg::FastTravelFailed { .. }
            )]
        ));
        assert_eq!(azoth(&mut world), short);
        assert!(
            (body(&mut world).x - START_X).abs() < f32::EPSILON,
            "not moved"
        );
    }

    #[test]
    fn an_unknown_landmark_or_an_undiscovered_territory_is_refused_until_discovered() {
        let mut world = world();
        call(
            &mut world,
            landmark(SHRINE_TERRITORY, SHRINE[0] + 20.0),
            NOW,
        );
        assert!(matches!(
            travel_answers(&mut world, NOW + 1).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
        call(&mut world, landmark(HIDDEN_TERRITORY, SHRINE[0]), NOW + 2);
        assert!(matches!(
            travel_answers(&mut world, NOW + 3).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
        let entity = entity(&mut world);
        world.discover_territory(entity, HIDDEN_TERRITORY);
        call(&mut world, landmark(HIDDEN_TERRITORY, SHRINE[0]), NOW + 4);
        assert!(travel_answers(&mut world, NOW + 5).is_empty(), "channels");
        let progression = world.departures()[0].progression.clone().expect("saved");
        assert!(progression.achievements.contains(&DISCOVERED_BIT));
    }

    #[test]
    fn a_discoverable_shrine_is_discovered_at_its_first_entry_and_charted_at_a_later_one() {
        use crate::charting::ChartRules;
        /// The test level's discoverable territory as a shrine with a chart (retail 40293,
        /// 40295 and 40183 were charted at an entry after their discovery).
        const CHARTED: &str = "Charted_40406";
        const CHARTED_BIT: u32 = 4;
        let mut world = world();
        let mut content = (*world.content).clone();
        content.charting =
            ChartRules::build([(u32::from(HIDDEN_TERRITORY), CHARTED.to_owned())], [], &[]);
        world.content = Arc::new(content);
        world.achievement_rules = AchievementRules::of(
            &AchievementIndexes::build([
                (DISCOVERED.to_owned(), DISCOVERED_BIT),
                (CHARTED.to_owned(), CHARTED_BIT),
            ]),
            &[],
        );
        let entity = entity(&mut world);
        let entered = nw_messages::ClientFacetMsg::TerritoryEntered {
            header: nw_messages::FacetHeader::of(0, PLAYER),
            area: [0; nw_messages::wire::RAW_ID_LEN],
            area_entity: 0,
            extra: 0,
            territory: HIDDEN_TERRITORY,
            unknown: 0,
        };
        let known = |world: &SimWorld| {
            let player = world.world.get::<Player>(entity).expect("player");
            let has = |id| player.flow.has_achievement(&world.achievement_rules, id);
            (has(DISCOVERED), has(CHARTED))
        };
        world
            .facet_message(&connection(PLAYER), entered.clone(), NOW)
            .expect("entered");
        assert_eq!(known(&world), (true, false), "the first entry discovers it");
        world
            .facet_message(&connection(PLAYER), entered, NOW + 1)
            .expect("entered again");
        assert_eq!(known(&world), (true, true), "a later entry charts it");
    }

    #[test]
    fn the_first_entry_into_a_chartable_area_gives_retails_550_experience() {
        use nw_content::progression::{Progression, ProgressionSource};
        use nw_content::rewards::{EventGrant, EventGrants, Reward, Rewards};

        use crate::charting::{ChartRules, RANK_UP_EVENT, TerritoryExperience};

        /// Retail Windsward, its start beach and the chart's achievement and bit.
        const WINDSWARD: u16 = 6;
        const BEACH: u16 = 9011;
        const BEACH_CHARTED: &str = "starterbeach_9011_charted";
        const CHARTED_BIT: u32 = 842;
        /// Content bundle 25810808: standing rank 0 and 1 caps, rank 1's `XPReward`, the
        /// events' `UniversalExpAmount`, `Chart_Location`'s standing, Windsward's modifiers.
        const STANDING_CAPS: [(i64, i64); 2] = [(0, 150), (1, 375)];
        const RANK_ONE_REWARD: i64 = 100;
        const CHART_PER_AMOUNT: u32 = 2;
        const RANK_UP_PER_AMOUNT: u32 = 1;
        const CHART_STANDING: f64 = 50.0;
        const WINDSWARD_EXPERIENCE: TerritoryExperience = TerritoryExperience {
            chart_amount: 75,
            standing_modifier: 1.25,
        };
        /// The retail remote config's multipliers.
        const CHARACTER_MULTIPLIER: f64 = 2.0;
        const STANDING_MULTIPLIER: f64 = 3.0;
        /// Retail newchar 2026-09-18: 120 after the tutorial, 670 after the chart.
        const TUTORIAL_EXPERIENCE: u32 = 120;
        const CHART_GAIN: u32 = 550;

        let mut world = world_with_spawns(Vec::new());
        let standing_id = WINDSWARD.to_string();
        let lists = [vec![WINDSWARD, BEACH]];
        let mut content = PlayerContent {
            charting: ChartRules::build(
                [(u32::from(BEACH), BEACH_CHARTED.to_owned())],
                [(u32::from(WINDSWARD), WINDSWARD_EXPERIENCE)],
                &lists,
            ),
            progression: Progression::default()
                .with_progressions([ProgressionSource {
                    id: standing_id.clone(),
                    max_level: 300,
                    auto_rank_up: true,
                    ranks: STANDING_CAPS.to_vec(),
                }])
                .with_rank_rewards([(standing_id.clone(), 1, RANK_ONE_REWARD)]),
            rewards: Rewards::build([
                (
                    CHART_EVENT.to_owned(),
                    Reward {
                        experience: CHART_PER_AMOUNT,
                        ..Reward::default()
                    },
                ),
                (
                    RANK_UP_EVENT.to_owned(),
                    Reward {
                        experience: RANK_UP_PER_AMOUNT,
                        ..Reward::default()
                    },
                ),
            ]),
            event_grants: EventGrants::build([(
                CHART_EVENT.to_owned(),
                EventGrant {
                    territory_standing: CHART_STANDING,
                    ..EventGrant::default()
                },
            )]),
            ..PlayerContent::default()
        };
        content.gameplay.xp.character = CHARACTER_MULTIPLIER;
        content.gameplay.xp.progression = [(standing_id.clone(), STANDING_MULTIPLIER)].into();
        world.content = Arc::new(content);
        world.achievement_rules = AchievementRules::of(
            &AchievementIndexes::build([(BEACH_CHARTED.to_owned(), CHARTED_BIT)]),
            &[],
        );
        join(&mut world, PLAYER, START_X);
        let entity = entity(&mut world);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .experience = TUTORIAL_EXPERIENCE;
        let state = |world: &SimWorld| {
            let player = world.world.get::<Player>(entity).expect("player");
            let standing = world
                .content
                .progression
                .categorical(&standing_id)
                .map(|mastery| player.masteries.experience(mastery));
            let charted = player
                .flow
                .has_achievement(&world.achievement_rules, BEACH_CHARTED);
            (player.experience, standing, charted)
        };
        for at in [NOW, NOW + 1] {
            world.territory_changed(entity, BEACH, true);
            world.chart_area(entity, PLAYER, BEACH, at);
            assert_eq!(
                state(&world),
                (TUTORIAL_EXPERIENCE + CHART_GAIN, Some(150), true),
                "(75 x 2 + 100 x 1.25 x 1) x 2, standing rank 1; the second entry gives nothing"
            );
        }
    }

    #[test]
    fn an_unstuck_waits_moves_to_the_oldest_breadcrumb_and_starts_its_cooldown() {
        let mut world = world();
        let unstuck = || TravelClientMsg::Unstuck {
            header: header(component::UNSTUCK, PLAYER),
        };
        call(&mut world, unstuck(), NOW);
        assert!(matches!(
            travel_answers(&mut world, NOW + 1).as_slice(),
            [TravelServerMsg::UnstuckDenied {
                reason: UnstuckDenial::NoBreadcrumbs,
                ..
            }]
        ));
        let entity = entity(&mut world);
        let first = Position {
            x: START_X,
            y: 10.0,
            z: 50.0,
        };
        let later = Position {
            x: START_X + 30.0,
            ..first
        };
        for (at, now_ms) in [(first, NOW + 2), (later, NOW + 3)] {
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .last_grounded = Some(at);
            tick(&mut world, now_ms);
        }
        call(&mut world, unstuck(), NOW + 4);
        assert_eq!(
            travel_answers(&mut world, NOW + 5),
            [TravelServerMsg::UnstuckAccepted {
                header: header(component::UNSTUCK, PLAYER),
                delay_ns: UNSTUCK_DELAY_MS * NS_PER_MS,
            }]
        );
        call(&mut world, unstuck(), NOW + 6);
        assert!(matches!(
            travel_answers(&mut world, NOW + 7).as_slice(),
            [TravelServerMsg::UnstuckDenied {
                reason: UnstuckDenial::RequestAlreadyActive,
                ..
            }]
        ));
        assert!(matches!(
            travel_answers(&mut world, NOW + 4 + UNSTUCK_DELAY_MS).as_slice(),
            [TravelServerMsg::UnstuckCompleted { .. }]
        ));
        assert_eq!(body(&mut world), first, "the oldest breadcrumb");
        call(&mut world, unstuck(), NOW + 5 + UNSTUCK_DELAY_MS);
        assert_eq!(
            travel_answers(&mut world, NOW + 6 + UNSTUCK_DELAY_MS),
            [TravelServerMsg::UnstuckDenied {
                header: header(component::UNSTUCK, PLAYER),
                reason: UnstuckDenial::OnCooldown,
                cooldown_ns: UNSTUCK_COOLDOWN_NS,
            }]
        );
        let saved = world.departures()[0].progression.clone().expect("saved");
        assert!(saved.travel.unstuck_ready_ms > 0, "the cooldown is saved");
    }

    #[test]
    fn the_cooldown_left_is_negative_once_it_ended() {
        assert_eq!(cooldown_left_ns(1_000, 400), 600 * 1_000_000);
        assert_eq!(cooldown_left_ns(400, 1_000), -600 * 1_000_000);
        assert_eq!(cooldown_left_ns(0, 0), 0);
    }

    #[test]
    fn the_marker_goes_out_as_the_waypoints_state_after_the_card_and_is_saved() {
        let mut world = world();
        let marker = [9_330.3, 2_683.3, 0.0];
        call(
            &mut world,
            TravelClientMsg::SetWaypoint {
                header: header(component::WAYPOINTS, PLAYER),
                position: marker,
            },
            NOW,
        );
        let entity = entity(&mut world);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .own_card_sent = false;
        world.tick_travel(NOW + 1);
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .travel
                .waypoint_due,
            "it waits for the card"
        );
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .own_card_sent = true;
        world.tick_travel(NOW + 2);
        let player = world.world.get::<Player>(entity).expect("player");
        assert!(player.extra_members.iter().any(|member| {
            member.component == WAYPOINTS_COMPONENT
                && member.state
                    == MemberState::Recorded {
                        class: WAYPOINTS_TYPE_INDEX,
                        body: WaypointsState {
                            waypoint: Some(marker),
                        }
                        .body(),
                    }
        }));
        let saved = player.travel.carried().waypoint.map(|point| point.point());
        assert_eq!(
            saved.map(|point| [point.x, point.y]),
            Some([marker[0], marker[1]])
        );
    }

    #[test]
    fn binding_a_camp_and_an_inn_answers_as_retail_and_keeps_the_respawn_ids() {
        let mut world = world();
        let entity = entity(&mut world);
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::BIND_TO_CAMP, CAMP),
            b"@camp",
            NOW
        ));
        let messages = answers(&mut world, NOW + 1);
        assert!(matches!(
            messages.first(),
            Some(ServerMessage::Travel(TravelServerMsg::DemolishCamp { .. }))
        ));
        assert_eq!(validated(&messages), [(kind::BIND_TO_CAMP, ACCEPTED)]);
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::OPEN_INN, INN),
            b"@inn",
            NOW
        ));
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::SET_SPAWN_POINT, INN),
            b"@inn",
            NOW
        ));
        assert_eq!(
            validated(&answers(&mut world, NOW + 2)),
            [
                (kind::OPEN_INN, ACCEPTED),
                (kind::SET_SPAWN_POINT, ACCEPTED)
            ]
        );
        let travel = world.departures()[0]
            .progression
            .clone()
            .expect("saved")
            .travel;
        assert_eq!((travel.camp, travel.inn), (Some(CAMP), Some(INN)));
        assert!(!world.travel_interaction(entity, PLAYER, request(1, 0), b"", NOW));
    }

    #[test]
    fn a_door_pass_takes_the_one_move_through_the_door() {
        let mut world = world();
        let entity = entity(&mut world);
        let short = START_X + 1.0;
        let through = START_X + 5.0;
        assert!(world.travel_interaction(entity, PLAYER, request(kind::DOOR, 0), b"@door", NOW));
        assert_eq!(
            validated(&answers(&mut world, NOW + 1)),
            [(kind::DOOR, ACCEPTED)]
        );
        world.travel_input(&connection(PLAYER), &[position_record(short)], NOW + 2);
        assert!(
            (body(&mut world).x - START_X).abs() < f32::EPSILON,
            "a step is no pass"
        );
        world.travel_input(&connection(PLAYER), &[position_record(through)], NOW + 3);
        assert!(
            (body(&mut world).x - through).abs() < 0.01,
            "through the door"
        );
        world.travel_input(
            &connection(PLAYER),
            &[position_record(through + 5.0)],
            NOW + 4,
        );
        assert!((body(&mut world).x - through).abs() < 0.01, "one pass only");
    }

    fn housing_call(world: &mut SimWorld, call: fn(FacetHeader) -> TravelClientMsg, now_ms: u64) {
        world.travel_call(
            &connection(PLAYER),
            call(header(component::PLAYER_HOUSING, PLAYER)),
            now_ms,
        );
    }

    fn plot_ref() -> PlotRef {
        PlotRef {
            id: PLOT,
            facet: id_of_raw(&PLOT),
            extra: 0x99ed_e7e1_d5a5_b24d,
        }
    }

    /// Joins the test player in the test territory with `coins` and a bed in its bag, and
    /// buys the plot's house.
    fn home_owner(coins: u64) -> (SimWorld, Entity) {
        let mut world = world();
        let entity = entity(&mut world);
        let bed = world.content.kits.item(BED).cloned().expect("bed");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.coins = coins;
            player.territories = vec![TERRITORY];
            if let Some(gear) = player.gear.as_mut() {
                assert!(gear.add(&bed, 1));
            }
        }
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::HOUSING_PURCHASE, 0),
            b"@ui_housing",
            NOW
        ));
        housing_call(
            &mut world,
            |header| TravelClientMsg::BuyHome { header },
            NOW + 1,
        );
        (world, entity)
    }

    fn bag_count(world: &SimWorld, entity: Entity, item: &str) -> u32 {
        world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map_or(0, |gear| gear.bag_count(item))
    }

    #[test]
    fn a_home_costs_its_type_and_names_its_type_taxes_and_house_info() {
        let (mut world, entity) = home_owner(TIER_2_COST);
        let messages = travel_answers(&mut world, NOW + 2);
        assert!(matches!(
            messages.as_slice(),
            [TravelServerMsg::HouseInfo { house_score: 0, .. }]
        ));
        let player = world.world.get::<Player>(entity).expect("player");
        assert_eq!(player.coins, 0, "the house cost its type's price");
        let home = &player.travel.saved.homes[0];
        assert_eq!(
            (home.record.house_type, home.record.territory),
            (lowercase_crc32(TIER_2), TERRITORY)
        );
        assert!(home.record.taxes_due_ms > home.purchased_ms);
        // A second house in the same territory is refused; so is one without the coins.
        let (mut poor, poor_entity) = home_owner(TIER_2_COST - 1);
        travel_answers(&mut poor, NOW + 2);
        assert!(
            poor.world
                .get::<Player>(poor_entity)
                .expect("player")
                .travel
                .saved
                .homes
                .is_empty()
        );
        housing_call(
            &mut world,
            |header| TravelClientMsg::TaxesDue {
                header,
                plot: plot_ref(),
            },
            NOW + 3,
        );
        housing_call(
            &mut world,
            |header| TravelClientMsg::PayTaxes {
                header,
                plot: plot_ref(),
            },
            NOW + 4,
        );
        housing_call(
            &mut world,
            |header| TravelClientMsg::ResidentData { header },
            NOW + 5,
        );
        let answers = travel_answers(&mut world, NOW + 6);
        let tax = world
            .content
            .travel
            .housing
            .tax(world.content.travel.house_type(0).expect("house type"));
        assert!(matches!(
            answers.as_slice(),
            [
                TravelServerMsg::TaxesDue { amount, .. },
                TravelServerMsg::PayTaxFinished { paid: false, .. },
                TravelServerMsg::ResidentData { territories, .. },
            ] if *amount == tax && territories.len() == 1 && territories[0].0 == TERRITORY
        ));
        let saved = world.departures()[0]
            .progression
            .clone()
            .expect("saved")
            .travel;
        assert_eq!(
            saved.homes[0].record.plot_extra,
            plot_ref().extra,
            "the ids were learnt"
        );
    }

    #[test]
    fn furniture_leaves_the_bag_and_comes_back_and_the_home_cooldown_holds() {
        let (mut world, entity) = home_owner(TIER_2_COST);
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::HOUSING_QUICK_ENTER, 0),
            b"@ui_housing",
            NOW + 2
        ));
        let had_bed = bag_count(&world, entity, BED);
        let placement = ItemPlacement {
            position: [-0.256, -1.217, 5.754],
            rotation: [0.0, 0.0, 0.967, 0.256],
        };
        let housing_header = header(component::PLAYER_HOUSING, PLAYER);
        call(
            &mut world,
            TravelClientMsg::PlaceItem {
                header: housing_header,
                item: BED_INDEX,
                placement,
                anchor: None,
            },
            NOW + 3,
        );
        let placed = had_bed > 0;
        assert_eq!(bag_count(&world, entity, BED), had_bed.saturating_sub(1));
        call(
            &mut world,
            TravelClientMsg::RemoveItem {
                header: housing_header,
                index: 0,
                anchor: None,
            },
            NOW + 4,
        );
        assert_eq!(bag_count(&world, entity, BED), had_bed);
        let results: Vec<u8> = travel_answers(&mut world, NOW + 5)
            .into_iter()
            .filter_map(|answer| match answer {
                TravelServerMsg::PlaceItemResult { result, .. }
                | TravelServerMsg::RemoveItemResult { result, .. } => Some(result),
                _ => None,
            })
            .collect();
        let expected = if placed {
            [DECORATION_SUCCESS, DECORATION_SUCCESS]
        } else {
            [DECORATION_INVALID_ITEM_ID, DECORATION_INVALID_ITEM_ID]
        };
        assert_eq!(results, expected);
        // A fast travel home starts the type's cooldown; a second one is refused until it ends,
        // and a reset needs azoth the test player does not have.
        let home = || TravelClientMsg::FastTravelHome {
            header: header(component::PLAYER_HOUSING, PLAYER),
            plot: plot_ref(),
        };
        call(&mut world, home(), NOW + 6);
        answers(&mut world, NOW + 6 + CHANNEL_MS);
        let ready = world
            .world
            .get::<Player>(entity)
            .expect("player")
            .travel
            .saved
            .homes[0]
            .record
            .fast_travel_ready_ms;
        assert!(ready > 0, "the cooldown runs");
        call(&mut world, home(), NOW + 7 + CHANNEL_MS);
        assert!(matches!(
            travel_answers(&mut world, NOW + 8 + CHANNEL_MS).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
        housing_call(
            &mut world,
            |header| TravelClientMsg::ResetCooldown {
                header,
                plot: plot_ref(),
            },
            NOW + 9 + CHANNEL_MS,
        );
        assert_eq!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .travel
                .saved
                .homes[0]
                .record
                .fast_travel_ready_ms,
            ready,
            "no azoth, no reset"
        );
        housing_call(
            &mut world,
            |header| TravelClientMsg::AbandonHome { header },
            NOW + 10 + CHANNEL_MS,
        );
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .travel
                .saved
                .homes
                .is_empty()
        );
    }

    #[test]
    fn an_inn_recall_goes_to_the_bound_inn_once_per_cooldown() {
        let mut world = world();
        let entity = entity(&mut world);
        let recall = |territory| TravelClientMsg::FastTravelLandmark {
            header: header(component::PLAYER_HOUSING, PLAYER),
            landmark: LandmarkRef {
                territory,
                landmark_type: INN_RESPAWN_LANDMARK,
                position: [INN_AT[0], INN_AT[1], 0.0],
            },
        };
        call(&mut world, recall(TERRITORY), NOW);
        assert!(
            matches!(
                travel_answers(&mut world, NOW + 1).as_slice(),
                [TravelServerMsg::FastTravelFailed { .. }]
            ),
            "no inn bound"
        );
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::SET_SPAWN_POINT, INN),
            b"@inn",
            NOW + 2
        ));
        call(&mut world, recall(TERRITORY), NOW + 3);
        answers(&mut world, NOW + 3 + CHANNEL_MS);
        let at = body(&mut world);
        assert_eq!([at.x, at.y], [INN_AT[0], INN_AT[1]], "at the inn");
        call(&mut world, recall(TERRITORY), NOW + 4 + CHANNEL_MS);
        assert!(matches!(
            travel_answers(&mut world, NOW + 5 + CHANNEL_MS).as_slice(),
            [TravelServerMsg::FastTravelFailed { .. }]
        ));
        let saved = world.departures()[0]
            .progression
            .clone()
            .expect("saved")
            .travel;
        assert!(saved.inn_recalled_ms > 0);
        assert_eq!(world.content.travel.inn_cooldown_ms, INN_COOLDOWN_MS);
    }

    #[test]
    fn a_teleporter_answers_then_moves_the_player_after_its_delay() {
        let mut world = world();
        let entity = entity(&mut world);
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::TELEPORTER, TELEPORTER_ID),
            b"@dungeon_reekwater_interact_teleporter",
            NOW
        ));
        assert_eq!(
            validated(&answers(&mut world, NOW + 1)),
            [(kind::TELEPORTER, ACCEPTED)]
        );
        let messages = answers(&mut world, NOW + TELEPORTER_DELAY_MS);
        assert!(matches!(
            messages.as_slice(),
            [
                ServerMessage::TeleportWithLoadingScreen(_),
                ServerMessage::Travel(TravelServerMsg::FastTravelCompleted { .. })
            ]
        ));
        let at = body(&mut world);
        assert_eq!([at.x, at.y], [TELEPORTER_TO[0], TELEPORTER_TO[1]]);
        assert!(world.travel_interaction(
            entity,
            PLAYER,
            request(kind::TELEPORTER, 1),
            b"",
            NOW + TELEPORTER_DELAY_MS
        ));
        assert_eq!(
            validated(&answers(&mut world, NOW + TELEPORTER_DELAY_MS + 1)),
            [(kind::TELEPORTER, INTERACTION_REFUSED)]
        );
    }

    #[test]
    fn a_camp_goes_in_the_last_known_zone_but_not_in_a_point_of_interest() {
        // Live 2026-11-01: the client named only points of interest (La Roussette 30129), never
        // the zone around them, and both camps were refused.
        const SHIPWRECK: u16 = 30_129;
        let mut world = world();
        let entity = entity(&mut world);
        let allowed = |world: &mut SimWorld, areas: Vec<u16>, zone: Option<u32>| {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.territories = areas;
            player.last_zone = zone;
            let player = world.world.get::<Player>(entity).expect("player");
            world.camp_allowed_where(player)
        };
        assert!(!allowed(&mut world, Vec::new(), None), "no zone known");
        let zone = Some(u32::from(TERRITORY));
        assert!(allowed(&mut world, Vec::new(), zone), "the last known zone");
        assert!(
            !allowed(&mut world, vec![SHIPWRECK], zone),
            "inside a point of interest"
        );
        assert!(
            allowed(&mut world, vec![TERRITORY], None),
            "inside the zone"
        );
    }

    #[test]
    fn a_camp_needs_the_camping_rank_and_answers_with_441() {
        let mut world = world();
        let entity = entity(&mut world);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .territories = vec![TERRITORY];
        let placement = StructurePlacement {
            position: HERE,
            rotation: [0.0, 0.0, -0.512, 0.859],
            skin: CAMP_SKIN,
            structure: [5; RAW_ID_LEN],
            owner: [6; RAW_ID_LEN],
            socket: u32::MAX,
            value: 50,
            flag: true,
        };
        let call_message = TravelClientMsg::PlaceStructure {
            header: header(BUILDER_COMPONENT, PLAYER),
            placement,
        };
        call(&mut world, call_message, NOW);
        // The test player has no `Camping` rank: refused, nothing placed.
        assert_eq!(
            travel_answers(&mut world, NOW + 1),
            [TravelServerMsg::PlacementResults {
                header: header(BUILDER_COMPONENT, PLAYER),
                result: PLACEMENT_REFUSED,
                value: PLACEMENT_VALUE,
            }]
        );
        assert!(
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .travel
                .saved
                .placed_camp
                .is_none()
        );
    }

    #[test]
    fn the_travel_states_go_out_after_the_card_and_list_the_home_points() {
        let mut world = world();
        let entity = entity(&mut world);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .own_card_sent = true;
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .travel
            .saved
            .inn = Some(INN);
        world.tick_travel(NOW + 1);
        let player = world.world.get::<Player>(entity).expect("player");
        let classes: Vec<u64> = player
            .extra_members
            .iter()
            .filter_map(|member| match &member.state {
                MemberState::Recorded { class, .. } => Some(*class),
                _ => None,
            })
            .collect();
        for class in [
            PLAYER_TIME_TYPE_INDEX,
            PLAYER_HOUSING_TYPE_INDEX,
            PLAYER_HOME_TYPE_INDEX,
            CAMPING_TYPE_INDEX,
        ] {
            assert!(classes.contains(&class), "class {class}");
        }
        let homes = home_state(&player.travel, &world.content.travel);
        let points = homes.home_points.expect("list").elements;
        assert_eq!(points.len(), 1);
        assert_eq!(
            (points[0].kind, points[0].position, points[0].id.clone()),
            (
                home_point_kind::INN,
                INN_AT,
                home_point_id(INN, SINGLE_POINT)
            )
        );
        assert_eq!(homes.home_point_id, Some(home_point_id(INN, SINGLE_POINT)));
        assert_eq!(
            housing_state(&player.travel).is_fast_travel_channeling,
            Some(false)
        );
    }

    #[test]
    fn a_furniture_change_outside_an_owned_home_is_refused() {
        let mut world = world();
        let housing_header = header(component::PLAYER_HOUSING, PLAYER);
        call(
            &mut world,
            TravelClientMsg::RemoveItem {
                header: housing_header,
                index: 0,
                anchor: None,
            },
            NOW,
        );
        assert_eq!(
            travel_answers(&mut world, NOW + 1),
            [TravelServerMsg::RemoveItemResult {
                header: housing_header,
                result: DECORATION_NOT_OWNER,
                index: 0,
            }]
        );
    }

    #[test]
    fn a_saved_travel_state_comes_back_with_its_marker_due() {
        let saved = PlayerTravel {
            waypoint: Some(StoredPoint::of(Point {
                x: 1.0,
                y: 2.0,
                z: 0.0,
            })),
            inn: Some(INN),
            ..PlayerTravel::default()
        };
        let state = TravelState::of(saved.clone());
        assert!(state.waypoint_due);
        assert_eq!(state.carried(), saved);
        assert!(!TravelState::of(PlayerTravel::default()).waypoint_due);
    }

    #[test]
    fn breadcrumbs_keep_their_spacing_and_their_count() {
        let mut state = TravelState::default();
        let at = |x: f32| Position { x, y: 0.0, z: 0.0 };
        for x in [0.0, 1.0, 30.0, 60.0, 90.0] {
            state.drop_crumb(at(x), 24.0, 3);
        }
        assert_eq!(state.breadcrumbs, [at(30.0), at(60.0), at(90.0)]);
    }

    /// A player with the `Camping` rank of [`CAMP_TIER`] in the world of [`world`], whose content
    /// has the retail PvP metalfest camp, its costs and a `camp1` recipe.
    mod camp {
        use nw_content::camps::{CampBlueprint, CampRules, CampSkin};
        use nw_content::crafting::{Ingredient, IngredientKind, Recipe, Recipes, StructureCosts};
        use nw_content::progression::{Progression, ProgressionSource};
        use nw_messages::wire::Reader;
        use nw_world::camps::{CampSlice, CampSlices};
        use nw_world::stations::StationSlice;

        use super::*;
        use crate::world::camps::tests::{
            RETAIL_AT, RETAIL_SPAWN_POINT, SPAWN_POINT_TOLERANCE_M, costs, pvp_slice,
            retail_rotation,
        };
        use crate::world::crafting::CraftChoice;
        use crate::world::gear;
        use crate::world::slots::Slot;
        use crate::world::tests::{SERVER_TIME, admit_at};

        const CAMPER: u64 = 8;
        const CAMP_TIER: u16 = 5;
        /// `Camping` (`MaxLevel` 5) with a cap of 100 per rank.
        const RANK_CAP: i64 = 100;
        const WOOD: &str = "WoodT1";
        const PVP_CAMP: &str = "CampSkin_MetalFest02_A_PVP";
        const CAMP_SKIN_ID: &str = "CampSkin_MetalFest02_A";
        /// `Hivecamp` 2236 at 1781367544.661 named `CampSkin_MetalFest02_A_PVP`.
        const PVP_CAMP_CRC: u32 = 0x89a7_af2d;
        /// A `camp1` recipe of the test content.
        const RECIPE: &str = "CampTestRecipe";
        /// The interaction kind of a crafting station (`OpenCraftingOption`).
        const CRAFTING_KIND: u8 = 20;
        /// Retail `Hivecamp` 1781367544743: the camping state (1292) of the owner of the new
        /// camp (component, class and body).
        const RETAIL_CAMPING: &str =
            "198c1401096e5ac4808181beaff1f846dcd11b879dafbe818180c45a6e18b8b065f6fa3470";
        /// The same bundle's home point list (3652, a list delta) adds the camp element: its
        /// name, its cooldown (1 s) and kind.
        const RETAIL_CAMP_NAME: &[u8] = b"@CampT5_MasterName";

        fn content(travel: TravelRules) -> PlayerContent {
            let camp = |id: &str, pvp: bool, variant: Option<&str>| CampBlueprint {
                id: id.to_owned(),
                hp: 300,
                pvp,
                pvp_variant: variant.map(str::to_owned),
            };
            PlayerContent {
                travel,
                kits: gear::tests::kits_with(&[(WOOD, "Resource")]),
                progression: Progression::default().with_progressions([ProgressionSource {
                    id: CAMPING_PROGRESSION.to_owned(),
                    max_level: i64::from(CAMP_TIER),
                    auto_rank_up: true,
                    ranks: (0..i64::from(CAMP_TIER))
                        .map(|rank| (rank, RANK_CAP))
                        .collect(),
                }]),
                camps: CampRules::build(
                    [
                        camp(CAMP_SKIN_ID, false, Some(PVP_CAMP)),
                        camp(PVP_CAMP, true, None),
                    ],
                    [CampSkin {
                        id: CAMP_SKIN_ID.to_owned(),
                        enabled: true,
                        required_achievement: None,
                    }],
                ),
                camp_slices: CampSlices::of([pvp_slice(), skin_slice()]),
                structure_costs: StructureCosts::build([(PVP_CAMP.to_owned(), costs())]),
                recipes: Recipes::build(
                    [Recipe {
                        id: RECIPE.to_owned(),
                        item: WOOD.to_owned(),
                        quantity: 1,
                        ingredients: vec![Ingredient {
                            kind: IngredientKind::Item(gear::tests::FLINT.to_owned()),
                            quantity: 1,
                        }],
                        stations: vec!["camp1".to_owned()],
                    }],
                    [],
                ),
                ..PlayerContent::default()
            }
        }

        /// The camp slice of the metalfest skin without PvP (`campskin_metalfest02_a`, its
        /// members as `camp-slices-25810808.json` has them).
        fn skin_slice() -> CampSlice {
            let pvp = pvp_slice();
            CampSlice {
                station: StationSlice {
                    structure: CAMP_SKIN_ID.to_owned(),
                    ownership: Some((7, 1)),
                    ..pvp.station.clone()
                },
                vitals: None,
                damage_receiver: None,
                delayed_events: vec![4],
                ..pvp
            }
        }

        /// The world and the camper, in a camp territory with the costs of a camp (and `extra`
        /// flint) in its bag when `items`.
        fn camp_world(items: bool, extra_flint: u32) -> (SimWorld, Entity) {
            let mut world = world();
            world.content = Arc::new(content(world.content.travel.clone()));
            let mut admit = admit_at(CAMPER, START_X, true);
            admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
            world.admit(&admit, NOW, SERVER_TIME).expect("admit");
            let entity = world.player_by_number(CAMPER).expect("camper");
            world
                .world
                .get_mut::<Vitals>(entity)
                .expect("vitals")
                .spawned = true;
            let content = Arc::clone(&world.content);
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.territories = vec![TERRITORY];
            player.own_card_sent = true;
            let camping = content
                .progression
                .categorical(CAMPING_PROGRESSION)
                .expect("camping");
            player.masteries.set(camping, u32::MAX);
            assert_eq!(
                player
                    .masteries
                    .rank(&content.progression, CAMPING_PROGRESSION),
                CAMP_TIER
            );
            let gear = player.gear.as_mut().expect("gear");
            let item = |id: &str| content.kits.item(id).cloned().expect("item");
            let flint_held = gear.bag_count(gear::tests::FLINT);
            gear.take(gear::tests::FLINT, flint_held);
            if items {
                for (id, quantity) in costs() {
                    assert!(gear.add(&item(&id), quantity));
                }
            }
            if extra_flint > 0 {
                assert!(gear.add(&item(gear::tests::FLINT), extra_flint));
            }
            (world, entity)
        }

        fn place(world: &mut SimWorld, now_ms: u64) -> Vec<TravelServerMsg> {
            let [x, y, z] = [RETAIL_AT.x, RETAIL_AT.y, RETAIL_AT.z];
            let placement = StructurePlacement {
                position: [x, y, z],
                rotation: retail_rotation(),
                skin: PVP_CAMP_CRC,
                structure: [5; RAW_ID_LEN],
                owner: [6; RAW_ID_LEN],
                socket: u32::MAX,
                value: 50,
                flag: true,
            };
            world.travel_call(
                &connection(CAMPER),
                TravelClientMsg::PlaceStructure {
                    header: header(BUILDER_COMPONENT, CAMPER),
                    placement,
                },
                now_ms,
            );
            messages_to(&tick(world, now_ms + 1).messages, CAMPER)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::Travel(answer) => Some(answer),
                    _ => None,
                })
                .collect()
        }

        fn placed(result: u8) -> TravelServerMsg {
            TravelServerMsg::PlacementResults {
                header: header(BUILDER_COMPONENT, CAMPER),
                result,
                value: PLACEMENT_VALUE,
            }
        }

        fn camp_of(world: &SimWorld, entity: Entity) -> Option<CampEntity> {
            world
                .world
                .get::<Player>(entity)
                .expect("player")
                .travel
                .camp
        }

        fn bag(world: &SimWorld, entity: Entity, item: &str) -> u32 {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.gear.as_ref())
                .map_or(0, |gear| gear.bag_count(item))
        }

        fn shown(world: &SimWorld, camp: &CampEntity) -> bool {
            world
                .stations
                .numbers_near(RETAIL_AT, 1.0)
                .any(|number| number == camp.number)
        }

        fn retail(bytes: &str) -> MemberState {
            let bytes = hex::decode(bytes).expect("hex");
            BundleMember::read_from(&bytes)
                .expect("retail member")
                .0
                .state
        }

        #[test]
        fn a_camp_without_its_costs_in_the_bag_is_refused() {
            let (mut world, entity) = camp_world(false, 0);
            assert_eq!(place(&mut world, NOW), [placed(PLACEMENT_REFUSED)]);
            assert_eq!(camp_of(&world, entity), None);
            let player = world.world.get::<Player>(entity).expect("player");
            assert!(player.travel.saved.placed_camp.is_none());
        }

        #[test]
        fn a_gm_sets_the_camping_rank_by_its_progression_id() {
            const RANK: u16 = 1;
            let (mut world, entity) = camp_world(true, 0);
            assert_eq!(
                world.set_mastery_rank(CAMPER, Some(CAMPING_PROGRESSION), RANK),
                nw_bus::AdminReply::Done
            );
            let content = Arc::clone(&world.content);
            let player = world.world.get::<Player>(entity).expect("player");
            assert_eq!(
                player
                    .masteries
                    .rank(&content.progression, CAMPING_PROGRESSION),
                RANK
            );
        }

        #[test]
        fn a_placed_camp_takes_its_costs_and_stands_in_the_world() {
            let (mut world, entity) = camp_world(true, 0);
            assert_eq!(place(&mut world, NOW), [placed(PLACEMENT_PLACED)]);
            assert!(
                world
                    .take_stat_events()
                    .iter()
                    .any(|event| event.game_mode == BIND_CAMP_EVENT),
                "a placed camp fires BindCamp"
            );
            assert_eq!(
                (
                    bag(&world, entity, WOOD),
                    bag(&world, entity, gear::tests::FLINT)
                ),
                (0, 0)
            );
            let camp = camp_of(&world, entity).expect("camp entity");
            assert!(shown(&world, &camp));
            let slot = Slot {
                interest: 1,
                new: true,
            };
            let group = world.stations.group(camp.number, slot).expect("group");
            assert_eq!(
                group.members.first().map(|member| &member.state),
                Some(&MemberState::GdeMetadata(
                    nw_messages::GdeMetadataReplicatedState {
                        asset: Some(nw_messages::gde::AssetId {
                            id: pvp_slice().asset,
                            sub_id: pvp_slice().asset_sub_id,
                        }),
                        gde_ref: Some(camp.gde_ref),
                    }
                ))
            );
            // A second camp: the first goes (6760), the new one has a slot key of its own.
            let (wood, flint) = (item_of(&world, WOOD), item_of(&world, gear::tests::FLINT));
            {
                let mut player = world.world.get_mut::<Player>(entity).expect("player");
                let gear = player.gear.as_mut().expect("gear");
                assert!(gear.add(&wood, 5) && gear.add(&flint, 1));
            }
            assert_eq!(
                place(&mut world, NOW + 10),
                [
                    TravelServerMsg::DemolishCamp {
                        header: header(component::CAMPING, CAMPER),
                    },
                    placed(PLACEMENT_PLACED)
                ]
            );
            let second = camp_of(&world, entity).expect("second camp");
            assert_ne!(second.number, camp.number);
            assert!(!shown(&world, &camp) && shown(&world, &second));
            // A bind to a respawn camp demolishes it: the save forgets it.
            assert!(world.travel_interaction(
                entity,
                CAMPER,
                request(kind::BIND_TO_CAMP, CAMP),
                b"@camp",
                NOW + 20
            ));
            assert!(!shown(&world, &second));
            let player = world.world.get::<Player>(entity).expect("player");
            assert_eq!(player.travel.camp, None);
            assert!(player.travel.saved.placed_camp.is_none());
        }

        fn item_of(world: &SimWorld, id: &str) -> nw_content::starter::KitItem {
            world.content.kits.item(id).cloned().expect("item")
        }

        #[test]
        fn the_camping_state_and_home_points_name_the_camp_as_retail() {
            let (mut world, entity) = camp_world(true, 0);
            place(&mut world, NOW);
            let camp = camp_of(&world, entity).expect("camp");
            let placed_ms = world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.travel.saved.placed_camp)
                .expect("saved")
                .placed_ms;
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .extra_members
                .clear();
            world
                .world
                .get_mut::<Player>(entity)
                .expect("player")
                .travel
                .due = StatesDue::ALL;
            world.tick_travel(NOW + 2);
            let player = world.world.get::<Player>(entity).expect("player");
            let body = |class| {
                player
                    .extra_members
                    .iter()
                    .find_map(|member| match &member.state {
                        MemberState::Recorded { class: found, body } if *found == class => {
                            Some(body.clone())
                        }
                        _ => None,
                    })
            };
            let camping =
                CampingState::read(&mut Reader::new(&body(CAMPING_TYPE_INDEX).expect("1292")))
                    .expect("camping state");
            let MemberState::Camping(retail_camping) = retail(RETAIL_CAMPING) else {
                panic!("retail 1292");
            };
            assert_eq!(
                camping,
                CampingState {
                    current_camp: Some(CampRef {
                        id: camp.gde_ref,
                        facet: camp.facet(),
                    }),
                    camp_skin: None,
                    under_attack: None,
                    bind_cooldown_end_ns: Some(unix_ms_ns(placed_ms + CAMP_BIND_COOLDOWN_MS)),
                }
            );
            let retail_camp = retail_camping.current_camp.expect("retail camp");
            assert_eq!(
                (
                    retail_camp.facet,
                    retail_camping.camp_skin,
                    retail_camping.under_attack
                ),
                (
                    u64::from_le_bytes(retail_camp.id[..8].try_into().expect("8")),
                    None,
                    None
                ),
                "the retail state of a new camp: its ref and bind end only"
            );
            let homes = PlayerHomeState::read(&mut Reader::new(
                &body(PLAYER_HOME_TYPE_INDEX).expect("3652"),
            ))
            .expect("home state");
            let ours = &homes.home_points.expect("ours").elements[0];
            assert_eq!(
                (ours.entity.id, ours.entity.facet),
                (camp.gde_ref, camp.facet())
            );
            assert_eq!(ours.id, home_point_id(camp.facet(), SINGLE_POINT));
            assert_eq!(
                (ours.name.as_slice(), ours.cooldown_ns, ours.kind),
                (RETAIL_CAMP_NAME, CAMP_COOLDOWN_NS, home_point_kind::CAMP)
            );
            for (ours, theirs) in ours
                .position
                .iter()
                .zip(RETAIL_SPAWN_POINT.map(f32::from_bits))
            {
                assert!((ours - theirs).abs() <= SPAWN_POINT_TOLERANCE_M);
            }
        }

        #[test]
        fn a_camp1_recipe_crafts_at_the_camp() {
            let (mut world, entity) = camp_world(true, 1);
            place(&mut world, NOW);
            let camp = camp_of(&world, entity).expect("camp");
            world.open_station(
                entity,
                CAMPER,
                InteractionRequest {
                    target: camp.gde_ref,
                    target_entity: camp.facet(),
                    interaction: 2,
                    kind: CRAFTING_KIND,
                    gatherable: None,
                },
            );
            let station = world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.station.clone());
            assert_eq!(station.as_deref(), Some("Camp5"));
            world.craft(
                entity,
                CAMPER,
                lowercase_crc32(RECIPE),
                1,
                0,
                &[],
                &CraftChoice::default(),
                NOW,
            );
            assert_eq!(world.crafts.len(), 1, "the camp is a station of the recipe");
        }

        #[test]
        fn a_respawn_at_the_camp_lands_at_its_respawn_point() {
            let (mut world, entity) = camp_world(true, 0);
            place(&mut world, NOW);
            let camp = camp_of(&world, entity).expect("camp");
            {
                let mut vitals = world.world.get_mut::<Vitals>(entity).expect("vitals");
                let max = vitals.health.max();
                vitals.health.take(max);
                vitals.dead_since_ms = Some(NOW);
            }
            let later = NOW + world.combat.min_respawn_ms;
            let at = world
                .request_respawn(&connection(CAMPER), Some(camp.facet()), later)
                .expect("respawn");
            assert_eq!(at, camp.spawn_point);
            assert!(
                at.horizontal_distance(RETAIL_AT) < 2.0,
                "1.7 m from the camp"
            );
            let retail = RETAIL_SPAWN_POINT.map(f32::from_bits);
            assert!((at.x - retail[0]).abs() <= SPAWN_POINT_TOLERANCE_M);
            assert!((at.y - retail[1]).abs() <= SPAWN_POINT_TOLERANCE_M);
        }

        #[test]
        fn a_saved_camp_stands_again_when_its_owner_comes_back() {
            let (mut world, entity) = camp_world(true, 0);
            place(&mut world, NOW);
            let first = camp_of(&world, entity).expect("camp");
            let departure = world.leave(&connection(CAMPER)).expect("departure");
            assert!(!shown(&world, &first), "the camp leaves with its owner");
            let travel = departure.progression.expect("saved").travel;
            let saved = travel.placed_camp.expect("a saved camp");
            assert_eq!(
                saved.rotation,
                retail_rotation().map(f32::to_bits),
                "its rotation"
            );
            assert_ne!(saved.blueprint, 0, "its blueprint");
            let mut admit = admit_at(CAMPER, START_X, true);
            admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
            admit.progression.travel = PlayerTravel {
                camp_skin: Some(lowercase_crc32(CAMP_SKIN_ID)),
                ..travel
            };
            world.admit(&admit, NOW + 30, SERVER_TIME).expect("admit");
            let entity = world.player_by_number(CAMPER).expect("camper");
            let again = camp_of(&world, entity).expect("the camp again");
            assert!(shown(&world, &again));
            assert_eq!(
                again.spawn_point, first.spawn_point,
                "turned as placed: its respawn point is where it was"
            );
        }
    }
}
