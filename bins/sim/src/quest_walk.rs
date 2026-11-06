// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sim quest-walk`: plays every quest of the level with a test character of its own, in a
//! world with no database and no bus, and writes a Markdown report: the quests that end, and
//! for each other one the step it stopped at and why ([`crate::world::QuestWalk`]).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use nw_bus::{BusId, SimSlot};
use nw_common::logging::init_logging;
use nw_content::Tables;
use nw_formats::objectstream::lowercase_crc32;
use nw_sim_core::Position;
use nw_world::WorldFile;

use crate::config::load_sim_config;
use crate::login::WorldIds;
use crate::player_content::PlayerContent;
use crate::run::{RunError, load_level, world_parts};
use crate::world::{Outcome, QuestGuide, QuestWalk, SimWorld};

/// The gateway the walk characters' connections name.
const WALK_GATEWAY: &str = "quest-walk";
/// The sim id of the walk's world.
const WALK_SIM: &str = "quest-walk";
/// The world time the walk starts at.
const START_MS: u64 = 10_000;
/// The quests between two progress lines.
const PROGRESS_EVERY: usize = 100;
/// The log filter of the walk: the world's own lines stay quiet.
const WALK_LOG_FILTER: &str = "warn";

/// Plays the quests of the level of the sim configuration at `config_path` (all, or those whose
/// id starts with `only`, any case) and writes the report to `report_path`.
///
/// # Errors
///
/// Returns [`RunError`] when the configuration, the level data or the bundle cannot be read,
/// or the report cannot be written.
pub fn walk(config_path: &Path, report_path: &Path, only: Option<&str>) -> Result<(), RunError> {
    let config = load_sim_config(config_path)?;
    init_logging(WALK_LOG_FILTER)?;
    let level = load_level(&config)?;
    let gateway = BusId::try_from(WALK_GATEWAY.to_owned()).map_err(RunError::WalkId)?;
    let (settings, combat, spawn_ai) = world_parts(&config, &level)?;
    let me = SimSlot {
        id: BusId::try_from(WALK_SIM.to_owned()).map_err(RunError::WalkId)?,
        generation: START_MS,
    };
    let mut world = SimWorld::new(
        settings,
        WorldIds::random(START_MS).map_err(RunError::WorldIds)?,
        level.ground.clone(),
        me,
        combat,
        Arc::clone(&level.content),
    );
    world.set_mob_ai(spawn_ai);
    world.set_design(&level.design);
    let lower_only = only.map(str::to_lowercase);
    let quests: Vec<(String, String)> = level
        .content
        .quests
        .listing()
        .into_iter()
        .filter(|(id, _)| {
            lower_only
                .as_deref()
                .is_none_or(|only| id.to_lowercase().starts_with(only))
        })
        .collect();
    let mut now_ms = START_MS;
    let mut walks = Vec::with_capacity(quests.len());
    for (index, (quest, kind)) in quests.iter().enumerate() {
        let number = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
        walks.push((
            kind.clone(),
            world.walk_quest(
                &level.content.quest_guide,
                &gateway,
                number,
                quest,
                &mut now_ms,
            ),
        ));
        if (index + 1) % PROGRESS_EVERY == 0 {
            eprintln!("walked {} of {} quests", index + 1, quests.len());
        }
    }
    std::fs::write(report_path, report(&config.level, &walks)).map_err(RunError::Report)?;
    eprintln!(
        "walked {} quests; report in {}",
        walks.len(),
        report_path.display()
    );
    Ok(())
}

/// What level `world_file` holds that the quests of `content` need: the NPC and territory
/// places, the quest givers and topic holders of `tables`, and what gives each achievement.
pub fn quest_guide(world_file: &WorldFile, tables: &Tables, content: &PlayerContent) -> QuestGuide {
    let at = |position: [f32; 3]| Position {
        x: position[0],
        y: position[1],
        z: position[2],
    };
    let mut npcs = HashMap::new();
    for npc in &world_file.npcs {
        npcs.entry(npc.npc_id).or_insert_with(|| at(npc.position));
    }
    let mut territory_spots = HashMap::new();
    let records = world_file
        .npcs
        .iter()
        .map(|npc| (npc.position, &npc.territory_ids))
        .chain(
            world_file
                .vitals
                .iter()
                .map(|spawn| (spawn.position, &spawn.territory_ids)),
        )
        .chain(
            world_file
                .gatherables
                .iter()
                .map(|point| (point.position, &point.territory_ids)),
        )
        .chain(
            world_file
                .landmarks
                .iter()
                .map(|landmark| (landmark.position, &landmark.territory_ids)),
        );
    for (position, territories) in records {
        for territory in territories {
            territory_spots
                .entry(*territory)
                .or_insert_with(|| at(position));
        }
    }
    let states: HashMap<String, Vec<String>> = tables
        .conversation_state_data
        .iter()
        .flat_map(|table| &table.rows)
        .filter_map(|row| {
            let id = row.conversation_state_id.clone()?;
            let offered = [
                &row.linear_objective_id1,
                &row.linear_objective_id2,
                &row.linear_objective_id3,
                &row.linear_objective_id4,
                &row.linear_objective_id5,
                &row.linear_objective_id6,
                &row.linear_objective_id7,
                &row.linear_objective_id8,
                &row.linear_objective_id9,
                &row.linear_objective_id10,
            ]
            .into_iter()
            .flatten()
            .filter(|objective| !objective.is_empty())
            .map(|objective| objective.to_lowercase())
            .collect();
            Some((id.to_lowercase(), offered))
        })
        .collect();
    let mut givers: HashMap<String, Vec<String>> = HashMap::new();
    let mut topic_npcs: HashMap<u32, Vec<String>> = HashMap::new();
    for row in tables.npc_data.iter().flat_map(|table| &table.rows) {
        for state in row.conversation_state_id.values() {
            for quest in states.get(&state.to_lowercase()).into_iter().flatten() {
                let npcs = givers.entry(quest.clone()).or_default();
                if !npcs.contains(&row.npc_id) {
                    npcs.push(row.npc_id.clone());
                }
            }
        }
        let topics = [
            &row.topic_id1,
            &row.topic_id2,
            &row.topic_id3,
            &row.topic_id4,
            &row.topic_id5,
            &row.topic_id6,
            &row.topic_id7,
            &row.topic_id8,
            &row.topic_id9,
            &row.topic_id10,
        ];
        for topic in topics
            .into_iter()
            .flatten()
            .filter(|topic| !topic.is_empty())
        {
            topic_npcs
                .entry(lowercase_crc32(topic))
                .or_default()
                .push(row.npc_id.clone());
        }
    }
    let mut achievement_sources = HashMap::new();
    let mut success_events = HashSet::new();
    for (id, _) in content.quests.listing() {
        let Some(def) = content.quests.quest(&id) else {
            continue;
        };
        if let Some(event) = &def.success_event {
            success_events.insert(event.to_lowercase());
        }
        if let Some(achievement) = &def.achievement {
            achievement_sources
                .entry(achievement.to_lowercase())
                .or_insert_with(|| format!("the end of quest {id}"));
        }
        for achievement in def.tree.unlocks() {
            achievement_sources
                .entry(achievement.to_lowercase())
                .or_insert_with(|| format!("a step of quest {id}"));
        }
    }
    let start = world_file
        .npcs
        .first()
        .map_or_else(|| at([0.0; 3]), |npc| at(npc.position));
    QuestGuide {
        npcs,
        territory_spots,
        givers,
        topic_npcs,
        achievement_sources,
        success_events,
        all_spawns: world_file.vitals.clone(),
        start,
    }
}

/// The title of each outcome's section.
fn outcome_title(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Completed => "Completed",
        Outcome::Broken => "Broken: a step cannot end on this server",
        Outcome::NeedsLook => "Needs a look: the walk cannot tell whether the step can end",
        Outcome::NotWalked => "Not walked: a step needs a flow the walk does not play",
        Outcome::NotStarted => "Not started",
    }
}

/// A Markdown table cell of `text`.
fn cell(text: &str) -> String {
    text.replace('|', "\\|").replace('\n', " ")
}

/// The report of `walks` (each quest's `Type` and walk) on level `level`.
fn report(level: &str, walks: &[(String, QuestWalk)]) -> String {
    let mut by_outcome: BTreeMap<Outcome, Vec<&(String, QuestWalk)>> = BTreeMap::new();
    for walk in walks {
        by_outcome.entry(walk.1.outcome).or_default().push(walk);
    }
    let mut by_kind: BTreeMap<&str, BTreeMap<Outcome, usize>> = BTreeMap::new();
    for (kind, walk) in walks {
        *by_kind
            .entry(kind.as_str())
            .or_default()
            .entry(walk.outcome)
            .or_default() += 1;
    }
    let mut text = String::new();
    let _ = writeln!(text, "# Quest walk of level {level}\n");
    let _ = writeln!(
        text,
        "A test character took each quest, played its steps and turned it in. Talks, accepts, \
         territory entries, topics, cinematics, screens, faction picks and turn-ins went through \
         the client calls. Kills, gathers, crafts, catches and the like went in as the game \
         event the server makes of them, after a check that the level has the mob, the item \
         source, the recipe or the place. The character is at the top level, and it was given \
         the achievements a quest requires.\n"
    );
    let _ = writeln!(text, "| Outcome | Quests |\n|---|---|");
    for (outcome, list) in &by_outcome {
        let _ = writeln!(text, "| {} | {} |", outcome_title(*outcome), list.len());
    }
    let _ = writeln!(text, "| Total | {} |\n", walks.len());
    let _ = writeln!(
        text,
        "## By quest type\n\n| Type | Completed | Broken | Needs a look | Not walked | Not started |\n|---|---|---|---|---|---|"
    );
    for (kind, counts) in &by_kind {
        let count = |outcome| counts.get(&outcome).copied().unwrap_or_default();
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} | {} | {} |",
            cell(if kind.is_empty() { "(none)" } else { kind }),
            count(Outcome::Completed),
            count(Outcome::Broken),
            count(Outcome::NeedsLook),
            count(Outcome::NotWalked),
            count(Outcome::NotStarted)
        );
    }
    for (outcome, list) in &by_outcome {
        let _ = writeln!(text, "\n## {}\n", outcome_title(*outcome));
        let _ = writeln!(text, "| Quest | Type | Stop | Notes |\n|---|---|---|---|");
        for (kind, walk) in list {
            let _ = writeln!(
                text,
                "| {} | {} | {} | {} |",
                cell(&walk.quest),
                cell(kind),
                cell(walk.stop.as_deref().unwrap_or_default()),
                cell(&walk.notes.join("; "))
            );
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walked(quest: &str, outcome: Outcome, stop: Option<&str>) -> QuestWalk {
        QuestWalk {
            quest: quest.to_owned(),
            outcome,
            stop: stop.map(str::to_owned),
            notes: vec!["a | note".to_owned()],
        }
    }

    #[test]
    fn the_report_counts_each_outcome_and_lists_each_quest_with_its_stop() {
        let walks = vec![
            ("Journey".to_owned(), walked("A", Outcome::Completed, None)),
            (
                "Journey".to_owned(),
                walked("B", Outcome::Broken, Some("no mob that counts as Boar")),
            ),
            (
                "Mission".to_owned(),
                walked("C", Outcome::NotWalked, Some("needs a dungeon")),
            ),
        ];
        let text = report("newworld_vitaeeterna", &walks);
        assert!(text.contains("| Completed | 1 |"));
        assert!(text.contains("| Broken: a step cannot end on this server | 1 |"));
        assert!(text.contains("| Total | 3 |"));
        assert!(text.contains("| Journey | 1 | 1 | 0 | 0 | 0 |"));
        assert!(text.contains("| B | Journey | no mob that counts as Boar | a \\| note |"));
        assert!(text.contains("| C | Mission | needs a dungeon |"));
    }
}
