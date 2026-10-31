// SPDX-License-Identifier: AGPL-3.0-or-later
//! This sim's side of the world's social service (run 15 social study): what the service knows
//! of its players, the delivery of the service's messages to their clients, and the coin moves
//! of the company treasury.

use std::collections::HashMap;

use bevy_ecs::entity::Entity;
use nw_bus::{
    CoinReply, CoinTransfer, SocialDelivery, SocialFacetKind, SocialHeader, SocialPlayer,
};
use nw_messages::FacetHeader;
use nw_messages::appearance::PlayerIconData;
use nw_messages::social::{SocialFacet, icon_bytes, push_envelope};
use uuid::Uuid;

use super::{Body, MESSAGE_CHANNEL, Outgoing, Player, SimWorld};

/// The component of a bus facet kind.
const fn facet_of(kind: SocialFacetKind) -> SocialFacet {
    match kind {
        SocialFacetKind::Chat => SocialFacet::Chat,
        SocialFacetKind::Social => SocialFacet::Social,
        SocialFacetKind::Groups => SocialFacet::Groups,
        SocialFacetKind::Guilds => SocialFacet::Guilds,
        SocialFacetKind::VoiceChat => SocialFacet::VoiceChat,
    }
}

impl SimWorld {
    /// What the social service knows of the player `entity`, as this sim `sim` has it.
    fn social_player_of(&self, entity: Entity, sim: &nw_bus::BusId) -> Option<SocialPlayer> {
        let player = self.world.get::<Player>(entity)?;
        let at = self.world.get::<Body>(entity)?.motion.position();
        let icon = player
            .look
            .as_ref()
            .map_or_else(PlayerIconData::default, PlayerIconData::of);
        Some(SocialPlayer {
            character: player.character,
            name: player.name.clone(),
            level: self.content.progression.level_of(player.experience),
            icon: icon_bytes(&icon),
            sim: sim.clone(),
            level_name: self.settings.level.folder.to_owned(),
            territories: player.territories.clone(),
            faction: player.faction.faction,
            position: [at.x, at.y],
        })
    }

    /// The social view of the player of `connection` on sim `sim`.
    pub fn social_player(
        &self,
        connection: &nw_bus::ConnectionRef,
        sim: &nw_bus::BusId,
    ) -> Option<SocialPlayer> {
        self.social_player_of(*self.by_connection.get(connection)?, sim)
    }

    /// The social view of every player of this sim `sim`.
    pub fn social_players(&self, sim: &nw_bus::BusId) -> Vec<SocialPlayer> {
        self.by_connection
            .values()
            .filter_map(|entity| self.social_player_of(*entity, sim))
            .collect()
    }

    /// The messages of `deliveries` for the players of this sim, as outgoing envelopes. A
    /// recipient that is not here (it left or moved on) is skipped.
    pub fn social_outgoing(&self, deliveries: &[SocialDelivery]) -> Vec<Outgoing> {
        let here: HashMap<Uuid, (u64, &nw_bus::ConnectionRef)> = self
            .by_connection
            .values()
            .filter_map(|entity| self.world.get::<Player>(*entity))
            .map(|player| (player.character, (player.number, &player.connection)))
            .collect();
        let mut outgoing = Vec::new();
        for delivery in deliveries {
            for recipient in &delivery.recipients {
                let Some((number, connection)) = here.get(recipient) else {
                    continue;
                };
                let header = match delivery.header {
                    SocialHeader::Echo { target, entity } => FacetHeader { target, entity },
                    SocialHeader::Facet(kind) => facet_of(kind).header(*number),
                };
                match push_envelope(delivery.type_index, header, &delivery.args) {
                    Ok(envelope) => outgoing.push(Outgoing {
                        connection: (*connection).clone(),
                        channel: MESSAGE_CHANNEL,
                        reliable: true,
                        envelopes: vec![envelope],
                    }),
                    Err(error) => {
                        tracing::warn!(type_index = delivery.type_index, %error, "social message not sent");
                    }
                }
            }
        }
        outgoing
    }

    /// Moves the coins of `transfer` (the company treasury): the player's coins change and its
    /// client gets them with the next own state.
    pub fn move_coins(&mut self, transfer: &CoinTransfer) -> CoinReply {
        let Some(entity) = self.by_connection.values().copied().find(|entity| {
            self.world
                .get::<Player>(*entity)
                .is_some_and(|player| player.character == transfer.character)
        }) else {
            return CoinReply::NotHere;
        };
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return CoinReply::NotHere;
        };
        let coins = if transfer.change < 0 {
            player.coins.checked_sub(transfer.change.unsigned_abs())
        } else {
            Some(player.coins.saturating_add(transfer.change.unsigned_abs()))
        };
        let Some(coins) = coins else {
            return CoinReply::TooFew;
        };
        player.coins = coins;
        player.coins_changed = true;
        CoinReply::Done { coins }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nw_bus::BusId;
    use nw_messages::social::chat::RECEIVE_CHAT_MESSAGE_TYPE_INDEX;
    use nw_reflect::Envelope;

    use crate::world::tests::{NOW, SERVER_TIME, admit_at, connection, world_with_spawns};

    /// The player's number.
    const PLAYER: u64 = 1;

    /// A world with one player; its character.
    fn admitted() -> (SimWorld, Uuid) {
        let mut world = world_with_spawns(Vec::new());
        let admit = admit_at(PLAYER, 0.0, true);
        world.admit(&admit, NOW, SERVER_TIME).expect("admit");
        (world, admit.character)
    }

    #[test]
    fn a_delivery_reaches_its_player_with_its_own_header() {
        let (world, character) = admitted();
        let sim = BusId::try_from("sim-1".to_owned()).expect("id");
        let player = world
            .social_player(&connection(PLAYER), &sim)
            .expect("a player");
        assert_eq!(player.character, character);
        assert_eq!(world.social_players(&sim).len(), 1);
        let delivery = SocialDelivery {
            recipients: vec![character, Uuid::from_u128(999)],
            header: SocialHeader::Facet(SocialFacetKind::Chat),
            type_index: RECEIVE_CHAT_MESSAGE_TYPE_INDEX,
            args: vec![7],
        };
        let outgoing = world.social_outgoing(&[delivery]);
        assert_eq!(outgoing.len(), 1, "the other recipient is not here");
        let envelope = Envelope::decode(&outgoing[0].envelopes[0]).expect("envelope");
        assert_eq!(envelope.type_index(), Some(RECEIVE_CHAT_MESSAGE_TYPE_INDEX));
        assert_eq!(envelope.body.last(), Some(&7));
    }

    #[test]
    fn coins_move_and_never_below_zero() {
        let (mut world, character) = admitted();
        let before = match world.move_coins(&CoinTransfer {
            character,
            change: 0,
        }) {
            CoinReply::Done { coins } => coins,
            other => panic!("done: {other:?}"),
        };
        assert_eq!(
            world.move_coins(&CoinTransfer {
                character,
                change: 500,
            }),
            CoinReply::Done {
                coins: before + 500
            }
        );
        assert_eq!(
            world.move_coins(&CoinTransfer {
                character,
                change: -i64::try_from(before + 501).expect("small"),
            }),
            CoinReply::TooFew
        );
        assert_eq!(
            world.move_coins(&CoinTransfer {
                character: Uuid::from_u128(999),
                change: 1,
            }),
            CoinReply::NotHere
        );
    }
}
