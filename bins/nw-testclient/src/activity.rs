// SPDX-License-Identifier: AGPL-3.0-or-later
//! The player activities of `nw-testclient play` besides quests: each is a script of steps, and
//! each step sends client calls (or a game master line) and waits for the server's proof that
//! it worked: an answer line, a server message of a type, or the answer to an interaction. The
//! places and ids (a gather node, a crafting station, an inn, a shrine, a game mode, a town
//! mission) come from the level's world file and the content bundle, as the client knows them.
//! An activity the bot cannot play names why instead of steps.

use std::time::Duration;

use clap::ValueEnum;
use nw_content::Tables;
use nw_content::crafting::{IngredientKind, Recipes};
use nw_content::instances::{InstanceCatalog, InstanceKind};
use nw_content::poi_tags::PoiTags;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::arena::{
    ArenaClientMsg, PLAYER_ARENA_COMPONENT, REMOTE_INTERACT_RESULT_TYPE_INDEX,
};
use nw_messages::client_input::{CRAFTING_STATION_KIND, GATHER_KIND, TOWN_BOARD_KIND};
use nw_messages::conversation_client::ObjectiveInteractorClientMsg;
use nw_messages::facet::{
    CRAFTING_PROGRESS_TYPE_INDEX, INGREDIENT_ITEM, TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX,
};
use nw_messages::mount::{MOUNT_FACET, MountClientMsg, SUMMON_PENDING_TYPE_INDEX};
use nw_messages::travel::{LandmarkRef, TravelClientMsg, component};
use nw_messages::wire::RAW_ID_LEN;
use nw_messages::{ClientFacetMsg, CraftIngredient, FacetHeader, InteractionRequest, MessageError};
use nw_world::WorldFile;
use nw_world::capitals::capital_id;

use crate::gather::{GatherNode, node_id};

/// The kind of an inn's open interaction (`travel::kind::OPEN_INN` of the sim; retail opened
/// the inn with it before the check in, live-inn note section 1).
const OPEN_INN_KIND: u8 = 70;
/// The kind of an inn check in on the same interaction (`travel::kind::SET_SPAWN_POINT`).
const SET_SPAWN_POINT_KIND: u8 = 18;
/// The landmark type of an inn recall (`INN_RESPAWN_LANDMARK` of the sim).
const INN_RECALL_LANDMARK: u8 = 20;
/// The slice folder of the settlement inns (Inferred: the world file's inn structures).
const INN_SLICE_MARK: &str = "/social structures/inn/";
/// The `POITag` of a fast travel shrine's territory (live-inn note section 4: all 137 rows are
/// shrines).
const FAST_TRAVEL_TAG: &str = "fasttravel";
/// The gatherable the harvest gathers by hand (Observed: the retail flint request of the
/// gather note, newchar 739603, names `SingleStone`).
const HARVESTED_GATHERABLE: &str = "SingleStone";
/// The item of the recipe the craft makes (Observed: the retail smelt of the crafting note,
/// four `OreT1` at a `smelting2` station).
const CRAFTED_ITEM: &str = "IngotT2";
/// The quest that unlocks the first horse ("My Kingdom for a Horse").
const MOUNT_QUEST: &str = "9401_BefriendHorse";
/// The wait from the summon's start to its authorization (retail: the client completed the
/// summon 3,049 to 3,070 ms after the 5747).
const SUMMON_WAIT: Duration = Duration::from_millis(3500);
/// How a `!queststate` line of a done quest starts.
const QUEST_DONE: &str = "done ";
/// The mount part of a `!state` line of a rider.
const STATE_RIDING: &str = " mount riding ";
/// The town board's interaction name (retail `@ui_territory_board_town_project`).
const TOWN_BOARD_NAME: &[u8] = b"@ui_territory_board_town_project";
/// The progression of azoth, which a fast travel spends.
const AZOTH: &str = "Azoth_Currency";
/// Azoth the bot gets before a fast travel: more than any trip costs.
const AZOTH_GIVEN: u32 = 500;
/// The level the level activity sets, then passes with experience.
const SET_LEVEL: u32 = 5;
/// Experience given after the set level: enough for the next levels.
const EXPERIENCE_GIVEN: u32 = 20_000;
/// Coins for a craft's fee.
const CRAFT_COINS: u32 = 10_000;
/// The rank of a craft's tradeskill.
const CRAFT_RANK: u32 = 200;
/// How long a step's proof may take.
const STEP_WAIT: Duration = Duration::from_secs(10);
/// How long a gather or a fast travel may take: the gather time, the 10 s channel.
const LONG_WAIT: Duration = Duration::from_secs(25);
/// The first words of the sim's `!state` line.
const STATE_LEVEL: &str = "state level ";
/// A game master line the sim carried out.
const DONE: &str = "Done.";
/// How the first word of the PvP part of the `!state` line starts.
const STATE_PVP: &str = " pvp ";

/// An activity a player does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ActivityKind {
    /// Level up: a set level, then experience.
    Level,
    /// The PvP flag on, then off.
    Pvp,
    /// Harvest a gather node by hand.
    Harvest,
    /// Craft a recipe at a crafting station.
    Craft,
    /// Check in at an inn, then recall to it.
    Inn,
    /// Fast travel at a shrine.
    Shrine,
    /// Ask to enter an expedition from afar.
    Dungeon,
    /// Take a town board mission.
    Board,
    /// Invite a second player into a group.
    Group,
    /// Summon and ride a mount.
    Mount,
    /// Play a song on an instrument.
    Music,
}

/// One call of a step, written once the bot knows its own entity number.
pub type Call = Box<dyn Fn(u64) -> Result<(u64, Vec<u8>), MessageError>>;

/// What a step sends.
pub enum Act {
    /// A game master line.
    Gm(String),
    /// A game master's teleport to the place; the bot's next inputs stand there.
    Teleport([f32; 3]),
    /// A press of the summon mount key in the bot's input.
    SummonKey,
    /// A wait before the next act (a summon's authorization).
    Pause(Duration),
    /// A client call: its type index and body for the bot's own entity number.
    Call(Call),
    /// An interaction in the bot's input, with the interaction's name.
    Interact {
        /// The interaction; the bot gives it a fresh id.
        request: InteractionRequest,
        /// The name the interaction carries (an NPC's, a board's); empty for none.
        name: Vec<u8>,
    },
}

/// What proves that a step worked.
pub enum Proof {
    /// A game master answer line that the test takes, described.
    Line(Box<dyn Fn(&str) -> bool>, String),
    /// A server message of this type index.
    Message(u64),
    /// The server took the step's interaction (3743 status 1).
    Accepted,
    /// The interaction ended (3746).
    Ended,
    /// Nothing: the server answers the call with no message (a toggle, a territory entry, a
    /// board take); a later step proves it.
    Sent,
}

/// One step of an activity.
pub struct Step {
    /// What it does, for the report.
    pub what: String,
    /// What it sends.
    pub acts: Vec<Act>,
    /// What proves it.
    pub proof: Proof,
    /// How long the proof may take.
    pub wait: Duration,
}

/// One activity: its steps, or why the bot cannot play it.
pub struct Activity {
    /// The activity.
    pub kind: ActivityKind,
    /// Its steps; `Err` names why it cannot be played.
    pub steps: Result<Vec<Step>, String>,
}

/// What the activities read: the level and the content.
pub struct ActivityData<'a> {
    /// The level's world file.
    pub world: &'a WorldFile,
    /// The content bundle's tables.
    pub tables: &'a Tables,
}

/// A step of `what` that sends `acts` and waits `wait` for `proof`.
fn step(what: impl Into<String>, acts: Vec<Act>, proof: Proof, wait: Duration) -> Step {
    Step {
        what: what.into(),
        acts,
        proof,
        wait,
    }
}

/// A game master line answered with [`DONE`].
fn gm_done(line: String) -> Step {
    step(
        line.clone(),
        vec![Act::Gm(line)],
        Proof::Line(Box::new(|answer| answer == DONE), DONE.to_owned()),
        STEP_WAIT,
    )
}

/// A `!state` line that `wanted` takes, described as `described`.
fn state_shows(what: &str, wanted: impl Fn(&str) -> bool + 'static, described: &str) -> Step {
    step(
        what,
        vec![Act::Gm("!state".to_owned())],
        Proof::Line(
            Box::new(move |line| line.starts_with(STATE_LEVEL) && wanted(line)),
            described.to_owned(),
        ),
        STEP_WAIT,
    )
}

/// A teleport to `at`.
const fn teleport(at: [f32; 3]) -> Act {
    Act::Teleport(at)
}

/// A facet call `call` of component `component` for the bot's entity.
fn facet(make: impl Fn(FacetHeader) -> ClientFacetMsg + 'static, component: u64) -> Act {
    Act::Call(Box::new(move |own| {
        let call = make(FacetHeader::of(component, own));
        Ok((call.type_index(), call.encode()?))
    }))
}

/// A fast travel to `landmark` (3713).
fn fast_travel(landmark: LandmarkRef) -> Act {
    Act::Call(Box::new(move |own| {
        let call = TravelClientMsg::FastTravelLandmark {
            header: FacetHeader::of(component::PLAYER_HOUSING, own),
            landmark,
        };
        Ok((call.type_index(), call.encode()))
    }))
}

/// The level of a `!state` line.
fn state_level(line: &str) -> Option<u32> {
    line.strip_prefix(STATE_LEVEL)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

impl Activity {
    /// The activity of `kind` on the level and content of `data`.
    pub fn of(kind: ActivityKind, data: &ActivityData<'_>) -> Self {
        let steps = match kind {
            ActivityKind::Level => Ok(level()),
            ActivityKind::Pvp => Ok(pvp()),
            ActivityKind::Harvest => harvest(data),
            ActivityKind::Craft => craft(data),
            ActivityKind::Inn => inn(data),
            ActivityKind::Shrine => shrine(data),
            ActivityKind::Dungeon => dungeon(data),
            ActivityKind::Board => board(data),
            ActivityKind::Group => Err(
                "needs a second bot in the world at the same time; a play run has one bot"
                    .to_owned(),
            ),
            ActivityKind::Mount => Ok(mount()),
            ActivityKind::Music => Err(
                "needs an instrument worn: the bot cannot read the bag slot of a given instrument to equip it"
                    .to_owned(),
            ),
        };
        Self { kind, steps }
    }
}

fn level() -> Vec<Step> {
    vec![
        gm_done(format!("!level {SET_LEVEL}")),
        state_shows(
            "the state shows the set level",
            |line| state_level(line) == Some(SET_LEVEL),
            &format!("level {SET_LEVEL}"),
        ),
        gm_done(format!("!xp {EXPERIENCE_GIVEN}")),
        state_shows(
            "the experience raised the level",
            |line| state_level(line).is_some_and(|level| level > SET_LEVEL),
            &format!("a level above {SET_LEVEL}"),
        ),
    ]
}

fn pvp() -> Vec<Step> {
    let toggle = || facet(|header| ClientFacetMsg::TogglePvpFlag { header }, 0);
    vec![
        step(
            "toggle the PvP flag on",
            vec![toggle()],
            Proof::Sent,
            Duration::ZERO,
        ),
        state_shows(
            "the flag is on its way on, or on",
            |line| {
                line.contains(&format!("{STATE_PVP}pending"))
                    || line.contains(&format!("{STATE_PVP}on"))
            },
            "pvp pending or on",
        ),
        step(
            "toggle the PvP flag off",
            vec![toggle()],
            Proof::Sent,
            Duration::ZERO,
        ),
        state_shows(
            "the flag is off",
            |line| line.contains(&format!("{STATE_PVP}off")),
            "pvp off",
        ),
    ]
}

/// A summon and ride of the horse that the mount quest unlocks.
fn mount() -> Vec<Step> {
    vec![
        step(
            format!("the mount quest {MOUNT_QUEST} is done (play it with --quests first)"),
            vec![Act::Gm(format!("!queststate {MOUNT_QUEST}"))],
            Proof::Line(
                Box::new(|line| line.starts_with(QUEST_DONE)),
                format!("done {MOUNT_QUEST}"),
            ),
            STEP_WAIT,
        ),
        step(
            "press the summon key",
            vec![Act::SummonKey],
            Proof::Message(SUMMON_PENDING_TYPE_INDEX),
            STEP_WAIT,
        ),
        step(
            "complete the summon once it is authorized",
            vec![
                Act::Pause(SUMMON_WAIT),
                Act::Call(Box::new(|own| {
                    let call = MountClientMsg::CompleteSummon {
                        header: FacetHeader::of(MOUNT_FACET, own),
                    };
                    Ok((call.type_index(), call.encode()))
                })),
            ],
            Proof::Sent,
            Duration::ZERO,
        ),
        state_shows(
            "the state shows the bot riding",
            |line| line.contains(STATE_RIDING),
            "mount riding",
        ),
    ]
}

/// A node of a gatherable a player gathers by hand with no rank or item.
fn harvest(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let gatherable = lowercase_crc32(HARVESTED_GATHERABLE);
    let (node, point) = data
        .world
        .gatherables
        .iter()
        .enumerate()
        .find(|(_, point)| point.gatherable_id == gatherable)
        .ok_or("the level has no node of the retail flint gather")?;
    Ok(gather_steps(
        HARVESTED_GATHERABLE,
        GatherNode {
            gatherable,
            at: point.position,
        },
        node,
    ))
}

/// Goes to `node` (its index in the level's gatherables, which names it to the server as a
/// client's node id does) of gatherable `name`, gathers it and waits for the gather's end.
pub fn gather_steps(name: &str, node: GatherNode, index: usize) -> Vec<Step> {
    let (target, target_entity) = node_id(index);
    let request = InteractionRequest {
        target,
        target_entity,
        interaction: 0,
        kind: GATHER_KIND,
        gatherable: Some(node.gatherable),
    };
    vec![
        step(
            format!("go to a {name} node and gather it"),
            vec![
                teleport(node.at),
                Act::Interact {
                    request,
                    name: Vec::new(),
                },
            ],
            Proof::Accepted,
            STEP_WAIT,
        ),
        step("the gather ends", Vec::new(), Proof::Ended, LONG_WAIT),
    ]
}

/// A recipe of item ingredients made at a station of the level.
fn craft(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let recipes = Recipes::from_tables(data.tables);
    let recipe = recipes
        .making(CRAFTED_ITEM)
        .ok_or("the content has no recipe of the retail smelt")?;
    let station = data
        .world
        .stations
        .iter()
        .find(|station| recipe.made_at(&station.components.station_type))
        .ok_or("no station of the level makes the retail smelt")?;
    if !recipe
        .ingredients
        .iter()
        .all(|ingredient| matches!(ingredient.kind, IngredientKind::Item(_)))
    {
        return Err("the retail smelt asks for an ingredient category".to_owned());
    }
    let details = recipes.details(lowercase_crc32(&recipe.id));
    let ingredients: Vec<(String, u32)> = recipe
        .ingredients
        .iter()
        .filter_map(|ingredient| match &ingredient.kind {
            IngredientKind::Item(item) => Some((item.clone(), ingredient.quantity)),
            IngredientKind::Category(_) => None,
        })
        .collect();
    let mut steps = vec![gm_done(format!("!coins {CRAFT_COINS}"))];
    if let Some(tradeskill) = &details.tradeskill {
        steps.push(gm_done(format!("!mastery {tradeskill} {CRAFT_RANK}")));
    }
    steps.extend(
        ingredients
            .iter()
            .map(|(item, quantity)| gm_done(format!("!give {item} {quantity}"))),
    );
    let mut target = [0; RAW_ID_LEN];
    target[..size_of::<u64>()].copy_from_slice(&station.id.to_le_bytes());
    steps.push(step(
        format!(
            "go to the {} station and open it",
            station.components.station_type
        ),
        vec![
            teleport(station.position),
            Act::Interact {
                request: InteractionRequest {
                    target,
                    target_entity: station.id,
                    interaction: 0,
                    kind: CRAFTING_STATION_KIND,
                    gatherable: None,
                },
                name: Vec::new(),
            },
        ],
        Proof::Accepted,
        STEP_WAIT,
    ));
    let recipe_crc = lowercase_crc32(&recipe.id);
    let fee = details.fee;
    steps.push(step(
        format!("craft {}", recipe.id),
        vec![facet(
            move |header| ClientFacetMsg::SetCraftingDetails {
                header,
                recipe: recipe_crc,
                quantity: 1,
                fee,
                gear_score: 0,
                perk_items: Vec::new(),
                unknown: 0,
                ingredients: ingredients
                    .iter()
                    .map(|(item, quantity)| CraftIngredient {
                        kind: INGREDIENT_ITEM,
                        quantity: *quantity,
                        item: lowercase_crc32(item),
                    })
                    .collect(),
            },
            0,
        )],
        Proof::Message(CRAFTING_PROGRESS_TYPE_INDEX),
        STEP_WAIT,
    ));
    Ok(steps)
}

/// A settlement inn of the level: a check in, then a recall to it.
fn inn(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let capital = data
        .world
        .capitals
        .iter()
        .find(|capital| capital.slice_name.contains(INN_SLICE_MARK))
        .ok_or(
            "no capital of the level is an inn: its inn structures are spawned slices, and which entity id a check in names is Unknown",
        )?;
    let id = capital_id(&capital.id).ok_or("the inn's capital has no entity id")?;
    let request = |kind| InteractionRequest {
        target: [0; RAW_ID_LEN],
        target_entity: id,
        interaction: 0,
        kind,
        gatherable: None,
    };
    let position = capital.position;
    Ok(vec![
        step(
            "go to an inn and open it",
            vec![
                teleport(position),
                Act::Interact {
                    request: request(OPEN_INN_KIND),
                    name: Vec::new(),
                },
            ],
            Proof::Accepted,
            STEP_WAIT,
        ),
        step(
            "check in",
            vec![Act::Interact {
                request: request(SET_SPAWN_POINT_KIND),
                name: Vec::new(),
            }],
            Proof::Accepted,
            STEP_WAIT,
        ),
        state_shows(
            "the state names the inn",
            move |line| line.contains(&format!("inn {id:016x}")),
            &format!("inn {id:016x}"),
        ),
        step(
            "recall to the inn",
            vec![fast_travel(LandmarkRef {
                territory: 0,
                landmark_type: INN_RECALL_LANDMARK,
                position: [position[0], position[1], 0.0],
            })],
            Proof::Message(TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX),
            LONG_WAIT,
        ),
    ])
}

/// A fast travel shrine of the level.
fn shrine(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let tags = PoiTags::from_tables(data.tables);
    let shrines = tags.territories_with(FAST_TRAVEL_TAG);
    let (landmark, territory) = data
        .world
        .landmarks
        .iter()
        .find_map(|landmark| {
            landmark
                .territory_ids
                .iter()
                .find(|territory| shrines.contains(&u32::from(**territory)))
                .map(|territory| (landmark, *territory))
        })
        .ok_or("the level has no landmark in a fast travel territory")?;
    let (landmark_type, position) = (landmark.landmark_type, landmark.position);
    Ok(vec![
        step(
            "enter the shrine's territory (its discovery)",
            vec![facet(
                move |header| ClientFacetMsg::TerritoryEntered {
                    header,
                    area: [0; RAW_ID_LEN],
                    area_entity: 0,
                    extra: 0,
                    territory,
                    unknown: 0,
                },
                0,
            )],
            Proof::Sent,
            Duration::ZERO,
        ),
        gm_done(format!("!masteryxp {AZOTH} {AZOTH_GIVEN}")),
        step(
            format!("fast travel to the shrine of territory {territory}"),
            vec![fast_travel(LandmarkRef {
                territory,
                landmark_type,
                position: [position[0], position[1], 0.0],
            })],
            Proof::Message(TELEPORT_WITH_LOADING_SCREEN_TYPE_INDEX),
            LONG_WAIT,
        ),
    ])
}

/// An expedition that takes a remote entry.
fn dungeon(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let catalog = InstanceCatalog::from_tables(data.tables);
    let mode = catalog
        .modes()
        .find(|mode| mode.kind == InstanceKind::Expedition && mode.remote_entry)
        .ok_or("no expedition takes a remote entry")?;
    let game_mode = mode.crc;
    let mut steps = Vec::new();
    if let Some(level) = mode.required_level {
        steps.push(gm_done(format!("!level {level}")));
    }
    steps.push(step(
        format!("ask to enter expedition {}", mode.id),
        vec![Act::Call(Box::new(move |own| {
            let call = ArenaClientMsg::RequestRemoteInteract {
                header: FacetHeader::of(PLAYER_ARENA_COMPONENT, own),
                game_mode,
            };
            Ok((call.type_index(), call.encode()))
        }))],
        Proof::Message(REMOTE_INTERACT_RESULT_TYPE_INDEX),
        STEP_WAIT,
    ));
    Ok(steps)
}

/// A town board mission of the content.
fn board(data: &ActivityData<'_>) -> Result<Vec<Step>, String> {
    let row = data
        .tables
        .mission_data
        .iter()
        .flat_map(|table| &table.rows)
        .find(|row| {
            row.enabled != Some(false)
                && !row.mission_id.is_empty()
                && row
                    .territory_project_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
        })
        .ok_or("the content has no town board mission")?;
    let mission = row.mission_id.clone();
    let mission_crc = lowercase_crc32(&mission);
    let project = lowercase_crc32(row.territory_project_id.as_deref().unwrap_or_default());
    let shown = mission.clone();
    Ok(vec![
        step(
            "open the town board",
            vec![Act::Interact {
                request: InteractionRequest {
                    target: [0; RAW_ID_LEN],
                    target_entity: 0,
                    interaction: 0,
                    kind: TOWN_BOARD_KIND,
                    gatherable: None,
                },
                name: TOWN_BOARD_NAME.to_vec(),
            }],
            Proof::Accepted,
            STEP_WAIT,
        ),
        step(
            format!("take mission {mission}"),
            vec![Act::Call(Box::new(move |own| {
                let call = ObjectiveInteractorClientMsg::SelectCommunityGoal {
                    header: FacetHeader::of(0, own),
                    mission: mission_crc,
                    project,
                };
                Ok((call.type_index(), call.encode()))
            }))],
            Proof::Sent,
            Duration::ZERO,
        ),
        step(
            "the mission runs",
            vec![Act::Gm(format!("!queststate {shown}"))],
            Proof::Line(
                Box::new(|line| line.starts_with("running ")),
                "running".to_owned(),
            ),
            STEP_WAIT,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_state_line_gives_its_level() {
        assert_eq!(
            state_level(
                "state level 7 xp 0 coins 0 faction 0 pvp off inn none mount foot quests 0 done 0"
            ),
            Some(7)
        );
        assert_eq!(state_level("Done."), None);
    }

    #[test]
    fn the_activities_without_a_flow_name_why() {
        let tables = Tables::default();
        let world = WorldFile::default();
        let data = ActivityData {
            world: &world,
            tables: &tables,
        };
        for kind in [ActivityKind::Group, ActivityKind::Music] {
            assert!(Activity::of(kind, &data).steps.is_err(), "{kind:?}");
        }
        assert_eq!(
            Activity::of(ActivityKind::Mount, &data)
                .steps
                .map(|steps| steps.len()),
            Ok(4),
            "the mount needs no level data"
        );
        assert!(
            Activity::of(ActivityKind::Harvest, &data).steps.is_err(),
            "an empty level has no node"
        );
        assert_eq!(
            Activity::of(ActivityKind::Level, &data)
                .steps
                .map(|steps| steps.len()),
            Ok(4)
        );
    }

    #[test]
    fn a_harvest_names_its_node_as_a_client_does() {
        let tables = Tables::default();
        let world = WorldFile {
            gatherables: vec![
                nw_world::records::GatherablePoint {
                    gatherable_id: lowercase_crc32("Other"),
                    ..Default::default()
                },
                nw_world::records::GatherablePoint {
                    gatherable_id: lowercase_crc32(HARVESTED_GATHERABLE),
                    position: [5.0, 6.0, 7.0],
                    ..Default::default()
                },
            ],
            ..WorldFile::default()
        };
        let data = ActivityData {
            world: &world,
            tables: &tables,
        };
        let steps = Activity::of(ActivityKind::Harvest, &data)
            .steps
            .expect("a node");
        let (target, entity) = node_id(1);
        assert!(matches!(
            steps[0].acts.as_slice(),
            [
                Act::Teleport([5.0, 6.0, 7.0]),
                Act::Interact { request, .. }
            ] if request.target == target
                && request.target_entity == entity
                && request.gatherable == Some(lowercase_crc32(HARVESTED_GATHERABLE))
        ), "an empty node id names the first node anyone gathered with it");
    }
}
