// SPDX-License-Identifier: AGPL-3.0-or-later
//! Small AI data sets built node by node, for the tests of this crate and of the sim (feature
//! `test-util`). Every value is the test's own; the trees of the game come from Server.pak.

use crate::data::{
    AiData, Archetype, CageAction, Completion, FragmentLength, FragmentVariant, Grid, NavConfig,
    ProcWindow, TagActivity, TreeAsset, TreeRef,
};
use crate::node::{
    ActionTarget, CageParams, Children, CooldownOverride, CooldownSpec, Gates, Motion, NavRayCast,
    NavigationParams, Node, NodeId, NodeKind, PositionType, SelectPositionParams, TargetLimits,
};
use crate::value::{CompareOp, Condition, ConditionGroup, KeyId, NameId, Param, Value};

/// The base utility of a node that sets none (Observed default of `Node.BaseUtility`, study
/// section 1.2).
pub const EPSILON_UTILITY: f32 = 1.19e-7;
/// The length of a test attack: the boar gore's 46 frames at 30 fps over speed 0.85, about 1.8 s.
pub const GORE_S: f32 = 1.8;
/// The completion offset of a test action: the boar gore's `CompletionFragmentOffset` 0.4 s.
pub const COMPLETION_OFFSET_S: f32 = 0.4;

/// A small data set built node by node.
#[derive(Default)]
pub struct Build {
    /// The data so far.
    pub data: AiData,
    /// The tasks of the CAGE actions so far, with their action indexes.
    pub tasks: Vec<(NameId, u32)>,
}

impl Build {
    /// The id of name `text`, added when new.
    pub fn name(&mut self, text: &str) -> NameId {
        match self.data.name_id(text) {
            Some(id) => id,
            None => {
                self.data.names.push(text.to_owned());
                (self.data.names.len() - 1) as NameId
            }
        }
    }

    /// The key named `text`, added when new.
    pub fn key(&mut self, text: &str) -> KeyId {
        match self.data.key(text) {
            Some(key) => key,
            None => {
                let name = self.name(text);
                self.data.keys.push(name);
                (self.data.keys.len() - 1) as KeyId
            }
        }
    }

    /// A node of `kind` with the default utility, interruptable.
    pub fn node(&mut self, kind: NodeKind) -> NodeId {
        self.node_with(kind, EPSILON_UTILITY, true)
    }

    /// A node of `kind` with `utility`.
    pub fn node_with(&mut self, kind: NodeKind, utility: f32, interruptable: bool) -> NodeId {
        let name = self.name("node");
        self.data.nodes.push(Node {
            name,
            utility,
            interruptable,
            kind,
        });
        (self.data.nodes.len() - 1) as NodeId
    }

    /// The child list `ids`.
    pub fn children(&mut self, ids: &[NodeId]) -> Children {
        let first = self.data.child_ids.len() as u32;
        self.data.child_ids.extend_from_slice(ids);
        Children {
            first,
            count: ids.len() as u32,
        }
    }

    /// An action target with no profile and no limits.
    pub fn target(&mut self) -> ActionTarget {
        ActionTarget {
            profile: None,
            tag_filter: self.name(""),
            limits: TargetLimits {
                min_targets: Param::UNSET,
                max_targets: Param::UNSET,
                top_exclude: Param::UNSET,
                range_behavior: Param::UNSET,
            },
        }
    }

    /// The CAGE fields of a node playing `task`, waiting for its end.
    pub fn cage_params(&mut self, task: &str) -> CageParams {
        let task = self.name(task);
        CageParams {
            task: Param::literal(Value::Text(task)),
            wait_for_completion: true,
            fail_timeout: Param::UNSET,
            nav_ray_cast: NavRayCast {
                direction: Param::UNSET,
                distance: Param::UNSET,
                distance_from_wall: Param::UNSET,
                check_at_execution: false,
                allow_dynamic_scaling: true,
            },
            align_position: Param::UNSET,
            align_orientation: Param::UNSET,
            can_kill_deaths_door: false,
            target: self.target(),
            target_evaluation_ms: Param::UNSET,
        }
    }

    /// The CAGE action of `task` (its action and fragment are named as the task), done
    /// [`COMPLETION_OFFSET_S`] before its end, with its procedural `windows`; its index.
    pub fn action(
        &mut self,
        task: &str,
        length: FragmentLength,
        tags: Vec<TagActivity>,
        windows: Vec<ProcWindow>,
    ) -> u32 {
        let name = self.name(task);
        self.data.cage_actions.push(CageAction {
            list: name,
            action: name,
            fragment: name,
            completion: Some(Completion {
                default_offset_s: COMPLETION_OFFSET_S,
                conditional: Vec::new(),
            }),
            tags,
            triggers: Vec::new(),
            motion_multiplier: None,
            facing: None,
            variants: vec![FragmentVariant {
                tags: Vec::new(),
                frag_tags: Vec::new(),
                length,
                windows,
            }],
            weapon_variants: Vec::new(),
        });
        let index = (self.data.cage_actions.len() - 1) as u32;
        self.tasks.push((name, index));
        index
    }

    /// A CAGE leaf whose action plays `length` and is done [`COMPLETION_OFFSET_S`] before its end.
    pub fn cage(&mut self, task: &str, length: FragmentLength, tags: Vec<TagActivity>) -> NodeId {
        self.action(task, length, tags, Vec::new());
        let params = self.cage_params(task);
        self.node(NodeKind::Cage(Box::new(params)))
    }

    /// A CAGE leaf of [`GORE_S`].
    pub fn gore(&mut self, task: &str) -> NodeId {
        self.cage(task, FragmentLength::Seconds(GORE_S), Vec::new())
    }

    /// A CAGE leaf whose fragment loops.
    pub fn idle(&mut self, task: &str) -> NodeId {
        self.cage(task, FragmentLength::Loops, Vec::new())
    }

    /// A navigation leaf playing `task` to the target, with the slice navigation config named
    /// `config` (the node's own fields unset).
    pub fn navigation(&mut self, task: &str, config: Option<&str>) -> NodeId {
        self.navigation_with(task, config, None)
    }

    /// A navigation leaf playing `task` to the position `to`, with the slice navigation config
    /// named `config`.
    pub fn navigation_to(&mut self, task: &str, config: Option<&str>, to: PositionType) -> NodeId {
        let exactly = Param::literal(Value::Float(0.0));
        let once = Param::literal(Value::Int(1));
        let pick = SelectPositionParams {
            origin: to,
            anchor: PositionType::Own,
            direction: Param::UNSET,
            mode: Param::UNSET,
            check_path_to_origin: false,
            check_path_to_anchor: false,
            start_angle: exactly,
            stop_angle: exactly,
            favor_middle_angle: false,
            range_min: exactly,
            range_max: exactly,
            distance_from_wall: Param::UNSET,
            query_radius: Param::UNSET,
            max_attempts: once,
            start_angle_param: Param::UNSET,
            stop_angle_param: Param::UNSET,
            range_min_param: Param::UNSET,
            range_max_param: Param::UNSET,
            distance_from_wall_param: Param::UNSET,
            query_radius_param: Param::UNSET,
            max_attempts_param: Param::UNSET,
            group_detector: Param::UNSET,
            group_range: Param::UNSET,
        };
        self.navigation_with(task, config, Some(pick))
    }

    fn navigation_with(
        &mut self,
        task: &str,
        config: Option<&str>,
        select_position: Option<SelectPositionParams>,
    ) -> NodeId {
        self.action(task, FragmentLength::Loops, Vec::new(), Vec::new());
        let cage = self.cage_params(task);
        let nav_config = config.map(|config| self.name(config));
        let motion = Motion {
            nav_config,
            max_speed: Param::UNSET,
            min_speed: Param::UNSET,
            linear_acceleration: Param::UNSET,
            linear_deceleration: Param::UNSET,
            arrival_threshold: Param::UNSET,
            target_repath_distance: Param::UNSET,
            allow_dynamic_scaling: true,
            allowed_stuck_resolutions: Param::UNSET,
            give_up_behavior: Param::UNSET,
            select_position,
        };
        self.node(NodeKind::Navigation(Box::new(NavigationParams {
            cage,
            motion,
            nav_key: None,
            turn_rate: Param::UNSET,
            angular_acceleration: Param::UNSET,
            angular_deceleration: Param::UNSET,
            facing_mode: Param::UNSET,
            face_movement_smooth_time: Param::UNSET,
            agent_radius: Param::UNSET,
            teleport: false,
            allow_partial_paths: false,
            calculate_path_on_exec: false,
            start_action_override: None,
            turn_action_override: None,
            observe_heat: false,
            apply_heat: false,
            max_corridor_width: Param::UNSET,
            nearby_nav_link_removal_threshold: Param::UNSET,
            waypoint_look_ahead_radius: Param::UNSET,
            max_steering_force: Param::UNSET,
            max_avoidance_force: Param::UNSET,
            max_separation_force: Param::UNSET,
            steering_ramp_down_range_min: Param::UNSET,
            steering_ramp_down_range_max: Param::UNSET,
            enable_avoidance: false,
            avoidance_look_ahead_horizon: Param::UNSET,
            avoidance_force: Param::UNSET,
            enable_separation: false,
            separation_circle_force: Param::UNSET,
            separation_circle_radius: Param::UNSET,
            separation_cone_force: Param::UNSET,
            separation_cone_range: Param::UNSET,
            separation_cone_angle: Param::UNSET,
        })))
    }

    /// A blackboard condition of `key op value` over `child`.
    pub fn gate(&mut self, key: &str, op: CompareOp, value: f32, child: NodeId) -> NodeId {
        let condition = self.condition(key, op, value);
        self.node(NodeKind::BlackboardCondition {
            child: Some(child),
            gates: Box::new(Gates {
                entry: ConditionGroup {
                    any: false,
                    conditions: vec![condition],
                },
                ..Gates::default()
            }),
        })
    }

    /// A distance condition over `child`: the position of key `key` is farther than `distance`
    /// from the mob.
    pub fn farther_than(&mut self, key: &str, distance: Param, child: NodeId) -> NodeId {
        let condition = Condition {
            left: Param::of_key(self.key(key)),
            op: CompareOp::GreaterThan,
            right: distance,
            auto_plan: false,
            allow_dynamic_scaling: false,
        };
        self.node(NodeKind::DistanceCondition {
            child: Some(child),
            gates: Box::new(Gates {
                entry: ConditionGroup {
                    any: false,
                    conditions: vec![condition],
                },
                ..Gates::default()
            }),
            vertical_offset: Param::UNSET,
            vertical_threshold: Param::UNSET,
            allow_dynamic_scaling: false,
        })
    }

    /// Status effect `effect` while `child` runs, removed at its end.
    pub fn status_effect(&mut self, effect: &str, child: NodeId) -> NodeId {
        let effect = self.name(effect);
        self.node(NodeKind::StatusEffect {
            child: Some(child),
            effect,
            remove_on_end: true,
        })
    }

    /// The condition `key op value`.
    pub fn condition(&mut self, key: &str, op: CompareOp, value: f32) -> Condition {
        Condition {
            left: Param::of_key(self.key(key)),
            op,
            right: Param::literal(Value::Float(value)),
            auto_plan: false,
            allow_dynamic_scaling: false,
        }
    }

    /// Cooldown `name` of `min` to `max` seconds.
    pub fn cooldown_spec(&mut self, name: &str, min: f32, max: f32) -> CooldownSpec {
        CooldownSpec {
            name: self.name(name),
            min: Param::literal(Value::Float(min)),
            max: Param::literal(Value::Float(max)),
            override_rule: CooldownOverride::Always,
        }
    }

    /// A cooldown decorator of `name` (`min` to `max` seconds) over `child`.
    pub fn cooldown(&mut self, name: &str, min: f32, max: f32, child: NodeId) -> NodeId {
        let cooldown = self.cooldown_spec(name, min, max);
        self.node(NodeKind::Cooldown {
            child: Some(child),
            cooldown,
            omit_on_failure: false,
        })
    }

    /// A selector of `ids`.
    pub fn selector(&mut self, ids: &[NodeId]) -> NodeId {
        let children = self.children(ids);
        self.node(NodeKind::Selector(children))
    }

    /// A sequencer of `ids`.
    pub fn sequence(&mut self, ids: &[NodeId]) -> NodeId {
        let children = self.children(ids);
        self.node(NodeKind::Sequencer(children))
    }

    /// The data with one tree of `root`, one grid of every action built and one archetype of
    /// that tree and grid.
    pub fn finish(mut self, root: NodeId) -> AiData {
        self.tasks.sort_unstable();
        let path = self.name("tree");
        self.data.trees.push(TreeAsset {
            path,
            root: Some(root),
            expanded_nodes: self.data.nodes.len() as u32,
        });
        let slice = self.name("slice");
        self.data.grids.push(Grid {
            path,
            animation_database: None,
            actions: self.tasks.clone(),
            aliases: Vec::new(),
            tasks: self.tasks,
        });
        self.data.archetypes.push(Archetype {
            slice,
            trees: vec![TreeRef {
                path,
                tree: Some(0),
            }],
            config: Vec::new(),
            auto_evaluate: Vec::new(),
            auto_evaluation_s: None,
            ambient_vision: None,
            combat_vision: None,
            group_aggro_distance: None,
            group_aggro_cap: None,
            targeting_profile: None,
            aggro_delay_override_s: None,
            nav_configs: Vec::new(),
            max_slope_deg: None,
            grid: Some(0),
            animation_database: None,
            damage_table: None,
            damage_table_id: None,
            main_hand_item: None,
            default_tags: Vec::new(),
            triggers: Vec::new(),
        });
        self.data
    }
}

/// A navigation config of `kind` with the beach boar's `RunToTarget` numbers (slice `boar`: 2 to
/// 7 m/s, 30 m/s², 120 °/s, arrival 0.5 m, repath 0.5 m).
pub fn run_config(kind: NameId) -> NavConfig {
    NavConfig {
        kind,
        max_speed: 7.0,
        min_speed: 2.0,
        turn_rate_deg: 120.0,
        linear_acceleration: 30.0,
        linear_deceleration: 30.0,
        arrival_threshold: 0.5,
        target_repath_distance: 0.5,
        select_position: None,
        teleport: false,
        turn_action: None,
        start_action: None,
        start_rule: 1,
        action_params: Vec::new(),
    }
}
