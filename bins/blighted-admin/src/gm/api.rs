// SPDX-License-Identifier: AGPL-3.0-or-later
//! The GM routes (admin studio spec section 8.2). Each checks the rank on the server, sends the
//! command to the sim that owns the target, and writes an audit row. Messages to players, mutes
//! and coin are not here: the server has no chat and no currency yet (their client messages are
//! Unknown).

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use nw_bus::{AdminAction, AdminCommand, AdminReply, BusId, Point, RunningEvent};
use nw_db::AdminRank;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::access::session::Actor;
use crate::gm::{Commands, SendError};
use crate::live::lock;
use crate::server::AppState;

/// `POST`: moves a player to a point.
pub const TELEPORT_ROUTE: &str = "/api/gm/teleport";
/// `POST`: moves a player to another player.
pub const TELEPORT_TO_PLAYER_ROUTE: &str = "/api/gm/teleport-to-player";
/// `POST`: saves a player and closes its connection.
pub const KICK_ROUTE: &str = "/api/gm/kick";
/// `POST`: fills a player's health.
pub const HEAL_ROUTE: &str = "/api/gm/heal";
/// `POST`: takes a player's health.
pub const KILL_ROUTE: &str = "/api/gm/kill";
/// `POST`: turns a player's damage off or on.
pub const GOD_ROUTE: &str = "/api/gm/god";
/// `POST`: puts items into a player's bag.
pub const GIVE_ITEM_ROUTE: &str = "/api/gm/give-item";
/// `POST`: starts an event at a point.
pub const START_EVENT_ROUTE: &str = "/api/gm/start-event";
/// `POST`: stops a running event on every sim.
pub const STOP_EVENT_ROUTE: &str = "/api/gm/stop-event";
/// `GET`: the events of the level and the running ones.
pub const EVENTS_ROUTE: &str = "/api/gm/events";
/// `POST`: starts a quest for a player.
pub const START_QUEST_ROUTE: &str = "/api/gm/start-quest";
/// `POST`: takes an item from a player.
pub const TAKE_ITEM_ROUTE: &str = "/api/gm/take-item";
/// `POST`: spawns GM mobs at a point.
pub const SPAWN_ROUTE: &str = "/api/gm/spawn";
/// `POST`: removes GM mobs.
pub const DESPAWN_ROUTE: &str = "/api/gm/despawn";
/// `POST`: bans or unbans an account.
pub const BAN_ROUTE: &str = "/api/gm/ban";
/// `GET`: the ranks; `POST`: gives or removes one.
pub const RANKS_ROUTE: &str = "/api/gm/ranks";
/// `GET`: the audit log.
pub const AUDIT_ROUTE: &str = "/api/gm/audit";
/// `POST`: adds experience points to a player.
pub const GIVE_EXPERIENCE_ROUTE: &str = "/api/gm/give-experience";
/// `POST`: sets a player's level.
pub const SET_LEVEL_ROUTE: &str = "/api/gm/set-level";
/// `POST`: adds experience to one weapon mastery of a player.
pub const GIVE_MASTERY_ROUTE: &str = "/api/gm/give-mastery";
/// `POST`: sets the rank of one weapon mastery of a player, or of every one.
pub const SET_MASTERY_RANK_ROUTE: &str = "/api/gm/set-mastery-rank";
/// `POST`: sets a player's coins.
pub const SET_COINS_ROUTE: &str = "/api/gm/set-coins";
/// `GET`: the weapon mastery ids, the top level and the top rank, for the page.
pub const PROGRESSION_ROUTE: &str = "/api/gm/progression";

/// Lowest rank of a kick.
pub const KICK_RANK: AdminRank = AdminRank::Csr;
/// Lowest rank of the other live commands and of bans.
pub const GM_RANK: AdminRank = AdminRank::GameMaster;
/// Lowest rank that changes ranks.
pub const RANKS_RANK: AdminRank = AdminRank::HeadAdmin;
/// Audit rows a page gets by default, and at most.
const AUDIT_DEFAULT_ROWS: i64 = 100;
const AUDIT_MAX_ROWS: i64 = 1000;

/// The GM routes.
pub fn gm_routes() -> Router<AppState> {
    Router::new()
        .route(TELEPORT_ROUTE, post(teleport))
        .route(TELEPORT_TO_PLAYER_ROUTE, post(teleport_to_player))
        .route(KICK_ROUTE, post(kick))
        .route(HEAL_ROUTE, post(heal))
        .route(KILL_ROUTE, post(kill))
        .route(GOD_ROUTE, post(god))
        .route(GIVE_ITEM_ROUTE, post(give_item))
        .route(START_QUEST_ROUTE, post(start_quest))
        .route(START_EVENT_ROUTE, post(start_event))
        .route(STOP_EVENT_ROUTE, post(stop_event))
        .route(EVENTS_ROUTE, get(events))
        .route(TAKE_ITEM_ROUTE, post(take_item))
        .route(SPAWN_ROUTE, post(spawn))
        .route(DESPAWN_ROUTE, post(despawn))
        .route(BAN_ROUTE, post(ban))
        .route(RANKS_ROUTE, get(ranks).post(set_rank))
        .route(AUDIT_ROUTE, get(audit))
        .route(GIVE_EXPERIENCE_ROUTE, post(give_experience))
        .route(SET_LEVEL_ROUTE, post(set_level))
        .route(GIVE_MASTERY_ROUTE, post(give_mastery))
        .route(SET_MASTERY_RANK_ROUTE, post(set_mastery_rank))
        .route(SET_COINS_ROUTE, post(set_coins))
        .route(PROGRESSION_ROUTE, get(progression))
}

/// A player to move to a point.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TeleportRequest {
    /// The player's entity number.
    pub entity: u64,
    /// East, metres.
    pub x: f32,
    /// North, metres.
    pub y: f32,
    /// Up, metres.
    pub z: f32,
}

/// A player to move to another.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TeleportToPlayerRequest {
    /// The player to move.
    pub entity: u64,
    /// The player to move it to.
    pub to_entity: u64,
}

/// A command on one player.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct PlayerRequest {
    /// The player's entity number.
    pub entity: u64,
}

/// God mode on or off.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct GodRequest {
    /// The player.
    pub entity: u64,
    /// True: no damage.
    pub on: bool,
}

/// An event to start at a point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventStartRequest {
    /// The event id.
    pub event: String,
    /// East, metres.
    pub x: f32,
    /// North, metres.
    pub y: f32,
    /// Up, metres.
    pub z: f32,
}

/// An event to stop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventStopRequest {
    /// The event id.
    pub event: String,
}

/// The events of the level and the running ones, by sim.
#[derive(Debug, Serialize)]
struct EventsView {
    defined: Vec<String>,
    running: Vec<(String, RunningEvent)>,
}

/// A quest for a player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestRequest {
    /// The player.
    pub entity: u64,
    /// The objective id.
    pub objective: String,
}

/// Items for a player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemRequest {
    /// The player.
    pub entity: u64,
    /// The item id.
    pub item: String,
    /// How many (give only; default 1).
    #[serde(default)]
    pub quantity: Option<u32>,
}

/// Experience points for a player.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExperienceRequest {
    /// The player.
    pub entity: u64,
    /// How many points.
    pub amount: u32,
}

/// A level for a player.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct LevelRequest {
    /// The player.
    pub entity: u64,
    /// The level as the client shows it (1 is the first).
    pub level: u32,
}

/// Experience for one weapon mastery of a player.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasteryRequest {
    /// The player.
    pub entity: u64,
    /// The `CategoricalProgressionData` id, for example `GreatSwordAbilityTable`.
    pub mastery: String,
    /// How many points.
    pub amount: u32,
}

/// A rank for one weapon mastery of a player, or for every one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasteryRankRequest {
    /// The player.
    pub entity: u64,
    /// The mastery id; `None` is every weapon mastery.
    #[serde(default)]
    pub mastery: Option<String>,
    /// The rank as the client shows it (1 is the first).
    pub rank: u16,
}

/// Coins for a player.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CoinsRequest {
    /// The player.
    pub entity: u64,
    /// The coins.
    pub coins: u64,
}

/// GM mobs to spawn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// The vitals id.
    pub vitals: String,
    /// Their level.
    pub level: u32,
    /// East, metres.
    pub x: f32,
    /// North, metres.
    pub y: f32,
    /// Up, metres.
    pub z: f32,
    /// How many.
    pub count: u32,
}

/// GM mobs to remove; empty: every GM mob.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DespawnRequest {
    /// The mobs.
    #[serde(default)]
    pub entities: Vec<u64>,
}

/// An account to ban or unban.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanRequest {
    /// Its login name.
    pub username: String,
    /// True: banned.
    pub banned: bool,
}

/// A rank to give or remove.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankRequest {
    /// The login name.
    pub username: String,
    /// The rank; `None` removes it.
    pub rank: Option<AdminRank>,
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
}

/// Where a command goes.
enum Owner {
    /// The sim that owns this player.
    Player(u64),
    /// The sim that owns the cell of this point.
    Point(f32, f32),
}

/// One audited GM command.
struct Gm<'a> {
    state: &'a AppState,
    actor: &'a Actor,
    command: &'static str,
    target: String,
    detail: Value,
}

impl Gm<'_> {
    async fn audit(&self, result: &str) {
        self.state
            .access
            .record(
                self.actor,
                self.command,
                &self.target,
                self.detail.clone(),
                result,
            )
            .await;
    }

    /// 403 and an audit row when the actor's rank is below `rank`.
    async fn refused(&self, rank: AdminRank) -> Option<Response> {
        if self.actor.rank >= rank {
            return None;
        }
        self.audit("refused: rank").await;
        Some(
            (
                StatusCode::FORBIDDEN,
                format!("this needs the {} rank", rank.name()),
            )
                .into_response(),
        )
    }

    /// Sends `action` to the sim of `owner`, audits and answers.
    async fn send(&self, owner: Owner, action: AdminAction) -> Response {
        let (Some(commands), Some(live)) = (&self.state.commands, &self.state.live) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let sim = {
            let state = lock(live.state());
            match owner {
                Owner::Player(entity) => state.sim_of_player(entity),
                Owner::Point(x, y) => state.sim_at(x, y),
            }
        };
        let Some(sim) = sim else {
            self.audit("not_found").await;
            return (StatusCode::NOT_FOUND, "no sim shows this target").into_response();
        };
        let (status, result) = answer(self.send_to(commands, &sim, action).await);
        self.audit(&result).await;
        (status, result).into_response()
    }

    async fn send_to(
        &self,
        commands: &Commands,
        sim: &BusId,
        action: AdminAction,
    ) -> Result<AdminReply, SendError> {
        let command = AdminCommand {
            request: Uuid::new_v4(),
            actor: self.actor.account,
            action,
        };
        commands.send(sim, &command).await
    }
}

/// The status and text of a sim's answer.
fn answer(reply: Result<AdminReply, SendError>) -> (StatusCode, String) {
    match reply {
        Ok(AdminReply::Done) => (StatusCode::OK, "done".to_owned()),
        Ok(AdminReply::Spawned { entities }) => {
            let list: Vec<String> = entities.iter().map(ToString::to_string).collect();
            (
                StatusCode::OK,
                format!("spawned {}: {}", entities.len(), list.join(", ")),
            )
        }
        Ok(AdminReply::NotMine) => (
            StatusCode::CONFLICT,
            "not_mine: the target moved to another sim; try again".to_owned(),
        ),
        Ok(AdminReply::Failed { reason }) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("failed: {reason}"),
        ),
        Ok(AdminReply::Events { running }) => {
            (StatusCode::OK, format!("{} events running", running.len()))
        }
        Err(SendError::Timeout) => (StatusCode::GATEWAY_TIMEOUT, "timeout".to_owned()),
        Err(SendError::Bus(error)) => (StatusCode::BAD_GATEWAY, format!("bus: {error}")),
    }
}

fn gm<'a>(
    state: &'a AppState,
    actor: &'a Actor,
    command: &'static str,
    target: String,
    detail: impl Serialize,
) -> Gm<'a> {
    Gm {
        state,
        actor,
        command,
        target,
        detail: serde_json::to_value(detail).unwrap_or_default(),
    }
}

fn player(entity: u64) -> String {
    format!("player {entity}")
}

async fn teleport(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<TeleportRequest>,
) -> Response {
    let gm = gm(&state, &actor, "teleport", player(request.entity), request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    let to = Point {
        x: request.x,
        y: request.y,
        z: request.z,
    };
    gm.send(
        Owner::Player(request.entity),
        AdminAction::TeleportPlayer {
            entity: request.entity,
            to,
        },
    )
    .await
}

async fn teleport_to_player(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<TeleportToPlayerRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "teleport_to_player",
        player(request.entity),
        request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    let position = state
        .live
        .as_ref()
        .and_then(|live| lock(live.state()).player_position(request.to_entity));
    let Some([x, y, z]) = position else {
        gm.audit("not_found").await;
        return (StatusCode::NOT_FOUND, "no sim shows the player to go to").into_response();
    };
    gm.send(
        Owner::Player(request.entity),
        AdminAction::TeleportPlayer {
            entity: request.entity,
            to: Point { x, y, z },
        },
    )
    .await
}

/// A command whose only value is the player.
async fn on_player(
    state: &AppState,
    actor: &Actor,
    command: &'static str,
    rank: AdminRank,
    request: PlayerRequest,
    action: AdminAction,
) -> Response {
    let gm = gm(state, actor, command, player(request.entity), request);
    if let Some(refused) = gm.refused(rank).await {
        return refused;
    }
    gm.send(Owner::Player(request.entity), action).await
}

async fn kick(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<PlayerRequest>,
) -> Response {
    let action = AdminAction::Kick {
        entity: request.entity,
    };
    on_player(&state, &actor, "kick", KICK_RANK, request, action).await
}

async fn heal(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<PlayerRequest>,
) -> Response {
    let action = AdminAction::Heal {
        entity: request.entity,
    };
    on_player(&state, &actor, "heal", GM_RANK, request, action).await
}

async fn kill(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<PlayerRequest>,
) -> Response {
    let action = AdminAction::Kill {
        entity: request.entity,
    };
    on_player(&state, &actor, "kill", GM_RANK, request, action).await
}

async fn god(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<GodRequest>,
) -> Response {
    let gm = gm(&state, &actor, "god_mode", player(request.entity), request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::GodMode {
            entity: request.entity,
            on: request.on,
        },
    )
    .await
}

async fn give_item(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<ItemRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "give_item",
        player(request.entity),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::GiveItem {
            entity: request.entity,
            item: request.item.clone(),
            quantity: request.quantity.unwrap_or(1),
        },
    )
    .await
}

async fn give_experience(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<ExperienceRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "give_experience",
        player(request.entity),
        request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::GiveExperience {
            entity: request.entity,
            amount: request.amount,
        },
    )
    .await
}

async fn set_level(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<LevelRequest>,
) -> Response {
    let gm = gm(&state, &actor, "set_level", player(request.entity), request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::SetLevel {
            entity: request.entity,
            level: request.level.saturating_sub(1),
        },
    )
    .await
}

async fn give_mastery(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<MasteryRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "give_mastery",
        player(request.entity),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::GiveMasteryExperience {
            entity: request.entity,
            mastery: request.mastery.clone(),
            amount: request.amount,
        },
    )
    .await
}

async fn set_mastery_rank(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<MasteryRankRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "set_mastery_rank",
        player(request.entity),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::SetMasteryRank {
            entity: request.entity,
            mastery: request.mastery.clone(),
            rank: request.rank.saturating_sub(1),
        },
    )
    .await
}

async fn set_coins(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<CoinsRequest>,
) -> Response {
    let gm = gm(&state, &actor, "set_coins", player(request.entity), request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::SetCoins {
            entity: request.entity,
            coins: request.coins,
        },
    )
    .await
}

/// The weapon masteries and the tops of the level and rank, as the page shows them.
#[derive(Debug, Serialize)]
struct ProgressionView {
    masteries: Vec<String>,
    max_level: u32,
    max_rank: u16,
}

async fn progression(State(state): State<AppState>) -> Response {
    let world = state.world.get();
    let progression = world.progression();
    Json(ProgressionView {
        masteries: progression
            .masteries()
            .iter()
            .map(|mastery| mastery.id.clone())
            .collect(),
        max_level: progression.max_level().saturating_add(1),
        max_rank: progression
            .masteries()
            .iter()
            .map(|mastery| mastery.max_rank)
            .max()
            .map_or(0, |rank| rank.saturating_add(1)),
    })
    .into_response()
}

async fn start_quest(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<QuestRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "start_quest",
        player(request.entity),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::StartQuest {
            entity: request.entity,
            objective: request.objective.clone(),
        },
    )
    .await
}

async fn start_event(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<EventStartRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "start_event",
        request.event.clone(),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Point(request.x, request.y),
        AdminAction::StartEvent {
            event: request.event.clone(),
            at: Point {
                x: request.x,
                y: request.y,
                z: request.z,
            },
        },
    )
    .await
}

/// Every sim the live state shows.
fn all_sims(state: &AppState) -> Vec<BusId> {
    state.live.as_ref().map_or_else(Vec::new, |live| {
        lock(live.state())
            .sim_ids()
            .into_iter()
            .filter_map(|id| BusId::try_from(id).ok())
            .collect()
    })
}

async fn stop_event(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<EventStopRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "stop_event",
        request.event.clone(),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    let Some(commands) = &state.commands else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut stopped = Vec::new();
    for sim in all_sims(&state) {
        let reply = gm
            .send_to(
                commands,
                &sim,
                AdminAction::StopEvent {
                    event: request.event.clone(),
                },
            )
            .await;
        if matches!(reply, Ok(AdminReply::Done)) {
            stopped.push(sim.to_string());
        }
    }
    let (status, result) = if stopped.is_empty() {
        (StatusCode::NOT_FOUND, "not running".to_owned())
    } else {
        (StatusCode::OK, format!("stopped on {}", stopped.join(", ")))
    };
    gm.audit(&result).await;
    (status, result).into_response()
}

async fn events(State(state): State<AppState>, Extension(actor): Extension<Actor>) -> Response {
    if actor.rank < GM_RANK {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(commands) = &state.commands else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut running = Vec::new();
    for sim in all_sims(&state) {
        let command = AdminCommand {
            request: Uuid::new_v4(),
            actor: actor.account,
            action: AdminAction::ListEvents,
        };
        if let Ok(AdminReply::Events { running: here }) = commands.send(&sim, &command).await {
            running.extend(here.into_iter().map(|event| (sim.to_string(), event)));
        }
    }
    Json(EventsView {
        defined: state.world.get().event_ids().await,
        running,
    })
    .into_response()
}

async fn take_item(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<ItemRequest>,
) -> Response {
    let gm = gm(
        &state,
        &actor,
        "take_item",
        player(request.entity),
        &request,
    );
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Player(request.entity),
        AdminAction::TakeItem {
            entity: request.entity,
            item: request.item.clone(),
        },
    )
    .await
}

async fn spawn(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<SpawnRequest>,
) -> Response {
    let target = format!(
        "{} x{} at ({}, {})",
        request.vitals, request.count, request.x, request.y
    );
    let gm = gm(&state, &actor, "spawn", target, &request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    gm.send(
        Owner::Point(request.x, request.y),
        AdminAction::SpawnMobs {
            vitals: request.vitals.clone(),
            level: request.level,
            at: Point {
                x: request.x,
                y: request.y,
                z: request.z,
            },
            count: request.count,
        },
    )
    .await
}

/// Removes the listed GM mobs from their sims, or every GM mob of every sim.
async fn despawn(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<DespawnRequest>,
) -> Response {
    let target = if request.entities.is_empty() {
        "all GM mobs".to_owned()
    } else {
        format!("{} GM mobs", request.entities.len())
    };
    let gm = gm(&state, &actor, "despawn", target, &request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    let (Some(commands), Some(live)) = (&state.commands, &state.live) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let batches = despawn_batches(&lock(live.state()), &request.entities);
    if batches.is_empty() {
        gm.audit("not_found").await;
        return (StatusCode::NOT_FOUND, "no sim shows these mobs").into_response();
    }
    let mut results = Vec::new();
    let mut worst = StatusCode::OK;
    for (sim, entities) in batches {
        let (status, result) = answer(
            gm.send_to(commands, &sim, AdminAction::DespawnMobs { entities })
                .await,
        );
        if status != StatusCode::OK {
            worst = status;
        }
        results.push(format!("{sim}: {result}"));
    }
    let result = results.join("; ");
    gm.audit(&result).await;
    (worst, result).into_response()
}

/// The despawn command of each sim: every sim with an empty list (all its GM mobs), or the
/// listed mobs grouped by the sim that shows them.
fn despawn_batches(
    state: &crate::live::state::LiveState,
    entities: &[u64],
) -> Vec<(BusId, Vec<u64>)> {
    if entities.is_empty() {
        return state
            .sim_ids()
            .into_iter()
            .filter_map(|id| BusId::try_from(id).ok())
            .map(|sim| (sim, Vec::new()))
            .collect();
    }
    let mut batches: Vec<(BusId, Vec<u64>)> = Vec::new();
    for entity in entities {
        let Some(sim) = state.sim_of_mob(*entity) else {
            continue;
        };
        match batches.iter_mut().find(|(known, _)| *known == sim) {
            Some((_, list)) => list.push(*entity),
            None => batches.push((sim, vec![*entity])),
        }
    }
    batches
}

async fn ban(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<BanRequest>,
) -> Response {
    let command = if request.banned { "ban" } else { "unban" };
    let username = request.username.trim().to_lowercase();
    let gm = gm(&state, &actor, command, username.clone(), &request);
    if let Some(refused) = gm.refused(GM_RANK).await {
        return refused;
    }
    let Some(store) = state.access.store() else {
        return (StatusCode::NOT_FOUND, "bans need [access] global_database").into_response();
    };
    let (status, result) = match store.set_banned(&username, request.banned).await {
        Ok(true) => (StatusCode::OK, "done".to_owned()),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            "no account has this login name".to_owned(),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed: {error}"),
        ),
    };
    gm.audit(&result).await;
    (status, result).into_response()
}

async fn ranks(State(state): State<AppState>, Extension(actor): Extension<Actor>) -> Response {
    if actor.rank < RANKS_RANK {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(store) = state.access.store() else {
        return Json(Vec::<nw_db::RankRow>::new()).into_response();
    };
    match store.ranks().await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response(),
    }
}

async fn set_rank(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<RankRequest>,
) -> Response {
    let username = request.username.trim().to_lowercase();
    let gm = gm(&state, &actor, "rank", username.clone(), &request);
    if let Some(refused) = gm.refused(RANKS_RANK).await {
        return refused;
    }
    let Some(store) = state.access.store() else {
        return (StatusCode::NOT_FOUND, "ranks need [access] global_database").into_response();
    };
    let (status, result) = match store.set_rank(&username, request.rank, actor.account).await {
        Ok(true) => (StatusCode::OK, "done".to_owned()),
        Ok(false) => (
            StatusCode::NOT_FOUND,
            "no account has this login name".to_owned(),
        ),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed: {error}"),
        ),
    };
    gm.audit(&result).await;
    (status, result).into_response()
}

async fn audit(
    State(state): State<AppState>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<AuditQuery>,
) -> Response {
    let Some(store) = state.access.store() else {
        return Json(Vec::<nw_db::AuditRow>::new()).into_response();
    };
    let limit = query
        .limit
        .unwrap_or(AUDIT_DEFAULT_ROWS)
        .clamp(1, AUDIT_MAX_ROWS);
    let own = (actor.rank < AdminRank::HeadAdmin)
        .then_some(actor.account)
        .flatten();
    match store.audit_rows(own, limit).await {
        Ok(rows) => Json(rows).into_response(),
        Err(error) => {
            tracing::warn!(%error, "audit rows");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::body::Body;
    use axum::http::{Method, Request, header};
    use nw_bus::{CellTable, SimHeartbeat, SimSlot, SimSnapshot, SnapMob, SnapPlayer};
    use tower::ServiceExt;

    use super::*;
    use crate::access::store::memory::{MemoryStore, lock as lock_store};
    use crate::access::store::{LoginRecord, Store};
    use crate::access::{Access, AccessMode};
    use crate::captures::Captures;
    use crate::gm::fake::{FakeSims, commands};
    use crate::guard::{ADMIN_HEADER, ADMIN_HEADER_VALUE, AdminOrigin};
    use crate::live::bots::idle_bots;
    use crate::live::{Live, shared};
    use crate::server::router;
    use crate::world::tests::test_layer;

    const CAP: usize = 5_000;
    const LISTEN: &str = "127.0.0.1:9180";
    const PLAYER: u64 = 41;
    const OTHER: u64 = 42;
    const OTHER_X: f32 = 77.0;
    const GM_MOB: u64 = 9_000;
    const SIM: &str = "sim-a";
    const LOGIN: &str = "ada";
    const LOGIN_ACCOUNT: u128 = 5;

    struct Fixture {
        sims: Arc<Mutex<FakeSims>>,
        store: Arc<Mutex<MemoryStore>>,
        state: AppState,
    }

    fn sim() -> BusId {
        BusId::try_from(SIM.to_owned()).expect("id")
    }

    fn snap_player(entity: u64, x: f32) -> SnapPlayer {
        SnapPlayer {
            entity,
            name: format!("Bot{entity}"),
            x,
            y: 2.0,
            z: 3.0,
            dead: false,
            frozen: false,
            health: 0,
            max_health: 0,
            target_entity: None,
        }
    }

    fn fixture() -> Fixture {
        let live_state = shared();
        {
            let mut live = lock(&live_state);
            live.snapshot_part(SimSnapshot {
                sim: sim(),
                time_ms: 1,
                part: 0,
                parts: 1,
                players: vec![snap_player(PLAYER, 1.0), snap_player(OTHER, OTHER_X)],
                awake_mobs: vec![SnapMob {
                    entity: GM_MOB,
                    x: 1.0,
                    y: 2.0,
                    z: 3.0,
                    state: 1,
                    health: 1,
                    max_health: 1,
                    target_entity: None,
                }],
                mob_counts: None,
            });
            live.table(
                CellTable {
                    version: 1,
                    cell_size_m: 1024.0,
                    columns: 1,
                    rows: 1,
                    sims: vec![SimSlot {
                        id: sim(),
                        generation: 1,
                    }],
                    owners: vec![Some(0)],
                },
                1,
            );
            live.heartbeat(
                SimHeartbeat {
                    id: sim(),
                    started_at_ms: 1,
                    players: 0,
                    max_players: 1,
                    level: "lv".to_owned(),
                    instance: None,
                },
                1,
            );
        }
        let live = Live::new(
            live_state.clone(),
            std::time::Duration::from_millis(1),
            idle_bots(live_state),
            tokio::sync::watch::channel(false).1,
        );
        let sims = Arc::new(Mutex::new(FakeSims::default()));
        let mut memory = MemoryStore::default();
        memory.logins.insert(
            LOGIN.to_owned(),
            LoginRecord {
                account: Uuid::from_u128(LOGIN_ACCOUNT),
                banned: false,
                password_hash: String::new(),
            },
        );
        let store = Arc::new(Mutex::new(memory));
        let listen = LISTEN.parse().expect("address");
        let state = AppState::new(
            test_layer(CAP),
            std::env::temp_dir(),
            AdminOrigin::of(listen),
            Captures::default(),
            Some(live),
        )
        .with_access(Access::new(
            AccessMode::Local,
            Some(Store::Memory(store.clone())),
            None,
        ))
        .with_commands(commands(&sims));
        Fixture { sims, store, state }
    }

    fn post(uri: &str, body: &Value) -> Request<Body> {
        Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, LISTEN)
            .header(ADMIN_HEADER, ADMIN_HEADER_VALUE)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("request")
    }

    async fn status(fixture: &Fixture, request: Request<Body>) -> StatusCode {
        router(fixture.state.clone())
            .oneshot(request)
            .await
            .expect("response")
            .status()
    }

    fn answer_next(fixture: &Fixture, reply: Option<AdminReply>) {
        fixture.sims.lock().expect("sims").answers.push_back(reply);
    }

    fn sent(fixture: &Fixture) -> Vec<AdminAction> {
        fixture
            .sims
            .lock()
            .expect("sims")
            .sent
            .iter()
            .map(|(_, command)| command.action.clone())
            .collect()
    }

    fn results(fixture: &Fixture) -> Vec<String> {
        lock_store(&fixture.store)
            .audit
            .iter()
            .map(|entry| entry.result.clone())
            .collect()
    }

    fn teleport_body(entity: u64) -> Value {
        serde_json::json!({ "entity": entity, "x": 10.0, "y": 20.0, "z": 30.0 })
    }

    #[tokio::test]
    async fn a_teleport_goes_to_the_owning_sim_and_is_audited() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::Done));
        let request = post(TELEPORT_ROUTE, &teleport_body(PLAYER));
        assert_eq!(status(&fixture, request).await, StatusCode::OK);
        assert!(matches!(
            sent(&fixture)[..],
            [AdminAction::TeleportPlayer { entity: PLAYER, .. }]
        ));
        assert_eq!(results(&fixture), ["done"]);
    }

    #[tokio::test]
    async fn a_quest_start_goes_to_the_players_sim() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::Done));
        let body = serde_json::json!({ "entity": PLAYER, "objective": "12_GetOffBeach" });
        let request = post(START_QUEST_ROUTE, &body);
        assert_eq!(status(&fixture, request).await, StatusCode::OK);
        assert!(matches!(
            &sent(&fixture)[..],
            [AdminAction::StartQuest { entity: PLAYER, objective }] if objective == "12_GetOffBeach"
        ));
    }

    #[tokio::test]
    async fn progression_commands_reach_the_players_sim_with_zero_based_values() {
        let fixture = fixture();
        let bodies = [
            (
                GIVE_EXPERIENCE_ROUTE,
                serde_json::json!({ "entity": PLAYER, "amount": 500 }),
            ),
            (
                SET_LEVEL_ROUTE,
                serde_json::json!({ "entity": PLAYER, "level": 60 }),
            ),
            (
                GIVE_MASTERY_ROUTE,
                serde_json::json!({ "entity": PLAYER, "mastery": "BowAbilityTable", "amount": 20 }),
            ),
            (
                SET_MASTERY_RANK_ROUTE,
                serde_json::json!({ "entity": PLAYER, "mastery": null, "rank": 20 }),
            ),
            (
                SET_COINS_ROUTE,
                serde_json::json!({ "entity": PLAYER, "coins": 1000 }),
            ),
        ];
        for (route, body) in &bodies {
            answer_next(&fixture, Some(AdminReply::Done));
            assert_eq!(
                status(&fixture, post(route, body)).await,
                StatusCode::OK,
                "{route}"
            );
        }
        assert_eq!(
            sent(&fixture),
            [
                AdminAction::GiveExperience {
                    entity: PLAYER,
                    amount: 500
                },
                AdminAction::SetLevel {
                    entity: PLAYER,
                    level: 59
                },
                AdminAction::GiveMasteryExperience {
                    entity: PLAYER,
                    mastery: "BowAbilityTable".to_owned(),
                    amount: 20
                },
                AdminAction::SetMasteryRank {
                    entity: PLAYER,
                    mastery: None,
                    rank: 19
                },
                AdminAction::SetCoins {
                    entity: PLAYER,
                    coins: 1000
                },
            ]
        );
        assert_eq!(results(&fixture), ["done"; 5]);
    }

    #[tokio::test]
    async fn not_mine_and_timeout_are_errors_with_audit_rows() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::NotMine));
        answer_next(&fixture, None);
        let first = post(TELEPORT_ROUTE, &teleport_body(PLAYER));
        assert_eq!(status(&fixture, first).await, StatusCode::CONFLICT);
        let second = post(TELEPORT_ROUTE, &teleport_body(PLAYER));
        assert_eq!(status(&fixture, second).await, StatusCode::GATEWAY_TIMEOUT);
        let results = results(&fixture);
        assert!(results[0].starts_with("not_mine"));
        assert_eq!(results[1], "timeout");
    }

    #[tokio::test]
    async fn a_csr_may_kick_but_not_teleport_or_change_ranks() {
        let fixture = fixture();
        let csr = Actor {
            account: Some(Uuid::from_u128(9)),
            username: "csr".to_owned(),
            rank: AdminRank::Csr,
        };
        let app = || {
            gm_routes()
                .layer(Extension(csr.clone()))
                .with_state(fixture.state.clone())
        };
        let refused = app()
            .oneshot(post(TELEPORT_ROUTE, &teleport_body(PLAYER)))
            .await
            .expect("response");
        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
        assert!(sent(&fixture).is_empty(), "no command for a refused rank");
        answer_next(&fixture, Some(AdminReply::Done));
        let kicked = app()
            .oneshot(post(KICK_ROUTE, &serde_json::json!({ "entity": PLAYER })))
            .await
            .expect("response");
        assert_eq!(kicked.status(), StatusCode::OK);
        let rank = serde_json::json!({ "username": LOGIN, "rank": "csr" });
        let ranks = app()
            .oneshot(post(RANKS_ROUTE, &rank))
            .await
            .expect("response");
        assert_eq!(ranks.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            results(&fixture),
            ["refused: rank", "done", "refused: rank"]
        );
    }

    #[tokio::test]
    async fn a_teleport_to_a_player_uses_its_position() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::Done));
        let body = serde_json::json!({ "entity": PLAYER, "to_entity": OTHER });
        let request = post(TELEPORT_TO_PLAYER_ROUTE, &body);
        assert_eq!(status(&fixture, request).await, StatusCode::OK);
        assert!(matches!(
            sent(&fixture)[..],
            [AdminAction::TeleportPlayer { entity: PLAYER, to: Point { x, .. } }] if x == OTHER_X
        ));
    }

    #[tokio::test]
    async fn a_spawn_goes_to_the_sim_of_the_point() {
        let fixture = fixture();
        answer_next(
            &fixture,
            Some(AdminReply::Spawned {
                entities: vec![GM_MOB],
            }),
        );
        let body = serde_json::json!({
            "vitals": "Wolf", "level": 10, "x": 100.0, "y": 100.0, "z": 5.0, "count": 1
        });
        assert_eq!(
            status(&fixture, post(SPAWN_ROUTE, &body)).await,
            StatusCode::OK
        );
        assert!(matches!(
            sent(&fixture)[..],
            [AdminAction::SpawnMobs { count: 1, .. }]
        ));
        assert!(results(&fixture)[0].starts_with("spawned 1"));
        let off_grid = serde_json::json!({
            "vitals": "Wolf", "level": 10, "x": 5000.0, "y": 100.0, "z": 5.0, "count": 1
        });
        assert_eq!(
            status(&fixture, post(SPAWN_ROUTE, &off_grid)).await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn an_event_starts_on_the_sim_of_its_point() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::Done));
        let body = serde_json::json!({ "event": "invasion", "x": 100.0, "y": 100.0, "z": 5.0 });
        assert_eq!(
            status(&fixture, post(START_EVENT_ROUTE, &body)).await,
            StatusCode::OK
        );
        assert!(matches!(
            &sent(&fixture)[..],
            [AdminAction::StartEvent { event, .. }] if event == "invasion"
        ));
    }

    #[tokio::test]
    async fn a_despawn_of_all_asks_every_sim_and_a_list_asks_their_sims() {
        let fixture = fixture();
        answer_next(&fixture, Some(AdminReply::Done));
        let all = post(DESPAWN_ROUTE, &serde_json::json!({}));
        assert_eq!(status(&fixture, all).await, StatusCode::OK);
        answer_next(&fixture, Some(AdminReply::Done));
        let listed = post(DESPAWN_ROUTE, &serde_json::json!({ "entities": [GM_MOB] }));
        assert_eq!(status(&fixture, listed).await, StatusCode::OK);
        let sent = sent(&fixture);
        assert!(matches!(&sent[0], AdminAction::DespawnMobs { entities } if entities.is_empty()));
        assert!(matches!(&sent[1], AdminAction::DespawnMobs { entities } if entities == &[GM_MOB]));
    }

    #[tokio::test]
    async fn a_ban_and_a_rank_change_reach_the_store() {
        let fixture = fixture();
        let ban = serde_json::json!({ "username": "ADA", "banned": true });
        assert_eq!(
            status(&fixture, post(BAN_ROUTE, &ban)).await,
            StatusCode::OK
        );
        assert!(lock_store(&fixture.store).logins[LOGIN].banned);
        let unknown = serde_json::json!({ "username": "nobody", "banned": true });
        assert_eq!(
            status(&fixture, post(BAN_ROUTE, &unknown)).await,
            StatusCode::NOT_FOUND
        );
        let rank = serde_json::json!({ "username": LOGIN, "rank": "game_master" });
        assert_eq!(
            status(&fixture, post(RANKS_ROUTE, &rank)).await,
            StatusCode::OK
        );
        assert_eq!(
            lock_store(&fixture.store)
                .ranks
                .get(&Uuid::from_u128(LOGIN_ACCOUNT)),
            Some(&AdminRank::GameMaster)
        );
        let removed = serde_json::json!({ "username": LOGIN, "rank": null });
        assert_eq!(
            status(&fixture, post(RANKS_ROUTE, &removed)).await,
            StatusCode::OK
        );
        assert!(lock_store(&fixture.store).ranks.is_empty());
    }

    #[tokio::test]
    async fn an_unknown_player_is_not_found() {
        let fixture = fixture();
        let request = post(TELEPORT_ROUTE, &teleport_body(PLAYER + 100));
        assert_eq!(status(&fixture, request).await, StatusCode::NOT_FOUND);
        assert!(sent(&fixture).is_empty());
    }
}
