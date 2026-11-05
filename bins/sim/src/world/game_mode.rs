// SPDX-License-Identifier: AGPL-3.0-or-later
//! Matchmaking queue calls (6211, 6272, 1980, 5741, 5450, 6254, 6824), war board calls
//! (3206, 5953, 6845) and territory war sign-up calls (2767, 2772, 2791, 2771, 2762, 2784)
//! of a player, with the server answers the retail captures show (1234, 4970, 2726, 2729,
//! 2169).
//!
//! The queued activities are the PvP game modes and the catacombs (6254 activity values
//! 0x3116 Outpost Rush, 0x3316 3v3 arena, 0x4316 capture the flag, 0x3016 and 0x0c16
//! catacombs, in 29 retail queue joins). Retail matched the queued players and moved them into
//! an instance of the game mode: in `opr_que_and_lost` the accept (5741 at 1781894841233) is
//! followed by the loading screen (3951) and the Outpost Rush level (1635
//! `NW_OPR_005_Vertical` at 1781894842131). This server runs none of these modes: their rows
//! are not instances of the game data catalog (`nw_content::instances`: PvP rows are left
//! out, a catacombs map is generated), and their rules (Outpost Rush scoring and waves, arena
//! rounds, flag carrying) are not made. So a queue join is refused at once: the queue group
//! goes away (1234), and no role selection, ready or accept follows. Inferred: retail's own
//! answer to a mode that cannot run is `OnQueueForGameModeFailedMsg` (3310), which no capture
//! holds (its layout is Unknown); 1234 is the captured message that ends a queue. The role,
//! ready and accept calls (6272, 6824, 5741) and the login confirm (5450) then name no queue
//! and change nothing.
//!
//! Three calls have no server answer in any capture and change no state here: the group
//! eligibility subscription (6211, a subscription to replicated group data, which the
//! social service sends), a client event (1980, meaning Unknown), the war board receipt
//! (5953), the remote board use (2791, after which the client itself asks for the counts),
//! and the spectator stop (6845, a client camera call; this server has no spectator
//! state). Their arms only take the call.
//!
//! A queue leave (1066, and 2385 for every queue) is answered with a 4970 of the left game mode
//! (retail 3v3 and expedition group runs: the 1066's own CRC; for a 2385, the CRC of the
//! expedition the group waits for, else 0). A group queue (1066 group byte 1, and every 2385)
//! gets the 1234 first. Retail then sends a second 4970 of a group mate's leave, which this sim
//! does not hold (Inferred from the two group ledgers). A game mode leave (3309) takes the player
//! out of its instance as a 926 does (retail: 3951, the open world level, then 4970).
//!
//! No territory war is scheduled on this server (no war board or territory owner data drives
//! one), so every sign-up (2762) is refused with the retail answer of a closed war: a 2169 of
//! result [`SIGNUP_RESULT_CLOSED`] and value [`SIGNUP_CLOSED_VALUE`]. No sign-up is kept: the
//! list (2767, and the roster 2772) is answered with an empty 2726 and the count (2771) with a
//! 2729 of 0, in the retail layouts; the war board (3206) lists no participant.

use nw_bus::ConnectionRef;
use nw_messages::facet::NO_GAME_MODE;
use nw_messages::{
    GameModeClientMsg, GameModeServerMsg, LeftGameModeQueueMsg, SIGNUP_LIST_TRAILER, ServerMessage,
    WarClientMsg, WarServerMsg, WarSignupClientMsg, WarSignupServerMsg, WarboardManifest,
};

use super::{Player, SimWorld, send_now};

/// The 2169 result of a sign-up for a war that is not open (retail failure).
const SIGNUP_RESULT_CLOSED: u8 = 1;
/// The 2169 value of a sign-up for a war that is not open (retail failure).
const SIGNUP_CLOSED_VALUE: u32 = 0;
/// The second `u32` of every retail 2726 (1; meaning Unknown).
const SIGNUP_LIST_EXTRA: u32 = 1;
/// The sign-ups of a war: none, as no war is scheduled.
const NO_SIGNUPS: u32 = 0;
/// Metric: queue joins refused because the game mode does not run here.
const METRIC_REFUSED_QUEUES: &str = "sim_game_mode_queue_refused_total";

/// A player's war sign-up view.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GameModePlayer {
    /// The war of its newest sign-up or list call, for the roster (2772).
    war_last: Option<u32>,
}

impl SimWorld {
    /// Carries out the matchmaking call `call` of the client of `connection`.
    pub fn game_mode_call(&mut self, connection: &ConnectionRef, call: GameModeClientMsg) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let header = call.header();
        let mut answers: Vec<ServerMessage> = Vec::new();
        let Some((number, queued_game_mode)) = self
            .world
            .get::<Player>(entity)
            .map(|player| (player.number, player.arena.queued_game_mode()))
        else {
            return;
        };
        match call {
            // No answer in any capture; see the module text.
            GameModeClientMsg::ModifyGroupEligibilitySubscription { .. }
            | GameModeClientMsg::ReceivedClientEvent { .. } => {}
            GameModeClientMsg::JoinQueueForActivities { activities, .. } => {
                if !activities.is_empty() {
                    metrics::counter!(METRIC_REFUSED_QUEUES).increment(1);
                    tracing::info!(
                        player = number,
                        ?activities,
                        "queue join refused: the game mode does not run here"
                    );
                }
                answers.push(ServerMessage::GameMode(
                    GameModeServerMsg::QueueGroupRemoved { header },
                ));
            }
            // No queue: nothing to confirm, pick, ready or accept (see the module text).
            GameModeClientMsg::MatchmakingLoginConfirmed { .. }
            | GameModeClientMsg::SetMatchmakingRole { .. }
            | GameModeClientMsg::SetReadiedUpStatus { .. }
            | GameModeClientMsg::AcceptMatch { .. } => {}
            GameModeClientMsg::LeaveQueueForGameMode {
                game_mode, group, ..
            } => {
                if group {
                    answers.push(ServerMessage::GameMode(
                        GameModeServerMsg::QueueGroupRemoved { header },
                    ));
                }
                answers.push(ServerMessage::LeftGameModeQueue(LeftGameModeQueueMsg::of(
                    header, game_mode,
                )));
            }
            GameModeClientMsg::LeaveQueueForAllGameModes { .. } => {
                let game_mode = queued_game_mode.unwrap_or(NO_GAME_MODE);
                answers.push(ServerMessage::GameMode(
                    GameModeServerMsg::QueueGroupRemoved { header },
                ));
                answers.push(ServerMessage::LeftGameModeQueue(LeftGameModeQueueMsg::of(
                    header, game_mode,
                )));
            }
            GameModeClientMsg::LeaveGameMode { .. } => self.exit_instance(entity),
        }
        send_now(&mut self.combat_messages, connection, &answers);
    }

    /// Carries out the war board call `call` of the client of `connection`.
    pub fn war_call(&mut self, connection: &ConnectionRef, call: WarClientMsg) {
        if !self.by_connection.contains_key(connection) {
            return;
        }
        let header = call.header();
        let mut answers: Vec<ServerMessage> = Vec::new();
        match call {
            WarClientMsg::WarboardEnabled { .. } => {
                match WarServerMsg::full_manifest(
                    header,
                    &WarboardManifest::of_participants(Vec::new()),
                ) {
                    Ok(manifest) => answers.push(ServerMessage::War(manifest)),
                    Err(error) => {
                        tracing::warn!(%error, "war board manifest not sent");
                    }
                }
            }
            // No answer in any capture; see the module text.
            WarClientMsg::WarboardManifestReceived { .. } | WarClientMsg::StopSpectating { .. } => {
            }
        }
        send_now(&mut self.combat_messages, connection, &answers);
    }

    /// Carries out the war sign-up call `call` of the client of `connection`.
    pub fn war_signup_call(&mut self, connection: &ConnectionRef, call: WarSignupClientMsg) {
        let Some(&entity) = self.by_connection.get(connection) else {
            return;
        };
        let header = call.header();
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        let empty_list = |war| {
            ServerMessage::WarSignup(WarSignupServerMsg::SignupListReceived {
                header,
                war,
                extra: SIGNUP_LIST_EXTRA,
                entries: Vec::new(),
                trailer: SIGNUP_LIST_TRAILER,
            })
        };
        let mut answers: Vec<ServerMessage> = Vec::new();
        match call {
            WarSignupClientMsg::RequestSignup { war, .. } => {
                player.game_mode.war_last = Some(war);
                answers.push(ServerMessage::WarSignup(
                    WarSignupServerMsg::SignupResponseReceived {
                        header,
                        result: SIGNUP_RESULT_CLOSED,
                        value: SIGNUP_CLOSED_VALUE,
                    },
                ));
            }
            // Nothing to leave: no sign-up is kept.
            WarSignupClientMsg::RequestLeave { .. } => {}
            WarSignupClientMsg::RequestSignupList { war, .. } => {
                player.game_mode.war_last = Some(war);
                answers.push(empty_list(war));
            }
            WarSignupClientMsg::RequestRoster { .. } => {
                if let Some(war) = player.game_mode.war_last {
                    answers.push(empty_list(war));
                }
            }
            WarSignupClientMsg::RequestSignupCount { war, .. } => {
                answers.push(ServerMessage::WarSignup(
                    WarSignupServerMsg::SignupCountReceived {
                        header,
                        war,
                        count: NO_SIGNUPS,
                    },
                ));
            }
            // No answer in any capture; the client itself asks for the counts next.
            WarSignupClientMsg::RequestRemoteInteract { .. } => {}
        }
        send_now(&mut self.combat_messages, connection, &answers);
    }
}

#[cfg(test)]
mod tests {
    use nw_messages::{FacetHeader, QueueActivity};
    use uuid::Uuid;

    use super::*;
    use crate::world::tests::{
        NOW, SERVER_TIME, admit_at, connection, messages_to, world_with_spawns,
    };

    /// The player number of the test player.
    const PLAYER: u64 = 41;
    /// The facet header of the test calls.
    const HEADER: FacetHeader = FacetHeader {
        target: 0,
        entity: 0,
    };
    /// The Outpost Rush activity of the retail queue calls.
    const ACTIVITY: u16 = 0x3116;
    /// A war and its territory.
    const WAR: u32 = 1;
    const TERRITORY: u16 = 11;

    fn world() -> SimWorld {
        let mut world = world_with_spawns(Vec::new());
        let admit = admit_at(PLAYER, 0.0, true);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        world
    }

    /// Retail 3v3 arena group and expedition runs: a 1066 is answered with a 4970 of its own
    /// game mode (a group queue gets the 1234 first), a 2385 with the 1234 and a 4970 of no
    /// queued expedition, and both clear the queue.
    #[test]
    fn a_queue_leave_answers_with_the_left_game_mode_and_clears_the_queue() {
        const ARENA_CRC: u32 = 0x1fad_9d42;
        let mut world = world();
        let join = GameModeClientMsg::JoinQueueForActivities {
            header: HEADER,
            activities: vec![QueueActivity {
                activity: ACTIVITY,
                extra: 0,
                flag: 1,
            }],
        };
        world.game_mode_call(&connection(PLAYER), join.clone());
        let _ = world.take_messages();
        world.game_mode_call(
            &connection(PLAYER),
            GameModeClientMsg::LeaveQueueForGameMode {
                header: HEADER,
                game_mode: ARENA_CRC,
                group: false,
            },
        );
        let sent = world.take_messages();
        assert_eq!(
            messages_to(&sent, PLAYER),
            [ServerMessage::LeftGameModeQueue(LeftGameModeQueueMsg::of(
                HEADER, ARENA_CRC
            ))]
        );
        world.game_mode_call(
            &connection(PLAYER),
            GameModeClientMsg::MatchmakingLoginConfirmed { header: HEADER },
        );
        assert!(
            messages_to(&world.take_messages(), PLAYER).is_empty(),
            "no queue left to open roles for"
        );
        world.game_mode_call(&connection(PLAYER), join);
        let _ = world.take_messages();
        world.game_mode_call(
            &connection(PLAYER),
            GameModeClientMsg::LeaveQueueForGameMode {
                header: HEADER,
                game_mode: ARENA_CRC,
                group: true,
            },
        );
        let sent = world.take_messages();
        assert_eq!(
            messages_to(&sent, PLAYER),
            [
                ServerMessage::GameMode(GameModeServerMsg::QueueGroupRemoved { header: HEADER }),
                ServerMessage::LeftGameModeQueue(LeftGameModeQueueMsg::of(HEADER, ARENA_CRC)),
            ]
        );
        world.game_mode_call(
            &connection(PLAYER),
            GameModeClientMsg::LeaveQueueForAllGameModes {
                header: HEADER,
                flags: 1,
            },
        );
        let sent = world.take_messages();
        assert_eq!(
            messages_to(&sent, PLAYER),
            [
                ServerMessage::GameMode(GameModeServerMsg::QueueGroupRemoved { header: HEADER }),
                ServerMessage::LeftGameModeQueue(LeftGameModeQueueMsg::of(HEADER, NO_GAME_MODE)),
            ]
        );
    }

    /// No game mode of a queue runs here: a join is refused with the 1234 at once, and the
    /// later queue calls name no match.
    #[test]
    fn a_queue_join_is_refused_and_no_match_follows() {
        let mut world = world();
        world.game_mode_call(
            &connection(PLAYER),
            GameModeClientMsg::JoinQueueForActivities {
                header: HEADER,
                activities: vec![QueueActivity {
                    activity: ACTIVITY,
                    extra: 0,
                    flag: 1,
                }],
            },
        );
        assert_eq!(
            messages_to(&world.take_messages(), PLAYER),
            [ServerMessage::GameMode(
                GameModeServerMsg::QueueGroupRemoved { header: HEADER }
            )]
        );
        for call in [
            GameModeClientMsg::MatchmakingLoginConfirmed { header: HEADER },
            GameModeClientMsg::SetMatchmakingRole {
                header: HEADER,
                role: 1,
            },
            GameModeClientMsg::SetReadiedUpStatus {
                header: HEADER,
                match_id: 1,
                ready: true,
            },
            GameModeClientMsg::AcceptMatch {
                header: HEADER,
                match_id: Uuid::nil().hyphenated().to_string(),
                flag: 1,
            },
        ] {
            world.game_mode_call(&connection(PLAYER), call);
            assert!(
                messages_to(&world.take_messages(), PLAYER).is_empty(),
                "no role selection, no match"
            );
        }
    }

    /// No war is scheduled: a sign-up gets the retail closed-war answer, and nothing is kept.
    #[test]
    fn a_sign_up_is_refused_and_nothing_is_listed() {
        let mut world = world();
        let call = |world: &mut SimWorld, call| {
            world.war_signup_call(&connection(PLAYER), call);
            messages_to(&world.take_messages(), PLAYER)
        };
        assert_eq!(
            call(
                &mut world,
                WarSignupClientMsg::RequestSignup {
                    header: HEADER,
                    war: WAR,
                    territory: TERRITORY,
                }
            ),
            [ServerMessage::WarSignup(
                WarSignupServerMsg::SignupResponseReceived {
                    header: HEADER,
                    result: SIGNUP_RESULT_CLOSED,
                    value: SIGNUP_CLOSED_VALUE,
                }
            )]
        );
        assert_eq!(
            call(
                &mut world,
                WarSignupClientMsg::RequestSignupCount {
                    header: HEADER,
                    war: WAR,
                }
            ),
            [ServerMessage::WarSignup(
                WarSignupServerMsg::SignupCountReceived {
                    header: HEADER,
                    war: WAR,
                    count: NO_SIGNUPS,
                }
            )]
        );
        let empty = [ServerMessage::WarSignup(
            WarSignupServerMsg::SignupListReceived {
                header: HEADER,
                war: WAR,
                extra: SIGNUP_LIST_EXTRA,
                entries: Vec::new(),
                trailer: SIGNUP_LIST_TRAILER,
            },
        )];
        assert_eq!(
            call(
                &mut world,
                WarSignupClientMsg::RequestSignupList {
                    header: HEADER,
                    war: WAR,
                }
            ),
            empty
        );
        assert_eq!(
            call(
                &mut world,
                WarSignupClientMsg::RequestRoster { header: HEADER }
            ),
            empty
        );
    }
}
