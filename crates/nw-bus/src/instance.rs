// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bus of the instances (expeditions, raids, trials; architecture spec sections 4.4 and
//! 4.5): a sim with the instance role runs one instance at a time; the director gives a group an
//! instance sim on demand, keeps the group's instance for re-entry, and ends it.
//!
//! - The open-world sim of a player who activates an instance asks the director
//!   ([`InstanceRequest`] on [`crate::subject::director_instance`], request/reply).
//! - The director asks the social service for the requester's group ([`GroupQuery`] on
//!   [`crate::subject::social_group`], request/reply), then opens the instance on a free
//!   instance sim of the level ([`InstanceCommand::Open`] on [`crate::subject::sim_instance`],
//!   request/reply) or finds the group's open one, and answers [`InstanceReply`].
//! - The players move with the handoff path (`HandoffOffer` with `enter_level`) to the instance
//!   sim and back.
//! - Each instance sim reports its instance in its heartbeat ([`InstanceBeat`]); the director
//!   ends an instance that ran out of time or stayed empty ([`InstanceCommand::Close`]).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::message::{SimSlot, StoredPoint};

/// Sim to director: open the instance of `game_mode` for the group of `requester`, or give back
/// the group's open one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceRequest {
    /// The game mode (`GameModeID`).
    pub game_mode: String,
    /// The level folder of its map.
    pub level: String,
    /// The character that asks.
    pub requester: Uuid,
    /// Most players (`TeamCapacity`).
    pub capacity: u32,
    /// Fewest group members (`MinGroupSize`).
    pub min_group: u32,
    /// Seconds the instance lasts (`OverrideDungeonTimeout`), when the game mode limits it.
    pub timeout_seconds: Option<u64>,
}

/// Why the director refused an instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceRefusal {
    /// The group has fewer members than the game mode needs.
    NotEnoughMembers,
    /// The group has more members than the game mode takes.
    TooManyMembers,
    /// The group has an open instance of another game mode.
    GroupInOtherInstance,
    /// No instance sim of the level is free, and none could be started.
    NoInstanceSim,
    /// The social service did not answer the group query in time.
    GroupTimeout,
    /// The instance sim did not open the instance.
    OpenFailed,
}

/// The answer to [`InstanceRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceReply {
    /// The instance is open: the player goes to `sim`.
    Granted {
        /// The instance.
        instance: Uuid,
        /// The instance sim.
        sim: SimSlot,
        /// The instance's phase id (every member's level change names it).
        phase: u64,
        /// The characters that may enter: the group at the time of the opening.
        members: Vec<Uuid>,
        /// Whether this request opened it (else the group's open instance).
        opened: bool,
    },
    /// No instance.
    Refused {
        /// Why.
        reason: InstanceRefusal,
    },
}

/// Director to the social service: the group of a character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupQuery {
    /// The character.
    pub character: Uuid,
}

/// The answer to [`GroupQuery`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupAnswer {
    /// The character is in a group.
    Group {
        /// The group.
        group: Uuid,
        /// Its leader.
        leader: Uuid,
        /// Every member, the character included.
        members: Vec<Uuid>,
    },
    /// The character is in no group.
    Alone,
}

/// Director to an instance sim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceCommand {
    /// Run this instance (request/reply: [`InstanceOpened`]).
    Open {
        /// The instance.
        instance: Uuid,
        /// The game mode (`GameModeID`).
        game_mode: String,
        /// The phase id of the instance.
        phase: u64,
        /// The characters that may enter.
        members: Vec<Uuid>,
        /// Unix milliseconds when it ends, when the game mode limits it.
        ends_at_ms: Option<u64>,
    },
    /// End this instance: every player in it goes back to its return spot.
    Close {
        /// The instance.
        instance: Uuid,
    },
    /// Add a character that joined the group after the opening.
    Admit {
        /// The instance.
        instance: Uuid,
        /// The character.
        character: Uuid,
    },
}

/// The answer to [`InstanceCommand::Open`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceOpened {
    /// The sim runs the instance.
    Opened,
    /// The sim cannot run it (it runs another, or it has another level).
    Refused {
        /// Why, for the log.
        reason: String,
    },
}

/// What an instance sim tells the director in each heartbeat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceBeat {
    /// The instance it runs; `None` when it is free.
    pub running: Option<RunningInstance>,
}

/// The instance a sim runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInstance {
    /// The instance.
    pub instance: Uuid,
    /// The game mode.
    pub game_mode: String,
    /// The characters that may enter.
    pub members: Vec<Uuid>,
    /// Players in it now.
    pub players: u32,
    /// Milliseconds since its last player left; `None` while a player is in it or none came yet.
    pub empty_for_ms: Option<u64>,
    /// Milliseconds since it opened.
    pub open_for_ms: u64,
    /// The phase id of the instance (the one of its [`InstanceCommand::Open`]); 0 when the sim
    /// does not know it.
    #[serde(default)]
    pub phase: u64,
    /// Most players (`TeamCapacity` of the game mode); 0 when the sim does not know it.
    #[serde(default)]
    pub capacity: u32,
    /// Unix milliseconds when it ends (the `ends_at_ms` of its [`InstanceCommand::Open`]), when
    /// its game mode limits it.
    #[serde(default)]
    pub ends_at_ms: Option<u64>,
}

/// A player's stay in an instance, as its arena state keeps it (world migration 0015): the
/// instance, and where the player goes back to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStay {
    /// The instance.
    pub instance: Uuid,
    /// The game mode.
    pub game_mode: String,
    /// The phase id of the instance.
    pub phase: u64,
    /// The level it came from.
    pub return_level: String,
    /// Where it was when it entered.
    pub return_position: StoredPoint,
    /// Unix nanoseconds of the activation (`m_singlePlayerDungeonTime` of a soul trial).
    pub activated_ns: u64,
    /// The mutation difficulty the player activated the instance with (1 to 3); `None` for a
    /// normal run. The instance sim takes it from the first player that arrives.
    pub mutation_difficulty: Option<u8>,
}

/// A player's arena state as a join, a handoff and a save carry it (world migration 0015): the
/// children of `PlayerArenaReplicatedState` (type 2406) that persist, and its instance stay.
/// Times named `_since_2000_ns` count nanoseconds from 2000-01-01; the others are Unix
/// nanoseconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerArena {
    /// `dungeonCooldownTime`.
    pub dungeon_cooldown_since_2000_ns: u64,
    /// `enterSoloTrialCooldownTime`.
    pub solo_trial_cooldown_since_2000_ns: u64,
    /// `dungeonRanks`: one byte per element.
    pub ranks: Vec<u8>,
    /// `m_lastDungeonsEntered`.
    pub last_dungeon: Option<[u8; 16]>,
    /// `m_lastMutatedDungeonEntered`.
    pub last_mutated_dungeon: Option<[u8; 16]>,
    /// `m_numBaseDungeonsEnteredSinceLastRefresh`.
    pub base_entered: u32,
    /// `m_numMutatedDungeonsEnteredSinceLastRefresh`.
    pub mutated_entered: u32,
    /// `m_numGroupTrialsEnteredSinceLastRefresh`.
    pub group_trials_entered: u32,
    /// `m_nextDungeonBaseMaxLimitRefreshTime`; 0 before the first state.
    pub next_base_refresh_ns: u64,
    /// `m_nextDungeonMutatedMaxLimitRefreshTime`; 0 before the first state.
    pub next_mutated_refresh_ns: u64,
    /// `m_nextGroupTrialMaxLimitRefreshTime`; 0 before the first state.
    pub next_group_trial_refresh_ns: u64,
    /// `hasMutationUnlockAwardBeenGranted`.
    pub mutation_award: bool,
    /// `m_lastDungeonEnteredWasMutated` (`PlayerArenaPersistentData`): the last instance entered
    /// was mutated.
    pub last_dungeon_mutated: bool,
    /// The instance the player is in, or entered last and has not left.
    pub stay: Option<InstanceStay>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::BusId;
    use crate::message::{Point, decode, encode};

    #[test]
    fn instance_messages_survive_the_bus() {
        let request = InstanceRequest {
            game_mode: "DungeonAmrine".to_owned(),
            level: "nw_dungeon_windsward_00".to_owned(),
            requester: Uuid::from_u128(1),
            capacity: 5,
            min_group: 3,
            timeout_seconds: None,
        };
        assert_eq!(
            decode::<InstanceRequest>(&encode(&request).expect("encode")).expect("decode"),
            request
        );
        let reply = InstanceReply::Granted {
            instance: Uuid::from_u128(2),
            sim: SimSlot {
                id: BusId::try_from("inst-1".to_owned()).expect("id"),
                generation: 7,
            },
            phase: u64::MAX,
            members: vec![Uuid::from_u128(1), Uuid::from_u128(3)],
            opened: true,
        };
        assert_eq!(
            decode::<InstanceReply>(&encode(&reply).expect("encode")).expect("decode"),
            reply
        );
        let arena = PlayerArena {
            ranks: vec![3, 3, 3],
            last_dungeon: Some([0x2d; 16]),
            base_entered: 2,
            stay: Some(InstanceStay {
                instance: Uuid::from_u128(2),
                game_mode: "DungeonAmrine".to_owned(),
                phase: 9,
                return_level: "newworld_vitaeeterna".to_owned(),
                return_position: StoredPoint::of(Point {
                    x: 7700.5,
                    y: 7300.25,
                    z: 120.0,
                }),
                activated_ns: 1_792_670_400_250_000_000,
                mutation_difficulty: Some(2),
            }),
            ..PlayerArena::default()
        };
        assert_eq!(
            decode::<PlayerArena>(&encode(&arena).expect("encode")).expect("decode"),
            arena
        );
        let beat = InstanceBeat {
            running: Some(RunningInstance {
                instance: Uuid::from_u128(2),
                game_mode: "DungeonAmrine".to_owned(),
                members: vec![Uuid::from_u128(1)],
                players: 1,
                empty_for_ms: None,
                open_for_ms: 30,
                phase: 9,
                capacity: 5,
                ends_at_ms: Some(1_792_670_400_250),
            }),
        };
        assert_eq!(
            decode::<InstanceBeat>(&encode(&beat).expect("encode")).expect("decode"),
            beat
        );
    }
}
