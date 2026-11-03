// SPDX-License-Identifier: AGPL-3.0-or-later
//! The content a sim needs for its players besides combat: the starter kit of each archetype
//! pick, the bit of each achievement, the look of each mob prefab, and the quests.

use nw_bus::MISSION_BOARD_ID_LEN;
use nw_content::attribute_bonuses::AttributeBonuses;
use nw_content::camps::CampRules;
use nw_content::charms::ItemNames;
use nw_content::chest_lockouts::ChestLockouts;
use nw_content::crafting::{Recipes, StructureCosts};
use nw_content::creature_animations::CreatureAnimations;
use nw_content::deaths_door::DeathsDoorRules;
use nw_content::drop_rolls::DropRolls;
use nw_content::equip_load::EquipLoads;
use nw_content::fishing::FishingRules;
use nw_content::gameplay_config::GameplayConfig;
use nw_content::gathering::{
    CrateLoot, CrateVariant, CyclicGatherables, DetectionSpheres, GatherRules, Gatherables,
    ToolEfficiencies,
};
use nw_content::instances::InstanceCatalog;
use nw_content::interact_tags::{InteractTags, gatherable_tags, variant_tags};
use nw_content::item_repair::GearScoreUpgrades;
use nw_content::item_rolls::ItemRolls;
use nw_content::item_rules::ItemRules;
use nw_content::kill_rewards::KillRewards;
use nw_content::leaderboards::LeaderboardStats;
use nw_content::loot::{LootTables, MobLoot, PlayerLootLevels};
use nw_content::loot_limits::{LootLimitRules, LootLimits, ResetClock};
use nw_content::meta_achievements::MetaAchievementRules;
use nw_content::mounts::MountRules;
use nw_content::music::MusicRules;
use nw_content::mutations::MutationCatalog;
use nw_content::objective_limits::ObjectiveLimitGroup;
use nw_content::perk_effects::PerkEffects;
use nw_content::poi_tags::PoiTags;
use nw_content::progression::Progression;
use nw_content::quick_courses::QuickCourseRules;
use nw_content::rewards::{EventGrants, KillExperience, Rewards};
use nw_content::salvage::Salvage;
use nw_content::skins::{DyeCatalog, SkinCatalog};
use nw_content::starting_ranks::StartingRanks;
use nw_content::status_effects::StatusEffectRules;
use nw_content::titles::TitleRules;
use nw_content::tradeskill_xp::{BonusChances, TradeskillCurves};
use nw_formats::objectstream::lowercase_crc32;
use nw_sim_core::{EquipLoad, EquipLoadLimits, Position};
use nw_world::camps::CampSlices;
use nw_world::collision::QuestVolume;
use nw_world::dungeon_scripts::DungeonScripts;
use nw_world::interact_tags::TagOwner;
use nw_world::quick_courses::QuickCoursePaths;
use nw_world::records::{Capital, GatherablePoint, WorldFile};
use nw_world::stations::CraftingStation;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use nw_content::Tables;
use nw_content::achievements::AchievementIndexes;
use nw_content::ftue_entities::FtueLevel;
use nw_content::mob_prefabs::{ASSET_ID_LEN, MobPrefabs, PrefabKey, PrefabMember};
use nw_content::starter::StarterKits;
use nw_messages::{AlcState, AssetId, BundleMember, MemberState, VitalsState};

use crate::asset_solids::AssetSolids;
use crate::charting::ChartRules;
use crate::invite_rules::InviteRules;
use crate::objectives::{
    KillTypes, ObjectiveTree, TaskBook, TaskRow, counted_game_events, task_rows,
};
use crate::shop::ShopCatalog;
use crate::trading_post::{Settlements, TradingCatalog, TradingPostConfig};
use crate::travel_rules::{TravelRules, capital_id};

/// The tutorial quest's objective id (`Objectives`).
pub const TUTORIAL_OBJECTIVE: &str = "01_SoleSurvivor2";
/// The tutorial's sword (the login kit's weapon; `01_SoleSurvivor2_Tutorial_RemoveLongsword`
/// takes it after the S1 scene): no player keeps it past the tutorial (retail newchar
/// 2026-09-18: the class weapons from 448 s on, the sword gone).
pub const TUTORIAL_SWORD: &str = "1hSwordT2_FTUE";
/// The first open-world quest, which a player gets when it leaves the tutorial (retail: in the
/// first open-world state, xp-potion-exit note, section 3.3).
pub const OPEN_WORLD_OBJECTIVE: &str = "12_GetOffBeach";
/// Its root task.
pub const OPEN_WORLD_QUEST_ROOT: &str = "12_getoffbeach_container";
/// The root task of the tutorial quest `01_SoleSurvivor2` (live-tutorial note, appendix A).
pub const TUTORIAL_QUEST_ROOT: &str = "01_SoleSurvivor2_Container";

/// One objective a player can run (admin studio spec section 6.3).
#[derive(Debug, Clone, PartialEq)]
pub struct QuestDef {
    /// `ObjectiveID`.
    pub id: String,
    /// Its task tree.
    pub tree: Arc<ObjectiveTree>,
    /// `ItemRewardName` and `ItemRewardQty`, given at completion.
    pub reward: Option<(String, u32)>,
    /// `Repeatable`: it can run again after it is done.
    pub repeatable: bool,
    /// `RequiredAchievementID`: the achievement expression an NPC offers it on.
    pub requirement: Option<String>,
    /// `RequiredFaction`: the faction its player must be of, or not be of
    /// ([`nw_content::factions::faction_requirement_met`]).
    pub required_faction: Option<String>,
    /// `AchievementId`: set at completion; the next quest of the chain requires it.
    pub achievement: Option<String>,
    /// `NPCDestinationId`: the NPC the player turns it in at; without one it is done when its
    /// tree is.
    pub turn_in: Option<String>,
    /// `SuccessGameEventID`: the game event whose reward the completion gives.
    pub success_event: Option<String>,
    /// The journal limit its `Type` counts against; `None`: no limit.
    pub limit_group: Option<ObjectiveLimitGroup>,
    /// The `ObjectiveID` the client's journal record names: the id, or a mission's template.
    pub wire_objective: String,
    /// A mission's CRC-32 of its lowercase `MissionID` (the record's `m_missionId`).
    pub mission: Option<u32>,
}

/// The `Objectives` cells a quest needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ObjectiveRow {
    /// `ObjectiveID`.
    pub id: String,
    /// `Task`: the root task.
    pub root: String,
    /// `ItemRewardName`.
    pub reward_item: Option<String>,
    /// `ItemRewardQty`.
    pub reward_quantity: Option<i64>,
    /// `Repeatable`.
    pub repeatable: bool,
    /// `RequiredAchievementID`.
    pub requirement: Option<String>,
    /// `RequiredFaction`.
    pub required_faction: Option<String>,
    /// `AchievementId`.
    pub achievement: Option<String>,
    /// `NPCDestinationId`.
    pub turn_in: Option<String>,
    /// `SuccessGameEventID`.
    pub success_event: Option<String>,
    /// `Type`.
    pub kind: String,
}

/// One `MissionData` row (finding "mission board"): an objective template (`ObjectiveID`)
/// filled in with the mission's values. The client takes a town board mission by the CRC-32
/// of its lowercase `MissionID` (3819).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MissionRow {
    /// `MissionID`.
    pub id: String,
    /// `ObjectiveID`: the template.
    pub objective: String,
    /// `AvailableTerritoryID`: the record's `m_originTerritoryId` (Inferred: retail
    /// `FM_C_16_*` missions had 16, `FM_98_03_*` 3).
    pub territory: u16,
    /// `SuccessGameEventIdOverride`.
    pub success_event: Option<String>,
    /// `RequiredFaction`: the faction board's faction (`Covenant`, `Marauder`, `Syndicate`).
    pub required_faction: Option<String>,
    /// `TaskHaveAndReturnItemsOverride`.
    pub items: Option<String>,
    /// `TaskHaveAndReturnItemsQtyOverride`.
    pub items_count: Option<i64>,
    /// `TaskHaveAndReturnItemsItemsDropVCOverride`: the enemy that drops the items.
    pub items_drop_enemy: Option<String>,
    /// `TaskHaveAndReturnItemsDropProbabilityOverride`.
    pub items_drop_percent: Option<i64>,
    /// `TaskKillContributionOverride`.
    pub kill: Option<String>,
    /// `TaskKillContributionQtyOverride`.
    pub kill_count: Option<i64>,
    /// `POITagOverride`: the place of its tasks that enforce a location.
    pub place: Option<String>,
    /// `TaskHaveAndReturnChestDropProbabilityOverride`: the chance a container in its place
    /// drops its items.
    pub chest_percent: Option<i64>,
    /// `TaskGiveAndTakeItemOverride`: the item its give task gives (a work order).
    pub given_item: Option<String>,
    /// `TerritoryIDOverride`: the territory of its go-to-territory tasks.
    pub territory_override: Option<u32>,
    /// `OverrideLocation`: the location of its go-to tasks.
    pub location: Option<String>,
    /// `PatrolPOI1`: the place of its patrol task.
    pub patrol_place: Option<String>,
    /// `PatrolDuration` (seconds): the stay of its patrol task.
    pub patrol_duration: Option<i64>,
    /// `TaskTimerOverride` (seconds): the duration of its timer task.
    pub timer: Option<i64>,
}

impl MissionRow {
    /// The mission's value of template task `row`; `None` when the mission changes nothing in
    /// it. Item tasks take its items, count, drops and container chance; kill tasks its enemy
    /// and count; give tasks its work order item; go-to tasks its place, territory and
    /// location; a patrol its first patrol place and stay; a timer its duration; a task that
    /// enforces its location takes the mission's place.
    fn override_of(&self, row: &TaskRow) -> Option<TaskRow> {
        let original = row;
        let placed = TaskRow {
            poi_tag: if row.enforce_location {
                self.place.clone().or(row.poi_tag.clone())
            } else {
                row.poi_tag.clone()
            },
            ..row.clone()
        };
        let row = &placed;
        match row.kind.to_ascii_lowercase().as_str() {
            "taskhaveandreturnitems" | "taskgatherandreturnitems" | "taskhaveitems" => {
                let items = self.items.clone()?;
                Some(TaskRow {
                    item_name: Some(items),
                    target_qty: self.items_count.or(row.target_qty),
                    item_drop_vc: self.items_drop_enemy.clone().or(row.item_drop_vc.clone()),
                    item_drop_probability: self.items_drop_percent.or(row.item_drop_probability),
                    chest_drop_probability: self.chest_percent.or(row.chest_drop_probability),
                    returned_on_turn_in: true,
                    ..row.clone()
                })
            }
            "taskgiveitem" | "taskgiveandtakeitem" => Some(TaskRow {
                item_name: Some(self.given_item.clone()?),
                ..row.clone()
            }),
            // A go-to template names no place: it goes to the mission's.
            "taskgotopoitag"
            | "taskexitpoitag"
            | "taskgototerritory"
            | "taskgotopoitagonterritory" => {
                let goes = TaskRow {
                    poi_tag: self.place.clone().or(row.poi_tag.clone()),
                    territory_id: self.territory_override.or(row.territory_id),
                    override_location: self.location.clone().or(row.override_location.clone()),
                    ..row.clone()
                };
                (goes != *original).then_some(goes)
            }
            "patroltask" => Some(TaskRow {
                poi_tag: Some(self.patrol_place.clone()?),
                duration: self
                    .patrol_duration
                    .map(|seconds| seconds as f64)
                    .or(row.duration),
                ..row.clone()
            }),
            "tasktimer" => Some(TaskRow {
                duration: Some(self.timer? as f64),
                ..row.clone()
            }),
            "taskkillcontribution" => Some(TaskRow {
                kill_enemy_type: Some(self.kill.clone()?),
                target_qty: self.kill_count.or(row.target_qty),
                ..row.clone()
            }),
            _ => None,
        }
    }
}

/// The missions of `tables`: the enabled rows with a mission id and a template.
fn mission_rows(tables: &Tables) -> Vec<MissionRow> {
    tables
        .mission_data
        .iter()
        .flat_map(|table| &table.rows)
        .filter(|row| row.enabled != Some(false))
        .filter(|row| !row.mission_id.is_empty() && !row.objective_id.is_empty())
        .map(|row| MissionRow {
            id: row.mission_id.clone(),
            objective: row.objective_id.clone(),
            territory: row
                .available_territory_id
                .as_deref()
                .and_then(|territory| territory.trim().parse().ok())
                .unwrap_or_default(),
            success_event: Some(row.success_game_event_id_override.clone())
                .filter(|event| !event.is_empty()),
            required_faction: cell(row.required_faction.as_ref()),
            items: cell(row.task_have_and_return_items_override.as_ref()),
            items_count: row.task_have_and_return_items_qty_override,
            items_drop_enemy: cell(
                row.task_have_and_return_items_items_drop_vc_override
                    .as_ref(),
            ),
            items_drop_percent: row.task_have_and_return_items_drop_probability_override,
            kill: cell(row.task_kill_contribution_override.as_ref()),
            kill_count: row.task_kill_contribution_qty_override,
            place: cell(row.poi_tag_override.as_ref()),
            chest_percent: row.task_have_and_return_chest_drop_probability_override,
            given_item: cell(row.task_give_and_take_item_override.as_ref()),
            territory_override: row
                .territory_id_override
                .and_then(|territory| u32::try_from(territory).ok())
                .filter(|territory| *territory != 0),
            location: cell(row.override_location.as_ref()),
            patrol_place: cell(row.patrol_poi1.as_ref()),
            patrol_duration: row.patrol_duration,
            timer: row.task_timer_override,
        })
        .collect()
}

/// Every objective and mission by lowercase id and by the CRC-32 of that id, with the task rows
/// to build their trees.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuestBook {
    tasks: TaskBook,
    objectives: HashMap<String, ObjectiveRow>,
    /// The lowercase id of each objective by its CRC-32 (the client names quests so: 3772, 794).
    by_crc: HashMap<u32, String>,
    /// The missions by lowercase `MissionID`.
    missions: HashMap<String, MissionRow>,
    /// The lowercase id of each mission by its CRC-32 (3819).
    mission_by_crc: HashMap<u32, String>,
}

/// A cell that is empty or missing as `None`.
fn cell(value: Option<&String>) -> Option<String> {
    value.filter(|text| !text.is_empty()).cloned()
}

impl QuestBook {
    /// The objectives of `tables` over their task rows `rows` ([`task_rows`]).
    pub fn from_tables(tables: &Tables, rows: Vec<TaskRow>) -> Self {
        Self::build(
            tables
                .objectives
                .iter()
                .flat_map(|table| &table.rows)
                .map(|row| ObjectiveRow {
                    id: row.objective_id.clone(),
                    root: row.task.clone(),
                    reward_item: row.item_reward_name.clone(),
                    reward_quantity: row.item_reward_qty,
                    repeatable: row.repeatable.unwrap_or_default(),
                    requirement: cell(row.required_achievement_id.as_ref()),
                    required_faction: cell(row.required_faction.as_ref()),
                    achievement: cell(row.achievement_id.as_ref()),
                    turn_in: cell(row.npc_destination_id.as_ref()),
                    success_event: cell(row.success_game_event_id.as_ref()),
                    kind: row.r#type.clone(),
                }),
            TaskBook::new(rows),
        )
        .with_missions(mission_rows(tables))
    }

    /// The same book with the missions `missions`; the first row of an id wins.
    pub fn with_missions(mut self, missions: impl IntoIterator<Item = MissionRow>) -> Self {
        for mission in missions {
            let id = mission.id.to_lowercase();
            self.mission_by_crc.insert(lowercase_crc32(&id), id.clone());
            self.missions.entry(id).or_insert(mission);
        }
        self
    }

    /// Mission `id` (any case).
    pub fn mission(&self, id: &str) -> Option<&MissionRow> {
        self.missions.get(&id.to_lowercase())
    }

    /// The id of the mission whose lowercase `MissionID` has the CRC-32 `crc`.
    pub fn mission_id_by_crc(&self, crc: u32) -> Option<&str> {
        let id = self.mission_by_crc.get(&crc)?;
        self.missions.get(id).map(|mission| mission.id.as_str())
    }

    /// The book of `objectives` over `tasks`; the first row of an id wins.
    pub fn build(objectives: impl IntoIterator<Item = ObjectiveRow>, tasks: TaskBook) -> Self {
        let mut by_id = HashMap::new();
        for row in objectives {
            by_id.entry(row.id.to_lowercase()).or_insert(row);
        }
        let by_crc = by_id
            .keys()
            .map(|id| (lowercase_crc32(id), id.clone()))
            .collect();
        Self {
            tasks,
            objectives: by_id,
            by_crc,
            missions: HashMap::new(),
            mission_by_crc: HashMap::new(),
        }
    }

    /// The quest whose lowercase id has the CRC-32 `crc`, as [`QuestBook::quest`].
    pub fn quest_by_crc(&self, crc: u32) -> Option<QuestDef> {
        self.quest(self.by_crc.get(&crc)?)
    }

    /// The quest of objective or mission `id` (any case); `None` when there is no such one or
    /// its root task is not a task row. A mission runs its template's tree with its own values,
    /// under its own id, and gives its own success event.
    pub fn quest(&self, id: &str) -> Option<QuestDef> {
        let lower = id.to_lowercase();
        if let Some(mission) = self.missions.get(&lower) {
            let template = self.objectives.get(&mission.objective.to_lowercase())?;
            let tree = self
                .tasks
                .tree_with(&template.root, |row| mission.override_of(row))?;
            return Some(QuestDef {
                id: mission.id.clone(),
                tree: Arc::new(tree),
                success_event: mission
                    .success_event
                    .clone()
                    .or_else(|| template.success_event.clone()),
                mission: Some(lowercase_crc32(&lower)),
                required_faction: mission
                    .required_faction
                    .clone()
                    .or_else(|| template.required_faction.clone()),
                // A board offers a mission again after its refresh (Inferred: retail boards
                // offer new missions every half hour).
                repeatable: true,
                ..self.def_of(template, self.tasks.tree(&template.root)?)
            });
        }
        let row = self.objectives.get(&lower)?;
        Some(self.def_of(row, self.tasks.tree(&row.root)?))
    }

    /// The quest of objective `row` with its tree `tree`.
    fn def_of(&self, row: &ObjectiveRow, tree: ObjectiveTree) -> QuestDef {
        let reward = row
            .reward_item
            .as_ref()
            .filter(|item| !item.is_empty())
            .map(|item| {
                let quantity = row
                    .reward_quantity
                    .and_then(|quantity| u32::try_from(quantity).ok())
                    .filter(|quantity| *quantity > 0)
                    .unwrap_or(1);
                (item.clone(), quantity)
            });
        QuestDef {
            id: row.id.clone(),
            tree: Arc::new(tree),
            reward,
            repeatable: row.repeatable,
            requirement: row.requirement.clone(),
            required_faction: row.required_faction.clone(),
            achievement: row.achievement.clone(),
            turn_in: row.turn_in.clone(),
            success_event: row.success_event.clone(),
            limit_group: ObjectiveLimitGroup::of_type(&row.kind),
            wire_objective: row.id.clone(),
            mission: None,
        }
    }
}

/// One token of an achievement expression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token<'a> {
    Name(&'a str),
    Not,
    And,
    Or,
    Open,
    Close,
}

/// The tokens of `expression`; `None` when it holds another character.
fn tokens(expression: &str) -> Option<Vec<Token<'_>>> {
    let mut tokens = Vec::new();
    let mut rest = expression.trim_start();
    while let Some(first) = rest.chars().next() {
        let (token, length) = match first {
            '!' => (Token::Not, 1),
            '(' => (Token::Open, 1),
            ')' => (Token::Close, 1),
            '&' if rest.starts_with("&&") => (Token::And, 2),
            '|' if rest.starts_with("||") => (Token::Or, 2),
            name if name.is_ascii_alphanumeric() || name == '_' => {
                let length = rest
                    .find(|next: char| !(next.is_ascii_alphanumeric() || next == '_'))
                    .unwrap_or(rest.len());
                (Token::Name(&rest[..length]), length)
            }
            _ => return None,
        };
        tokens.push(token);
        rest = rest[length..].trim_start();
    }
    Some(tokens)
}

/// Reads an expression of `tokens` from `at`: terms joined by `||`.
fn any_of(tokens: &[Token<'_>], at: &mut usize, has: &dyn Fn(&str) -> bool) -> Option<bool> {
    let mut value = all_of(tokens, at, has)?;
    while tokens.get(*at) == Some(&Token::Or) {
        *at += 1;
        // Every term is read, so a bad one later fails the whole expression.
        value |= all_of(tokens, at, has)?;
    }
    Some(value)
}

/// Reads a term: factors joined by `&&`.
fn all_of(tokens: &[Token<'_>], at: &mut usize, has: &dyn Fn(&str) -> bool) -> Option<bool> {
    let mut value = factor(tokens, at, has)?;
    while tokens.get(*at) == Some(&Token::And) {
        *at += 1;
        value &= factor(tokens, at, has)?;
    }
    Some(value)
}

/// Reads a factor: a name, a `!` factor or a bracketed expression.
fn factor(tokens: &[Token<'_>], at: &mut usize, has: &dyn Fn(&str) -> bool) -> Option<bool> {
    let token = *tokens.get(*at)?;
    *at += 1;
    match token {
        Token::Name(name) => Some(has(name)),
        Token::Not => factor(tokens, at, has).map(|value| !value),
        Token::Open => {
            let value = any_of(tokens, at, has)?;
            (tokens.get(*at) == Some(&Token::Close)).then(|| {
                *at += 1;
                value
            })
        }
        Token::And | Token::Or | Token::Close => None,
    }
}

/// Whether the achievement expression `expression` (`RequiredAchievementID`: names joined by
/// `&&` and `||`, `!` before a name or a bracket) holds for a player who has the achievements
/// that `has` names. An expression this cannot read does not hold.
pub fn requirement_met(expression: &str, has: impl Fn(&str) -> bool) -> bool {
    let Some(tokens) = tokens(expression) else {
        return false;
    };
    if tokens.is_empty() {
        return true;
    }
    let mut at = 0;
    any_of(&tokens, &mut at, &has).is_some_and(|value| value && at == tokens.len())
}

/// What a client needs to show one mob prefab (mob-cards note, section 2).
#[derive(Debug, Clone, PartialEq)]
pub struct MobLook {
    /// The creature slice asset of its spawn card.
    pub asset: AssetId,
    /// The component of its `ALCReplicatedState`.
    pub alc_component: u64,
    /// The component of its vitals state.
    pub vitals_component: u64,
    /// Its retail first `ALCReplicatedState`, moved to each spawn point; `None` when the table
    /// has none.
    pub alc: Option<AlcState>,
    /// Its retail first vitals state, with each mob's health written in; `None` when the table
    /// has none.
    pub vitals: Option<VitalsState>,
    /// Its other states when it comes into view.
    pub first: Vec<BundleMember>,
    /// Its other states when it dies.
    pub death: Vec<BundleMember>,
    /// The paperdoll states that draw its weapon right after a fresh spawn (retail sailor 1:
    /// with its first states, 102 to 136 ms after its card).
    pub drawn: Vec<BundleMember>,
}

/// The look of every mob prefab, by vitals id and slice asset.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MobLooks {
    by_key: HashMap<PrefabKey, MobLook>,
    /// The asset of each vitals id's most seen prefab.
    default_asset: HashMap<u32, [u8; ASSET_ID_LEN]>,
}

impl MobLooks {
    /// The looks of `prefabs`. A state whose body does not decode is left out of its prefab;
    /// the second value counts them.
    pub fn of(prefabs: &MobPrefabs) -> (Self, usize) {
        let mut skipped = 0;
        let mut members = |rows: &[PrefabMember]| -> Vec<BundleMember> {
            rows.iter()
                .filter_map(|row| match MemberState::decode(row.class, &row.body) {
                    Ok(state) => Some(BundleMember {
                        component: row.component,
                        state,
                    }),
                    Err(_) => {
                        skipped += 1;
                        None
                    }
                })
                .collect()
        };
        let by_key = prefabs
            .iter()
            .map(|(key, prefab)| {
                // The ALC and vitals states of the first group are taken apart: each mob gets
                // them with its own place and health.
                let mut alc = None;
                let mut vitals = None;
                let mut first = Vec::new();
                for member in members(&prefab.first_members) {
                    match member.state {
                        MemberState::Alc(state) => alc = Some(state),
                        MemberState::Vitals(state) => vitals = Some(state),
                        state => first.push(BundleMember {
                            component: member.component,
                            state,
                        }),
                    }
                }
                let look = MobLook {
                    asset: AssetId {
                        id: prefab.asset,
                        sub_id: prefab.sub_id,
                    },
                    alc_component: prefab.alc_component,
                    vitals_component: prefab.vitals_component,
                    alc,
                    vitals,
                    first,
                    death: members(&prefab.death_members),
                    drawn: members(&prefab.drawn_members),
                };
                (key, look)
            })
            .collect();
        let looks = Self {
            by_key,
            default_asset: prefabs.default_assets().collect(),
        };
        (looks, skipped)
    }

    /// The look of vitals id `vitals_id` with slice asset `asset`; without an asset, or when the
    /// table has no prefab of it, the vitals id's most seen look.
    pub fn look(&self, vitals_id: u32, asset: Option<[u8; ASSET_ID_LEN]>) -> Option<&MobLook> {
        asset
            .and_then(|asset| self.by_key.get(&(vitals_id, asset)))
            .or_else(|| {
                let asset = self.default_asset.get(&vitals_id)?;
                self.by_key.get(&(vitals_id, *asset))
            })
    }

    /// Prefabs with a look.
    pub fn len(&self) -> usize {
        self.by_key.len()
    }

    /// Whether there is no look.
    pub fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }
}

/// The player content of one content bundle and mob prefab table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerContent {
    /// The login kit of each archetype pick.
    pub kits: StarterKits,
    /// The bit of each achievement.
    pub achievements: AchievementIndexes,
    /// The look of each mob prefab.
    pub mob_looks: MobLooks,
    /// The animation names of each creature slice.
    pub creature_animations: CreatureAnimations,
    /// The achievement each archetype pick unlocks: (backstory id, achievement id), from
    /// `BackstoryDefinition.AchievementUnlockOverride`.
    pub archetype_unlocks: Vec<(String, String)>,
    /// The vitals id and categories of every mob type, for quest kill tasks.
    pub kill_types: KillTypes,
    /// The task tree of the tutorial quest; `None` when the content lacks it.
    pub tutorial_quest: Option<Arc<ObjectiveTree>>,
    /// Every quest; the first open-world quest is one of them.
    pub quests: QuestBook,
    /// The tutorial level's entities: its static entities, its asset entities and the retail
    /// causes of their groups.
    pub tutorial_level: FtueLevel,
    /// The gatherables.
    pub gatherables: Gatherables,
    /// The crafting recipes.
    pub recipes: Recipes,
    /// The reward of each game event, for quest completions.
    pub rewards: Rewards,
    /// Every loot table and bucket, for gathering and mob kills.
    pub loot: LootTables,
    /// What each mob type drops when killed.
    pub mob_loot: MobLoot,
    /// The loot table of each loot crate of the level.
    pub crates: CrateLoot,
    /// The interact tags of the level's objects, for `TaskInteract`.
    pub interact_tags: InteractTags,
    /// The level's quest trigger volumes, for `TaskTriggerArea` (their `m_targetTag` is a place
    /// the player is in while inside).
    pub quest_volumes: Vec<QuestVolume>,
    /// The level's zone capital plots: each one's place and raw id, the provider a mission board
    /// in its zone names.
    pub zone_plots: Vec<([f32; 3], [u8; MISSION_BOARD_ID_LEN])>,
    /// The level's crafting stations.
    pub stations: Vec<CraftingStation>,
    /// The resources each structure blueprint costs, for the stations' committed resources.
    pub structure_costs: StructureCosts,
    /// The camp blueprints and skins.
    pub camps: CampRules,
    /// The camp slices, for a placed camp's spawn group; empty until the sim loads the camp
    /// slice file.
    pub camp_slices: CampSlices,
    /// The quick course rules.
    pub quick_courses: QuickCourseRules,
    /// The quick course paths; empty until the sim loads the quick course file.
    pub quick_course_paths: QuickCoursePaths,
    /// The loot value of a player's level by its experience.
    pub player_loot_levels: PlayerLootLevels,
    /// The point-of-interest tags of each territory, for go-to and exit tasks.
    pub poi_tags: PoiTags,
    /// The level's quest objects that the player breaks with its weapons.
    pub quest_objects: QuestObjects,
    /// The leaderboards each stat feeds.
    pub leaderboards: LeaderboardStats,
    /// The gameplay values of the remote config layers (`crate::gameplay`).
    pub gameplay: GameplayConfig,
    /// The experience of a kill by the mob's level, before the multipliers.
    pub kill_experience: KillExperience,
    /// The creature type and level gap factors of a kill and the weapon mastery of each
    /// weapon item.
    pub kill_rewards: KillRewards,
    /// The meta achievements of the achievements window.
    pub meta_achievements: MetaAchievementRules,
    /// The lowercase ids of the items that `javelin.disabled-item-classes` disables.
    pub disabled_items: HashSet<String>,
    /// The loot limits and their resets; `None` applies no limit.
    pub loot_limits: Option<LootLimitRules>,
    /// The level table and the weapon masteries.
    pub progression: Progression,
    /// The player titles and what earns each.
    pub titles: TitleRules,
    /// The solids of the tutorial's asset entities; empty until the sim sets them from the
    /// level collision.
    pub asset_solids: AssetSolids,
    /// The fishing rules and the level's fishing hotspots.
    pub fishing: FishingRules,
    /// The musical performance rules: songs, sheets, rewards and grades.
    pub music: MusicRules,
    /// The item weights and the player's equip load limits.
    pub equip_loads: EquipLoads,
    /// The status effects worn perks hold ([`nw_content::perk_effects`]).
    pub perk_effects: PerkEffects,
    /// The abilities attribute values hold and their mana and cooldown shares
    /// ([`nw_content::attribute_bonuses`]).
    pub attribute_bonuses: AttributeBonuses,
    /// When a player goes down to death's door first ([`nw_content::deaths_door`]).
    pub deaths_door: DeathsDoorRules,
    /// The mounts, their types, movement rows and dyes.
    pub mounts: MountRules,
    /// The landmarks, discoverable territories, spawn points and timings of travel.
    pub travel: TravelRules,
    /// The chartable areas, the territory of each area and what a chart and a standing rank
    /// give.
    pub charting: ChartRules,
    /// The level's phasing areas ([`phasing_areas`]).
    pub phasing_areas: Vec<u32>,
    /// The categorical progression and coins of each game event, for the tradeskill experience
    /// of gathers and crafts and the coins of salvages.
    pub event_grants: EventGrants,
    /// The gather level modifiers and craft roll bonuses of each tradeskill's rank table.
    pub tradeskill_curves: TradeskillCurves,
    /// The `BonusItemChance` of each crafting recipe.
    pub bonus_chances: BonusChances,
    /// The categorical progression ranks a new character of each archetype pick starts with.
    pub starting_ranks: StartingRanks,
    /// How long each loot container stays locked for the player who opened it.
    pub chest_lockouts: ChestLockouts,
    /// The tradeskill ranks and items that gatherables ask for.
    pub gather_rules: GatherRules,
    /// The cyclic states each gatherable shows.
    pub cyclic_gatherables: CyclicGatherables,
    /// The gathering efficiency of each tool item.
    pub tools: ToolEfficiencies,
    /// What the salvage of each item gives.
    pub salvage: Salvage,
    /// What using and dropping each item does.
    pub item_rules: ItemRules,
    /// The rules of every status effect and the effects attack rows put on their target.
    pub status_effects: StatusEffectRules,
    /// Each item's gear score range, perks, durability and repair data, and the perk table.
    pub item_rolls: ItemRolls,
    /// The skin each item gives to the transmog collection and the free skins.
    pub skins: SkinCatalog,
    /// The dye colors and the dye items.
    pub dyes: DyeCatalog,
    /// The gear score rules of dropped items by player and mob level.
    pub drop_rolls: DropRolls,
    /// The `Name` text key of each item (the charm notification's `itemname`).
    pub item_names: ItemNames,
    /// The cost of each gear score upgrade level (3008).
    pub gear_score_upgrades: GearScoreUpgrades,
    /// The items the trading post takes, with their trading fields.
    pub trading: TradingCatalog,
    /// The trading post's durations, limits, fees and taxes (`[trading_post]` of the config).
    pub trading_post: TradingPostConfig,
    /// The level's settlements and outposts, which trading post orders and storages name.
    pub settlements: Settlements,
    /// Test worlds only: the tools a new character wears at its first spawn (`start_tools` of
    /// the config).
    pub start_tools: Vec<crate::config::StartTool>,
    /// What the shops sell (faction, camp, stable and event shops).
    pub shops: ShopCatalog,
    /// The generic invite activities (trade invites).
    pub invites: InviteRules,
    /// The expeditions, raids and trials.
    pub instances: InstanceCatalog,
    /// The weekly expedition mutations and what each difficulty does.
    pub mutations: MutationCatalog,
    /// The expedition entrances of the level: the slice name of each entrance capital, by its
    /// entity id (the facet entity of an `OpenDungeonMenu` or `EnterDungeon` interaction).
    pub entrances: HashMap<u64, String>,
    /// The encounters and loot containers of each instance game mode's level script
    /// (`dungeon-scripts-<build>.json`).
    pub dungeon_scripts: DungeonScripts,
}

/// The lower-case start of an expedition entrance's slice name
/// (`Dungeon/Entrance/DungeonEntrance_Amrine`, `DungeonEntranceTrial_Season_01`).
pub const ENTRANCE_PREFIX: &str = "dungeonentrance";

/// The expedition entrances among the capitals of `level`: each one's slice name by its entity
/// id (`capital_id`).
pub fn dungeon_entrances(level: &WorldFile) -> HashMap<u64, String> {
    level
        .capitals
        .iter()
        .filter(|capital| {
            capital
                .slice_name
                .rsplit('/')
                .next()
                .is_some_and(|name| name.to_lowercase().starts_with(ENTRANCE_PREFIX))
        })
        .filter_map(|capital| Some((capital_id(&capital.id)?, capital.slice_name.clone())))
        .collect()
}

/// The phasing areas of `tables`: the `TerritoryID` of each row with a `PhasingBoundsMargin` (10
/// rows in build 25810808: the start beaches 9009 and 9011, the settlements 30155, 40186 and
/// 20333, 30112, 40102, 9196, 10330 and 30082). Retail's area phasing entries named only these
/// (40102, 9009, 30112, 40186, 9011 and 30155 over Monarch's Bluffs, Windsward and Everfall).
pub fn phasing_areas(tables: &Tables) -> Vec<u32> {
    tables
        .territory_definition
        .iter()
        .flat_map(|table| &table.rows)
        .filter(|row| {
            row.phasing_bounds_margin
                .as_deref()
                .is_some_and(|margin| !margin.is_empty())
        })
        .filter_map(|row| u32::try_from(row.territory_id).ok())
        .collect()
}

/// The smallest side of a detection square, in metres.
const MIN_DETECTION_CELL_M: f32 = 1.0;

/// The detection square of side `cell_m` that holds `at`.
fn detection_cell(at: Position, cell_m: f32) -> (i32, i32) {
    // Level places are a few thousand metres: the squares fit an i32.
    (
        (at.x / cell_m).floor() as i32,
        (at.y / cell_m).floor() as i32,
    )
}

/// A quest object of the level: a gatherable point whose gather is a game event that a quest's
/// `TaskGameEvent` counts (Grace's corruption nodules, `MSQ_CorruptedNodule_1_interact`). Retail
/// 2026-09-18 broke them with weapon hits, with no interaction from the client.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestObject {
    /// The game event of its gatherable.
    pub event: String,
    /// Where it stands (the level's gatherable point).
    pub position: Position,
    /// How long it stays broken: its gatherable's `MinRespawnRate`.
    pub respawn_ms: u64,
    /// The GDE id of the level capital that stands on its point, when one does: the client knows
    /// the object by it (Observed: retail newchar 2026-09-18 carded Grace's four nodules, the
    /// capitals `IO_Custom_MSQ_CorruptedNodule_A` to `_D` on their points, by these ids).
    pub id: Option<u64>,
    /// The lowercase CRC-32 of its gatherable's `FinalLootTable`: the interact tag of its broken
    /// entry in the player's loot tracker (Observed: `io_msq_grace_corruptednodule_1`,
    /// `0x8d40_a880`, on the retail nodules' entries).
    pub loot_tag: Option<u32>,
    /// The radius of its gatherable's detection sphere ([`DetectionSpheres`]), in metres: a
    /// player inside it sets off its game event.
    pub detection_m: Option<f32>,
}

/// The slices of the zone capital plots (`Darkness/Zone_CapitalPlot_SouthWindsward`), one per
/// zone.
const ZONE_PLOT_SLICE: &str = "zone_capitalplot";

/// The zone capital plots of `level`: each one's place and raw id (its capital key's bytes, low
/// byte first, then zeros). Observed: both retail board captures (2026-06-13, the Windsward town
/// board and faction representative) named `Zone_CapitalPlot_SouthWindsward` as the provider.
fn zone_plots(level: &WorldFile) -> Vec<([f32; 3], [u8; MISSION_BOARD_ID_LEN])> {
    level
        .capitals
        .iter()
        .filter(|capital| capital.slice_name.to_lowercase().contains(ZONE_PLOT_SLICE))
        .filter_map(|capital| {
            let mut id = [0; MISSION_BOARD_ID_LEN];
            let key = capital_id(&capital.id)?.to_le_bytes();
            id[..key.len()].copy_from_slice(&key);
            Some((capital.position, id))
        })
        .collect()
}

/// The interact tags of `level`'s capitals and spawned slices: their slices' tags and their
/// gatherable variants' `InteractTag` (`tables`), and those of each gatherable's variants for the
/// gathers of region points.
fn level_interact_tags(tables: &Tables, level: &WorldFile) -> InteractTags {
    let mut capital_tags = HashMap::new();
    let mut spawned_tags = HashMap::new();
    for tagged in &level.interact_tags {
        match &tagged.owner {
            TagOwner::Capital(guid) => {
                capital_tags.insert(guid.as_str(), tagged.tags.as_slice());
            }
            TagOwner::Spawned(id) => {
                spawned_tags.insert(*id, tagged.tags.as_slice());
            }
        }
    }
    InteractTags::build(
        &variant_tags(tables),
        level.capitals.iter().map(|capital| {
            (
                capital.id.as_str(),
                capital.variant_name.as_deref(),
                capital_tags
                    .get(capital.id.as_str())
                    .copied()
                    .unwrap_or(&[]),
            )
        }),
        level.spawned.iter().map(|slice| {
            (
                slice.id,
                slice.variant.as_str(),
                spawned_tags.get(&slice.id).copied().unwrap_or(&[]),
            )
        }),
    )
    .with_gatherables(gatherable_tags(tables))
}

/// The largest difference on each axis between a quest object's point and the capital on it, m
/// (Observed: 0 on the four nodules; a float's rounding).
const CAPITAL_ON_POINT_M: f32 = 0.01;
/// The folder of the generated spawn masters (`CoatGen\\<hash>\\WorldEvent_SpawnGroup_…_Master_…`,
/// `Master_Plants_…`). Such a capital spawns the object on its point as an entity of its own; it
/// is a spawner, not the object (Observed: the tutorial's spawn group at (484.5, 218.9); carding
/// it by its id crashed the client in its spawner facet, `FUN_143c13cd0`, live 2026-09-29
/// twice at 99.7 m. The nodules' capitals are placed slices, `IO_Custom_MSQ_CorruptedNodule_A`).
const SPAWN_MASTER_FOLDER: &str = "CoatGen\\";

/// The quest objects of a level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuestObjects {
    objects: Vec<QuestObject>,
    /// The objects with a detection sphere, by their square of [`Self::detection_cell_m`].
    detecting: HashMap<(i32, i32), Vec<usize>>,
    /// The side of a detection square: the largest sphere radius, so a sphere reaches no farther
    /// than the squares next to its own.
    detection_cell_m: f32,
}

impl QuestObjects {
    /// The quest objects among the level's gatherable `points`: those of a gatherable in
    /// `gatherables` whose game event is one of `counted` (lowercase ids), each with the GDE id
    /// of the capital of `capitals` on its point and its sphere of `spheres`.
    pub fn build(
        points: &[GatherablePoint],
        gatherables: &Gatherables,
        spheres: &DetectionSpheres,
        counted: &HashSet<String>,
        capitals: &[Capital],
    ) -> Self {
        let objects: Vec<QuestObject> = points
            .iter()
            .filter_map(|point| {
                let gatherable = gatherables.get(point.gatherable_id)?;
                let event = gatherable.game_event.clone()?;
                counted.contains(&event.to_lowercase()).then(|| {
                    let [x, y, z] = point.position;
                    let position = Position { x, y, z };
                    let id = capitals
                        .iter()
                        .find(|capital| {
                            !capital.slice_name.starts_with(SPAWN_MASTER_FOLDER)
                                && capital
                                    .position
                                    .iter()
                                    .zip(point.position)
                                    .all(|(a, b)| (a - b).abs() <= CAPITAL_ON_POINT_M)
                        })
                        .and_then(|capital| capital_id(&capital.id));
                    QuestObject {
                        event,
                        position,
                        respawn_ms: gatherable.respawn_ms.0,
                        id,
                        loot_tag: gatherable.loot_table.as_deref().map(lowercase_crc32),
                        detection_m: spheres.radius_of(point.gatherable_id),
                    }
                })
            })
            .collect();
        let detection_cell_m = objects
            .iter()
            .filter_map(|object| object.detection_m)
            .fold(MIN_DETECTION_CELL_M, f32::max);
        let mut detecting: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (index, object) in objects.iter().enumerate() {
            if object.detection_m.is_some() {
                detecting
                    .entry(detection_cell(object.position, detection_cell_m))
                    .or_default()
                    .push(index);
            }
        }
        Self {
            objects,
            detecting,
            detection_cell_m,
        }
    }

    /// The indexes of the objects whose detection sphere holds `at`.
    pub fn detected_at(&self, at: Position) -> Vec<usize> {
        if self.detecting.is_empty() {
            return Vec::new();
        }
        let (east, north) = detection_cell(at, self.detection_cell_m);
        (east - 1..=east + 1)
            .flat_map(|cell_east| {
                (north - 1..=north + 1).map(move |cell_north| (cell_east, cell_north))
            })
            .filter_map(|cell| self.detecting.get(&cell))
            .flatten()
            .copied()
            .filter(|index| {
                self.objects[*index].detection_m.is_some_and(|radius_m| {
                    let object = self.objects[*index].position;
                    let (east, north, up) = (at.x - object.x, at.y - object.y, at.z - object.z);
                    east * east + north * north + up * up <= radius_m * radius_m
                })
            })
            .collect()
    }

    /// The nearest object within `reach_m` of `at` that `wanted` takes, with its index.
    pub fn nearest(
        &self,
        at: Position,
        reach_m: f32,
        wanted: impl Fn(usize, &QuestObject) -> bool,
    ) -> Option<(usize, &QuestObject)> {
        self.objects
            .iter()
            .enumerate()
            .map(|(index, object)| (index, object, object.position.horizontal_distance(at)))
            .filter(|(index, object, distance)| *distance <= reach_m && wanted(*index, object))
            .min_by(|(_, _, a), (_, _, b)| a.total_cmp(b))
            .map(|(index, object, _)| (index, object))
    }

    /// The object of index `index`.
    pub fn get(&self, index: usize) -> Option<&QuestObject> {
        self.objects.get(index)
    }

    /// The indexes of the objects with a GDE id within `radius_m` of `at`.
    pub fn known_near(&self, at: Position, radius_m: f32) -> impl Iterator<Item = usize> + '_ {
        self.objects
            .iter()
            .enumerate()
            .filter(move |(_, object)| {
                object.id.is_some() && object.position.horizontal_distance(at) <= radius_m
            })
            .map(|(index, _)| index)
    }

    /// Objects in the level.
    pub fn len(&self) -> usize {
        self.objects.len()
    }

    /// Whether the level has none.
    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }
}

impl PlayerContent {
    /// The player content of `tables` (with the datasheet overrides of `gameplay` applied)
    /// with the mob looks `mob_looks`, the creature animations `creature_animations`, the
    /// tutorial level's entities `tutorial_level`, the gatherable points and capitals of the
    /// sim's level `level`, the gameplay values `gameplay` and the loot limit resets
    /// `loot_reset`.
    pub fn new(
        tables: &Tables,
        mob_looks: MobLooks,
        creature_animations: CreatureAnimations,
        tutorial_level: FtueLevel,
        level: &WorldFile,
        gameplay: GameplayConfig,
        loot_reset: ResetClock,
    ) -> Self {
        let level_gatherables = &level.gatherables;
        let rows = task_rows(tables);
        let gatherables = Gatherables::from_tables(tables);
        let quest_objects = QuestObjects::build(
            level_gatherables,
            &gatherables,
            &DetectionSpheres::from_tables(tables),
            &counted_game_events(&rows),
            &level.capitals,
        );
        let kits = StarterKits::from_tables(tables);
        let travel = TravelRules::of(tables, level, &gameplay, &kits);
        let charting = ChartRules::of(tables, level);
        Self {
            kits,
            achievements: AchievementIndexes::from_tables(tables),
            mob_looks,
            creature_animations,
            archetype_unlocks: tables
                .backstory_definition
                .iter()
                .flat_map(|table| &table.rows)
                .filter_map(|row| {
                    let unlock = row.achievement_unlock_override.clone()?;
                    Some((row.backstory_id.clone(), unlock))
                })
                .collect(),
            kill_types: KillTypes::from_tables(tables),
            tutorial_quest: ObjectiveTree::build(&rows, TUTORIAL_QUEST_ROOT).map(Arc::new),
            quests: QuestBook::from_tables(tables, rows),
            tutorial_level,
            gatherables,
            recipes: Recipes::from_tables(tables),
            rewards: Rewards::from_tables(tables),
            loot: LootTables::from_tables(tables),
            mob_loot: MobLoot::from_tables(tables),
            crates: CrateLoot::build(
                CrateVariant::from_tables(tables),
                level.capitals.iter().filter_map(|capital| {
                    Some((capital.id.as_str(), capital.variant_name.as_deref()?))
                }),
                level
                    .spawned
                    .iter()
                    .map(|slice| (slice.id, slice.variant.as_str())),
            ),
            interact_tags: level_interact_tags(tables, level),
            quest_volumes: Vec::new(),
            zone_plots: zone_plots(level),
            stations: level.stations.clone(),
            structure_costs: StructureCosts::from_tables(tables),
            camps: CampRules::from_tables(tables),
            camp_slices: CampSlices::default(),
            quick_courses: QuickCourseRules::from_tables(tables),
            quick_course_paths: QuickCoursePaths::default(),
            player_loot_levels: PlayerLootLevels::from_tables(tables),
            poi_tags: PoiTags::from_tables(tables),
            quest_objects,
            leaderboards: LeaderboardStats::from_tables(tables),
            kill_experience: KillExperience::from_tables(tables),
            kill_rewards: KillRewards::from_tables(tables),
            meta_achievements: MetaAchievementRules::from_tables(tables),
            disabled_items: disabled_items(
                tables
                    .master_item_definitions
                    .iter()
                    .flat_map(|table| &table.rows)
                    .map(|row| (row.item_id.as_str(), row.item_class.as_deref())),
                &gameplay,
            ),
            loot_limits: Some(LootLimitRules {
                limits: LootLimits::from_tables(tables),
                clock: loot_reset,
            }),
            gameplay,
            progression: Progression::of_tables(tables),
            titles: TitleRules::from_tables(tables),
            equip_loads: EquipLoads::from_tables(tables),
            perk_effects: PerkEffects::from_tables(tables),
            attribute_bonuses: AttributeBonuses::from_tables(tables),
            deaths_door: DeathsDoorRules::from_tables(tables),
            mounts: MountRules::from_tables(tables),
            item_rules: ItemRules::from_tables(tables),
            status_effects: StatusEffectRules::from_tables(tables),
            item_rolls: ItemRolls::from_tables(tables),
            skins: SkinCatalog::from_tables(tables),
            dyes: DyeCatalog::from_tables(tables),
            drop_rolls: DropRolls::from_tables(tables),
            item_names: ItemNames::from_tables(tables),
            gear_score_upgrades: GearScoreUpgrades::from_tables(tables),
            trading: TradingCatalog::from_tables(tables),
            trading_post: TradingPostConfig::default(),
            start_tools: Vec::new(),
            settlements: Settlements::from_capitals(&level.capitals),
            shops: ShopCatalog::from_tables(tables),
            invites: InviteRules::from_tables(tables),
            travel,
            charting,
            phasing_areas: phasing_areas(tables),
            event_grants: EventGrants::from_tables(tables),
            tradeskill_curves: TradeskillCurves::from_tables(tables),
            bonus_chances: BonusChances::from_tables(tables),
            starting_ranks: StartingRanks::from_tables(tables),
            chest_lockouts: ChestLockouts::from_tables(tables),
            gather_rules: GatherRules::from_tables(tables),
            cyclic_gatherables: CyclicGatherables::from_tables(tables),
            tools: ToolEfficiencies::from_tables(tables),
            salvage: Salvage::from_tables(tables),
            asset_solids: AssetSolids::default(),
            fishing: FishingRules::from_tables(tables).with_hotspots(
                level
                    .spawned
                    .iter()
                    .map(|slice| (slice.slice.as_str(), slice.position)),
            ),
            music: MusicRules::from_tables(tables),
            instances: InstanceCatalog::from_tables(tables),
            mutations: MutationCatalog::from_tables(tables),
            entrances: dungeon_entrances(level),
            dungeon_scripts: DungeonScripts::default(),
        }
    }

    /// The equip load category of a character wearing `items`; `None` without the player's
    /// limits.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "equip loads and limits of a few hundred"
    )]
    pub fn equip_load<'a>(&self, items: impl IntoIterator<Item = &'a str>) -> Option<EquipLoad> {
        let ratios = self.equip_loads.limits?;
        let limits = EquipLoadLimits {
            base_max: ratios.base_max as f32,
            normal_percent: ratios.normal_percent as f32,
            slow_percent: ratios.slow_percent as f32,
            overburdened_percent: ratios.overburdened_percent as f32,
        };
        Some(limits.category(self.equip_loads.load(items) as f32))
    }

    /// Whether item `item_id` (any case) is disabled.
    pub fn is_item_disabled(&self, item_id: &str) -> bool {
        self.disabled_items.contains(&item_id.to_lowercase())
    }

    /// Whether item `item_id` (any case) is of UI item class number `class` (the by-class item
    /// calls 3938, 2946, 2945; `nw_content::ui_item_class`).
    pub fn is_of_ui_class(&self, item_id: &str, class: u64) -> bool {
        self.item_rolls.item(item_id).is_some_and(|row| {
            nw_content::ui_item_class::is_of_ui_class(row.ui_item_class.as_deref(), class)
        })
    }
}

/// The lowercase ids of the master `items` (`ItemID`, `ItemClass`) whose class list holds a
/// class that `gameplay` disables (client check `FUN_14443ea80`, build 25810808: an item is
/// disabled when one of its classes is in `javelin.disabled-item-classes`).
fn disabled_items<'a>(
    items: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    gameplay: &GameplayConfig,
) -> HashSet<String> {
    items
        .into_iter()
        .filter(|(_, class)| class.is_some_and(|class| gameplay.is_class_disabled(class)))
        .map(|(item_id, _)| item_id.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs the build 25810808 content bundle in NW_CONTENT_DIR"]
    fn the_real_phasing_areas_are_the_rows_with_a_phasing_margin() {
        const BUILD: &str = "25810808";
        let dir = std::env::var("NW_CONTENT_DIR").expect("NW_CONTENT_DIR");
        let path = nw_content::bundle::bundle_path(std::path::Path::new(&dir), BUILD);
        let bundle = nw_content::ContentBundle::load(&path, BUILD).expect("bundle");
        let mut areas = phasing_areas(&bundle.tables);
        areas.sort_unstable();
        assert_eq!(
            areas,
            [
                9_009, 9_011, 9_196, 10_330, 20_333, 30_082, 30_112, 30_155, 40_102, 40_186
            ]
        );
    }

    #[test]
    fn a_mission_fills_its_go_to_patrol_timer_and_transport_templates() {
        // Template rows `M_task_goto_POITag`, `M_task_goto_territoryID`, `PVP_task_patrol`,
        // `M_task_give_transport_item` and a timer.
        let mission = MissionRow {
            place: Some("Windsward_Settlement_01".to_owned()),
            territory_override: Some(12),
            patrol_place: Some("Fort_A".to_owned()),
            patrol_duration: Some(60),
            timer: Some(90),
            given_item: Some("WorkOrder_T1".to_owned()),
            ..MissionRow::default()
        };
        let row = |kind: &str| TaskRow {
            task_id: kind.to_owned(),
            kind: kind.to_owned(),
            ..TaskRow::default()
        };
        let goes = mission.override_of(&row("TaskGoToPOITag")).expect("go-to");
        assert_eq!(goes.poi_tag.as_deref(), Some("Windsward_Settlement_01"));
        let territory = mission
            .override_of(&row("TaskGoToTerritory"))
            .expect("territory");
        assert_eq!(territory.territory_id, Some(12));
        let patrol = mission.override_of(&row("PatrolTask")).expect("patrol");
        assert_eq!(
            (patrol.poi_tag.as_deref(), patrol.duration),
            (Some("Fort_A"), Some(60.0))
        );
        let timer = mission.override_of(&row("TaskTimer")).expect("timer");
        assert_eq!(timer.duration, Some(90.0));
        let carried = mission
            .override_of(&row("TaskGiveAndTakeItem"))
            .expect("transport");
        assert_eq!(carried.item_name.as_deref(), Some("WorkOrder_T1"));
        assert_eq!(
            MissionRow::default().override_of(&row("TaskGoToPOITag")),
            None,
            "a mission without a place changes nothing"
        );
    }

    #[test]
    fn a_quest_object_takes_the_id_of_a_placed_capital_not_of_a_spawn_master() {
        const NODULE: &str = "IO_MSQ_Nodule";
        const EVENT: &str = "MSQ_CorruptedNodule_1_interact";
        const PLACED: &str = "aaaaaaaa-0000-0000-0000-000000000001";
        const MASTER: &str = "bbbbbbbb-0000-0000-0000-000000000002";
        let gatherables = Gatherables::build([nw_content::gathering::Gatherable {
            id: NODULE.to_owned(),
            gather_ms: 0,
            respawn_ms: (0, 0),
            loot_table: None,
            loot_container: false,
            tradeskill: None,
            tool_required: false,
            by_hand: true,
            game_event: Some(EVENT.to_owned()),
        }]);
        let point = |x| GatherablePoint {
            gatherable_id: lowercase_crc32(NODULE),
            position: [x, 0.0, 0.0],
            ..GatherablePoint::default()
        };
        let capital = |id: &str, slice: &str, x| Capital {
            id: id.to_owned(),
            slice_name: slice.to_owned(),
            position: [x, 0.0, 0.0],
            ..Capital::default()
        };
        let objects = QuestObjects::build(
            &[point(0.0), point(10.0)],
            &gatherables,
            &DetectionSpheres::default(),
            &HashSet::from([EVENT.to_lowercase()]),
            &[
                capital(PLACED, "slices/IO_Custom_MSQ_CorruptedNodule_A", 0.0),
                capital(
                    MASTER,
                    "CoatGen\\831a57a1\\WorldEvent_SpawnGroup_Single_Master_bbbbbbbb",
                    10.0,
                ),
            ],
        );
        assert_eq!(
            objects.get(0).and_then(|object| object.id),
            capital_id(PLACED)
        );
        assert_eq!(
            objects.get(1).and_then(|object| object.id),
            None,
            "a spawn master is not the object"
        );
    }

    #[test]
    fn the_tutorial_swordsman_s_gear_is_a_slow_equip_load() {
        // The player row of the retail encumbrance limits, the Swordsman kit's tutorial armor
        // overrides and sword: 304.5 of 500, slow; retail's newchar dodges fit the slow clips.
        let content = PlayerContent {
            equip_loads: EquipLoads::new(
                [
                    (
                        "SwordsmanArchetypeChest",
                        Some(120.0),
                        Some("HeavyChest_Balanced"),
                    ),
                    (
                        "SwordsmanArchetypeHands",
                        Some(60.0),
                        Some("HeavyHands_Balanced"),
                    ),
                    (
                        "SwordsmanArchetypeLegs",
                        Some(80.0),
                        Some("HeavyLegs_Balanced"),
                    ),
                    (
                        "SwordsmanArchetypeFeet",
                        Some(60.0),
                        Some("HeavyFeet_Balanced"),
                    ),
                    ("1hSwordT2_FTUE", Some(36.0), Some("1hSwordT2_FTUE")),
                ],
                [
                    ("HeavyChest_Balanced", Some(110.555_807_775)),
                    ("HeavyHands_Balanced", Some(47.381_060_475)),
                    ("HeavyLegs_Balanced", Some(63.174_747_3)),
                    ("HeavyFeet_Balanced", Some(47.381_060_475)),
                ],
                Some(nw_content::equip_load::EquipLoadRatios {
                    base_max: 500.0,
                    normal_percent: 26.0,
                    slow_percent: 46.0,
                    overburdened_percent: 100.0,
                }),
            ),
            ..PlayerContent::default()
        };
        let worn = [
            "SwordsmanArchetypeChest",
            "SwordsmanArchetypeHands",
            "SwordsmanArchetypeLegs",
            "SwordsmanArchetypeFeet",
            "1hSwordT2_FTUE",
        ];
        assert_eq!(content.equip_load(worn), Some(EquipLoad::Slow));
        assert_eq!(
            content.equip_load(["1hSwordT2_FTUE"]),
            Some(EquipLoad::Fast)
        );
        assert_eq!(PlayerContent::default().equip_load(worn), None);
    }

    #[test]
    fn the_items_of_a_disabled_class_are_disabled() {
        let gameplay = GameplayConfig {
            disabled_item_classes: vec!["Dagger".to_owned()],
            ..GameplayConfig::default()
        };
        let content = PlayerContent {
            disabled_items: disabled_items(
                [
                    (
                        "1hDaggerDropT2",
                        Some("Weapon+EquippableMainHand+Dagger+Melee"),
                    ),
                    (
                        "1hSwordT2_FTUE",
                        Some("Weapon+EquippableMainHand+Sword+Melee"),
                    ),
                    ("NoClass", None),
                ],
                &gameplay,
            ),
            ..PlayerContent::default()
        };
        assert!(content.is_item_disabled("1hdaggerdropt2"));
        assert!(!content.is_item_disabled("1hSwordT2_FTUE"));
        assert!(!content.is_item_disabled("NoClass"));
    }

    const SAILOR: u32 = 2_288_616_137;

    #[test]
    fn a_requirement_reads_the_retail_expressions() {
        // Retail RequiredAchievementID cells (beach-quests note).
        const GET_OFF_BEACH: &str = "!S_introtosettlement && !Watched_IGC_Aelstrom_WW";
        const BOAR_FEAST: &str = "S_skinningknife_mb && !Archetype_Mercenary && \
            !Archetype_Warmonger && !Archetype_Tracker && !Archetype_Musketeer && \
            !Archetype_Occultist && !Archetype_Theurgist && !Archetype_Wanderer";
        const BOAR_FEAST_MYSTIC: &str = "S_skinningknife_mb && Archetype_Theurgist";
        let mystic = |name: &str| ["S_skinningknife_mb", "Archetype_Theurgist"].contains(&name);
        let nobody = |_: &str| false;
        assert!(requirement_met(GET_OFF_BEACH, nobody));
        assert!(!requirement_met(GET_OFF_BEACH, |name| name == "S_introtosettlement"));
        assert!(requirement_met(BOAR_FEAST_MYSTIC, mystic));
        assert!(
            !requirement_met(BOAR_FEAST, mystic),
            "the mystic gets its own variant"
        );
        assert!(requirement_met(BOAR_FEAST, |name| name == "S_skinningknife_mb"));
        assert!(requirement_met("(A || B) && !C", |name| name == "B"));
        assert!(!requirement_met("(A || B) && !C", |name| name != "A"));
        assert!(requirement_met("", nobody), "no requirement");
        assert!(!requirement_met("A &&", |_| true), "a cut expression");
        assert!(!requirement_met("(A", |_| true));
        assert!(!requirement_met("A = B", |_| true));
    }

    #[test]
    fn a_quest_is_found_by_the_crc_of_its_id() {
        // Retail 3772 of 12_skinningknife.
        const SKINNING_KNIFE: u32 = 0xf523_32f7;
        let book = QuestBook::build(
            [ObjectiveRow {
                id: "12_skinningknife".to_owned(),
                root: "root".to_owned(),
                turn_in: Some("01_UrdaMB_01".to_owned()),
                ..ObjectiveRow::default()
            }],
            TaskBook::new(vec![crate::objectives::TaskRow {
                task_id: "root".to_owned(),
                kind: "ConsecutiveTaskContainer".to_owned(),
                ..crate::objectives::TaskRow::default()
            }]),
        );
        let quest = book.quest_by_crc(SKINNING_KNIFE).expect("quest");
        assert_eq!(quest.turn_in.as_deref(), Some("01_UrdaMB_01"));
        assert!(book.quest_by_crc(0).is_none());
    }

    #[test]
    fn prefab_bodies_become_states_and_bad_ones_are_left_out() {
        let text = format!(
            r#"[{{"vitals_id":{SAILOR},"vitals_name":null,
                "asset":"b7e33b294bcf57b8a476abdc3a31fae7","sub_id":2,
                "alc_component":5,"vitals_component":9,
                "first_members":[{{"component":6,"class":12,"body":"010104"}},
                                 {{"component":3,"class":1528,"body":"01"}}],
                "death_members":[{{"component":20,"class":3139,"body":"010201"}}],
                "seen":1}}]"#
        );
        let prefabs = MobPrefabs::parse(&text).expect("table");
        let (looks, skipped) = MobLooks::of(&prefabs);
        assert_eq!(skipped, 1, "a cut damage receiver body");
        let sailor = looks.look(SAILOR, None).expect("sailor");
        assert_eq!(sailor.asset.sub_id, 2);
        assert_eq!(sailor.first.len(), 1);
        assert_eq!(sailor.first[0].component, 6);
        assert_eq!(sailor.death.len(), 1);
    }

    #[test]
    fn a_mob_with_its_own_asset_gets_its_own_look() {
        // Retail: the door legionnaire (179393) and the signifier (202592) share their vitals id.
        const LEGIONNAIRE: u32 = 3_705_015_359;
        const DOOR: [u8; ASSET_ID_LEN] =
            0xc05f_c957_8487_5559_afe8_74a7_8f95_fb64_u128.to_be_bytes();
        const SIGNIFIER: [u8; ASSET_ID_LEN] =
            0x87a3_35e7_6b86_50d5_a465_fe08_10bd_a5df_u128.to_be_bytes();
        let row = |asset: [u8; ASSET_ID_LEN], sub_id: u32, seen: u32| {
            let asset: String = asset.iter().map(|byte| format!("{byte:02x}")).collect();
            format!(
                r#"{{"vitals_id":{LEGIONNAIRE},"vitals_name":null,"asset":"{asset}",
                    "sub_id":{sub_id},"alc_component":5,"vitals_component":9,
                    "first_members":[],"death_members":[],"seen":{seen}}}"#
            )
        };
        let text = format!("[{},{}]", row(DOOR, 1, 2), row(SIGNIFIER, 2, 3));
        let (looks, _) = MobLooks::of(&MobPrefabs::parse(&text).expect("table"));
        let sub_id = |asset| looks.look(LEGIONNAIRE, asset).map(|look| look.asset.sub_id);
        assert_eq!(sub_id(Some(DOOR)), Some(1));
        assert_eq!(sub_id(Some(SIGNIFIER)), Some(2));
        assert_eq!(sub_id(None), Some(2), "the most seen look without an asset");
        assert_eq!(
            sub_id(Some([0; ASSET_ID_LEN])),
            Some(2),
            "and for an asset not in the table"
        );
    }
}
