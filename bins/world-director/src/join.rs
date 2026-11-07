// SPDX-License-Identifier: AGPL-3.0-or-later
//! One join (M5 spec section 3.1): check the account, load the position, the items, the quests
//! and the progression, read the account's admin rank, take an entity number, and ask the owner
//! of the cell to take the player.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nw_bus::{
    Admit, AdmitReply, Bus, CarriedItem, CarriedMount, HomeRecord, HousingItem, InstanceStay,
    ItemLookState, ItemPlace, ItemState, Join, LootLimitCount, MOB_ENTITY_BASE, OwnedHome,
    PlacedCamp, PlayerArena, PlayerInventory, PlayerMounts, PlayerProgression, PlayerQuests,
    PlayerTravel, Point, QuestRun, SavedLoadout, SimSlot, StoredItem, StoredPoint, subject,
};
use nw_db::{
    AdminRank, PgPool, SavedArena, SavedInventory, SavedItem, SavedItemRolls, SavedMounts,
    SavedPlace, SavedPosition, SavedProgression, SavedQuests, SavedStatusEffect, SavedTravel,
    account_by_id, account_entitlements, admin_rank, character_by_id, character_inventory,
    character_items, character_position, character_progression, character_quests,
    next_entity_number,
};
use nw_login::look::{saved_backstory, saved_look};
use nw_sim_core::{Position, TUTORIAL_FOLDER, TUTORIAL_START};
use uuid::Uuid;

use crate::instances::InstanceTable;
use crate::sims::Assignment;

/// Longest wait for one read of an account's admin rank, age and store entitlements.
const RANK_READ_TIMEOUT: Duration = Duration::from_millis(300);
/// Reads of the account before the join is refused, and the pause between two. Inferred: three
/// reads and two pauses take at most 1.1 s, so with the admit timeout (3 s,
/// `config/world-director.toml`) the join stays below the gateway's 5 s
/// (`config/rep-gateway.toml` `join_timeout_ms`).
const ACCOUNT_READ_ATTEMPTS: u32 = 3;
const ACCOUNT_READ_PAUSE: Duration = Duration::from_millis(100);
/// An account younger than this is in probation: its client blocks global and trade chat
/// (the client's `javelin.account-probation-time-hours` default 72, the text
/// `ui_cannot_chat_global_blocked` "...until your account is older than {hoursNeeded} hours").
const ACCOUNT_PROBATION_HOURS: i64 = 72;
const SECONDS_PER_HOUR: i64 = 3_600;

/// The handoff count and the bundle sequence of a new player.
const NEW_PLAYER_COUNT: u64 = 0;

/// What every join needs.
#[derive(Debug, Clone)]
pub struct JoinContext {
    /// The world.
    pub world_id: Uuid,
    /// The open map level.
    pub level: String,
    /// Where characters that were never in the world appear, in turn.
    pub starts: Vec<Position>,
    /// Longest wait for the sim.
    pub admit_timeout: Duration,
    /// The world database.
    pub db: PgPool,
    /// The global database with the admin ranks; `None` gives no account the GM flag.
    pub ranks: Option<PgPool>,
    /// NATS.
    pub bus: Bus,
}

/// Why a join was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JoinRefusal {
    /// The world has no such character.
    #[error("no such character")]
    NoSuchCharacter,
    /// The character belongs to another account.
    #[error("the character belongs to another account")]
    OtherAccount,
    /// The database failed.
    #[error("database: {0}")]
    Database(String),
    /// No live sim owns the player's cell.
    #[error("no sim owns the cell")]
    NoOwner,
    /// The director has no start position.
    #[error("no start position")]
    NoStart,
    /// The sim refused the player.
    #[error("the sim refused: {0}")]
    SimRefused(String),
    /// The sim did not answer in time.
    #[error("the sim did not answer: {0}")]
    SimSilent(String),
    /// The entity number is in the mob range.
    #[error("entity number {0} is in the mob range")]
    EntityRange(u64),
    /// The account's rank, age and store entitlements did not read: the player tries again
    /// (a join without them would drop its GM rights, its probation and its bought items).
    #[error("the account did not read; try again: {0}")]
    AccountUnavailable(String),
    /// The account is banned (global database): a ban ends its live game sessions in
    /// `edge-http`, and an entry ticket it got before the ban enters no world.
    #[error("the account is banned")]
    Banned,
}

impl JoinRefusal {
    /// Metric label.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::NoSuchCharacter => "no_such_character",
            Self::OtherAccount => "other_account",
            Self::Database(_) => "database",
            Self::NoOwner => "no_owner",
            Self::NoStart => "no_start",
            Self::SimRefused(_) => "sim_refused",
            Self::SimSilent(_) => "sim_silent",
            Self::EntityRange(_) => "entity_range",
            Self::AccountUnavailable(_) => "account_unavailable",
            Self::Banned => "banned",
        }
    }
}

/// `entity` as a player's entity number: refused in the mob range (M6-A spec section 6.1).
pub fn player_entity(entity: u64) -> Result<u64, JoinRefusal> {
    if entity >= MOB_ENTITY_BASE {
        return Err(JoinRefusal::EntityRange(entity));
    }
    Ok(entity)
}

/// The start position of a new character with entity number `entity`: the start positions in
/// turn (M5 load spec section 6).
pub fn start_for(starts: &[Position], entity: u64) -> Option<Position> {
    let count = u64::try_from(starts.len())
        .ok()
        .filter(|count| *count > 0)?;
    // The remainder is below the length, so it fits usize.
    starts.get((entity % count) as usize).copied()
}

/// Where a character with entity number `entity` appears: its saved spot when that spot is on
/// `level` and `on_map` holds for it, else its start ([`start_for`]). A spot that is off the map
/// would refuse every later join of the character; the sim puts the height on the ground.
pub fn join_position(
    saved: Option<&SavedPosition>,
    level: &str,
    on_map: impl Fn(Position) -> bool,
    starts: &[Position],
    entity: u64,
) -> Option<Position> {
    saved
        .filter(|saved| saved.level == level)
        .map(|saved| {
            let [x, y, z] = saved.metres();
            Position { x, y, z }
        })
        .filter(|position| on_map(*position))
        .or_else(|| start_for(starts, entity))
}

/// Whether a character with the saved spot `saved` starts in the tutorial: it was never in the
/// world, or it left during the tutorial (which then starts again).
pub fn starts_in_tutorial(saved: Option<&SavedPosition>) -> bool {
    saved.is_none_or(|saved| saved.level == TUTORIAL_FOLDER)
}

/// Where a character that has an instance stay joins.
#[derive(Debug, Clone, PartialEq)]
pub enum StayRoute {
    /// Back into its instance, still open for it, on its instance sim at its saved spot.
    Instance(SimSlot, Position),
    /// Back at the spot it left the open map from: the instance ended (retail returns a player
    /// whose expedition is gone to where it entered from).
    Return(Position),
}

/// The route of a character with the stay `stay` and the saved spot `saved`, on the open map
/// `level` with the open instances `instances`; `None` when the stay returns to another level
/// (the normal join applies).
pub fn stay_route(
    stay: &InstanceStay,
    saved: Option<&SavedPosition>,
    instances: &InstanceTable,
    character: Uuid,
    level: &str,
) -> Option<StayRoute> {
    if let Some(open) = instances.sim_for(&stay.instance, &character)
        && let Some(saved) = saved.filter(|saved| saved.level == open.level)
    {
        let [x, y, z] = saved.metres();
        return Some(StayRoute::Instance(open.sim.clone(), Position { x, y, z }));
    }
    (stay.return_level == level)
        .then(|| StayRoute::Return(Position::from(stay.return_position.point())))
}

/// Routes `join`: a new player to the tutorial sim `tutorial` (when one is live) at the tutorial
/// start, a player with an instance stay back into its open instance (`instances`) or to its
/// return spot, every other player to the owner of its cell in `assignment`. Returns the sim,
/// the entity number (a new one, or the resumed one) and whether the account is a game master.
///
/// # Errors
///
/// Returns the [`JoinRefusal`] of the first step that fails.
pub async fn route_join(
    context: &JoinContext,
    assignment: &Assignment,
    instances: &InstanceTable,
    tutorial: Option<&SimSlot>,
    join: &Join,
) -> Result<(SimSlot, u64, bool), JoinRefusal> {
    let database = |error: nw_db::DbError| JoinRefusal::Database(error.to_string());
    let character = character_by_id(&context.db, join.character)
        .await
        .map_err(database)?
        .ok_or(JoinRefusal::NoSuchCharacter)?;
    if character.account_id != join.account {
        return Err(JoinRefusal::OtherAccount);
    }
    let saved = character_position(&context.db, join.character)
        .await
        .map_err(database)?;
    let (entity, handoffs, bundles_sent) = match join.resume {
        Some(resume) => (
            player_entity(resume.entity)?,
            resume.handoffs,
            resume.bundles_sent,
        ),
        None => (
            player_entity(next_entity_number(&context.db).await.map_err(database)?)?,
            NEW_PLAYER_COUNT,
            NEW_PLAYER_COUNT,
        ),
    };
    let tutorial = tutorial.filter(|_| starts_in_tutorial(saved.as_ref()));
    // The tutorial always starts from the starter kit, no quest and no progression (admin
    // studio S5, S7; the tutorial sim saves none of them).
    let (items, inventory, quests, progression) = match tutorial {
        Some(_) => (
            None,
            PlayerInventory::default(),
            PlayerQuests::default(),
            PlayerProgression::default(),
        ),
        None => (
            character_items(&context.db, join.character)
                .await
                .map_err(database)?
                .map(carried_items),
            player_inventory(
                character_inventory(&context.db, join.character)
                    .await
                    .map_err(database)?,
            ),
            character_quests(&context.db, join.character)
                .await
                .map_err(database)?
                .map(player_quests)
                .unwrap_or_default(),
            character_progression(&context.db, join.character)
                .await
                .map_err(database)?
                .map(|saved| player_progression(saved, unix_now_ns()))
                .unwrap_or_default(),
        ),
    };
    let standing = account_standing(context.ranks.as_ref(), join.account).await?;
    let game_master = standing.game_master;
    let stay = progression.arena.stay.as_ref().and_then(|stay| {
        stay_route(
            stay,
            saved.as_ref(),
            instances,
            join.character,
            &context.level,
        )
    });
    let (sim, position) = match (tutorial, stay) {
        (Some(tutorial), _) => (tutorial.clone(), TUTORIAL_START),
        (None, Some(StayRoute::Instance(sim, position))) => (sim, position),
        (None, Some(StayRoute::Return(position))) if assignment.covers(position) => {
            let sim = assignment
                .owner_at(position)
                .ok_or(JoinRefusal::NoOwner)?
                .clone();
            (sim, position)
        }
        (None, _) => {
            let position = join_position(
                saved.as_ref(),
                &context.level,
                |position| assignment.covers(position),
                &context.starts,
                entity,
            )
            .ok_or(JoinRefusal::NoStart)?;
            let sim = assignment
                .owner_at(position)
                .ok_or(JoinRefusal::NoOwner)?
                .clone();
            (sim, position)
        }
    };
    let admit = Admit {
        connection: join.connection.clone(),
        account: join.account,
        character: join.character,
        name: character.name,
        entity,
        position: Point::from(position),
        login: join.resume.is_none(),
        handoffs,
        bundles_sent,
        look: saved_look(&character.appearance).map(|look| look.choices()),
        backstory: saved_backstory(&character.appearance),
        items,
        inventory,
        quests,
        progression,
        game_master,
        account_in_probation: standing.in_probation,
        entitlements: standing.entitlements,
    };
    let reply = context
        .bus
        .request::<Admit, AdmitReply>(
            subject::sim_admit(context.world_id, &sim.id),
            &admit,
            context.admit_timeout,
        )
        .await
        .map_err(|error| JoinRefusal::SimSilent(error.to_string()))?;
    match reply {
        AdmitReply::Admitted => Ok((sim, entity, game_master)),
        AdmitReply::Refused { reason } => Err(JoinRefusal::SimRefused(reason)),
    }
}

/// What an account's admin rank, age and store entitlements give it in the world.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Standing {
    /// It has the game master rank (or a higher one).
    game_master: bool,
    /// It is in probation ([`in_probation`]).
    in_probation: bool,
    /// The store entitlements it holds.
    entitlements: Vec<String>,
}

/// The standing of `account` from the global database `ranks`. No database gives no GM rights,
/// no probation and no store entitlements (a world with no global database configured). A read
/// that fails or is slower than [`RANK_READ_TIMEOUT`] is tried again, [`ACCOUNT_READ_ATTEMPTS`]
/// times in all.
///
/// # Errors
///
/// Returns [`JoinRefusal::AccountUnavailable`] when no read succeeded, and
/// [`JoinRefusal::Banned`] for a banned account.
async fn account_standing(ranks: Option<&PgPool>, account: Uuid) -> Result<Standing, JoinRefusal> {
    let Some(ranks) = ranks else {
        return Ok(Standing::default());
    };
    let read = retry_read(ACCOUNT_READ_ATTEMPTS, ACCOUNT_READ_PAUSE, || async {
        let read = async {
            Ok::<_, nw_db::DbError>((
                admin_rank(ranks, account).await?,
                account_by_id(ranks, account).await?,
                account_entitlements(ranks, account).await?,
            ))
        };
        match tokio::time::timeout(RANK_READ_TIMEOUT, read).await {
            Ok(Ok(read)) => Ok(read),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err(format!("slower than {RANK_READ_TIMEOUT:?}")),
        }
    })
    .await
    .map_err(|error| {
        tracing::warn!(%error, %account, "account read failed; join refused");
        JoinRefusal::AccountUnavailable(error)
    })?;
    let (rank, found, held) = read;
    standing_of(rank, found, held, unix_now())
}

/// What the admin rank `rank`, the account row `found` and the entitlements `held` give an
/// account at Unix second `now_unix`; a banned account is refused.
fn standing_of(
    rank: Option<AdminRank>,
    found: Option<nw_db::Account>,
    held: Vec<nw_db::AccountEntitlement>,
    now_unix: i64,
) -> Result<Standing, JoinRefusal> {
    if found.as_ref().is_some_and(|found| found.banned) {
        return Err(JoinRefusal::Banned);
    }
    Ok(Standing {
        game_master: rank.is_some_and(|rank| rank >= AdminRank::GameMaster),
        in_probation: in_probation(rank, found.map(|found| found.created_at_unix), now_unix),
        entitlements: held.into_iter().map(|held| held.entitlement).collect(),
    })
}

/// The first success of `read` within `attempts` tries with `pause` between two; else the last
/// error.
async fn retry_read<T, F, Fut>(attempts: u32, pause: Duration, mut read: F) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let mut last = String::new();
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(pause).await;
        }
        match read().await {
            Ok(value) => return Ok(value),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// Whether an account with the admin rank `rank`, created at `created_at_unix`, is in
/// probation at `now_unix`: it is younger than [`ACCOUNT_PROBATION_HOURS`]. Any admin rank (a
/// CSR, a game master, a head admin) is never in probation, and neither is an account whose age
/// is not known. Retail's rule also waits for a character play time
/// (`javelin.character-probation-time-minutes` 180); this server does not count play time.
fn in_probation(rank: Option<AdminRank>, created_at_unix: Option<i64>, now_unix: i64) -> bool {
    rank.is_none()
        && created_at_unix.is_some_and(|created| {
            now_unix.saturating_sub(created) < ACCOUNT_PROBATION_HOURS * SECONDS_PER_HOUR
        })
}

/// The time since 1970; zero for a clock before 1970.
fn since_unix_epoch() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// Unix seconds now; 0 for a clock before 1970.
fn unix_now() -> i64 {
    i64::try_from(since_unix_epoch().as_secs()).unwrap_or(i64::MAX)
}

/// Unix nanoseconds now; 0 for a clock before 1970.
fn unix_now_ns() -> u64 {
    u64::try_from(since_unix_epoch().as_nanos()).unwrap_or(u64::MAX)
}

/// The saved quests as a sim runs them; a running quest whose run no longer reads is dropped.
fn player_quests(saved: SavedQuests) -> PlayerQuests {
    let active = saved
        .active
        .into_iter()
        .filter_map(
            |(objective, run)| match serde_json::from_value::<QuestRun>(run) {
                Ok(run) => Some((objective, run)),
                Err(error) => {
                    tracing::warn!(%error, objective, "saved quest run does not read");
                    None
                }
            },
        )
        .collect();
    PlayerQuests {
        active,
        done: saved.done,
        ..PlayerQuests::default()
    }
}

/// The saved progression as a sim keeps it, loaded at unix ns `now_ns`.
fn player_progression(saved: SavedProgression, now_ns: u64) -> PlayerProgression {
    PlayerProgression {
        experience: saved.experience,
        coins: saved.coins,
        achievements: saved.achievements,
        loot_limits: saved
            .loot_limits
            .into_iter()
            .map(|limit| LootLimitCount {
                limit_id: limit.limit_id,
                count: limit.count,
                counted_ns: limit.counted_ns,
                ends_ns: limit.ends_ns,
            })
            .collect(),
        masteries: saved.masteries,
        attributes: saved.attributes,
        weapon_points: saved.weapon_points,
        territory_points: saved.territory_points,
        travel: player_travel(saved.travel),
        ability_slots: saved.ability_slots,
        meta_counters: saved.meta_counters,
        meta_completed: saved.meta_completed,
        title: saved.title,
        mounts: player_mounts(saved.mounts),
        arena: player_arena(saved.arena),
        music: nw_bus::PlayerMusic {
            next_performance_ms: saved.music.next_performance_ms,
        },
        notifications: saved.notifications.shown,
        recipe_cooldowns: saved
            .recipe_cooldowns
            .into_iter()
            .map(|cooldown| nw_bus::RecipeCooldownCount {
                recipe_id: cooldown.recipe_id,
                count: cooldown.count,
                ends_ns: cooldown.ends_ns,
            })
            .collect(),
        faction: nw_bus::PlayerFaction {
            faction: saved.faction.faction,
            changes: saved.faction.changes,
            changed_ns: saved.faction.changed_ns,
        },
        status_effects: saved
            .status_effects
            .into_iter()
            .map(|effect| player_status_effect(effect, now_ns))
            .collect(),
        // Their ends are on the server clock: they need no shift.
        consumable_cooldowns: saved
            .consumable_cooldowns
            .into_iter()
            .map(|cooldown| nw_bus::PlayerConsumableCooldown {
                group: cooldown.group,
                consumable: cooldown.consumable,
                end_us: cooldown.end_us,
            })
            .collect(),
    }
}

/// A saved consumable effect as a sim keeps it at unix ns `now_ns`: a pausing effect (no
/// `DisablePauseOnPersist`) and its repeats move on by the time since the save, so it keeps the
/// time it had left.
fn player_status_effect(saved: SavedStatusEffect, now_ns: u64) -> nw_bus::PlayerStatusEffect {
    let paused_ns = if saved.runs_offline {
        0
    } else {
        now_ns.saturating_sub(saved.saved_ns)
    };
    nw_bus::PlayerStatusEffect {
        consumable_id: saved.consumable_id,
        status_id: saved.status_id,
        ends_ns: saved.ends_ns.saturating_add(paused_ns),
        stacks: saved.stacks,
        runs_offline: saved.runs_offline,
        repeat: saved.repeat.map(|(next_ns, ends_ns)| nw_bus::EffectRepeat {
            next_ns: next_ns.saturating_add(paused_ns),
            ends_ns: ends_ns.saturating_add(paused_ns),
        }),
    }
}

/// The saved arena state as a sim keeps it.
fn player_arena(saved: SavedArena) -> PlayerArena {
    PlayerArena {
        dungeon_cooldown_since_2000_ns: saved.dungeon_cooldown_since_2000_ns,
        solo_trial_cooldown_since_2000_ns: saved.solo_trial_cooldown_since_2000_ns,
        ranks: saved.ranks,
        last_dungeon: saved.last_dungeon,
        last_mutated_dungeon: saved.last_mutated_dungeon,
        base_entered: saved.base_entered,
        mutated_entered: saved.mutated_entered,
        group_trials_entered: saved.group_trials_entered,
        next_base_refresh_ns: saved.next_base_refresh_ns,
        next_mutated_refresh_ns: saved.next_mutated_refresh_ns,
        next_group_trial_refresh_ns: saved.next_group_trial_refresh_ns,
        mutation_award: saved.mutation_award,
        last_dungeon_mutated: saved.last_dungeon_mutated,
        // The instance sim holds the mutation of a stay's instance; a join does not carry it.
        stay: saved.stay.map(|stay| InstanceStay {
            instance: stay.instance,
            game_mode: stay.game_mode,
            phase: stay.phase,
            return_level: stay.return_level,
            return_position: StoredPoint::from_bits(stay.return_position),
            activated_ns: stay.activated_ns,
            mutation_difficulty: None,
        }),
    }
}

/// The saved mounts as a sim keeps them: a login starts on foot (retail login cards).
fn player_mounts(saved: SavedMounts) -> PlayerMounts {
    PlayerMounts {
        selected: saved.selected,
        customized: saved
            .customized
            .into_iter()
            .map(|mount| CarriedMount {
                mount_id: mount.mount_id,
                dye: mount.dye,
                name: mount.name,
            })
            .collect(),
        riding: None,
        summon_cooldown_end_ns: saved.summon_cooldown_end_ns,
    }
}

/// The saved travel state as a sim keeps it.
fn player_travel(saved: SavedTravel) -> PlayerTravel {
    PlayerTravel {
        waypoint: saved.waypoint.map(StoredPoint::from_bits),
        inn: saved.inn,
        camp: saved.camp,
        camp_skin: saved.camp_skin,
        unstuck_ready_ms: saved.unstuck_ready_ms,
        homes: saved
            .homes
            .into_iter()
            .map(|home| OwnedHome {
                plot: home.plot,
                position: StoredPoint::from_bits(home.position),
                purchased_ms: home.purchased_ms,
                items: home
                    .items
                    .into_iter()
                    .map(|item| HousingItem {
                        index: item.index,
                        item: item.item,
                        position: StoredPoint::from_bits(item.position),
                        rotation: item.rotation,
                    })
                    .collect(),
                record: HomeRecord {
                    plot_facet: home.record.plot_facet,
                    plot_extra: home.record.plot_extra,
                    house_data: home.record.house_data,
                    house_data_facet: home.record.house_data_facet,
                    house_type: home.record.house_type,
                    territory: home.record.territory,
                    taxes_due_ms: home.record.taxes_due_ms,
                    fast_travel_ready_ms: home.record.fast_travel_ready_ms,
                },
            })
            .collect(),
        played_ms: saved.played_ms,
        inn_recalled_ms: saved.inn_recalled_ms,
        placed_camp: saved.placed_camp.map(|camp| PlacedCamp {
            position: StoredPoint::from_bits(camp.position),
            tier: camp.tier,
            placed_ms: camp.placed_ms,
            rotation: camp.rotation,
            blueprint: camp.blueprint,
        }),
        visited_towns: saved.visited_towns,
    }
}

/// The saved items as the list a sim builds gear from.
fn carried_items(saved: Vec<SavedItem>) -> Vec<CarriedItem> {
    saved
        .into_iter()
        .map(|item| CarriedItem {
            item_id: item.item_id,
            quantity: item.quantity,
            place: match item.place {
                SavedPlace::Worn(slot) => ItemPlace::Worn(slot),
                SavedPlace::Bag(slot) => ItemPlace::Bag(slot),
            },
            state: item_state(item.instance, item.locked, item.rolls),
        })
        .collect()
}

/// The carried state of a saved item: its instance id, lock, rolls and look.
fn item_state(instance: Option<Uuid>, locked: bool, rolls: SavedItemRolls) -> ItemState {
    ItemState {
        instance: instance.map(Uuid::into_bytes),
        locked,
        gear_score: rolls.gear_score,
        durability: rolls.durability,
        perks: rolls.perks,
        bound: rolls.bound,
        look: ItemLookState {
            dye: rolls.dye,
            appearance: rolls.appearance,
            skin_dye: rolls.skin_dye,
        },
    }
}

/// The saved storage chests and loadouts as a sim takes them.
fn player_inventory(saved: SavedInventory) -> PlayerInventory {
    PlayerInventory {
        stored: saved
            .stored
            .into_iter()
            .map(|item| StoredItem {
                storage: item.storage.into_bytes(),
                slot: item.slot,
                item_id: item.item_id,
                quantity: item.quantity,
                state: item_state(item.instance, item.locked, item.rolls),
            })
            .collect(),
        loadouts: saved
            .loadouts
            .into_iter()
            .map(|loadout| SavedLoadout {
                index: loadout.index,
                name: loadout.name,
                items: loadout
                    .items
                    .into_iter()
                    .map(|(slot, instance)| (slot, instance.into_bytes()))
                    .collect(),
                attributes: loadout.attributes,
            })
            .collect(),
        hidden_skins: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_failed_account_read_is_tried_again_then_refuses_the_join() {
        let pause = Duration::from_millis(1);
        let mut calls = 0;
        let second = retry_read(ACCOUNT_READ_ATTEMPTS, pause, || {
            calls += 1;
            let call = calls;
            async move {
                if call < 2 {
                    Err("slow".to_owned())
                } else {
                    Ok(call)
                }
            }
        })
        .await;
        assert_eq!(second, Ok(2));
        let mut tries = 0;
        let failed: Result<(), String> = retry_read(ACCOUNT_READ_ATTEMPTS, pause, || {
            tries += 1;
            async { Err("down".to_owned()) }
        })
        .await;
        assert_eq!(failed, Err("down".to_owned()));
        assert_eq!(tries, ACCOUNT_READ_ATTEMPTS);
        assert_eq!(
            JoinRefusal::AccountUnavailable("down".to_owned()).label(),
            "account_unavailable"
        );
        // No global database configured: the join goes on with no rights.
        assert_eq!(
            account_standing(None, Uuid::from_u128(1)).await,
            Ok(Standing::default())
        );
    }

    #[test]
    fn a_banned_account_is_refused_and_another_gets_its_standing() {
        const NOW: i64 = 1_800_000_000;
        let account = |banned| nw_db::Account {
            id: Uuid::from_u128(1),
            display_name: "Ann".to_owned(),
            created_at_unix: NOW,
            banned,
        };
        assert_eq!(
            standing_of(None, Some(account(true)), Vec::new(), NOW),
            Err(JoinRefusal::Banned)
        );
        assert_eq!(JoinRefusal::Banned.label(), "banned");
        let standing = standing_of(
            Some(AdminRank::GameMaster),
            Some(account(false)),
            Vec::new(),
            NOW,
        )
        .expect("not banned");
        assert!(standing.game_master && !standing.in_probation);
    }

    #[test]
    fn a_young_account_is_in_probation_unless_it_has_an_admin_rank() {
        const NOW: i64 = 1_800_000_000;
        let hours_ago = |hours: i64| Some(NOW - hours * SECONDS_PER_HOUR);
        assert!(in_probation(None, hours_ago(1), NOW));
        assert!(in_probation(
            None,
            hours_ago(ACCOUNT_PROBATION_HOURS - 1),
            NOW
        ));
        assert!(!in_probation(None, hours_ago(ACCOUNT_PROBATION_HOURS), NOW));
        for rank in [AdminRank::Csr, AdminRank::GameMaster, AdminRank::HeadAdmin] {
            assert!(!in_probation(Some(rank), hours_ago(1), NOW), "{rank:?}");
        }
        assert!(!in_probation(None, None, NOW), "age not known");
    }

    #[test]
    fn saved_quests_read_their_run_and_drop_a_bad_one() {
        let run = QuestRun {
            tasks: vec![nw_bus::QuestTask::Done],
            listed: vec![0],
            mission: None,
        };
        let good = player_quests(SavedQuests {
            active: vec![(
                "12_GetOffBeach".to_owned(),
                serde_json::to_value(&run).expect("json"),
            )],
            done: vec!["01_SoleSurvivor2".to_owned()],
        });
        assert_eq!(good.active, [("12_GetOffBeach".to_owned(), run)]);
        assert_eq!(good.done, ["01_SoleSurvivor2"]);
        let bad = player_quests(SavedQuests {
            active: vec![("Q".to_owned(), serde_json::json!({"tasks": "no"}))],
            done: Vec::new(),
        });
        assert!(bad.active.is_empty());
    }

    #[test]
    fn saved_progression_keeps_every_value_and_a_paused_effect_its_time_left() {
        let saved = SavedProgression {
            experience: 1_250,
            coins: 375,
            achievements: vec![14_809, 15_317],
            loot_limits: vec![nw_db::SavedLootLimit {
                limit_id: "2hwarhammer_ftue_corruptedsailer_v2".to_owned(),
                count: 1,
                counted_ns: 1_793_091_600_894_621_000,
                ends_ns: 1_793_091_600_894_621_000,
            }],
            masteries: std::collections::BTreeMap::from([("BowAbilityTable".to_owned(), 30)]),
            attributes: vec![(2, 15)],
            weapon_points: vec![("Ability_Greatsword_Combo".to_owned(), 1)],
            territory_points: vec![(0xd65a_1577, 0x1234_5678, 3)],
            travel: SavedTravel::default(),
            ability_slots: vec![(
                "GreatSwordAbilityTable".to_owned(),
                0,
                "Ability_Greatsword_Combo".to_owned(),
            )],
            meta_counters: std::collections::BTreeMap::from([("pve_totalkills".to_owned(), 4)]),
            meta_completed: vec![("CharacterProgression1".to_owned(), false)],
            title: Some(0x431d_f4ea),
            arena: SavedArena {
                base_entered: 2,
                ranks: vec![3, 3, 3],
                stay: Some(nw_db::SavedStay {
                    instance: Uuid::from_u128(7),
                    game_mode: "DungeonAmrine".to_owned(),
                    phase: u64::MAX,
                    return_level: LEVEL.to_owned(),
                    return_position: [1, 2, 3],
                    activated_ns: 4,
                }),
                ..SavedArena::default()
            },
            mounts: SavedMounts {
                selected: Some(0x0c13_84ae),
                customized: vec![nw_db::SavedMount {
                    mount_id: 0xde1f_a300,
                    dye: [0x19, 0x25, 0x19, 0x25],
                    name: b"Rex".to_vec(),
                }],
                summon_cooldown_end_ns: 1_790_861_002_000_000_000,
            },
            music: nw_db::SavedMusic {
                next_performance_ms: Some(846_118_000_000),
            },
            notifications: nw_db::SavedNotifications {
                shown: vec![(0x1e06_e356, 12)],
            },
            recipe_cooldowns: vec![nw_db::SavedRecipeCooldown {
                recipe_id: "leathert51".to_owned(),
                count: 10,
                ends_ns: 0x18b8_d351_b144_f000,
            }],
            faction: nw_db::SavedFaction {
                faction: 1,
                changes: 1,
                changed_ns: 0x18e1_bb69_e04f_ba64,
            },
            consumable_cooldowns: vec![nw_db::SavedConsumableCooldown {
                group: 0x7a1d_2c34,
                consumable: 0x0b6e_19f2,
                end_us: 846_118_020_000_000,
            }],
            status_effects: vec![
                saved_food(false),
                SavedStatusEffect {
                    repeat: None,
                    ..saved_food(true)
                },
            ],
        };
        assert_eq!(
            player_progression(saved, SAVED_NS + AWAY_NS),
            PlayerProgression {
                experience: 1_250,
                coins: 375,
                achievements: vec![14_809, 15_317],
                loot_limits: vec![LootLimitCount {
                    limit_id: "2hwarhammer_ftue_corruptedsailer_v2".to_owned(),
                    count: 1,
                    counted_ns: 1_793_091_600_894_621_000,
                    ends_ns: 1_793_091_600_894_621_000,
                }],
                masteries: std::collections::BTreeMap::from([("BowAbilityTable".to_owned(), 30)]),
                attributes: vec![(2, 15)],
                weapon_points: vec![("Ability_Greatsword_Combo".to_owned(), 1)],
                territory_points: vec![(0xd65a_1577, 0x1234_5678, 3)],
                travel: PlayerTravel::default(),
                ability_slots: vec![(
                    "GreatSwordAbilityTable".to_owned(),
                    0,
                    "Ability_Greatsword_Combo".to_owned(),
                )],
                meta_counters: std::collections::BTreeMap::from([("pve_totalkills".to_owned(), 4)]),
                meta_completed: vec![("CharacterProgression1".to_owned(), false)],
                title: Some(0x431d_f4ea),
                arena: PlayerArena {
                    base_entered: 2,
                    ranks: vec![3, 3, 3],
                    stay: Some(InstanceStay {
                        instance: Uuid::from_u128(7),
                        game_mode: "DungeonAmrine".to_owned(),
                        phase: u64::MAX,
                        return_level: LEVEL.to_owned(),
                        return_position: StoredPoint::from_bits([1, 2, 3]),
                        activated_ns: 4,
                        mutation_difficulty: None,
                    }),
                    ..PlayerArena::default()
                },
                mounts: PlayerMounts {
                    selected: Some(0x0c13_84ae),
                    customized: vec![CarriedMount {
                        mount_id: 0xde1f_a300,
                        dye: [0x19, 0x25, 0x19, 0x25],
                        name: b"Rex".to_vec(),
                    }],
                    riding: None,
                    summon_cooldown_end_ns: 1_790_861_002_000_000_000,
                },
                music: nw_bus::PlayerMusic {
                    next_performance_ms: Some(846_118_000_000),
                },
                notifications: vec![(0x1e06_e356, 12)],
                recipe_cooldowns: vec![nw_bus::RecipeCooldownCount {
                    recipe_id: "leathert51".to_owned(),
                    count: 10,
                    ends_ns: 0x18b8_d351_b144_f000,
                }],
                faction: nw_bus::PlayerFaction {
                    faction: 1,
                    changes: 1,
                    changed_ns: 0x18e1_bb69_e04f_ba64,
                },
                consumable_cooldowns: vec![nw_bus::PlayerConsumableCooldown {
                    group: 0x7a1d_2c34,
                    consumable: 0x0b6e_19f2,
                    end_us: 846_118_020_000_000,
                }],
                status_effects: vec![
                    nw_bus::PlayerStatusEffect {
                        consumable_id: "FoodRecoveryT2".to_owned(),
                        status_id: "FoodRecoveryT2".to_owned(),
                        ends_ns: SAVED_NS + FOOD_LEFT_NS + AWAY_NS,
                        stacks: 1,
                        runs_offline: false,
                        repeat: Some(nw_bus::EffectRepeat {
                            next_ns: SAVED_NS + TICK_NS + AWAY_NS,
                            ends_ns: SAVED_NS + FOOD_LEFT_NS + AWAY_NS,
                        }),
                    },
                    nw_bus::PlayerStatusEffect {
                        consumable_id: "FoodRecoveryT2".to_owned(),
                        status_id: "FoodRecoveryT2".to_owned(),
                        ends_ns: SAVED_NS + FOOD_LEFT_NS,
                        stacks: 1,
                        runs_offline: true,
                        repeat: None,
                    },
                ],
            }
        );
    }

    /// When the food of [`saved_food`] was saved: unix ns.
    const SAVED_NS: u64 = 1_790_860_000_000_000_000;
    /// The retail `Hivecamp` 2236 rotation (x, y, z, w bits).
    const CAMP_ROTATION: [u32; 4] = [0, 0, 0x3f5f_dd61, 0xbef8_5bf1];
    /// Any blueprint CRC.
    const CAMP_BLUEPRINT: u32 = 0x5e2a_0c11;
    /// A town territory (Edengrove's settlement row).
    const VISITED_TOWN: u16 = 53_010;
    /// What it had left then (retail `target-lock-test`: 303.5 s).
    const FOOD_LEFT_NS: u64 = 303_500_000_000;
    /// When its next heal came.
    const TICK_NS: u64 = 2_000_000_000;
    /// How long the player was away.
    const AWAY_NS: u64 = 3_600_000_000_000;

    /// A saved `FoodRecoveryT2` that pauses unless it `runs_offline`.
    fn saved_food(runs_offline: bool) -> SavedStatusEffect {
        SavedStatusEffect {
            status_id: "FoodRecoveryT2".to_owned(),
            consumable_id: "FoodRecoveryT2".to_owned(),
            ends_ns: SAVED_NS + FOOD_LEFT_NS,
            stacks: 1,
            runs_offline,
            repeat: Some((SAVED_NS + TICK_NS, SAVED_NS + FOOD_LEFT_NS)),
            saved_ns: SAVED_NS,
        }
    }

    #[test]
    fn a_saved_travel_state_keeps_its_homes_furniture_and_bits() {
        let position = [
            6_061.9_f32.to_bits(),
            9_856.0_f32.to_bits(),
            324.9_f32.to_bits(),
        ];
        let rotation = [0, 0, 0.967_457_f32.to_bits(), 0.255_766_f32.to_bits()];
        let plot = [
            0xc7, 0x37, 0x66, 0xcd, 0xa0, 0x43, 0x41, 0x6a, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let saved = SavedTravel {
            waypoint: Some(position),
            inn: Some(17_680_737_194_352_937_251),
            camp: Some(16_695_474_225_659_806_323),
            camp_skin: Some(0x3e89_166b),
            unstuck_ready_ms: 1_790_860_734_747,
            homes: vec![nw_db::SavedHome {
                plot,
                position,
                purchased_ms: 1_782_815_151_259,
                items: vec![nw_db::SavedHousingItem {
                    index: 24,
                    item: 0xb39e,
                    position,
                    rotation,
                }],
                record: nw_db::SavedHomeRecord {
                    house_type: 0xd0e4_0188,
                    territory: 16,
                    taxes_due_ms: 1_783_419_951_259,
                    ..nw_db::SavedHomeRecord::default()
                },
            }],
            played_ms: 448_201,
            inn_recalled_ms: 1_790_860_000_000,
            placed_camp: Some(nw_db::SavedCamp {
                position,
                tier: 2,
                placed_ms: 1_790_861_000_000,
                rotation: CAMP_ROTATION,
                blueprint: CAMP_BLUEPRINT,
            }),
            visited_towns: vec![VISITED_TOWN],
        };
        let travel = player_travel(saved);
        assert_eq!(travel.waypoint.map(StoredPoint::bits), Some(position));
        assert_eq!(
            (
                travel.inn,
                travel.camp,
                travel.camp_skin,
                travel.unstuck_ready_ms
            ),
            (
                Some(17_680_737_194_352_937_251),
                Some(16_695_474_225_659_806_323),
                Some(0x3e89_166b),
                1_790_860_734_747
            )
        );
        assert_eq!(
            travel.homes,
            [OwnedHome {
                plot,
                position: StoredPoint::from_bits(position),
                purchased_ms: 1_782_815_151_259,
                items: vec![HousingItem {
                    index: 24,
                    item: 0xb39e,
                    position: StoredPoint::from_bits(position),
                    rotation,
                }],
                record: HomeRecord {
                    house_type: 0xd0e4_0188,
                    territory: 16,
                    taxes_due_ms: 1_783_419_951_259,
                    ..HomeRecord::default()
                },
            }]
        );
        assert_eq!(
            (travel.played_ms, travel.inn_recalled_ms),
            (448_201, 1_790_860_000_000)
        );
        assert_eq!(
            travel.placed_camp,
            Some(PlacedCamp {
                position: StoredPoint::from_bits(position),
                tier: 2,
                placed_ms: 1_790_861_000_000,
                rotation: CAMP_ROTATION,
                blueprint: CAMP_BLUEPRINT,
            })
        );
        assert_eq!(travel.visited_towns, [VISITED_TOWN]);
    }

    #[test]
    fn saved_items_keep_their_places() {
        const SWORD_INSTANCE: Uuid = Uuid::from_u128(0x83544b8b_eb8d_4150_9668_eb37262c7422);
        const SWORD_GEAR_SCORE: u32 = 590;
        const SWORD_PERK: &str = "PerkID_StatSwap_BaseDamage";
        const SWORD_DYE: [u8; 4] = [4, 0, 184, 0];
        let carried = carried_items(vec![
            SavedItem {
                item_id: "1hSwordT2_FTUE".to_owned(),
                quantity: 1,
                place: SavedPlace::Worn(3),
                instance: Some(SWORD_INSTANCE),
                locked: true,
                rolls: SavedItemRolls {
                    gear_score: Some(SWORD_GEAR_SCORE),
                    perks: Some(vec![SWORD_PERK.to_owned()]),
                    dye: SWORD_DYE,
                    ..SavedItemRolls::default()
                },
            },
            SavedItem {
                item_id: "PotionHealthT1".to_owned(),
                quantity: 5,
                place: SavedPlace::Bag(7),
                instance: None,
                locked: false,
                rolls: SavedItemRolls::default(),
            },
        ]);
        assert_eq!(
            carried
                .iter()
                .map(|item| (item.item_id.as_str(), item.quantity, item.place))
                .collect::<Vec<_>>(),
            [
                ("1hSwordT2_FTUE", 1, ItemPlace::Worn(3)),
                ("PotionHealthT1", 5, ItemPlace::Bag(7)),
            ]
        );
        assert_eq!(
            carried[0].state,
            ItemState {
                instance: Some(SWORD_INSTANCE.into_bytes()),
                locked: true,
                gear_score: Some(SWORD_GEAR_SCORE),
                perks: Some(vec![SWORD_PERK.to_owned()]),
                look: ItemLookState {
                    dye: SWORD_DYE,
                    ..ItemLookState::default()
                },
                ..ItemState::default()
            }
        );
        assert_eq!(carried[1].state, ItemState::default());
    }

    #[test]
    fn saved_storages_and_loadouts_keep_their_ids() {
        const STORAGE: Uuid = Uuid::from_u128(0x6bbf_90ec_53d9_f35f << 64);
        const INSTANCE: Uuid = Uuid::from_u128(0x149c_193f_8983_4221_9275_b2d7_288d_d861);
        let inventory = player_inventory(SavedInventory {
            stored: vec![nw_db::SavedStoredItem {
                storage: STORAGE,
                slot: 1,
                item_id: "FlintT1".to_owned(),
                quantity: 12,
                instance: Some(INSTANCE),
                locked: false,
                rolls: SavedItemRolls {
                    durability: Some(0),
                    ..SavedItemRolls::default()
                },
            }],
            loadouts: vec![nw_db::SavedLoadout {
                index: 5,
                name: "HiveshadowMMO".to_owned(),
                items: vec![(24, INSTANCE)],
                attributes: vec![(0, 150)],
            }],
        });
        assert_eq!(inventory.stored[0].storage, STORAGE.into_bytes());
        assert_eq!(
            inventory.stored[0].state.instance,
            Some(INSTANCE.into_bytes())
        );
        assert_eq!(inventory.stored[0].state.durability, Some(0));
        assert_eq!(inventory.loadouts[0].items, [(24, INSTANCE.into_bytes())]);
        assert_eq!(inventory.loadouts[0].attributes, [(0, 150)]);
    }

    #[test]
    fn a_number_in_the_mob_range_is_refused() {
        assert_eq!(player_entity(MOB_ENTITY_BASE - 1), Ok(MOB_ENTITY_BASE - 1));
        assert_eq!(
            player_entity(MOB_ENTITY_BASE),
            Err(JoinRefusal::EntityRange(MOB_ENTITY_BASE))
        );
    }

    const STARTS: [Position; 3] = [
        Position {
            x: 1.0,
            y: 0.0,
            z: 0.0,
        },
        Position {
            x: 2.0,
            y: 0.0,
            z: 0.0,
        },
        Position {
            x: 3.0,
            y: 0.0,
            z: 0.0,
        },
    ];

    #[test]
    fn new_characters_and_tutorial_leavers_start_in_the_tutorial() {
        assert!(starts_in_tutorial(None));
        assert!(starts_in_tutorial(Some(&saved_at(TUTORIAL_FOLDER, 1.0))));
        assert!(!starts_in_tutorial(Some(&saved_at(LEVEL, 1.0))));
    }

    #[test]
    fn new_characters_take_the_start_points_in_turn() {
        let xs: Vec<f32> = (1..=6)
            .map(|entity| start_for(&STARTS, entity).map(|start| start.x))
            .collect::<Option<_>>()
            .expect("starts");
        assert_eq!(xs, vec![2.0, 3.0, 1.0, 2.0, 3.0, 1.0]);
        assert_eq!(start_for(&[], 1), None);
    }

    const LEVEL: &str = "newworld_vitaeeterna";
    const MAP_EDGE_M: f32 = 100.0;

    fn saved_at(level: &str, x: f64) -> SavedPosition {
        SavedPosition {
            level: level.to_owned(),
            x,
            y: 10.0,
            z: 50.0,
            yaw: 0.0,
            saved_at_ms: 0,
        }
    }

    fn on_map(position: Position) -> bool {
        (0.0..=MAP_EDGE_M).contains(&position.x)
    }

    #[test]
    fn a_saved_spot_on_the_map_is_kept_and_one_off_it_gets_a_start() {
        let entity = 1;
        let saved = saved_at(LEVEL, 42.0);
        let kept = join_position(Some(&saved), LEVEL, on_map, &STARTS, entity);
        assert_eq!(
            kept,
            Some(Position {
                x: 42.0,
                y: 10.0,
                z: 50.0
            })
        );
        let start = start_for(&STARTS, entity);
        let off_map = saved_at(LEVEL, f64::from(MAP_EDGE_M) + 1.0);
        assert_eq!(
            join_position(Some(&off_map), LEVEL, on_map, &STARTS, entity),
            start
        );
        let other_level = saved_at("nw_dungeon_edengrove_00", 42.0);
        assert_eq!(
            join_position(Some(&other_level), LEVEL, on_map, &STARTS, entity),
            start
        );
        assert_eq!(join_position(None, LEVEL, on_map, &STARTS, entity), start);
        assert_eq!(
            join_position(Some(&off_map), LEVEL, on_map, &[], entity),
            None
        );
    }

    /// The Amrine expedition level.
    const DUNGEON: &str = "nw_dungeon_windsward_00";

    #[test]
    fn a_stay_goes_back_into_its_open_instance_or_to_its_return_spot() {
        use crate::instances::{Decision, Instances};
        let member = Uuid::from_u128(1);
        let stay = InstanceStay {
            instance: Uuid::from_u128(7),
            game_mode: "DungeonAmrine".to_owned(),
            phase: 1,
            return_level: LEVEL.to_owned(),
            return_position: StoredPoint::of(Point {
                x: 42.0,
                y: 10.0,
                z: 50.0,
            }),
            activated_ns: 0,
            mutation_difficulty: None,
        };
        let back = Some(StayRoute::Return(Position {
            x: 42.0,
            y: 10.0,
            z: 50.0,
        }));
        let empty = InstanceTable::default();
        let inside = saved_at(DUNGEON, 600.0);
        assert_eq!(
            stay_route(&stay, Some(&inside), &empty, member, LEVEL),
            back
        );
        let mut instances = Instances::default();
        let sim = SimSlot {
            id: nw_bus::BusId::try_from("inst-1".to_owned()).expect("id"),
            generation: 1,
        };
        let request = nw_bus::InstanceRequest {
            game_mode: "DungeonAmrine".to_owned(),
            level: DUNGEON.to_owned(),
            requester: member,
            capacity: 5,
            min_group: 0,
            timeout_seconds: None,
        };
        let Decision::Open(open) = instances.decide(
            &request,
            &nw_bus::GroupAnswer::Alone,
            std::slice::from_ref(&sim),
            (stay.instance, 1),
            0,
        ) else {
            panic!("opens");
        };
        instances.opened(open.id, true);
        let table = instances.table();
        assert_eq!(
            stay_route(&stay, Some(&inside), &table, member, LEVEL),
            Some(StayRoute::Instance(
                sim,
                Position {
                    x: 600.0,
                    y: 10.0,
                    z: 50.0
                }
            ))
        );
        // Saved on the open map (it left before the save), or not a member: the return spot.
        let outside = saved_at(LEVEL, 7.0);
        assert_eq!(
            stay_route(&stay, Some(&outside), &table, member, LEVEL),
            back
        );
        assert_eq!(
            stay_route(&stay, Some(&inside), &table, Uuid::from_u128(2), LEVEL),
            back
        );
        assert_eq!(stay_route(&stay, None, &empty, member, "other_level"), None);
    }
}
