// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crafting at a station (beach-quests note, section 4). The player opens a station (interaction
//! kind 20; the server answers with the station's name and the player's interactor target), sends
//! the recipe, how many crafts, the fee its client shows and the ingredients it picked (2997),
//! and 1,057 ms later the server answers with the recipe and the crafts made and an idle update
//! (2993 twice; retail: 1,022 to 1,110 ms for 1 to 85 crafts alike, 34 crafts of four crafting
//! ledgers). The close of the window (2999) gets one more idle update.
//!
//! A craft needs a station of the recipe's name and tier ([`Recipe::made_at`]), its tradeskill's
//! rank at the recipe's `RecipeLevel`, the ingredients in the bag and the fee in the wallet: the
//! fee the client sent, at most the recipes' `CraftingFee` (retail took exactly the client's fee
//! at 34 of 34 crafts; the tax that lowers it is Unknown here). Each craft fires the recipe's
//! game event: its tradeskill experience ([`SimWorld::grant_event`]).
//!
//! The stations are the level's ([`nw_world::stations`]): a client shows a station's interaction
//! only once it has the station's spawn group (campfire note: retail sends it when the station
//! comes into view; run 12 sent none and the beach campfire had no prompt). The group holds the
//! card and a state per replicated component of the station's slice, at the slice's component
//! numbers ([`station_members`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use bevy_ecs::entity::Entity;
use nw_content::crafting::{
    IngredientGearScore, IngredientKind, Recipe, RecipeDetails, StructureCosts,
};
use nw_content::perks::PERK_SLOTS;
use nw_content::recipe_cooldowns::RecipeCooldown;
use nw_content::tradeskill_xp::bonus_crafts;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::crafting_state::{CraftingCooldown, CraftingState};
use nw_messages::facet::component;
use nw_messages::items::{FIRST_VERSION, SYNC_CRAFT_USED, SYNC_CRAFTED};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{
    BuildableControllerState, BuildableGridState, BundleGroup, BundleMember, CommittedResource,
    CraftIngredient, CraftProgressMsg, DelayedEventState, FacetHeader, FullList,
    GdeMetadataReplicatedState, GridSide, GuildCrest, InteractionRequest, MemberState,
    OwnerIdentification, OwnershipState, PlacementObstructionState, ServerMessage,
    TerritoryInterfaceState,
};
use nw_sim_core::Position;
use nw_world::stations::CraftingStation;

use super::gear::unix_ns;
use super::loot_bag::LimitsSent;
use super::slots::Slot;
use super::statics::interactor_target;
use super::tradeskill::EventScale;
use super::{Player, SimWorld, gather_answer, send_now};
use crate::objectives::Event;

/// First slot key of a crafting station: its index is added. Keys are never sent; they stay
/// above every player and mob number and below the loot bags.
pub(super) const STATION_ENTITY_BASE: u64 = 1 << 37;
/// First slot key of a player's camp: the count of camps placed in this sim is added. The camps
/// take the upper half of the station range (the level has far fewer than `1 << 36` stations),
/// below the loot bags (`1 << 38`).
const CAMP_ENTITY_BASE: u64 = STATION_ENTITY_BASE | (1 << 36);
/// The component of a spawn card.
pub(super) const CARD_COMPONENT: u64 = 0;
/// The status of an accepted and of a refused station interaction.
const STATION_ACCEPTED: u8 = 1;
const STATION_REFUSED: u8 = 2;
/// How long a craft takes (retail: 2993 1,057 ms after the 2997, knife and ration alike).
const CRAFT_MS: u64 = 1_057;
/// The crafts one request makes when it asks for none.
const ONE_CRAFT: u32 = 1;
/// How far above the range this server computes a craft's gear score may be: the crafting
/// status effects the client counts and this server does not hold (best guess, the largest
/// bonuses of `StatusEffectData` from sources this server does not run: three
/// `HousingMinGearBonus_AllCraftingT3` trophies 3 x 5, an earring perk 5 and a tradeskill
/// mastery 5). The held effects, tradeskill food among them, add their own `MGS_*` and
/// `MaxGS_*` ([`super::status_effects::StatusEffects::craft_gear_score`]).
const CRAFT_STATUS_GS_ALLOWANCE: u32 = 25;
/// The values of a built station that no data sets, as the retail campfire and town station
/// groups have them: no foundation events, a territory whose references are set, a ruin timer
/// that is not armed, and no guild, owner, raid or faction.
pub(super) const CHECK_FOUNDATION_EVENTS: bool = false;
pub(super) const TERRITORY_REFS_SET: bool = true;
pub(super) const RUIN_DELAY: u16 = 0;
pub(super) const RUIN_COMPLETED: bool = false;
pub(super) const NO_GUILD_ID: [u8; nw_messages::buildables::GUILD_ID_LEN] =
    [0; nw_messages::buildables::GUILD_ID_LEN];
pub(super) const NO_RAID: u64 = 0;
pub(super) const NO_FACTION: u8 = 0;
const NO_PERMISSION_OPTIONS: u8 = 0;

/// A crafting station: one of the level's, or a player's camp.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Station {
    /// The `GdeRef` its card and the client's interaction name.
    pub(super) gde_ref: [u8; RAW_ID_LEN],
    /// Where it stands.
    pub(super) position: Position,
    /// Its station type, as recipes list them (`StationType1` to `StationType4`).
    pub(super) station_type: String,
    /// The name its interaction's answer carries.
    pub(super) name: Vec<u8>,
    /// Its spawn group's members.
    pub(super) members: Vec<BundleMember>,
}

/// The crafting stations of one sim: the level's, and the players' camps by slot key.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Stations {
    stations: Vec<Station>,
    by_ref: HashMap<[u8; RAW_ID_LEN], usize>,
    camps: BTreeMap<u64, Station>,
    /// The camps placed in this sim so far.
    camps_placed: u64,
}

/// The `GdeRef` of a level entity with GDE id `id`: the id's little-endian bytes, then the
/// open world's zero phase.
pub(super) fn gde_ref_of(id: u64) -> [u8; RAW_ID_LEN] {
    let mut gde_ref = [0; RAW_ID_LEN];
    gde_ref[..size_of::<u64>()].copy_from_slice(&id.to_le_bytes());
    gde_ref
}

/// The spawn group members of `station`, whose blueprint costs `costs` lists, with replicated
/// lists at `version`: the card, then a state per replicated component the slice has, by
/// component number (retail orders them differently in each group).
fn station_members(
    station: &CraftingStation,
    costs: &StructureCosts,
    version: u64,
) -> Vec<BundleMember> {
    let slice = &station.components;
    let member = |component: u8, state| BundleMember {
        component: u64::from(component),
        state,
    };
    let mut members = vec![BundleMember {
        component: CARD_COMPONENT,
        state: MemberState::GdeMetadata(GdeMetadataReplicatedState {
            asset: None,
            gde_ref: Some(gde_ref_of(station.id)),
        }),
    }];
    if let Some((component, state)) = slice.buildable {
        let resources = committed(costs.of(&slice.structure));
        members.push(member(
            component,
            MemberState::BuildableController(BuildableControllerState {
                current_state: Some(state),
                check_foundation_events: Some(CHECK_FOUNDATION_EVENTS),
                committed_resources: Some(list_at(version, resources)),
                ..BuildableControllerState::default()
            }),
        ));
    }
    if let Some((component, completion)) = slice.obstruction {
        members.push(member(
            component,
            MemberState::PlacementObstruction(PlacementObstructionState {
                has_completion_obstruction: Some(u8::from(completion)),
            }),
        ));
    }
    if let Some(component) = slice.territory {
        members.push(member(
            component,
            MemberState::TerritoryInterface(TerritoryInterfaceState {
                functionality_down: None,
                refs_set: Some(TERRITORY_REFS_SET),
            }),
        ));
    }
    if let Some((component, sides)) = &slice.grid {
        let sides = sides
            .iter()
            .map(|side| GridSide {
                side: side.side,
                points: side.points,
                active: side.active,
            })
            .collect();
        members.push(member(
            *component,
            MemberState::BuildableGrid(BuildableGridState {
                grid_sides_active: Some(list_at(version, sides)),
            }),
        ));
    }
    if let Some(component) = slice.delayed_event {
        members.push(member(
            component,
            MemberState::DelayedEvent(DelayedEventState {
                total_delay: Some(RUIN_DELAY),
                completed: Some(RUIN_COMPLETED),
            }),
        ));
    }
    if let Some((component, rule)) = slice.ownership {
        members.push(member(
            component,
            MemberState::Ownership(OwnershipState {
                guild_id: Some(NO_GUILD_ID),
                guild_owner: Some(Vec::new()),
                guild_crest: Some(GuildCrest::NONE),
                owner: Some(OwnerIdentification::none()),
                permission_rule: Some(rule),
                structure_name: Some(slice.structure_name.as_bytes().to_vec()),
                raid_id: Some(NO_RAID),
                faction_type: Some(NO_FACTION),
                owner_pvp: Some(false),
                owner_ffa: Some(false),
                permission_options: Some(NO_PERMISSION_OPTIONS),
            }),
        ));
    }
    members.sort_by_key(|member| member.component);
    members
}

/// The header crafting answers go to: the request's, else player `number`'s entity plus
/// [`component::CRAFTING`].
fn answer_header(request: Option<FacetHeader>, number: u64) -> FacetHeader {
    request.unwrap_or_else(|| FacetHeader::of(component::CRAFTING, number))
}

/// The committed resources of a structure that costs `costs` (item id, quantity).
pub(super) fn committed(costs: &[(String, u32)]) -> Vec<CommittedResource> {
    costs
        .iter()
        .map(|(item, quantity)| CommittedResource::of(item, lowercase_crc32(item), *quantity))
        .collect()
}

/// A replicated list of `elements` at `version`.
pub(super) const fn list_at<T>(version: u64, elements: Vec<T>) -> FullList<T> {
    FullList {
        version: Some(version),
        elements,
    }
}

impl Stations {
    /// The level's `stations`, whose blueprints cost `costs`.
    pub(super) fn build(stations: &[CraftingStation], costs: &StructureCosts) -> Self {
        let stations: Vec<Station> = stations
            .iter()
            .map(|station| {
                let [x, y, z] = station.position;
                Station {
                    gde_ref: gde_ref_of(station.id),
                    position: Position { x, y, z },
                    station_type: station.components.station_type.clone(),
                    name: station.components.name.as_bytes().to_vec(),
                    members: station_members(station, costs, FIRST_VERSION),
                }
            })
            .collect();
        let by_ref = stations
            .iter()
            .enumerate()
            .map(|(index, station)| (station.gde_ref, index))
            .collect();
        Self {
            stations,
            by_ref,
            camps: BTreeMap::new(),
            camps_placed: 0,
        }
    }

    /// Puts the camp that `station` makes for its new slot key in; returns the key. Each camp
    /// gets a key of its own, so a viewer's client gets a new slot and card for it.
    pub(super) fn place_camp(&mut self, station: impl FnOnce(u64) -> Station) -> u64 {
        let number = CAMP_ENTITY_BASE + self.camps_placed;
        self.camps_placed += 1;
        self.camps.insert(number, station(number));
        number
    }

    /// Takes the camp of slot key `number` away: it leaves every view.
    pub(super) fn remove_camp(&mut self, number: u64) {
        self.camps.remove(&number);
    }

    /// The slot keys of the stations within `radius_m` of `position`.
    pub(super) fn numbers_near(
        &self,
        position: Position,
        radius_m: f32,
    ) -> impl Iterator<Item = u64> + '_ {
        let near =
            move |station: &Station| station.position.horizontal_distance(position) <= radius_m;
        (0_u64..)
            .zip(&self.stations)
            .filter(move |(_, station)| near(station))
            .map(|(index, _)| STATION_ENTITY_BASE + index)
            .chain(
                self.camps
                    .iter()
                    .filter(move |(_, camp)| near(camp))
                    .map(|(number, _)| *number),
            )
    }

    /// The group of the station of slot key `number` in `slot`: its spawn members when the slot
    /// is new (the station came into view), else none.
    pub(super) fn group(&self, number: u64, slot: Slot) -> Option<BundleGroup> {
        let station = match self.camps.get(&number) {
            Some(camp) => camp,
            None => {
                let index = usize::try_from(number.checked_sub(STATION_ENTITY_BASE)?).ok()?;
                self.stations.get(index)?
            }
        };
        slot.new.then(|| BundleGroup {
            interest: u64::from(slot.interest),
            members: station.members.clone(),
        })
    }

    /// The station whose `GdeRef` is `gde_ref`.
    fn named(&self, gde_ref: &[u8; RAW_ID_LEN]) -> Option<&Station> {
        self.by_ref
            .get(gde_ref)
            .map(|index| &self.stations[*index])
            .or_else(|| self.camps.values().find(|camp| &camp.gde_ref == gde_ref))
    }
}

/// A craft that runs: at `due_ms` its items and fee are taken and its output comes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PendingCraft {
    number: u64,
    /// The stacks it takes at its end.
    taken: Vec<Taken>,
    /// The coins it takes at its end.
    fee: u32,
    recipe: String,
    item: String,
    /// The items it makes: the recipe's output times the crafts.
    quantity: u32,
    /// How many crafts the request asked for.
    crafts: u32,
    /// The sum of the recipe's ingredient quantities: each craft's game event counts them
    /// ([`EventScale::craft`]).
    ingredients: u32,
    /// The recipe's game event, fired once per craft.
    event: Option<String>,
    due_ms: u64,
    /// What each item it makes rolls with; `None`: an item without rolls of its own.
    roll: Option<CraftRoll>,
    /// The achievements it sets at its end (`UnlockedAchievementID`, `FirstCraftAchievementId`).
    achievements: Vec<String>,
    /// The recipe's cooldown, which its crafts count on at its end.
    cooldown: Option<RecipeCooldown>,
    /// The header the answers go to.
    header: FacetHeader,
}

/// What the player picked for a craft besides the recipe and ingredients (2997).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct CraftChoice {
    /// The gear score it asked for (0: none).
    pub(super) gear_score: u32,
    /// The CRC-32 of each perk item's lowercase item id.
    pub(super) perk_items: Vec<u32>,
    /// The header of the request: the answers go to it ([`CraftProgressMsg::crafted`]); `None`
    /// answers the player's entity plus [`component::CRAFTING`].
    pub(super) header: Option<FacetHeader>,
}

/// The rolls of a crafted gear item: the gear score and the perks its perk items force.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CraftRoll {
    gear_score: u32,
    forced: Vec<String>,
}

/// One stack a craft takes: its item, quantity, and whether it fills a `Category_Only`
/// ingredient.
type Taken = (String, u32, bool);

/// Why a craft request is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CraftRefusal {
    /// The player uses no station of the recipe's name and tier.
    Station,
    /// Its tradeskill's rank is below the recipe's `RecipeLevel`.
    RecipeLevel,
    /// The wallet holds less than the fee.
    Fee,
    /// The bag lacks an ingredient.
    Ingredient,
    /// The player lacks the recipe's required achievement, has its hiding one, or has the one
    /// it unlocks and that blocks a recraft.
    Locked,
    /// The crafts would pass the recipe's `CooldownQuantity` before its cooldown resets.
    Cooldown,
}

impl SimWorld {
    /// Player `number` (`entity`) uses the crafting station of `request`: a known station is
    /// accepted and becomes the player's interactor target; another is refused.
    pub(super) fn open_station(
        &mut self,
        entity: Entity,
        number: u64,
        request: InteractionRequest,
    ) {
        let station = self.stations.named(&request.target);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(station) = station else {
            tracing::debug!(
                player = number,
                ?request,
                "use of an unknown crafting station"
            );
            let refusal = gather_answer(number, request, STATION_REFUSED, &[]);
            send_now(&mut self.combat_messages, &player.connection, &[refusal]);
            return;
        };
        tracing::debug!(
            player = number,
            station = %station.station_type,
            "crafting station opened"
        );
        let answer = gather_answer(number, request, STATION_ACCEPTED, &station.name);
        send_now(&mut self.combat_messages, &player.connection, &[answer]);
        player
            .extra_members
            .push(interactor_target(request.target_entity));
        player.station = Some(station.station_type.clone());
        player.station_interaction = Some(request.interaction);
    }

    /// Player `number` (`entity`) crafts `crafts` of the recipe whose lowercase id has the CRC-32
    /// `recipe` at `now_ms`, from the `picked` ingredients, for the fee `fee` its client shows:
    /// at a station of the recipe's name and tier, at the recipe's level, with every ingredient
    /// in the bag and the fee in the wallet. The ingredients and the fee are taken with the
    /// output, [`CRAFT_MS`] later ([`SimWorld::crafts_due`]): retail newchar 2026-09-18 changed
    /// the bag (1755) only after the result (2993) at 535.01 s, the request at 533.94 s took
    /// nothing; taking them at the request made the live client say the craft attempt failed
    /// (login-world-entry-01). A refused craft gets an idle update. A gear item is made at the gear score of `choice` held to the craft's range,
    /// with the perks its perk items force ([`SimWorld::craft_roll`]).
    #[allow(clippy::too_many_arguments, reason = "the fields of one craft request")]
    pub(super) fn craft(
        &mut self,
        entity: Entity,
        number: u64,
        recipe_crc: u32,
        crafts: u32,
        fee: u32,
        picked: &[CraftIngredient],
        choice: &CraftChoice,
        now_ms: u64,
    ) {
        let crafts = crafts.max(ONE_CRAFT);
        let header = answer_header(choice.header, number);
        let Some(recipe) = self.content.recipes.get(recipe_crc).cloned() else {
            tracing::debug!(
                player = number,
                recipe = recipe_crc,
                "craft of an unknown recipe"
            );
            self.send_idle(entity, header);
            return;
        };
        let details = self.content.recipes.details(recipe_crc);
        let level_reached = details.tradeskill.as_deref().is_none_or(|tradeskill| {
            self.tradeskill_perks(entity, tradeskill).1.recipe_level >= details.recipe_level
        });
        let open = self.world.get::<Player>(entity).is_some_and(|player| {
            details.open_to(|id| player.flow.has_achievement(&self.achievement_rules, id))
        });
        let now_ns = unix_ns(self.server_ms);
        let cooled = details.cooldown.is_none_or(|cooldown| {
            self.world.get::<Player>(entity).is_some_and(|player| {
                player
                    .recipe_cooldowns
                    .allows(&recipe.id, cooldown, crafts, now_ns)
            })
        });
        let charged = fee.min(details.fee.saturating_mul(crafts));
        let perk_items = self.perk_items_of(entity, &recipe.item, choice, details.max_perk_items);
        let taken = if !open {
            Err(CraftRefusal::Locked)
        } else if !cooled {
            Err(CraftRefusal::Cooldown)
        } else if level_reached {
            self.ingredients_of(entity, &recipe, crafts, picked, &perk_items, charged)
        } else {
            Err(CraftRefusal::RecipeLevel)
        };
        let taken = match taken {
            Ok(taken) => taken,
            Err(refusal) => {
                tracing::debug!(player = number, recipe = %recipe.id, ?refusal, "craft refused");
                self.send_idle(entity, header);
                return;
            }
        };
        let roll = self.craft_roll(entity, &recipe, &details, &taken, &perk_items, choice);
        let bonus = self.bonus_crafts(entity, recipe_crc, details.tradeskill.as_deref(), crafts);
        tracing::debug!(player = number, recipe = %recipe.id, crafts, bonus, charged, "craft");
        let achievements = details.craft_achievements().map(str::to_owned).collect();
        let ingredients = recipe.ingredients.iter().fold(0_u32, |sum, ingredient| {
            sum.saturating_add(ingredient.quantity)
        });
        self.crafts.push(PendingCraft {
            header,
            number,
            taken,
            fee: charged,
            recipe: recipe.id.clone(),
            item: recipe.item,
            quantity: recipe.quantity.saturating_mul(crafts.saturating_add(bonus)),
            crafts,
            ingredients,
            event: details.game_event,
            due_ms: now_ms.saturating_add(CRAFT_MS),
            roll,
            achievements,
            cooldown: details.cooldown,
        });
    }

    /// Counts `crafts` crafts of `recipe` with `cooldown` for player `entity` at the server's
    /// time, on the reset clock of the loot limits (`nw_content::recipe_cooldowns`), and its
    /// client gets the changed counts.
    fn count_recipe_cooldown(
        &mut self,
        entity: Entity,
        recipe: &str,
        cooldown: RecipeCooldown,
        crafts: u32,
    ) {
        let Some(clock) = self.content.loot_limits.as_ref().map(|rules| rules.clock) else {
            return;
        };
        let now_ns = unix_ns(self.server_ms);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player
            .recipe_cooldowns
            .add(clock, recipe, cooldown, crafts, now_ns);
        send_recipe_cooldowns(&mut player, LimitsSent::Changed);
    }

    /// Player `number`'s client gets its recipe cooldown counts as `sent` calls for (see
    /// [`LimitsSent`]): the crafting state goes with its next bundle.
    pub(super) fn send_recipe_cooldowns(&mut self, number: u64, sent: LimitsSent) {
        if let Some(mut player) = self
            .player_by_number(number)
            .and_then(|entity| self.world.get_mut::<Player>(entity))
        {
            send_recipe_cooldowns(&mut player, sent);
        }
    }

    /// The perk items of `choice` that player `entity` carries and that may go on `item`: the
    /// item ids of those whose CRC-32 the choice names and that have a perk bucket, at most
    /// `allowed` (the recipe's `MaxPerkItemsAllowed`, else the item's perk slots).
    fn perk_items_of(
        &self,
        entity: Entity,
        item: &str,
        choice: &CraftChoice,
        allowed: Option<u32>,
    ) -> Vec<String> {
        let rolls = &self.content.item_rolls;
        if choice.perk_items.is_empty() || !rolls.item(item).is_some_and(|row| row.rolls()) {
            return Vec::new();
        }
        let Some(gear) = self
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
        else {
            return Vec::new();
        };
        let limit = allowed.map_or(PERK_SLOTS, |allowed| {
            usize::try_from(allowed).unwrap_or(PERK_SLOTS)
        });
        let carried: Vec<String> = gear
            .carried()
            .into_iter()
            .map(|carried| carried.item_id)
            .collect();
        choice
            .perk_items
            .iter()
            .filter_map(|crc| {
                carried
                    .iter()
                    .find(|item_id| lowercase_crc32(item_id) == *crc)
                    .filter(|item_id| {
                        rolls
                            .item(item_id)
                            .is_some_and(|row| row.resource_bucket.is_some())
                    })
                    .cloned()
            })
            .take(limit)
            .collect()
    }

    /// The rolls of the gear item `recipe` makes for player `entity` from the `taken` stacks
    /// and `perk_items`: the gear score `choice` asks for (its range's lowest for none), held
    /// between the lowest of the craft's range and its highest (both raised by the player's
    /// held crafting effects) plus
    /// [`CRAFT_STATUS_GS_ALLOWANCE`] and under the item's `MaxCraftGS`, and the perk each perk
    /// item's bucket gives the item. `None` for an item without rolls of its own.
    fn craft_roll(
        &mut self,
        entity: Entity,
        recipe: &Recipe,
        details: &RecipeDetails,
        taken: &[Taken],
        perk_items: &[String],
        choice: &CraftChoice,
    ) -> Option<CraftRoll> {
        let content = Arc::clone(&self.content);
        let rolls = &content.item_rolls;
        let row = rolls.item(&recipe.item).filter(|row| row.rolls())?;
        let base = row.gear_score_override.unwrap_or(row.min_gear_score);
        let rank_bonus = details.tradeskill.as_deref().map_or((0, 0), |tradeskill| {
            self.tradeskill_perks(entity, tradeskill)
                .1
                .gear_score_bonus_of(details.base_tier)
        });
        let ingredients: Vec<IngredientGearScore> = taken
            .iter()
            .filter_map(|(item_id, _, category)| {
                let ingredient = rolls.item(item_id)?;
                Some(IngredientGearScore {
                    base_bonus: ingredient.ingredient_base_bonus,
                    max_bonus: ingredient.ingredient_max_bonus,
                    category_tier: category.then_some(ingredient.tier),
                })
            })
            .collect();
        let mut range = details.gear_score_range(base, rank_bonus, &ingredients);
        if let Some(tradeskill) = details.tradeskill.as_deref()
            && let Some(player) = self.world.get::<Player>(entity)
        {
            let (least, most) = player
                .status_effects
                .craft_gear_score(&content.status_effects, tradeskill);
            // Gear score bonuses of a few tens.
            range.min = range.min.saturating_add(least.max(0.0) as u32);
            range.max = range
                .max
                .saturating_add(most.max(0.0) as u32)
                .max(range.min);
        }
        let wanted = if choice.gear_score == 0 {
            range.min
        } else {
            choice.gear_score.clamp(
                range.min,
                range.max.saturating_add(CRAFT_STATUS_GS_ALLOWANCE),
            )
        };
        let gear_score = row
            .max_craft_gear_score
            .map_or(wanted, |cap| wanted.min(cap));
        let dice = &mut self.dice;
        let forced = perk_items
            .iter()
            .filter_map(|perk_item| {
                let bucket = rolls.item(perk_item)?.resource_bucket.as_deref()?;
                rolls
                    .perks
                    .pick(bucket, &row.item_id, &row.classes, &[], &mut |below| {
                        dice.below(below)
                    })
            })
            .collect();
        Some(CraftRoll { gear_score, forced })
    }

    /// The stacks `crafts` of `recipe` take from the bag of player `entity`, with one of each
    /// of `perk_items` per craft, and `fee` coins from its wallet: for each ingredient, the item
    /// the player picked that fits it, else the first carried one that fits. Refused away from
    /// a station of the recipe, without the coins, or without an ingredient or perk item.
    /// Nothing is taken here ([`SimWorld::take_stacks`]).
    fn ingredients_of(
        &self,
        entity: Entity,
        recipe: &Recipe,
        crafts: u32,
        picked: &[CraftIngredient],
        perk_items: &[String],
        fee: u32,
    ) -> Result<Vec<Taken>, CraftRefusal> {
        let recipes = &self.content.recipes;
        let Some(player) = self.world.get::<Player>(entity) else {
            return Err(CraftRefusal::Station);
        };
        if !player
            .station
            .as_deref()
            .is_some_and(|station| recipe.made_at(station))
        {
            return Err(CraftRefusal::Station);
        }
        if player.coins < u64::from(fee) {
            return Err(CraftRefusal::Fee);
        }
        let Some(gear) = player.gear.as_ref() else {
            return Err(CraftRefusal::Ingredient);
        };
        let carried: Vec<String> = gear
            .carried()
            .into_iter()
            .map(|item| item.item_id)
            .collect();
        let mut takes = Vec::new();
        for ingredient in &recipe.ingredients {
            let needed = ingredient.quantity.saturating_mul(crafts);
            let fits = |item: &&String| recipes.fits(ingredient, item);
            let chosen = carried
                .iter()
                .filter(fits)
                .find(|item| picked.iter().any(|pick| pick.item == lowercase_crc32(item)))
                .or_else(|| carried.iter().find(fits));
            let category = matches!(ingredient.kind, IngredientKind::Category(_));
            match chosen {
                Some(item) if gear.bag_count(item) >= needed => {
                    takes.push((item.clone(), needed, category));
                }
                _ => return Err(CraftRefusal::Ingredient),
            }
        }
        for perk_item in perk_items {
            let needed = takes
                .iter()
                .filter(|(item, _, _)| item == perk_item)
                .map(|(_, quantity, _)| quantity)
                .sum::<u32>()
                .saturating_add(crafts);
            if gear.bag_count(perk_item) < needed {
                return Err(CraftRefusal::Ingredient);
            }
            takes.push((perk_item.clone(), crafts, false));
        }
        Ok(takes)
    }

    /// Takes the stacks `taken` out of the bag of player `entity` and `fee` coins out of its
    /// wallet; refused (nothing taken) when the bag or the wallet no longer holds them. The
    /// stacks taken count for no item change: retail sends one 7048 per item a craft makes
    /// ([`super::storage_search`]).
    fn take_stacks(
        &mut self,
        entity: Entity,
        taken: &[Taken],
        fee: u32,
    ) -> Result<(), CraftRefusal> {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return Err(CraftRefusal::Ingredient);
        };
        if player.coins < u64::from(fee) {
            return Err(CraftRefusal::Fee);
        }
        let Some(gear) = player.gear.as_mut() else {
            return Err(CraftRefusal::Ingredient);
        };
        let mut needed: Vec<(&str, u32)> = Vec::new();
        for (item, quantity, _) in taken {
            match needed.iter_mut().find(|(known, _)| *known == item.as_str()) {
                Some((_, sum)) => *sum = sum.saturating_add(*quantity),
                None => needed.push((item.as_str(), *quantity)),
            }
        }
        if needed
            .iter()
            .any(|(item, quantity)| gear.bag_count(item) < *quantity)
        {
            return Err(CraftRefusal::Ingredient);
        }
        for (item, quantity, _) in taken {
            gear.take_as(item, *quantity, SYNC_CRAFT_USED);
        }
        let (carried, stored) = (gear.carried(), gear.stored_stacks());
        let used: Vec<&str> = needed.iter().map(|(item, _)| *item).collect();
        player.item_changes.absorb(&used, &carried, &stored);
        if fee > 0 {
            player.coins -= u64::from(fee);
            player.coins_changed = true;
        }
        Ok(())
    }

    /// The crafts whose time came at `now_ms` end: their stacks and fee are taken, the output
    /// goes into the bag, the client gets the crafts made and an idle update, each craft fires
    /// the recipe's game event, and the player's quests take the craft. A craft whose stacks or
    /// coins are gone by then makes nothing and gets an idle update.
    pub(super) fn crafts_due(&mut self, now_ms: u64) {
        let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut self.crafts)
            .into_iter()
            .partition(|craft| now_ms >= craft.due_ms);
        self.crafts = later;
        for craft in due {
            let Some(entity) = self.player_by_number(craft.number) else {
                continue;
            };
            if let Err(refusal) = self.take_stacks(entity, &craft.taken, craft.fee) {
                tracing::debug!(player = craft.number, recipe = %craft.recipe, ?refusal, "craft failed at its end");
                self.send_idle(entity, craft.header);
                continue;
            }
            match &craft.roll {
                Some(roll) => {
                    let content = Arc::clone(&self.content);
                    let dice = &mut self.dice;
                    let rolled = (0..craft.quantity)
                        .map(|_| {
                            content.item_rolls.roll(
                                &craft.item,
                                roll.gear_score,
                                &roll.forced,
                                &mut |below| dice.below(below),
                            )
                        })
                        .collect();
                    self.give_rolled(craft.number, &craft.item, rolled, SYNC_CRAFTED, now_ms);
                }
                None => {
                    let made = vec![(craft.item.clone(), craft.quantity)];
                    self.give_loot_as(craft.number, made, SYNC_CRAFTED, now_ms);
                }
            }
            // Retail sends the item change (7048) and the bundle of the made items before these
            // (Basic Needs and Campfire Feast, 2026-09-27: 7048, then both 2993 in one tick);
            // sent before the bundle, the live client said "Crafting attempt failed" on every
            // good craft (2026-10-01).
            if let Some(player) = self.world.get::<Player>(entity) {
                let messages = [
                    ServerMessage::CraftProgress(CraftProgressMsg::crafted(
                        craft.header,
                        craft.recipe.as_bytes().to_vec(),
                        craft.crafts,
                    )),
                    ServerMessage::CraftProgress(CraftProgressMsg::idle(craft.header)),
                ];
                send_now(&mut self.after_bundles, &player.connection, &messages);
            }
            if let Some(event) = &craft.event {
                let scale = EventScale::craft(craft.ingredients);
                self.grant_scaled_event(craft.number, event, craft.crafts, scale);
            }
            self.quest_event(craft.number, Event::Crafted(&craft.recipe), now_ms);
            let families = super::item_families(&self.content, &craft.item);
            self.quest_event(craft.number, Event::CraftedFamilies(&families), now_ms);
            for achievement in &craft.achievements {
                self.earn_achievement(craft.number, achievement, now_ms);
            }
            if let Some(cooldown) = craft.cooldown {
                self.count_recipe_cooldown(entity, &craft.recipe, cooldown, craft.crafts);
            }
        }
    }

    /// How many of `crafts` crafts of the recipe whose wire id is `recipe_crc` player `entity`
    /// makes a bonus item of: each rolls the recipe's `BonusItemChance` plus the `RollBonus` of
    /// the player's rank in `tradeskill` ([`nw_content::tradeskill_xp`]); a recipe without a
    /// chance makes none. **Best guess:** a bonus is one more craft's output (retail's measured
    /// recipes all make one item, so one item or `OutputQty` could not be told apart).
    fn bonus_crafts(
        &mut self,
        entity: Entity,
        recipe_crc: u32,
        tradeskill: Option<&str>,
        crafts: u32,
    ) -> u32 {
        let content = Arc::clone(&self.content);
        let Some(chance) = content.bonus_chances.of(recipe_crc) else {
            return 0;
        };
        let rank_bonus = tradeskill.map_or(0.0, |tradeskill| {
            let (rank, _) = self.tradeskill_perks(entity, tradeskill);
            content.tradeskill_curves.roll_bonus(tradeskill, rank)
        });
        let dice = &mut self.dice;
        bonus_crafts(chance + rank_bonus, crafts, &mut |below| dice.below(below))
    }

    /// The client of player `number` (`entity`) cancels its craft with a request of header
    /// `header` (2999): it gets an idle update and stays at its station. Retail clients send it
    /// 18 to 26 ms after each craft's answer; the server answers with one more idle update, and
    /// the player crafts again at the same station (`12-2_skinningknife`: three crafts, each
    /// followed by a 2999, with no new interaction; live 2026-10-29: a station closed here
    /// refused every next craft).
    pub(super) fn craft_cancelled(&mut self, entity: Entity, number: u64, header: FacetHeader) {
        self.send_idle(entity, answer_header(Some(header), number));
    }

    /// The interaction `interaction` of player `entity` ends: when it opened the player's
    /// station, the player leaves the station.
    pub(super) fn leave_station(&mut self, entity: Entity, interaction: u64) {
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && player.station_interaction == Some(interaction)
        {
            player.station = None;
            player.station_interaction = None;
        }
    }

    fn send_idle(&mut self, entity: Entity, header: FacetHeader) {
        if let Some(player) = self.world.get::<Player>(entity) {
            let idle = ServerMessage::CraftProgress(CraftProgressMsg::idle(header));
            send_now(&mut self.combat_messages, &player.connection, &[idle]);
        }
    }
}

/// Queues the crafting state of `player` with its recipe cooldown counts when `sent` calls for
/// it: after a change, or at a spawn with counts (retail's login state lists them; with none it
/// sends no `m_cooldowns`).
fn send_recipe_cooldowns(player: &mut Player, sent: LimitsSent) {
    let changed = player.recipe_cooldowns.take_changed();
    let shown = player.recipe_cooldowns.shown();
    let due = match sent {
        LimitsSent::Changed => changed,
        LimitsSent::Spawn => !shown.is_empty(),
    };
    if !due {
        return;
    }
    let version = player.list_version + 1;
    let state = CraftingState {
        cooldowns: FullList {
            version: Some(version),
            elements: shown
                .into_iter()
                .map(|cooldown| CraftingCooldown {
                    key: cooldown.key,
                    // `CooldownQuantity` is at most 10 in build 25810808; the wire count is a u8.
                    count: u8::try_from(cooldown.count).unwrap_or(u8::MAX),
                    ends_ns: cooldown.ends_ns,
                })
                .collect(),
        },
    };
    match state.member() {
        Ok(member) => {
            player.list_version = version;
            player.extra_members.push(member);
        }
        Err(error) => tracing::warn!(%error, "crafting state not encoded"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_content::crafting::{Ingredient, IngredientKind, RecipeDetails, Recipes};
    use nw_content::item_rolls::{ItemRollRow, ItemRolls};
    use nw_content::perks::PerkTable;
    use nw_content::progression::{Progression, RankPerks, TradeskillRankRow, TradeskillSource};
    use nw_content::rewards::{EventGrant, EventGrants};
    use nw_content::tradeskill_xp::BonusChances;
    use nw_world::stations::{StationGridSide, StationSlice};

    use super::*;
    use crate::player_content::PlayerContent;
    use crate::world::gear;
    use crate::world::tests::{
        NOW, SERVER_TIME, admit_at, connection, messages_for, world_with_spawns,
    };

    /// The retail WW campfire group (`newchar-retail-2026-09-18`, 448630 ms): its members as
    /// (component, class, body), the card naming `a761c93f095ab84d`. Retail lists them in another
    /// order; the order differs between retail groups.
    const WW_MEMBERS: [(u64, u64, &str); 7] = [
        (0, 10, "0102a761c93f095ab84d0000000000000000"),
        (
            1,
            1594,
            "010705000001ac100206576f6f6454318a91442700000000000500ffffffff0000003300000000000007466c696e7454310c9280c800000000000100ffffffff00000033000000000000",
        ),
        (3, 1746, "010201"),
        (6, 3139, "0103000000"),
        (7, 2134, "01010001ac1004010100020100030100040100"),
        (8, 2187, "010101"),
        (
            9,
            3217,
            "01ff000000000000000000000000000000000000003f8000003f8000003f8000003f80000000003f8000003f8000003f8000003f800000000002010000000000000000000f00000000",
        ),
    ];
    /// The component and class bytes of the WW group's ownership member, as retail sent them.
    const RETAIL_OWNERSHIP_HEAD: &str = "099132";
    /// The WW group's list version (`ac10`): the retail server's counter.
    const WW_LIST_VERSION: u64 = 1068;
    const WW_CAMPFIRE: u64 = 0x4db8_5a09_3fc9_61a7;
    const NEAR: Position = Position {
        x: 9_606.5,
        y: 2_367.8,
        z: 44.9,
    };
    const RADIUS_M: f32 = 100.0;

    /// The WW campfire as the world file has it (`int_watchtowercampt1`'s components).
    fn campfire() -> CraftingStation {
        CraftingStation {
            id: WW_CAMPFIRE,
            slice: "slices/pois/interactions/int_watchtowercampt1".to_owned(),
            position: [NEAR.x, NEAR.y, NEAR.z],
            components: StationSlice {
                station_type: "Camp1".to_owned(),
                name: "@ui_watcherscampfire".to_owned(),
                buildable: Some((1, 5)),
                structure: "CampT1".to_owned(),
                ownership: Some((9, 1)),
                structure_name: String::new(),
                obstruction: Some((8, true)),
                territory: Some(3),
                grid: Some((
                    7,
                    (1..=4)
                        .map(|side| StationGridSide {
                            side,
                            points: 1,
                            active: false,
                        })
                        .collect(),
                )),
                delayed_event: Some(6),
            },
        }
    }

    fn costs() -> StructureCosts {
        StructureCosts::build([(
            "CampT1".to_owned(),
            vec![("WoodT1".to_owned(), 5), ("FlintT1".to_owned(), 1)],
        )])
    }

    #[test]
    fn the_campfire_group_holds_the_retail_member_bodies() {
        let retail: Vec<BundleMember> = WW_MEMBERS
            .iter()
            .map(|(component, class, body)| BundleMember {
                component: *component,
                state: MemberState::decode(*class, &hex::decode(body).expect("hex"))
                    .expect("retail body"),
            })
            .collect();
        let members = station_members(&campfire(), &costs(), WW_LIST_VERSION);
        assert_eq!(members, retail);
        let encoded = |members: &[BundleMember]| -> Vec<Vec<u8>> {
            members
                .iter()
                .map(|member| member.encode().expect("encode"))
                .collect()
        };
        assert_eq!(encoded(&members), encoded(&retail));
        // The retail wire bytes of member 9: component, class `91 32` (3217), body.
        let ownership = members.last().expect("ownership").encode().expect("encode");
        assert_eq!(
            hex::encode(ownership),
            format!("{RETAIL_OWNERSHIP_HEAD}{}", WW_MEMBERS[6].2)
        );
        let served = station_members(&campfire(), &costs(), FIRST_VERSION);
        let MemberState::BuildableController(controller) = &served[1].state else {
            panic!("controller");
        };
        assert_eq!(
            controller
                .committed_resources
                .as_ref()
                .and_then(|list| list.version),
            Some(FIRST_VERSION),
            "this server's lists start at the version of its other first states"
        );
    }

    #[test]
    fn a_station_goes_out_on_view_entry_only_and_names_its_answer() {
        let stations = Stations::build(&[campfire()], &costs());
        let number = STATION_ENTITY_BASE;
        assert_eq!(
            stations.numbers_near(NEAR, RADIUS_M).collect::<Vec<_>>(),
            [number]
        );
        let far = Position {
            x: NEAR.x + RADIUS_M + 1.0,
            ..NEAR
        };
        assert_eq!(stations.numbers_near(far, RADIUS_M).count(), 0);
        let new = Slot {
            interest: 5,
            new: true,
        };
        let group = stations.group(number, new).expect("group");
        assert_eq!(group.interest, 5);
        assert_eq!(group.members.len(), WW_MEMBERS.len());
        let kept = Slot { new: false, ..new };
        assert_eq!(stations.group(number, kept), None, "once per view entry");
        let station = stations
            .named(&gde_ref_of(WW_CAMPFIRE))
            .expect("named by its GdeRef");
        assert_eq!(station.name, b"@ui_watcherscampfire");
        assert_eq!(station.station_type, "Camp1");
    }

    /// The player's number.
    const NUMBER: u64 = 1;
    /// Retail refining rows (build 25810808, cut): `IngotT2` (4 `OreT1`, `smelting2`, fee 1,
    /// `Craft_SmeltingT2`) and `IngotT53` (recipe level 250).
    const INGOT: &str = "IngotT2";
    const HIGH_INGOT: &str = "IngotT53";
    const ORE: &str = "OreT1";
    const ORE_PER_CRAFT: u32 = 4;
    const FEE_PER_CRAFT: u32 = 1;
    const SMELTING_EVENT: &str = "Craft_SmeltingT2";
    const SMELTING_REWARD: f64 = 10.0;
    const HIGH_LEVEL: u32 = 250;
    /// A T5 smelter's station type (a tier above the recipe's).
    const SMELTER: &str = "smelting5";
    const COINS: u64 = 100;

    fn content() -> PlayerContent {
        let recipe = |id: &str| Recipe {
            id: id.to_owned(),
            item: id.to_owned(),
            quantity: 1,
            ingredients: vec![Ingredient {
                kind: IngredientKind::Item(ORE.to_owned()),
                quantity: ORE_PER_CRAFT,
            }],
            stations: vec!["smelting2".to_owned()],
        };
        let details = |recipe_level| RecipeDetails {
            tradeskill: Some("Smelting".to_owned()),
            recipe_level,
            game_event: Some(SMELTING_EVENT.to_owned()),
            fee: FEE_PER_CRAFT,
            ..RecipeDetails::default()
        };
        PlayerContent {
            kits: gear::tests::kits_with(&[
                (INGOT, "Resource+Ingot"),
                (HIGH_INGOT, "Resource+Ingot"),
                (ORE, "Resource+Ore"),
            ]),
            recipes: Recipes::build([recipe(INGOT), recipe(HIGH_INGOT)], []).with_details([
                (INGOT.to_owned(), details(0)),
                (HIGH_INGOT.to_owned(), details(HIGH_LEVEL)),
            ]),
            progression: Progression::default().with_tradeskills([TradeskillSource {
                id: "Smelting".to_owned(),
                max_levels: vec![200],
                ranks: vec![TradeskillRankRow {
                    rank: 0,
                    cap: 108.0,
                    perks: RankPerks::default(),
                }],
            }]),
            event_grants: EventGrants::build([(
                SMELTING_EVENT.to_owned(),
                EventGrant {
                    progression: Some(("Smelting".to_owned(), SMELTING_REWARD)),
                    coins: (0, 0),
                    territory_standing: 0.0,
                },
            )]),
            ..PlayerContent::default()
        }
    }

    /// A world with the test content and one player at a T5 smelter with ore and coins.
    fn at_the_smelter(ore: u32, coins: u64) -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        world.content = Arc::new(content());
        let mut admit = admit_at(NUMBER, 0.0, true);
        admit.backstory = Some(gear::tests::BACKSTORY.to_owned());
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        let entity = world.by_connection[&connection(NUMBER)];
        let ore_item = world.content.kits.item(ORE).cloned().expect("ore");
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.station = Some(SMELTER.to_owned());
            player.coins = coins;
            assert!(player.gear.as_mut().expect("gear").add(&ore_item, ore));
        }
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
    fn a_batch_craft_takes_its_fee_and_gives_its_items_and_tradeskill_experience() {
        const CRAFTS: u32 = 2;
        // The client shows a fee above the recipes' own: at most the recipes' is taken.
        const CLIENT_FEE: u32 = 18;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT * CRAFTS, COINS);
        let crc = lowercase_crc32(INGOT);
        world.craft(
            entity,
            NUMBER,
            crc,
            CRAFTS,
            CLIENT_FEE,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        let coins = |world: &SimWorld| world.world.get::<Player>(entity).expect("player").coins;
        assert_eq!(
            (bag_count(&world, entity, ORE), coins(&world)),
            (ORE_PER_CRAFT * CRAFTS, COINS),
            "the request takes nothing (retail: the bag changes after the result)"
        );
        let [craft] = world.crafts.as_slice() else {
            panic!("one craft runs");
        };
        assert_eq!((craft.crafts, craft.quantity), (CRAFTS, CRAFTS));
        world.crafts_due(NOW + CRAFT_MS);
        assert!(world.crafts.is_empty());
        assert_eq!(bag_count(&world, entity, ORE), 0, "taken at the end");
        assert_eq!(coins(&world), COINS - u64::from(FEE_PER_CRAFT * CRAFTS));
        assert_eq!(bag_count(&world, entity, INGOT), CRAFTS);
        let smelting = world
            .content
            .progression
            .tradeskill("Smelting")
            .expect("smelting");
        let experience = world
            .world
            .get::<Player>(entity)
            .map(|player| player.masteries.experience(smelting));
        assert_eq!(
            experience,
            Some(80),
            "10 per ingredient (4 ore) per craft under a neutral multiplier"
        );
    }

    #[test]
    fn a_recipe_cooldown_counts_its_crafts_and_refuses_past_its_quantity() {
        use nw_content::loot_limits::{LootLimitRules, ResetClock, Weekday};
        use nw_content::recipe_cooldowns::RecipeCooldown;
        use nw_messages::crafting_state::{CRAFTING_STATE_COMPONENT, CRAFTING_STATE_TYPE_INDEX};
        /// A daily cooldown of 2 crafts (`LeatherT51` has 10).
        const QUANTITY: u32 = 2;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT * (QUANTITY + 1), COINS);
        let content = Arc::make_mut(&mut world.content);
        content.loot_limits = Some(LootLimitRules {
            limits: Default::default(),
            clock: ResetClock {
                hour_utc: 3,
                weekly_day: Weekday::Tuesday,
            },
        });
        let cooldown = RecipeCooldown::from_cells(Some(86_400), Some(i64::from(QUANTITY)));
        let crc = lowercase_crc32(INGOT);
        let mut details = content.recipes.details(crc);
        details.cooldown = cooldown;
        content.recipes = Recipes::build([content.recipes.get(crc).cloned().expect("recipe")], [])
            .with_details([(INGOT.to_owned(), details)]);
        let craft = |world: &mut SimWorld, crafts| {
            world.craft(
                entity,
                NUMBER,
                crc,
                crafts,
                FEE_PER_CRAFT * crafts,
                &[],
                &CraftChoice::default(),
                NOW,
            );
        };
        craft(&mut world, QUANTITY);
        world.crafts_due(NOW + CRAFT_MS);
        assert_eq!(bag_count(&world, entity, INGOT), QUANTITY);
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let state = player
                .extra_members
                .iter()
                .find(|member| member.state.type_index() == CRAFTING_STATE_TYPE_INDEX)
                .expect("the crafting state goes to the client");
            assert_eq!(state.component, CRAFTING_STATE_COMPONENT);
            player.extra_members.clear();
            let saved = player.progression().recipe_cooldowns;
            assert_eq!(saved.len(), 1);
            assert_eq!(
                (saved[0].recipe_id.as_str(), saved[0].count),
                ("ingott2", QUANTITY)
            );
        }
        craft(&mut world, 1);
        assert!(world.crafts.is_empty(), "past the quantity: refused");
        assert_eq!(
            bag_count(&world, entity, ORE),
            ORE_PER_CRAFT,
            "nothing taken"
        );
    }

    #[test]
    fn a_craft_whose_ingredients_are_gone_at_its_end_makes_nothing() {
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        assert_eq!(world.crafts.len(), 1);
        {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            player.gear.as_mut().expect("gear").take(ORE, 1);
        }
        world.take_messages();
        world.crafts_due(NOW + CRAFT_MS);
        assert_eq!(bag_count(&world, entity, INGOT), 0);
        assert_eq!(
            bag_count(&world, entity, ORE),
            ORE_PER_CRAFT - 1,
            "nothing more taken"
        );
        let coins = world.world.get::<Player>(entity).expect("player").coins;
        assert_eq!(coins, COINS);
        let answers = craft_answers(&mut world);
        assert!(
            answers.iter().all(|answer| answer.recipe.is_empty()),
            "idle only"
        );
    }

    /// The craft answers of `world`'s player since the last call, also those waiting for the
    /// bundles of the tick.
    fn craft_answers(world: &mut SimWorld) -> Vec<CraftProgressMsg> {
        let mut sent = world.take_messages();
        sent.append(&mut world.after_bundles);
        messages_for(&sent, &connection(NUMBER))
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::CraftProgress(answer) => Some(answer),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_craft_answers_go_to_the_header_of_the_clients_request() {
        // Retail newchar 2026-09-18: the request (2997) and its answers (2993) share the target
        // `1ad9661ac8b0dca8`, which is not the entity plus the crafting component.
        const REQUEST: FacetHeader = FacetHeader {
            target: 0x1ad9_661a_c8b0_dca8,
            entity: 0x0013_ccd3_47e7_3f49,
        };
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        let choice = CraftChoice {
            header: Some(REQUEST),
            ..CraftChoice::default()
        };
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &choice,
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        let answers = craft_answers(&mut world);
        assert_eq!(answers.len(), 2, "crafted, then idle");
        assert!(answers.iter().all(|answer| answer.header == REQUEST));
        world.craft_cancelled(entity, NUMBER, REQUEST);
        assert_eq!(
            craft_answers(&mut world),
            [CraftProgressMsg::idle(REQUEST)],
            "the cancel (2999) is answered at its own header"
        );
        assert!(
            world
                .world
                .get::<Player>(entity)
                .is_some_and(|player| player.station.is_some()),
            "retail crafts again at the same station after the cancel"
        );
    }

    #[test]
    fn the_end_of_the_stations_interaction_leaves_the_station() {
        const OPENED_WITH: u64 = 5;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        let station = |world: &SimWorld| {
            world
                .world
                .get::<Player>(entity)
                .and_then(|player| player.station.clone())
        };
        if let Some(mut player) = world.world.get_mut::<Player>(entity) {
            player.station_interaction = Some(OPENED_WITH);
        }
        world.leave_station(entity, OPENED_WITH + 1);
        assert!(station(&world).is_some(), "another interaction ended");
        world.leave_station(entity, OPENED_WITH);
        assert_eq!(station(&world), None, "the station's own interaction ended");
    }

    #[test]
    fn the_craft_answers_go_out_after_the_items_they_made() {
        // Retail Basic Needs and Campfire Feast (2026-09-27): the item change (7048) and the
        // bundle came before the two 2993 of the craft, in one tick.
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        world.take_messages();
        let sent = world
            .tick(NOW + CRAFT_MS, NOW + CRAFT_MS)
            .expect("tick")
            .messages;
        let own = connection(NUMBER);
        let at =
            |wanted: fn(&ServerMessage) -> bool| messages_for(&sent, &own).iter().position(wanted);
        let item_change = at(|message| matches!(message, ServerMessage::StorageSearchItems(_)))
            .expect("the item change");
        let crafted = at(|message| matches!(message, ServerMessage::CraftProgress(_)))
            .expect("the craft answer");
        assert!(item_change < crafted, "the items first, then the answer");
        assert!(world.after_bundles.is_empty(), "sent with the tick");
    }

    #[test]
    fn a_crafts_bag_records_mark_the_used_and_the_made_stacks() {
        // Retail Basic Needs (1790514751716 ms): the flint and the wood the knife used went out
        // with sync reason 2, the knife with 3; other reasons left the live client saying
        // "Crafting attempt failed" (2026-10-29).
        use nw_messages::items::{SYNC_CRAFT_USED, SYNC_CRAFTED};
        use nw_messages::list_delta::ReplicatedList;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT * 2, COINS);
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        let index_of = |id: &str| world.content.kits.item(id).expect("item").item_index;
        let gear = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .expect("gear")
            .states();
        let Some(ReplicatedList::Full(bag)) = gear.bag.items else {
            panic!("a full bag list");
        };
        let reason_of = |id: &str| {
            bag.elements
                .iter()
                .find(|record| record.item_index == index_of(id))
                .map(|record| record.sync_reason)
        };
        assert_eq!(reason_of(ORE), Some(SYNC_CRAFT_USED), "the ore left");
        assert_eq!(reason_of(INGOT), Some(SYNC_CRAFTED), "the ingot made");
    }

    #[test]
    fn a_craft_that_uses_up_a_stack_empties_its_slot_as_used() {
        // Retail `Refining` 98.832 s and `Craft-Gypsum-Orbs` 16.524 s: the slot of the stack a
        // craft used up went out empty with sync reason 2.
        use nw_messages::items::SYNC_CRAFT_USED;
        use nw_messages::list_delta::ReplicatedList;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        let ingot = world.content.kits.item(INGOT).cloned().expect("ingot");
        let ore_slot = {
            let mut player = world.world.get_mut::<Player>(entity).expect("player");
            let gear = player.gear.as_mut().expect("gear");
            // The ingot made joins this stack, as retail's outputs joined theirs.
            assert!(gear.add(&ingot, 1));
            let slots = u32::try_from(nw_messages::STARTER_BAG_SLOTS).expect("slots");
            (0..slots)
                .find(|slot| gear.bag_item(*slot).is_some_and(|(id, _)| id == ORE))
                .expect("the ore's slot")
        };
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        let gear = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .expect("gear")
            .states();
        let Some(ReplicatedList::Full(bag)) = gear.bag.items else {
            panic!("a full bag list");
        };
        let emptied = bag.elements[usize::try_from(ore_slot).expect("slot")];
        assert_eq!(emptied.quantity, 0, "the ore is gone");
        assert_eq!(emptied.sync_reason, SYNC_CRAFT_USED);
    }

    #[test]
    fn a_craft_sends_one_item_change_per_item_made_with_the_last_search() {
        // Retail `Refining` 14.899 s and 15.962 s: a craft that used one item and made another
        // sent one 7048, with the search of 11.583 s and its 21 results (44 of 44 crafts of one
        // output kind alike, module notes of `storage_search`).
        use super::super::OWN_CARD_DELAY_MS;
        use super::super::storage_search::SearchFor;
        const SEARCH: [u8; RAW_ID_LEN] = [5; RAW_ID_LEN];
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        let card_at = NOW + OWN_CARD_DELAY_MS;
        world.tick(card_at, card_at).expect("the first card");
        world
            .world
            .get_mut::<super::super::attack::Vitals>(entity)
            .expect("vitals")
            .spawned = true;
        world.take_messages();
        world.search_storage(entity, NUMBER, SEARCH, SearchFor::Unknown, false, card_at);
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            card_at,
        );
        world.take_messages();
        let due = card_at + CRAFT_MS;
        let sent = world.tick(due, due).expect("tick").messages;
        let changes: Vec<_> = messages_for(&sent, &connection(NUMBER))
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::StorageSearchItems(items) => Some(items),
                _ => None,
            })
            .collect();
        assert_eq!(changes.len(), 1, "the ingot; the ore used counts for none");
        assert_eq!(changes[0].search, SEARCH, "the last search goes with it");
        assert_eq!(changes[0].total, 0, "its result count");
    }

    #[test]
    fn a_sure_bonus_chance_makes_one_more_crafts_output_per_craft() {
        const CRAFTS: u32 = 2;
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT * CRAFTS, COINS);
        let mut content = (*world.content).clone();
        content.bonus_chances = BonusChances::build([(INGOT, 1.0)]);
        world.content = Arc::new(content);
        let crc = lowercase_crc32(INGOT);
        world.craft(
            entity,
            NUMBER,
            crc,
            CRAFTS,
            CRAFTS,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        assert_eq!(bag_count(&world, entity, INGOT), CRAFTS * 2);
        // A recipe without a chance makes no bonus.
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        world.craft(entity, NUMBER, crc, 1, 1, &[], &CraftChoice::default(), NOW);
        world.crafts_due(NOW + CRAFT_MS);
        assert_eq!(bag_count(&world, entity, INGOT), 1);
    }

    /// The smelter's world with the ingot made a gear item of gear score 400 to 500 (tier 4).
    fn rolled_ingots(ore: u32) -> (SimWorld, Entity) {
        const INGOT_TIER: u32 = 4;
        let (mut world, entity) = at_the_smelter(ore, COINS);
        let mut content = (*world.content).clone();
        content.item_rolls = ItemRolls::build(
            [ItemRollRow {
                item_id: INGOT.to_owned(),
                tier: INGOT_TIER,
                min_gear_score: 400,
                max_gear_score: 500,
                ..ItemRollRow::default()
            }],
            PerkTable::default(),
        );
        world.content = Arc::new(content);
        (world, entity)
    }

    /// The gear scores of the player's ingot stacks.
    fn ingot_gear_scores(world: &SimWorld, entity: Entity) -> Vec<u32> {
        let carried = world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.gear.as_ref())
            .map(|gear| gear.carried())
            .unwrap_or_default();
        carried
            .iter()
            .filter(|item| item.item_id == INGOT)
            .filter_map(|item| item.state.gear_score)
            .collect()
    }

    #[test]
    fn a_gear_craft_makes_each_item_at_the_picked_gear_score_held_to_its_range() {
        const CRAFTS: u32 = 2;
        const PICKED: u32 = 420;
        let (mut world, entity) = rolled_ingots(ORE_PER_CRAFT * CRAFTS);
        let choice = CraftChoice {
            gear_score: PICKED,
            perk_items: Vec::new(),
            header: None,
        };
        let crc = lowercase_crc32(INGOT);
        world.craft(
            entity,
            NUMBER,
            crc,
            CRAFTS,
            FEE_PER_CRAFT * CRAFTS,
            &[],
            &choice,
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        assert_eq!(ingot_gear_scores(&world, entity), [PICKED, PICKED]);
        // The range is the item's 400 with no bonus: a pick above it stops at the status
        // allowance, no pick takes the lowest.
        let (mut world, entity) = rolled_ingots(ORE_PER_CRAFT * CRAFTS);
        let too_high = CraftChoice {
            gear_score: 900,
            perk_items: Vec::new(),
            header: None,
        };
        world.craft(entity, NUMBER, crc, 1, FEE_PER_CRAFT, &[], &too_high, NOW);
        world.craft(
            entity,
            NUMBER,
            crc,
            1,
            FEE_PER_CRAFT,
            &[],
            &CraftChoice::default(),
            NOW,
        );
        world.crafts_due(NOW + CRAFT_MS);
        let mut scores = ingot_gear_scores(&world, entity);
        scores.sort_unstable();
        assert_eq!(scores, [400, 400 + CRAFT_STATUS_GS_ALLOWANCE]);
    }

    #[test]
    fn a_craft_needs_its_recipe_level_its_fee_and_its_station() {
        let (mut world, entity) = at_the_smelter(ORE_PER_CRAFT, COINS);
        let none = CraftChoice::default();
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(HIGH_INGOT),
            1,
            1,
            &[],
            &none,
            NOW,
        );
        assert!(
            world.crafts.is_empty(),
            "Smelting rank 0 is below level 250"
        );
        assert_eq!(
            bag_count(&world, entity, ORE),
            ORE_PER_CRAFT,
            "nothing taken"
        );
        let (mut broke, broke_entity) = at_the_smelter(ORE_PER_CRAFT, 0);
        broke.craft(
            broke_entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            1,
            &[],
            &none,
            NOW,
        );
        assert!(broke.crafts.is_empty(), "no coins for the fee");
        assert_eq!(bag_count(&broke, broke_entity, ORE), ORE_PER_CRAFT);
        world
            .world
            .get_mut::<Player>(entity)
            .expect("player")
            .station = Some("smelting1".to_owned());
        world.craft(
            entity,
            NUMBER,
            lowercase_crc32(INGOT),
            1,
            1,
            &[],
            &none,
            NOW,
        );
        assert!(
            world.crafts.is_empty(),
            "a tier-1 station makes no tier-2 recipe"
        );
    }
}
