// SPDX-License-Identifier: AGPL-3.0-or-later
//! The bus between the sims and the world's social service (run 15 social study): chat, players,
//! groups, companies and voice-chat setup are one state per world, so a player's friends, group
//! and company reach it on any sim.
//!
//! - Each sim sends its players' social calls and their presence ([`SocialBatch`] on
//!   [`crate::subject::social_input`]) once per tick.
//! - The service sends each sim the messages for its players ([`SocialDeliveries`] on
//!   [`crate::subject::sim_social`]); the sim puts the facet header in front and sends them to
//!   their clients.
//! - The service also sends each sim the replicated social states of its players
//!   ([`OwnStateDelivery`]: groups, friends and blocks, company, chat mutes) and the state of
//!   each player's group entity ([`GroupEntityDelivery`]); the sim puts them into the player's
//!   next bundle.
//! - The treasury moves a player's coins through the sim that has the player ([`CoinTransfer`]
//!   on [`crate::subject::sim_coins`], request/reply).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::id::BusId;

/// Bytes of a player icon (`nw_messages::appearance::PlayerIconData`).
pub const ICON_LEN: usize = 9;

/// The player component a delivery goes to (`nw_messages::social::SocialFacet`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SocialFacetKind {
    /// `ChatComponent`.
    Chat,
    /// `SocialComponent`.
    Social,
    /// `GroupsComponent`.
    Groups,
    /// `GuildsComponent`.
    Guilds,
    /// `VoiceChatComponent`.
    VoiceChat,
}

/// What the service knows of an online player from its sim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SocialPlayer {
    /// The character.
    pub character: Uuid,
    /// The character's name.
    pub name: String,
    /// Its zero-based level (`PlayerDataView` level).
    pub level: u32,
    /// Its icon (`PlayerIconData`, 9 bytes).
    pub icon: [u8; ICON_LEN],
    /// The sim that has the player.
    pub sim: BusId,
    /// The level the player is in (the sim's level: area chat stays in it).
    pub level_name: String,
    /// The territories the player is in, innermost last.
    pub territories: Vec<u16>,
    /// The client's `FactionType` (0 none, 1 to 3).
    pub faction: u8,
    /// Where the player is, in metres (east, north): its positional voice channel.
    pub position: [f32; 2],
}

/// Why a player left a sim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaveReason {
    /// The client is gone.
    Logout,
    /// Another sim took the player; its `Joined` follows or came first.
    Handoff,
}

/// One input of a sim to the service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SocialInput {
    /// A player is now on the sending sim (a login, a handoff or a resume).
    Joined(SocialPlayer),
    /// A player of the sending sim changed level, territory, faction or place.
    Updated(SocialPlayer),
    /// A player left the sending sim.
    Left {
        /// The character.
        character: Uuid,
        /// Why.
        reason: LeaveReason,
    },
    /// A player's health, mana or life changed (its group shows them).
    Vitals(PlayerVitals),
    /// A social call of a player's client: its type index and whole body (header first).
    Call {
        /// The character.
        character: Uuid,
        /// The type index.
        type_index: u64,
        /// The body.
        body: Vec<u8>,
    },
}

/// What a player's group shows of its health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerVitals {
    /// The character.
    pub character: Uuid,
    /// Health in percent of its maximum.
    pub health_percent: u8,
    /// Mana in percent of its maximum.
    pub mana_percent: u8,
    /// The player is dead.
    pub dead: bool,
}

/// A sim's inputs of one tick, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SocialBatch {
    /// The sim.
    pub sim: BusId,
    /// The inputs.
    pub inputs: Vec<SocialInput>,
}

/// The header a delivery takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SocialHeader {
    /// The header of the call it answers (target, entity).
    Echo {
        /// The target id.
        target: u64,
        /// The entity.
        entity: u64,
    },
    /// The receiving player's header of this component.
    Facet(SocialFacetKind),
}

/// One server message for some players of a sim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialDelivery {
    /// The receiving characters.
    pub recipients: Vec<Uuid>,
    /// The header.
    pub header: SocialHeader,
    /// The type index.
    pub type_index: u64,
    /// The body after the header.
    pub args: Vec<u8>,
}

/// A replicated social state of a player (`nw_messages::social::states`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SocialStateKind {
    /// `GroupsComponentReplicatedState` (type 1994).
    Groups,
    /// `SocialReplicatedState` (type 4176).
    Social,
    /// `GuildsComponentReplicatedState` (type 3147).
    Guilds,
    /// `ChatReplicatedState` (type 1566).
    Chat,
}

/// A change of one replicated social state of one player: the state's encoded body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnStateDelivery {
    /// The player.
    pub character: Uuid,
    /// The state.
    pub kind: SocialStateKind,
    /// The encoded state body.
    pub body: Vec<u8>,
}

/// A player's group entity as it is now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntityView {
    /// The group.
    pub group: Uuid,
    /// The encoded group data body (`GroupDataComponentReplicatedState`, whole).
    pub body: Vec<u8>,
}

/// A player's group entity: its state now, or none when the player left its group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupEntityDelivery {
    /// The player.
    pub character: Uuid,
    /// The group entity; `None` removes it.
    pub view: Option<GroupEntityView>,
}

/// The service's messages and states for the players of one sim, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialDeliveries {
    /// The deliveries.
    pub deliveries: Vec<SocialDelivery>,
    /// The replicated social state changes.
    pub own_states: Vec<OwnStateDelivery>,
    /// The group entity changes.
    pub group_entities: Vec<GroupEntityDelivery>,
}

/// Service to sim, request/reply: add `change` coins to a player (a negative change takes them).
/// The service asks again with the same `id` when no answer came; the sim moves the coins of one
/// id once and answers a repeat with the first answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoinTransfer {
    /// The move.
    pub id: Uuid,
    /// The character.
    pub character: Uuid,
    /// The change, in hundredths of a coin.
    pub change: i64,
}

/// The answer to a [`CoinTransfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoinReply {
    /// The coins moved; the player has this many now.
    Done {
        /// The player's coins after the change.
        coins: u64,
    },
    /// The player is not on this sim.
    NotHere,
    /// The player has fewer coins than the change takes.
    TooFew,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{decode, encode};

    #[test]
    fn a_batch_and_deliveries_round_trip() {
        let sim = BusId::try_from("sim-1".to_owned()).expect("id");
        let character = Uuid::from_u128(7);
        let batch = SocialBatch {
            sim: sim.clone(),
            inputs: vec![
                SocialInput::Joined(SocialPlayer {
                    character,
                    name: "Ann".to_owned(),
                    level: 4,
                    icon: [1; ICON_LEN],
                    sim,
                    level_name: "newworld_vitaeeterna".to_owned(),
                    territories: vec![3, 7],
                    faction: 1,
                    position: [10.0, 20.0],
                }),
                SocialInput::Call {
                    character,
                    type_index: 1291,
                    body: vec![1, 2, 3],
                },
                SocialInput::Vitals(PlayerVitals {
                    character,
                    health_percent: 50,
                    mana_percent: 100,
                    dead: false,
                }),
                SocialInput::Left {
                    character,
                    reason: LeaveReason::Logout,
                },
            ],
        };
        assert_eq!(
            decode::<SocialBatch>(&encode(&batch).expect("encode")).expect("decode"),
            batch
        );
        let deliveries = SocialDeliveries {
            deliveries: vec![SocialDelivery {
                recipients: vec![character],
                header: SocialHeader::Facet(SocialFacetKind::Chat),
                type_index: 293,
                args: vec![0],
            }],
            own_states: vec![OwnStateDelivery {
                character,
                kind: SocialStateKind::Groups,
                body: vec![1, 1],
            }],
            group_entities: vec![
                GroupEntityDelivery {
                    character,
                    view: Some(GroupEntityView {
                        group: Uuid::from_u128(9),
                        body: vec![1],
                    }),
                },
                GroupEntityDelivery {
                    character,
                    view: None,
                },
            ],
        };
        assert_eq!(
            decode::<SocialDeliveries>(&encode(&deliveries).expect("encode")).expect("decode"),
            deliveries
        );
        let transfer = CoinTransfer {
            id: Uuid::from_u128(8),
            character,
            change: -5,
        };
        assert_eq!(
            decode::<CoinTransfer>(&encode(&transfer).expect("encode")).expect("decode"),
            transfer
        );
        let reply = CoinReply::Done { coins: 5 };
        assert_eq!(
            decode::<CoinReply>(&encode(&reply).expect("encode")).expect("decode"),
            reply
        );
    }
}
