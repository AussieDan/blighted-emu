// SPDX-License-Identifier: AGPL-3.0-or-later
//! Quick courses: mount races, rallies, tours and time trials (quick course note 2026-10-30,
//! `local-data/findings`). A course runs from its quest task (`TaskQuickCourse`): the server
//! finds the node hits from the rider's position (retail: the client sends nothing for a
//! start, a hit or a finish), keeps the player's `activeRaceSection` (6044) at the next section
//! to reach, and at the finish sends `OnQuickCourseEnded` (6065, result 6) before it clears it.
//!
//! **Observed** (retail `ef_tour_0-Hermit-s-Rally`): the course became active 0.9 s after the
//! mount summon completed, with the course's quest task running; each hit came when the input
//! position was 8.5 to 9.3 m (on the ground plane) from the node, whose `DetectionRadius` is 5
//! ([`HIT_MARGIN_M`]). **Inferred**: a running course task and a mount start the course; a
//! timed course whose time runs out ends without the 6065 (no retail fail was captured, so its
//! result value is Unknown) and starts again only after the rider got off. **Unknown**: what a
//! dismount does to a running course (here it ends it as a timeout does).

use bevy_ecs::entity::Entity;
use nw_formats::objectstream::lowercase_crc32;
use nw_messages::facet::FacetHeader;
use nw_messages::items::FullList;
use nw_messages::mount::{MountServerMsg, PLAYER_QUICK_COURSE_FACET};
use nw_messages::quick_course_state::QuickCourseState;

use super::mount::push_mount_message;
use super::{Body, Player, SimWorld};
use crate::objectives::Event;

/// How much farther than its `DetectionRadius` a node counts as hit (Observed: retail's hits
/// came 8.5 to 9.3 m from nodes of radius 5).
pub(super) const HIT_MARGIN_M: f32 = 4.5;
/// The `OnQuickCourseEnded` result of a finished course (all 42 retail calls).
const FINISHED: u8 = 6;
/// Milliseconds per second.
const MS_PER_SECOND: u64 = 1_000;

/// The course a player rides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ActiveCourse {
    /// The `QuickCourseId`.
    course: String,
    /// The index of the next section to reach.
    next: usize,
    /// When its time runs out (server ms); `None`: no clock yet, or an untimed course.
    deadline_ms: Option<u64>,
}

/// A player's quick course state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct PlayerCourse {
    active: Option<ActiveCourse>,
    /// A course ended undone: it starts again only after the rider gets off.
    ended_undone: bool,
}

/// What a tick found for one rider.
enum Step {
    Start(String),
    Hit { course: String, section: usize },
    Undone,
    Idle,
}

impl SimWorld {
    /// Runs the quick courses of every player at `now_ms`.
    pub(super) fn tick_quick_courses(&mut self, now_ms: u64) {
        let players: Vec<(Entity, u64)> = self
            .world
            .query::<(Entity, &Player)>()
            .iter(&self.world)
            .map(|(entity, player)| (entity, player.number))
            .collect();
        for (entity, number) in players {
            match self.course_step(entity, number, now_ms) {
                Step::Start(course) => self.start_course(entity, &course, now_ms),
                Step::Hit { course, section } => {
                    self.course_hit(entity, number, &course, section, now_ms);
                }
                Step::Undone => self.end_course(entity, number, false),
                Step::Idle => {}
            }
        }
    }

    /// What player `number` (`entity`) does on its course at `now_ms`.
    fn course_step(&mut self, entity: Entity, number: u64, now_ms: u64) -> Step {
        let content = &self.content;
        let Some(at) = self
            .world
            .get::<Body>(entity)
            .map(|body| body.motion.position())
        else {
            return Step::Idle;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return Step::Idle;
        };
        let riding = player.mount.riding().is_some();
        if !riding {
            player.course.ended_undone = false;
        }
        let Some(active) = player.course.active.as_ref() else {
            if !riding || player.course.ended_undone {
                return Step::Idle;
            }
            let wanted = self.quests.get(&number).and_then(|quests| {
                quests
                    .iter()
                    .flat_map(|quest| quest.run.courses())
                    .find(|course| path_of(content, course).is_some())
                    .map(str::to_owned)
            });
            return wanted.map_or(Step::Idle, Step::Start);
        };
        if !riding || active.deadline_ms.is_some_and(|deadline| now_ms > deadline) {
            return Step::Undone;
        }
        let Some(node) =
            path_of(content, &active.course).and_then(|path| path.nodes.get(active.next))
        else {
            return Step::Idle;
        };
        let radius = content
            .quick_courses
            .node_type(&node.node_type)
            .map_or(0.0, |node_type| node_type.radius_m);
        let [x, y, _] = node.position;
        if (at.x - x).hypot(at.y - y) <= radius + HIT_MARGIN_M {
            Step::Hit {
                course: active.course.clone(),
                section: active.next,
            }
        } else {
            Step::Idle
        }
    }

    /// Player `entity` starts course `course` at `now_ms`: its clock is the course's starting
    /// time, and its client gets the course at section 0.
    fn start_course(&mut self, entity: Entity, course: &str, now_ms: u64) {
        let starting = self
            .content
            .quick_courses
            .course(course)
            .filter(|rules| rules.timed)
            .and_then(|rules| rules.starting_seconds);
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player.course.active = Some(ActiveCourse {
            course: course.to_owned(),
            next: 0,
            deadline_ms: starting
                .map(|seconds| now_ms.saturating_add(u64::from(seconds) * MS_PER_SECOND)),
        });
        tracing::info!(player = player.number, course, "quick course started");
        push_course_state(&mut player);
    }

    /// Player `number` (`entity`) hit section `section` of course `course` at `now_ms`: its
    /// clock gains the node's time, and the next section is due; the last one finishes it.
    fn course_hit(
        &mut self,
        entity: Entity,
        number: u64,
        course: &str,
        section: usize,
        now_ms: u64,
    ) {
        let content = std::sync::Arc::clone(&self.content);
        let rules = content.quick_courses.course(course);
        let nodes = path_of(&content, course).map_or(0, |path| path.nodes.len());
        let added = path_of(&content, course)
            .and_then(|path| path.nodes.get(section))
            .and_then(|node| content.quick_courses.node_type(&node.node_type))
            .zip(rules.filter(|rules| rules.timed))
            .map(|(node_type, rules)| rules.added_ms(node_type));
        let last = section + 1 >= nodes;
        if let Some(mut player) = self.world.get_mut::<Player>(entity)
            && let Some(active) = player.course.active.as_mut()
        {
            active.next = section + 1;
            if let Some(added) = added {
                active.deadline_ms =
                    Some(active.deadline_ms.unwrap_or(now_ms).saturating_add(added));
            }
            // Retail's last value was the finish's section; the finish empties the list.
            if !last {
                push_course_state(&mut player);
            }
        }
        let passed = u32::try_from(section).unwrap_or(u32::MAX);
        self.quest_event(
            number,
            Event::QuickCourseSection {
                course,
                section: passed,
                sections: u32::try_from(nodes).unwrap_or(u32::MAX),
            },
            now_ms,
        );
        if last {
            self.end_course(entity, number, true);
        }
    }

    /// Player `number`'s course ends: `finished` (the client gets 6065; the quests took the finish
    /// as its last section) or undone; the client's course list empties.
    fn end_course(&mut self, entity: Entity, number: u64, finished: bool) {
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let Some(active) = player.course.active.take() else {
            return;
        };
        player.course.ended_undone = !finished;
        if finished {
            let message = MountServerMsg::QuickCourseEnded {
                header: FacetHeader::of(PLAYER_QUICK_COURSE_FACET, number),
                course: lowercase_crc32(&active.course),
                result: FINISHED,
            };
            push_mount_message(
                &mut self.combat_messages,
                player.connection.clone(),
                &message,
            );
        }
        push_course_state(&mut player);
        tracing::info!(player = number, course = %active.course, finished, "quick course ended");
    }
}

/// The path course `course` rides: its reference course's, else its own.
fn path_of<'a>(
    content: &'a crate::player_content::PlayerContent,
    course: &str,
) -> Option<&'a nw_world::quick_courses::QuickCoursePath> {
    let path = content
        .quick_courses
        .course(course)
        .map_or(course, |rules| rules.path_id());
    content.quick_course_paths.get(path)
}

/// Queues `player`'s course state: its course at its next section, or none.
fn push_course_state(player: &mut Player) {
    let version = player.list_version + 1;
    let active = player
        .course
        .active
        .iter()
        .map(|active| {
            (
                lowercase_crc32(&active.course),
                // Courses have a few tens of sections.
                u8::try_from(active.next).unwrap_or(u8::MAX),
            )
        })
        .collect();
    let state = QuickCourseState {
        active: FullList {
            version: Some(version),
            elements: active,
        },
    };
    match state.member() {
        Ok(member) => {
            player.list_version = version;
            player.extra_members.push(member);
        }
        Err(error) => tracing::warn!(%error, "quick course state not encoded"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nw_bus::PlayerMounts;
    use nw_content::quick_courses::{QuickCourse, QuickCourseNodeType, QuickCourseRules};
    use nw_messages::MemberState;
    use nw_messages::quick_course_state::QUICK_COURSE_STATE_TYPE_INDEX;
    use nw_sim_core::{Motion, Position};
    use nw_world::quick_courses::{QuickCourseNode, QuickCoursePath, QuickCoursePaths};

    use super::*;
    use crate::objectives::{TaskBook, TaskRow};
    use crate::player_content::{ObjectiveRow, QuestBook};
    use crate::world::mount::PlayerMount;
    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    const PLAYER: u64 = 1;
    /// Hermit's Rally (`EF_Tour_0`, cut to one checkpoint): its quest and its nodes.
    const COURSE: &str = "EF_Tour_0";
    const QUEST: &str = "EF_Tour_0_Objective";
    const START: [f32; 3] = [8921.0, 5130.3, 151.1];
    const CHECKPOINT: [f32; 3] = [8957.4, 5008.1, 150.0];
    const FINISH: [f32; 3] = [9492.9, 4260.6, 110.4];
    /// `StartingTimerSeconds` 30, `Start` 35 s, `Checkpoint45` 45 s.
    const STARTING_MS: u64 = 30_000;
    const START_MS: u64 = 35_000;
    /// A mount id of the retail captures.
    const MOUNT: u32 = 0x0c13_84ae;

    fn world() -> (SimWorld, Entity) {
        let mut world = world_with_spawns(Vec::new());
        let content = Arc::make_mut(&mut world.content);
        content.quests = QuestBook::build(
            [ObjectiveRow {
                id: QUEST.to_owned(),
                root: "root".to_owned(),
                ..ObjectiveRow::default()
            }],
            TaskBook::new(vec![
                TaskRow {
                    task_id: "root".to_owned(),
                    kind: "ConsecutiveTaskContainer".to_owned(),
                    sub_tasks: vec!["ride".to_owned()],
                    ..TaskRow::default()
                },
                TaskRow {
                    task_id: "ride".to_owned(),
                    kind: "TaskQuickCourse".to_owned(),
                    item_name: Some(COURSE.to_owned()),
                    ..TaskRow::default()
                },
            ]),
        );
        let node_type = |id: &str, add_seconds| {
            (
                id.to_owned(),
                QuickCourseNodeType {
                    add_seconds,
                    radius_m: 5.0,
                },
            )
        };
        content.quick_courses = QuickCourseRules::build(
            [QuickCourse {
                id: COURSE.to_owned(),
                timed: true,
                starting_seconds: Some(30),
                path: None,
                node_time_multiplier: 1.0,
            }],
            [
                node_type("Start", 35),
                node_type("Checkpoint45", 45),
                node_type("Finish", 0),
            ],
        );
        let node = |position, node_type: &str| QuickCourseNode {
            position,
            node_type: node_type.to_owned(),
        };
        content.quick_course_paths = QuickCoursePaths::of([QuickCoursePath {
            course: COURSE.to_owned(),
            nodes: vec![
                node(START, "Start"),
                node(CHECKPOINT, "Checkpoint45"),
                node(FINISH, "Finish"),
            ],
        }]);
        world
            .admit(&admit_at(PLAYER, 0.0, true), NOW, SERVER_TIME)
            .expect("admit");
        let entity = world.by_connection[&connection(PLAYER)];
        (world, entity)
    }

    fn mount(world: &mut SimWorld, entity: Entity) {
        world.world.get_mut::<Player>(entity).expect("player").mount = PlayerMount::of(
            &PlayerMounts {
                riding: Some(MOUNT),
                ..PlayerMounts::default()
            },
            0,
            0,
        );
    }

    fn ride_to(world: &mut SimWorld, entity: Entity, [x, y, z]: [f32; 3], now_ms: u64) {
        let limits = world.settings.limits;
        world.world.get_mut::<Body>(entity).expect("body").motion =
            Motion::new(Position { x, y, z }, now_ms, &limits);
        world.tick_quick_courses(now_ms);
    }

    /// The next sections of the course states queued for the player (`None`: an empty list),
    /// and the queue emptied.
    fn states(world: &mut SimWorld, entity: Entity) -> Vec<Option<u8>> {
        let mut player = world.world.get_mut::<Player>(entity).expect("player");
        let states = player
            .extra_members
            .iter()
            .filter_map(|member| match &member.state {
                MemberState::Recorded { class, body }
                    if *class == QUICK_COURSE_STATE_TYPE_INDEX =>
                {
                    // A list of one entry ends with its value; an empty one with its count 0.
                    (body.len() > 6)
                        .then(|| body.last().copied())
                        .or(Some(None))
                }
                _ => None,
            })
            .collect();
        player.extra_members.clear();
        states
    }

    fn active(world: &SimWorld, entity: Entity) -> Option<ActiveCourse> {
        world
            .world
            .get::<Player>(entity)
            .and_then(|player| player.course.active.clone())
    }

    #[test]
    fn a_rider_with_the_course_task_rides_it_to_its_end() {
        let (mut world, entity) = world();
        world.accept_quest(PLAYER, QUEST, None, NOW).expect("quest");
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            NOW,
        );
        assert!(active(&world, entity).is_none(), "on foot: no course");
        states(&mut world, entity);
        mount(&mut world, entity);
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            NOW,
        );
        assert_eq!(states(&mut world, entity), [Some(0)], "the course starts");
        assert_eq!(
            active(&world, entity).and_then(|course| course.deadline_ms),
            Some(NOW + STARTING_MS)
        );
        let at_start = NOW + 5_000;
        ride_to(
            &mut world,
            entity,
            [START[0] + 9.0, START[1], START[2]],
            at_start,
        );
        assert_eq!(states(&mut world, entity), [Some(1)], "9 m away hits");
        let ride = |world: &SimWorld| {
            world
                .quests_of_player(PLAYER)
                .iter()
                .find(|quest| quest.def.id == QUEST)
                .and_then(|quest| {
                    quest
                        .run
                        .progress()
                        .into_iter()
                        .find(|task| task.task == lowercase_crc32("ride"))
                })
                .map(|task| task.progress)
        };
        assert_eq!(ride(&world), Some(1), "the tracker counts the start gate");
        assert_eq!(
            active(&world, entity).and_then(|course| course.deadline_ms),
            Some(NOW + STARTING_MS + START_MS),
            "the start's time is added"
        );
        ride_to(&mut world, entity, CHECKPOINT, at_start + 10_000);
        assert_eq!(states(&mut world, entity), [Some(2)]);
        assert_eq!(ride(&world), Some(2), "and the checkpoint");
        ride_to(&mut world, entity, FINISH, at_start + 40_000);
        assert_eq!(
            states(&mut world, entity),
            [None],
            "the list empties at the end"
        );
        assert!(active(&world, entity).is_none());
        let quest = world
            .quests_of_player(PLAYER)
            .iter()
            .find(|quest| quest.def.id == QUEST)
            .map(|quest| quest.run.finished());
        assert_ne!(quest, Some(false), "the course task is done");
    }

    #[test]
    fn a_course_whose_time_runs_out_ends_and_waits_for_a_new_ride() {
        let (mut world, entity) = world();
        world.accept_quest(PLAYER, QUEST, None, NOW).expect("quest");
        mount(&mut world, entity);
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            NOW,
        );
        states(&mut world, entity);
        let late = NOW + STARTING_MS + 1;
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            late,
        );
        assert_eq!(states(&mut world, entity), [None], "out of time");
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            late + 1,
        );
        assert!(
            active(&world, entity).is_none(),
            "not again while still mounted"
        );
        world.world.get_mut::<Player>(entity).expect("player").mount =
            PlayerMount::of(&PlayerMounts::default(), 0, 0);
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            late + 2,
        );
        mount(&mut world, entity);
        ride_to(
            &mut world,
            entity,
            [START[0] + 60.0, START[1], START[2]],
            late + 3,
        );
        assert!(
            active(&world, entity).is_some(),
            "a new ride starts it again"
        );
    }
}
