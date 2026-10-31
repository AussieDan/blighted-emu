// SPDX-License-Identifier: AGPL-3.0-or-later
//! Blackboard values, tree parameters and conditions (behavior-tree study, section 1.2).
//!
//! A `BehaviorParameter` names a blackboard key and/or holds a literal: with a key name the node
//! reads the blackboard and the literal is the fall-back; without one the literal is the value
//! (Observed field layout). The client's coerce functions log "Failed to coerce blackboard
//! variable "%s" to a boolean value, and no fall back value is present" (exe rdata, study
//! `ai-rdata.txt`): a key that is not set and has no fall-back gives no value, and the node that
//! needs it fails.

use serde::{Deserialize, Serialize};

/// Index of a string in [`crate::data::AiData::names`].
pub type NameId = u32;
/// Index of a blackboard key in [`crate::data::AiData::keys`].
pub type KeyId = u16;

/// A blackboard value or a parameter literal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Value {
    /// `bool`.
    Bool(bool),
    /// `int`, `unsigned int`, `unsigned char`, `unsigned short`, `AZ::u64` (as far as it fits).
    Int(i64),
    /// `float`.
    Float(f32),
    /// `AZStd::string`, by name id.
    Text(NameId),
    /// `Vector3` (metres).
    Vec3([f32; 3]),
}

impl Value {
    /// The value as a number: a bool is 0 or 1 (the trees compare bool keys with 0 and 1, for
    /// example `$Target_Directly_Reachable Equal 0` and `$WaypointLeashActive|0 Equal 1`,
    /// Observed in boar_base and DistanceLeash).
    pub fn as_number(self) -> Option<f64> {
        match self {
            Self::Bool(value) => Some(if value { 1.0 } else { 0.0 }),
            // The trees hold small counts; f64 keeps every i64 below 2^53 exactly.
            Self::Int(value) => Some(value as f64),
            Self::Float(value) => Some(f64::from(value)),
            Self::Text(_) | Self::Vec3(_) => None,
        }
    }

    /// The value as a bool: a number is true when not zero, a position is true (the trees test
    /// a position key against `True`, for example `$Last_Damage_Pos Equal True` in boar_base;
    /// Inferred: a set position counts as true), text has no bool.
    pub fn as_bool(self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(value),
            Self::Int(value) => Some(value != 0),
            Self::Float(value) => Some(value != 0.0),
            Self::Vec3(_) => Some(true),
            Self::Text(_) => None,
        }
    }

    /// The value as a position.
    pub fn as_vec3(self) -> Option<[f32; 3]> {
        match self {
            Self::Vec3(value) => Some(value),
            Self::Bool(_) | Self::Int(_) | Self::Float(_) | Self::Text(_) => None,
        }
    }
}

/// `Condition.Operator` (Observed: `FUN_1426053b0` switch, study section 1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareOp {
    /// 1.
    Equal,
    /// 2.
    NotEqual,
    /// 3.
    LessThan,
    /// 4.
    GreaterThan,
    /// 5.
    LessThanOrEqual,
    /// 6.
    GreaterThanOrEqual,
}

impl CompareOp {
    /// The operator of a stored number; `None` for 0 (`None`) and out-of-range numbers.
    pub fn from_stored(value: i64) -> Option<Self> {
        Some(match value {
            1 => Self::Equal,
            2 => Self::NotEqual,
            3 => Self::LessThan,
            4 => Self::GreaterThan,
            5 => Self::LessThanOrEqual,
            6 => Self::GreaterThanOrEqual,
            _ => return None,
        })
    }

    fn holds(self, ordering: std::cmp::Ordering) -> bool {
        use std::cmp::Ordering::{Equal, Greater, Less};
        match self {
            Self::Equal => ordering == Equal,
            Self::NotEqual => ordering != Equal,
            Self::LessThan => ordering == Less,
            Self::GreaterThan => ordering == Greater,
            Self::LessThanOrEqual => ordering != Greater,
            Self::GreaterThanOrEqual => ordering != Less,
        }
    }

    /// Whether `left op right` holds. Numbers (and bools as 0/1) compare by value; a bool
    /// compares with anything that has a bool; text and positions compare only for (in)equality
    /// with their own kind. Any other pair cannot be compared and the condition fails (the client
    /// logs "Cannot Compare %s to %s" and fails the condition, study `ai-rdata.txt`).
    pub fn compare(self, left: Value, right: Value) -> bool {
        if let (Some(a), Some(b)) = (left.as_number(), right.as_number()) {
            return a
                .partial_cmp(&b)
                .is_some_and(|ordering| self.holds(ordering));
        }
        let equal = match (left, right) {
            (Value::Text(a), Value::Text(b)) => a == b,
            (Value::Vec3(a), Value::Vec3(b)) => a == b,
            (Value::Bool(_), _) | (_, Value::Bool(_)) => match (left.as_bool(), right.as_bool()) {
                (Some(a), Some(b)) => a == b,
                _ => return false,
            },
            _ => return false,
        };
        match self {
            Self::Equal => equal,
            Self::NotEqual => !equal,
            Self::LessThan
            | Self::GreaterThan
            | Self::LessThanOrEqual
            | Self::GreaterThanOrEqual => false,
        }
    }
}

/// A `BehaviorParameter`: a blackboard key, a literal, or both (the literal is then the
/// fall-back). Neither means the editor default was kept and no value was stored
/// (`IsDefaultValue` true with an empty `Storage`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Param {
    /// The blackboard key it reads.
    pub key: Option<KeyId>,
    /// The literal, or the fall-back of the key.
    pub value: Option<Value>,
}

impl Param {
    /// A literal.
    pub const fn literal(value: Value) -> Self {
        Self {
            key: None,
            value: Some(value),
        }
    }

    /// A key without a fall-back.
    pub const fn of_key(key: KeyId) -> Self {
        Self {
            key: Some(key),
            value: None,
        }
    }

    /// No value.
    pub const UNSET: Self = Self {
        key: None,
        value: None,
    };

    /// Whether neither a key nor a literal is set.
    pub const fn is_unset(&self) -> bool {
        self.key.is_none() && self.value.is_none()
    }

    /// The value: the key's value when the blackboard has it, else the literal.
    pub fn resolve(&self, board: &impl Blackboard) -> Option<Value> {
        self.key.and_then(|key| board.value(key)).or(self.value)
    }

    /// The value as a number.
    pub fn number(&self, board: &impl Blackboard) -> Option<f64> {
        self.resolve(board).and_then(Value::as_number)
    }

    /// The value as a bool.
    pub fn boolean(&self, board: &impl Blackboard) -> Option<bool> {
        self.resolve(board).and_then(Value::as_bool)
    }
}

/// Read access to a mob's blackboard.
pub trait Blackboard {
    /// The value of `key`, when set.
    fn value(&self, key: KeyId) -> Option<Value>;
    /// The scale of a dynamically scaled entity (engine key `Dynamic_Entity_Scalar`), when set.
    fn dynamic_scale(&self) -> Option<f64>;
}

/// A reference reads the blackboard it refers to (the sim reads node fields through the
/// `&dyn Blackboard` of [`crate::brain::World`]).
impl<B: Blackboard + ?Sized> Blackboard for &B {
    fn value(&self, key: KeyId) -> Option<Value> {
        (**self).value(key)
    }

    fn dynamic_scale(&self) -> Option<f64> {
        (**self).dynamic_scale()
    }
}

/// `Condition`: `LeftOperand Operator RightOperand`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    /// The left operand.
    pub left: Param,
    /// The operator.
    pub op: CompareOp,
    /// The right operand.
    pub right: Param,
    /// `AutoPlan`.
    pub auto_plan: bool,
    /// `AllowDynamicScaling`: a number on the right is multiplied by the entity's
    /// `Dynamic_Entity_Scalar` when the engine sets one (Inferred from the names; the client logs
    /// "Attempting to scale a value beyond the max. valueScalar= %.4f").
    pub allow_dynamic_scaling: bool,
}

impl Condition {
    /// Whether the condition holds; a missing operand fails it (the client logs "Left or Right
    /// operands are missing values" and fails).
    pub fn holds(&self, board: &impl Blackboard) -> bool {
        let (Some(left), Some(right)) = (self.left.resolve(board), self.right.resolve(board))
        else {
            return false;
        };
        self.op.compare(left, self.scaled(right, board))
    }

    /// `right` scaled by the dynamic entity scale when the condition allows it.
    pub fn scaled(&self, right: Value, board: &impl Blackboard) -> Value {
        match (self.allow_dynamic_scaling, board.dynamic_scale(), right) {
            (true, Some(scale), Value::Float(value)) => {
                Value::Float((f64::from(value) * scale) as f32)
            }
            (true, Some(scale), Value::Int(value)) => Value::Float((value as f64 * scale) as f32),
            _ => right,
        }
    }
}

/// `ConditionGroup`: conditions joined by And or Or (study section 1.4: 0 And, 1 Or, Inferred
/// from `Target_Angle < TurnMinAngle Or > TurnMaxAngle` in Turn.behaviortree).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConditionGroup {
    /// Whether one condition is enough (Or) instead of all (And).
    pub any: bool,
    /// The conditions; empty means the group is not used.
    pub conditions: Vec<Condition>,
}

impl ConditionGroup {
    /// Whether the group has no condition.
    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }

    /// Whether the group holds; an empty group holds.
    pub fn holds(&self, board: &impl Blackboard) -> bool {
        if self.any {
            self.conditions.is_empty() || self.conditions.iter().any(|c| c.holds(board))
        } else {
            self.conditions.iter().all(|c| c.holds(board))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIST_KEY: KeyId = 3;
    const FLAG_KEY: KeyId = 4;
    const NEAR_M: f32 = 3.0;
    const FAR_M: f32 = 7.0;

    #[derive(Default)]
    struct Board {
        dist: Option<f32>,
        flag: Option<bool>,
        scale: Option<f64>,
    }

    impl Blackboard for Board {
        fn value(&self, key: KeyId) -> Option<Value> {
            match key {
                DIST_KEY => self.dist.map(Value::Float),
                FLAG_KEY => self.flag.map(Value::Bool),
                _ => None,
            }
        }

        fn dynamic_scale(&self) -> Option<f64> {
            self.scale
        }
    }

    fn dist_vs(op: CompareOp, limit: f32) -> Condition {
        Condition {
            left: Param {
                key: Some(DIST_KEY),
                value: None,
            },
            op,
            right: Param::literal(Value::Float(limit)),
            auto_plan: false,
            allow_dynamic_scaling: true,
        }
    }

    #[test]
    fn a_dyn_reference_reads_its_board() {
        let board = Board {
            dist: Some(NEAR_M),
            ..Board::default()
        };
        let shared: &dyn Blackboard = &board;
        assert_eq!(
            Param::of_key(DIST_KEY).number(&shared),
            Some(f64::from(NEAR_M))
        );
        assert_eq!(shared.dynamic_scale(), None);
    }

    #[test]
    fn numbers_compare_by_value() {
        use CompareOp::*;
        let cases = [
            (Equal, 1.0, 1.0, true),
            (NotEqual, 1.0, 1.0, false),
            (LessThan, 1.0, 2.0, true),
            (GreaterThan, 1.0, 2.0, false),
            (LessThanOrEqual, 2.0, 2.0, true),
            (GreaterThanOrEqual, 1.0, 2.0, false),
        ];
        for (op, a, b, expected) in cases {
            assert_eq!(
                op.compare(Value::Float(a), Value::Int(b as i64)),
                expected,
                "{op:?}"
            );
        }
    }

    #[test]
    fn bools_compare_with_numbers_and_positions() {
        assert!(CompareOp::Equal.compare(Value::Bool(false), Value::Int(0)));
        assert!(CompareOp::Equal.compare(Value::Bool(true), Value::Int(1)));
        assert!(CompareOp::Equal.compare(Value::Vec3([1.0, 2.0, 3.0]), Value::Bool(true)));
        assert!(!CompareOp::Equal.compare(Value::Text(1), Value::Bool(true)));
        assert!(CompareOp::Equal.compare(Value::Text(1), Value::Text(1)));
        assert!(CompareOp::NotEqual.compare(Value::Text(1), Value::Text(2)));
        assert!(!CompareOp::LessThan.compare(Value::Text(1), Value::Text(2)));
    }

    #[test]
    fn a_missing_key_without_fall_back_fails_the_condition() {
        let board = Board::default();
        assert!(!dist_vs(CompareOp::LessThan, NEAR_M).holds(&board));
        let mut with_fallback = dist_vs(CompareOp::LessThan, NEAR_M);
        with_fallback.left.value = Some(Value::Float(0.0));
        assert!(with_fallback.holds(&board));
    }

    #[test]
    fn the_boar_mid_range_band_is_an_and_group() {
        // boar_base MidRangeBehavior: Target_Dist <= 7 And Target_Dist > 3.
        let group = ConditionGroup {
            any: false,
            conditions: vec![
                dist_vs(CompareOp::LessThanOrEqual, FAR_M),
                dist_vs(CompareOp::GreaterThan, NEAR_M),
            ],
        };
        let at = |dist: f32| Board {
            dist: Some(dist),
            ..Board::default()
        };
        assert!(group.holds(&at(5.0)));
        assert!(!group.holds(&at(2.0)));
        assert!(!group.holds(&at(8.0)));
        let or = ConditionGroup {
            any: true,
            ..group.clone()
        };
        assert!(or.holds(&at(2.0)));
        assert!(ConditionGroup::default().holds(&at(2.0)));
    }

    #[test]
    fn dynamic_scaling_multiplies_the_right_number() {
        let board = Board {
            dist: Some(5.0),
            scale: Some(2.0),
            ..Board::default()
        };
        assert!(dist_vs(CompareOp::LessThan, NEAR_M).holds(&board));
        let mut fixed = dist_vs(CompareOp::LessThan, NEAR_M);
        fixed.allow_dynamic_scaling = false;
        assert!(!fixed.holds(&board));
    }

    #[test]
    fn a_param_prefers_the_key_over_its_fall_back() {
        let param = Param {
            key: Some(FLAG_KEY),
            value: Some(Value::Bool(false)),
        };
        let board = Board {
            flag: Some(true),
            ..Board::default()
        };
        assert_eq!(param.boolean(&board), Some(true));
        assert_eq!(param.boolean(&Board::default()), Some(false));
        assert!(Param::UNSET.is_unset());
        assert_eq!(Param::UNSET.resolve(&board), None);
    }
}
