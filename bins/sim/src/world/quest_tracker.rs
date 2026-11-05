// SPDX-License-Identifier: AGPL-3.0-or-later
//! The objectives state of one player's journal (objectives-state note, section 8): the client's
//! quest tracker and journal show it. A player runs many objectives at once, up to the journal
//! limit of each kind ([`nw_content::objective_limits`]); each is one active record, one
//! instance and its own task entries in the lists.
//!
//! The first own card carries the tracked objectives, the active objectives and their task
//! lists. Every later change of a task list goes whole again, at a higher version, in the bundle
//! of the same tick. When an objective ends, retail adds it to the completed list as one change,
//! in [`END_REPEATS`] bundles, then sends the tracked, active and task lists without it, in
//! [`EMPTY_REPEATS`] bundles at the next version (newchar 2026-09-18, 492.95 to 493.05 s: its
//! only objective, so the lists went empty).

use std::collections::VecDeque;

use nw_bus::MissionTake;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::items::FullList;
use nw_messages::{
    ListChange, ObjectiveRecord, ObjectivesState, ReplicatedList, TaskEntry, TaskWireState,
};
use uuid::Uuid;

use crate::objectives::{TaskProgress, TaskStatus};

/// The version of the first lists (retail first own card).
const FIRST_VERSION: u64 = 3;
/// How many bundles in a row carry the state of an accepted quest (retail: 4, all at one
/// version, quest-journal note).
const ACCEPT_REPEATS: usize = 4;
/// How many bundles in a row carry the completed change of an ended quest (retail: 4).
const END_REPEATS: usize = 4;
/// How many bundles in a row carry the lists without it after that (retail: 3).
const EMPTY_REPEATS: usize = 3;
/// How many bundles in a row carry the lists without a dropped objective (retail faction
/// missions: 6, all at one version, for each of 4 drops).
const ABANDON_REPEATS: usize = 6;
/// Names the objective instance ids this server makes.
const INSTANCE_NAMESPACE: u64 = 0x6f62_6a65_6374_6976;

/// The instance id of objective `objective` (its id) for player `player`: the same on every call
/// and every sim, never 0.
pub(super) fn instance_of(objective: &str, player: u64) -> u64 {
    let namespace = Uuid::from_u64_pair(INSTANCE_NAMESPACE, player);
    let (high, _) = Uuid::new_v5(&namespace, objective.as_bytes()).as_u64_pair();
    high.max(1)
}

/// What the journal record of an objective names besides its instance: the objective (a
/// mission's template), and a mission's id and take.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct JournalRecord {
    /// CRC-32 of the lowercase `ObjectiveID`.
    pub objective: u32,
    /// CRC-32 of the lowercase `MissionID`; 0 for a quest.
    pub mission: u32,
    /// The mission's take.
    pub take: Option<MissionTake>,
}

impl JournalRecord {
    /// The record of quest `objective` (its id).
    pub fn of(objective: &str) -> Self {
        Self {
            objective: lowercase_crc32(objective),
            ..Self::default()
        }
    }
}

/// One objective of the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    /// CRC-32 of the lowercase id of the quest or mission it runs.
    key: u32,
    /// What its record names.
    record: JournalRecord,
    /// The player's instance of the objective: the same on every call for a player, never 0.
    instance: u64,
    /// Its task list of the newest state.
    tasks: Vec<TaskProgress>,
    /// The quest tracker shows it.
    tracked: bool,
}

/// The tracker state of one player's journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuestTracker {
    /// The player's entity number: it names the instances.
    player: u64,
    /// The version of the newest lists sent.
    version: u64,
    /// The running objectives, in the order they started.
    entries: Vec<Entry>,
    /// The instances of the objectives the player finished.
    completed: Vec<u64>,
    /// A new accept pins its objective (2709). Off when every client flag is 0.
    auto_pin: bool,
    /// States to send, one per bundle.
    pending: VecDeque<ObjectivesState>,
}

impl QuestTracker {
    /// The empty journal of player `player`.
    pub(super) fn new(player: u64) -> Self {
        Self {
            player,
            version: FIRST_VERSION,
            entries: Vec::new(),
            completed: Vec::new(),
            auto_pin: true,
            pending: VecDeque::new(),
        }
    }

    /// The version of the newest lists it made.
    pub(super) const fn version(&self) -> u64 {
        self.version
    }

    /// The journal of a client that has lists up to `version` from another sim: its lists go
    /// on above it.
    pub(super) const fn above(mut self, version: u64) -> Self {
        self.version = version + 1;
        self
    }

    /// The journal of a player who finished the objectives `finished` (retail: the first
    /// open-world state lists the tutorial as completed).
    pub(super) fn after(mut self, finished: &[&str]) -> Self {
        self.completed = finished
            .iter()
            .map(|objective| instance_of(objective, self.player))
            .collect();
        self
    }

    /// Whether objective `objective` (its id) runs in the journal.
    pub(super) fn has(&self, objective: &str) -> bool {
        let crc = lowercase_crc32(objective);
        self.entries.iter().any(|entry| entry.key == crc)
    }

    /// The instance of quest or mission `objective` (its id) for this player.
    pub(super) fn instance(&self, objective: &str) -> u64 {
        instance_of(objective, self.player)
    }

    fn list<T>(&self, elements: Vec<T>) -> Option<FullList<T>> {
        Some(FullList {
            version: Some(self.version),
            elements,
        })
    }

    fn tracked(&self) -> Vec<u64> {
        self.entries
            .iter()
            .filter(|entry| entry.tracked)
            .map(|entry| entry.instance)
            .collect()
    }

    fn records(&self) -> Vec<ObjectiveRecord> {
        self.entries
            .iter()
            .map(|entry| {
                let record = ObjectiveRecord::available(entry.record.objective, entry.instance);
                match entry.record.take {
                    Some(take) => ObjectiveRecord {
                        mission: entry.record.mission,
                        destination: take.board,
                        accept_time: take.accepted_ns,
                        origin_territory: take.territory,
                        ..record
                    },
                    None => record,
                }
            })
            .collect()
    }

    fn task_entries(&self) -> Vec<TaskEntry> {
        self.entries
            .iter()
            .flat_map(|entry| {
                entry.tasks.iter().map(|task| TaskEntry {
                    instance: entry.instance,
                    index: task.index,
                    task: task.task,
                    progress: task.progress,
                    state: match task.status {
                        TaskStatus::Running => TaskWireState::Active,
                        TaskStatus::Done => TaskWireState::Complete,
                        TaskStatus::DoneActive => TaskWireState::CompleteActive,
                        TaskStatus::Stopped => TaskWireState::Inactive,
                    },
                })
            })
            .collect()
    }

    /// The tracked, active and task lists of the running objectives.
    fn running_state(&self) -> ObjectivesState {
        ObjectivesState {
            tracked: self.list(self.tracked()),
            completed: None,
            active: self.list(self.records()),
            tasks: self.list(self.task_entries()),
        }
    }

    /// The state of the first own card: the running objectives with their task lists, and the
    /// finished ones.
    pub(super) fn first_state(&self) -> ObjectivesState {
        ObjectivesState {
            completed: if self.completed.is_empty() {
                None
            } else {
                self.list(self.completed.clone()).map(ReplicatedList::Full)
            },
            ..self.running_state()
        }
    }

    /// The state of the first own card, which makes every queued state moot: its lists are the
    /// newest, and the client drops a list version lower than one it has (live run 12; live
    /// 2026-09-26: a beach card at version 11 followed by the accept's four states at 10).
    pub(super) fn take_first_state(&mut self) -> ObjectivesState {
        self.pending.clear();
        self.first_state()
    }

    /// The client is in game: the whole state goes once more, at the newest lists. Its quest
    /// tracker pins only a tracked objective it gets in game (retail newchar 2026-09-18: the
    /// first card's state came every 0.34 s from 0.31 s on; the client's in-game burst, its
    /// auto-pin flags (2709) among it, came at 9.12 s, the next copy at 9.32 s and the pin (1458)
    /// at 9.37 s. Live 2026-09-26: our two copies came before the client was in game, and it
    /// pinned nothing for 171 s). A journal with nothing running sends nothing.
    pub(super) fn in_game(&mut self) {
        if !self.entries.is_empty() {
            self.pending.push_back(self.first_state());
        }
    }

    /// Keeps the client's auto-pin choice (2709) for the next accept.
    pub(super) fn set_auto_pin(&mut self, on: bool) {
        self.auto_pin = on;
    }

    /// The same journal with auto-pin `on`.
    pub(super) fn with_auto_pin(mut self, on: bool) -> Self {
        self.auto_pin = on;
        self
    }

    /// Objective `objective` (its id), with record `record`, goes on from another sim or a save
    /// with the task list `tasks`, tracked; the first own card sends it.
    pub(super) fn resume(
        &mut self,
        objective: &str,
        record: JournalRecord,
        tasks: Vec<TaskProgress>,
    ) {
        if self.has(objective) {
            return;
        }
        self.entries.push(Entry {
            key: lowercase_crc32(objective),
            record,
            instance: instance_of(objective, self.player),
            tasks,
            tracked: true,
        });
    }

    /// The player accepted objective `objective` (its id), whose task list is `tasks`: the client
    /// learns it is tracked and active, with its tasks, in [`ACCEPT_REPEATS`] bundles at one new
    /// version (retail reply to a 3772; without the active record the journal shows nothing,
    /// live 2026-09-18). A client that turned every auto-pin flag off gets it active and not
    /// tracked. A repeatable objective that ran before leaves the completed list while it runs.
    pub(super) fn accept(
        &mut self,
        objective: &str,
        record: JournalRecord,
        tasks: Vec<TaskProgress>,
    ) {
        let tracked = self.auto_pin;
        let crc = lowercase_crc32(objective);
        let instance = instance_of(objective, self.player);
        self.completed.retain(|done| *done != instance);
        match self.entries.iter_mut().find(|entry| entry.key == crc) {
            Some(entry) => {
                entry.record = record;
                entry.tasks = tasks;
                entry.tracked = tracked;
            }
            None => self.entries.push(Entry {
                key: crc,
                record,
                instance,
                tasks,
                tracked,
            }),
        }
        self.version += 1;
        let state = self.running_state();
        self.pending
            .extend(std::iter::repeat_n(state, ACCEPT_REPEATS));
    }

    /// Takes objective `objective`'s (its id) task list `tasks` and whether it `finished` (a
    /// quest with a turn-in NPC finishes at its turn-in, after its last task); queues the states
    /// that change the client's tracker. An objective not in the journal joins it (the
    /// tutorial).
    pub(super) fn update(&mut self, objective: &str, tasks: Vec<TaskProgress>, finished: bool) {
        let crc = lowercase_crc32(objective);
        let position = match self.entries.iter().position(|entry| entry.key == crc) {
            Some(position) => position,
            None => {
                let instance = instance_of(objective, self.player);
                if self.completed.contains(&instance) {
                    return;
                }
                self.entries.push(Entry {
                    key: crc,
                    record: JournalRecord::of(objective),
                    instance,
                    tasks: Vec::new(),
                    tracked: true,
                });
                self.entries.len() - 1
            }
        };
        if tasks != self.entries[position].tasks {
            self.entries[position].tasks = tasks;
            self.version += 1;
            self.pending.push_back(ObjectivesState {
                tasks: self.list(self.task_entries()),
                ..ObjectivesState::default()
            });
        }
        if !finished {
            return;
        }
        let ended = self.entries.remove(position);
        self.version += 1;
        let added = ListChange {
            index: u32::try_from(self.completed.len()).unwrap_or(u32::MAX),
            version: self.version,
            value: ended.instance,
        };
        self.completed.push(ended.instance);
        let completed = ObjectivesState {
            completed: Some(ReplicatedList::Changes(vec![added])),
            ..ObjectivesState::default()
        };
        self.pending
            .extend(std::iter::repeat_n(completed, END_REPEATS));
        self.version += 1;
        let without = self.running_state();
        self.pending
            .extend(std::iter::repeat_n(without, EMPTY_REPEATS));
    }

    /// Drops objective `objective` (its id) from the journal (3836): the tracked, active and task
    /// lists go out without it, and it does not join the completed list (retail faction
    /// missions). An objective not in the journal changes nothing.
    pub(super) fn abandon(&mut self, objective: &str) {
        let crc = lowercase_crc32(objective);
        let Some(position) = self.entries.iter().position(|entry| entry.key == crc) else {
            return;
        };
        self.entries.remove(position);
        self.version += 1;
        let without = self.running_state();
        self.pending
            .extend(std::iter::repeat_n(without, ABANDON_REPEATS));
    }

    /// The next state to send, one per bundle.
    pub(super) fn take_change(&mut self) -> Option<ObjectivesState> {
        self.pending.pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAYER: u64 = 7;
    const TUTORIAL: &str = "01_SoleSurvivor2";
    const SKINNING_KNIFE: &str = "12_skinningknife";
    const TRUFFLES: &str = "1225_0101_truffles";

    fn task(index: u8, progress: u32, done: bool) -> TaskProgress {
        TaskProgress {
            index,
            task: u32::from(index),
            progress,
            status: if done {
                TaskStatus::Done
            } else {
                TaskStatus::Running
            },
        }
    }

    fn instance(objective: &str) -> u64 {
        instance_of(objective, PLAYER)
    }

    #[test]
    fn an_accept_sends_the_quest_tracked_and_active_above_the_old_lists() {
        const OLD_VERSION: u64 = 40;
        let mut tracker = QuestTracker::new(PLAYER)
            .after(&[TUTORIAL])
            .above(OLD_VERSION);
        tracker.accept(
            SKINNING_KNIFE,
            JournalRecord::of(SKINNING_KNIFE),
            vec![task(0, 0, false)],
        );
        let states: Vec<_> = std::iter::from_fn(|| tracker.take_change()).collect();
        assert_eq!(states.len(), ACCEPT_REPEATS);
        let state = &states[0];
        assert!(
            states.iter().all(|repeat| repeat == state),
            "one state, repeated"
        );
        assert_eq!(state.completed, None, "retail sends no completed list");
        let version = |list: Option<u64>| list.expect("list");
        assert!(version(state.tracked.as_ref().and_then(|list| list.version)) > OLD_VERSION);
        assert_eq!(
            state.tracked.as_ref().map(|list| list.elements.clone()),
            Some(vec![instance(SKINNING_KNIFE)])
        );
        assert_eq!(
            state.active.as_ref().map(|list| list.elements.len()),
            Some(1)
        );
        assert_eq!(
            state.tasks.as_ref().map(|list| list.elements.len()),
            Some(1)
        );
        tracker.update(SKINNING_KNIFE, vec![task(0, 0, false)], false);
        assert_eq!(tracker.take_change(), None, "no change, nothing more");
    }

    #[test]
    fn a_second_quest_joins_the_lists_and_the_first_stays() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.accept(
            SKINNING_KNIFE,
            JournalRecord::of(SKINNING_KNIFE),
            vec![task(0, 0, false)],
        );
        tracker.accept(
            TRUFFLES,
            JournalRecord::of(TRUFFLES),
            vec![task(0, 0, false), task(1, 0, false)],
        );
        let last = std::iter::from_fn(|| tracker.take_change())
            .last()
            .expect("the second accept");
        assert_eq!(
            last.tracked.map(|list| list.elements),
            Some(vec![instance(SKINNING_KNIFE), instance(TRUFFLES)])
        );
        assert_eq!(
            last.active.map(|list| list.elements),
            Some(vec![
                ObjectiveRecord::available(
                    lowercase_crc32(SKINNING_KNIFE),
                    instance(SKINNING_KNIFE)
                ),
                ObjectiveRecord::available(lowercase_crc32(TRUFFLES), instance(TRUFFLES)),
            ])
        );
        let tasks = last.tasks.expect("tasks").elements;
        assert_eq!(
            tasks.len(),
            3,
            "every quest's tasks, each under its instance"
        );
        assert_eq!(tasks[0].instance, instance(SKINNING_KNIFE));
        assert_eq!(tasks[2].instance, instance(TRUFFLES));
        tracker.update(SKINNING_KNIFE, vec![task(0, 1, true)], true);
        let states: Vec<_> = std::iter::from_fn(|| tracker.take_change()).collect();
        let without = states.last().expect("the lists without it");
        assert_eq!(
            without.tracked.as_ref().map(|list| list.elements.clone()),
            Some(vec![instance(TRUFFLES)]),
            "the other quest goes on"
        );
        assert_eq!(
            without.active.as_ref().map(|list| list.elements.len()),
            Some(1)
        );
        assert!(tracker.has(TRUFFLES) && !tracker.has(SKINNING_KNIFE));
    }

    #[test]
    fn the_first_card_tracks_the_quest_and_changes_send_the_whole_task_list() {
        let mut tracker = QuestTracker::new(PLAYER);
        assert_ne!(instance(TUTORIAL), 0);
        assert_ne!(instance(TUTORIAL), instance_of(TUTORIAL, PLAYER + 1));
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(7, 1, false)], false);
        assert!(tracker.take_change().is_some());
        let first = tracker.first_state();
        assert_eq!(
            first.active.map(|list| list.elements),
            Some(vec![ObjectiveRecord::available(
                lowercase_crc32(TUTORIAL),
                instance(TUTORIAL)
            )])
        );
        assert_eq!(first.tasks.map(|list| list.elements.len()), Some(2));
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(7, 1, false)], false);
        assert_eq!(tracker.take_change(), None, "no change, nothing to send");
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(7, 2, false)], false);
        let change = tracker.take_change().expect("change");
        assert_eq!(change.tracked, None);
        let tasks = change.tasks.expect("tasks");
        assert_eq!(tasks.version, Some(FIRST_VERSION + 2));
        assert_eq!(tasks.elements[1].progress, 2);
    }

    #[test]
    fn the_first_card_drops_the_older_queued_states() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.accept(
            TUTORIAL,
            JournalRecord::of(TUTORIAL),
            vec![task(0, 0, false)],
        );
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(1, 0, false)], false);
        let card = tracker.take_first_state();
        assert_eq!(
            card.tasks.as_ref().and_then(|list| list.version),
            Some(tracker.version())
        );
        assert_eq!(
            card.tasks.map(|list| list.elements.len()),
            Some(2),
            "the newest list"
        );
        assert!(card.tracked.is_some() && card.active.is_some());
        assert_eq!(
            tracker.take_change(),
            None,
            "nothing older follows the card"
        );
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(1, 1, true)], true);
        let ended = tracker.take_first_state();
        assert_eq!(
            ended.active.map(|list| list.elements.len()),
            Some(0),
            "an ended quest is not on the card"
        );
    }

    #[test]
    fn a_client_in_game_gets_the_whole_state_once_more() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.update(TUTORIAL, vec![task(0, 0, false)], false);
        let card = tracker.take_first_state();
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(1, 0, false)], false);
        let change = tracker.take_change().expect("the task change");
        tracker.in_game();
        let again = tracker.take_change().expect("the state in game");
        assert_eq!(again, tracker.first_state());
        assert_eq!(
            again.tracked.map(|list| list.elements),
            card.tracked.map(|list| list.elements)
        );
        assert_eq!(
            again.tasks.and_then(|list| list.version),
            change.tasks.and_then(|list| list.version),
            "at the newest lists"
        );
        tracker.update(TUTORIAL, vec![task(0, 1, true), task(1, 1, true)], true);
        while tracker.take_change().is_some() {}
        tracker.in_game();
        assert_eq!(
            tracker.take_change(),
            None,
            "a journal with nothing running sends nothing more"
        );
    }

    /// The instances a completed change of `state` adds.
    fn added(state: &ObjectivesState) -> Vec<(u32, u64)> {
        match &state.completed {
            Some(ReplicatedList::Changes(changes)) => changes
                .iter()
                .map(|change| (change.index, change.value))
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn the_end_sends_the_retail_completed_change_then_the_empty_lists() {
        let done = vec![task(133, 5, true)];
        let mut tracker = QuestTracker::new(PLAYER).after(&["Earlier"]);
        tracker.update(TUTORIAL, done.clone(), false);
        assert!(tracker.take_change().is_some(), "the last task list");
        tracker.update(TUTORIAL, done.clone(), true);
        for _ in 0..END_REPEATS {
            let change = tracker.take_change().expect("completed");
            assert_eq!(
                added(&change),
                [(1, instance(TUTORIAL))],
                "appended after the earlier"
            );
            assert_eq!((&change.tracked, &change.tasks), (&None, &None));
        }
        for _ in 0..EMPTY_REPEATS {
            let empty = tracker.take_change().expect("empty");
            assert_eq!(empty.tracked.map(|list| list.elements), Some(Vec::new()));
            assert!(empty.active.is_some_and(|list| list.elements.is_empty()));
            assert_eq!(empty.tasks.map(|list| list.elements), Some(Vec::new()));
            assert_eq!(empty.completed, None);
        }
        tracker.update(TUTORIAL, done, true);
        assert_eq!(tracker.take_change(), None, "once only");
    }

    #[test]
    fn a_turn_in_ends_a_quest_whose_tasks_are_all_done_already() {
        let mut tracker = QuestTracker::new(PLAYER);
        let done = vec![task(19, 4, true)];
        tracker.update(SKINNING_KNIFE, done.clone(), false);
        assert!(tracker.take_change().is_some(), "the last task list");
        tracker.update(SKINNING_KNIFE, done.clone(), false);
        assert_eq!(tracker.take_change(), None, "it waits for the turn-in");
        tracker.update(SKINNING_KNIFE, done, true);
        let completed = tracker.take_change().expect("completed");
        assert_eq!(added(&completed), [(0, instance(SKINNING_KNIFE))]);
    }

    #[test]
    fn a_dropped_quest_leaves_the_lists_without_joining_the_completed_list() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.update(SKINNING_KNIFE, vec![task(19, 1, false)], false);
        tracker.update(TRUFFLES, vec![task(3, 0, false)], false);
        while tracker.take_change().is_some() {}
        tracker.abandon(SKINNING_KNIFE);
        for _ in 0..ABANDON_REPEATS {
            let without = tracker.take_change().expect("the lists without it");
            let active: Vec<u64> = without
                .active
                .expect("active")
                .elements
                .iter()
                .map(|record| record.instance)
                .collect();
            assert_eq!(active, [instance(TRUFFLES)]);
            assert_eq!(without.completed, None);
        }
        assert_eq!(tracker.take_change(), None);
        tracker.abandon(SKINNING_KNIFE);
        assert_eq!(
            tracker.take_change(),
            None,
            "a quest not running changes nothing"
        );
        tracker.update(SKINNING_KNIFE, vec![task(19, 0, false)], false);
        assert!(tracker.has(SKINNING_KNIFE), "it can be taken again");
    }

    #[test]
    fn a_stopped_branch_goes_out_inactive() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.update(
            TUTORIAL,
            vec![TaskProgress {
                index: 17,
                task: 17,
                progress: 2,
                status: TaskStatus::Stopped,
            }],
            false,
        );
        let change = tracker.take_change().expect("change");
        let entries = change.tasks.expect("tasks").elements;
        assert_eq!(entries[0].state, TaskWireState::Inactive);
    }

    #[test]
    fn an_accept_with_auto_pin_off_is_not_tracked() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.set_auto_pin(false);
        tracker.accept(
            TUTORIAL,
            JournalRecord::of(TUTORIAL),
            vec![task(1, 0, false)],
        );
        let accepted = tracker.take_change().expect("accepted");
        assert_eq!(
            accepted.tracked.map(|list| list.elements),
            Some(Vec::new()),
            "every auto-pin flag off leaves the quest untracked"
        );
    }

    #[test]
    fn a_repeated_quest_runs_and_ends_again() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.accept(
            TRUFFLES,
            JournalRecord::of(TRUFFLES),
            vec![task(0, 0, false)],
        );
        tracker.update(TRUFFLES, vec![task(0, 1, true)], true);
        tracker.accept(
            TRUFFLES,
            JournalRecord::of(TRUFFLES),
            vec![task(0, 0, false)],
        );
        assert!(tracker.has(TRUFFLES), "it runs again");
        while tracker.take_change().is_some() {}
        tracker.update(TRUFFLES, vec![task(0, 1, true)], true);
        let completed = std::iter::from_fn(|| tracker.take_change())
            .find(|state| state.completed.is_some())
            .expect("its second end");
        assert_eq!(added(&completed), [(0, instance(TRUFFLES))]);
    }

    #[test]
    fn a_resumed_quest_goes_out_with_the_first_card_only() {
        let mut tracker = QuestTracker::new(PLAYER);
        tracker.resume(
            TRUFFLES,
            JournalRecord::of(TRUFFLES),
            vec![task(0, 1, false)],
        );
        tracker.resume(
            TRUFFLES,
            JournalRecord::of(TRUFFLES),
            vec![task(0, 1, false)],
        );
        assert_eq!(tracker.take_change(), None, "nothing queued");
        let card = tracker.take_first_state();
        assert_eq!(
            card.tracked.map(|list| list.elements),
            Some(vec![instance(TRUFFLES)]),
            "once"
        );
    }
}
