// SPDX-License-Identifier: AGPL-3.0-or-later
//! The economy's client calls and server messages in one place: the trading post
//! ([`crate::contracts`]), player trade ([`crate::p2p_trade`]), the seasons
//! ([`crate::seasons`]), the store calls ([`crate::store`]), the shops ([`crate::shop`]) and the
//! generic invites that start a trade ([`crate::generic_invite`]).

use crate::contracts::{ContractCall, ContractMessage};
use crate::generic_invite::{InviteCall, InviteFailedMsg};
use crate::p2p_trade::{TradeCall, TradeMessage};
use crate::seasons::{SeasonsCall, SeasonsMessage};
use crate::shop::ShopPurchaseCall;
use crate::store::StoreCall;
use crate::wire::MessageError;

/// One economy call of the client.
#[derive(Debug, Clone, PartialEq)]
pub enum EconomyCall {
    /// A trading post call.
    Contract(ContractCall),
    /// A trade call.
    Trade(TradeCall),
    /// A season call.
    Seasons(SeasonsCall),
    /// A store call.
    Store(StoreCall),
    /// A shop purchase.
    Shop(ShopPurchaseCall),
    /// A generic invite call.
    Invite(InviteCall),
}

impl EconomyCall {
    /// Decodes `body` when `type_index` is an economy call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        if let Some(call) = ContractCall::decode(type_index, body) {
            return Some(call.map(Self::Contract));
        }
        if let Some(call) = TradeCall::decode(type_index, body) {
            return Some(call.map(Self::Trade));
        }
        if let Some(call) = SeasonsCall::decode(type_index, body) {
            return Some(call.map(Self::Seasons));
        }
        if let Some(call) = StoreCall::decode(type_index, body) {
            return Some(call.map(Self::Store));
        }
        if let Some(call) = ShopPurchaseCall::decode(type_index, body) {
            return Some(call.map(Self::Shop));
        }
        InviteCall::decode(type_index, body).map(|call| call.map(Self::Invite))
    }

    /// The type index of the call.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Contract(call) => call.type_index(),
            Self::Trade(call) => call.type_index(),
            Self::Seasons(call) => call.type_index(),
            Self::Store(call) => call.type_index(),
            Self::Shop(_) => crate::shop::PURCHASE_ITEM_TYPE_INDEX,
            Self::Invite(call) => call.type_index(),
        }
    }

    /// The body bytes, as the client sends them.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a contract, trade, store or invite call does not encode.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        match self {
            Self::Contract(call) => call.encode(),
            Self::Trade(call) => call.encode(),
            Self::Seasons(call) => Ok(call.encode()),
            Self::Store(call) => call.encode(),
            Self::Shop(call) => Ok(call.encode()),
            Self::Invite(call) => call.encode(),
        }
    }
}

/// One economy message of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EconomyMessage {
    /// A trading post answer or notice (boxed: it is far larger than the other messages).
    Contract(Box<ContractMessage>),
    /// A trade message.
    Trade(TradeMessage),
    /// A season answer.
    Seasons(SeasonsMessage),
    /// A generic invite failed.
    InviteFailed(InviteFailedMsg),
}

impl EconomyMessage {
    /// Decodes `body` when `type_index` is an economy message; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        if let Some(message) = ContractMessage::decode(type_index, body) {
            return Some(message.map(|message| Self::Contract(Box::new(message))));
        }
        if let Some(message) = TradeMessage::decode(type_index, body) {
            return Some(message.map(Self::Trade));
        }
        if let Some(message) = SeasonsMessage::decode(type_index, body) {
            return Some(message.map(Self::Seasons));
        }
        InviteFailedMsg::decode(type_index, body).map(|message| message.map(Self::InviteFailed))
    }

    /// The type index.
    pub const fn type_index(&self) -> u64 {
        match self {
            Self::Contract(message) => message.type_index(),
            Self::Trade(message) => message.type_index(),
            Self::Seasons(message) => message.type_index(),
            Self::InviteFailed(_) => crate::generic_invite::INVITE_FAILED_TYPE_INDEX,
        }
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError`] when a contract does not encode.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        match self {
            Self::Contract(message) => message.encode(),
            Self::Trade(message) => Ok(message.encode()),
            Self::Seasons(message) => Ok(message.encode()),
            Self::InviteFailed(message) => Ok(message.encode()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facet::FacetHeader;
    use crate::p2p_trade::{COIN_RESPONSE_TYPE_INDEX, failure};
    use crate::seasons::{NEW_CARD_TYPE_INDEX, response};
    use crate::wire::Writer;

    const HEADER: FacetHeader = FacetHeader {
        target: 0x65cd_4219_5901_da56,
        entity: 0x2896_c8c1_4210_296e,
    };

    #[test]
    fn every_kind_of_message_goes_through_its_index() {
        let messages = [
            EconomyMessage::Trade(TradeMessage::Response {
                type_index: COIN_RESPONSE_TYPE_INDEX,
                header: HEADER,
                failure: failure::NONE,
            }),
            EconomyMessage::Seasons(
                SeasonsCall::NewCard { header: HEADER }.answer(response::FAILURE),
            ),
            EconomyMessage::InviteFailed(InviteFailedMsg {
                header: HEADER,
                activity: 0x7e1a_4366,
                reason: crate::generic_invite::end::ELIGIBILITY_FAILED,
            }),
        ];
        for message in messages {
            let body = message.encode().expect("encode");
            assert_eq!(
                EconomyMessage::decode(message.type_index(), &body),
                Some(Ok(message))
            );
        }
    }

    #[test]
    fn calls_are_told_apart_by_their_index() {
        let mut writer = Writer::new();
        HEADER.write(&mut writer);
        let header_only = writer.finish();
        assert!(matches!(
            EconomyCall::decode(NEW_CARD_TYPE_INDEX, &header_only),
            Some(Ok(EconomyCall::Seasons(_)))
        ));
        let purchase = crate::shop::ShopPurchaseCall {
            header: HEADER,
            conversion: 1,
            count: 2,
        };
        assert_eq!(
            EconomyCall::decode(crate::shop::PURCHASE_ITEM_TYPE_INDEX, &purchase.encode()),
            Some(Ok(EconomyCall::Shop(purchase)))
        );
        assert!(EconomyCall::decode(u64::from(u16::MAX), &header_only).is_none());
    }

    #[test]
    fn every_kind_of_call_writes_what_it_reads() {
        let calls = [
            EconomyCall::Contract(ContractCall::TransferCheck {
                header: HEADER,
                lookup: [0x2b; crate::wire::RAW_ID_LEN],
            }),
            EconomyCall::Trade(TradeCall::UpdateCoin {
                header: HEADER,
                coin: 12_345,
            }),
            EconomyCall::Seasons(SeasonsCall::Redeem {
                header: HEADER,
                reward: 0xa17b_8b9a_2221_5ad9,
            }),
            EconomyCall::Store(StoreCall::ClaimEntitlement {
                header: HEADER,
                entitlement: 0x0102_0304,
            }),
            EconomyCall::Shop(ShopPurchaseCall {
                header: HEADER,
                conversion: 0x87c0_53c5,
                count: 3,
            }),
            EconomyCall::Invite(InviteCall::Reply {
                header: HEADER,
                accept: true,
            }),
        ];
        for call in calls {
            let body = call.encode().expect("encode");
            assert_eq!(
                EconomyCall::decode(call.type_index(), &body),
                Some(Ok(call))
            );
        }
    }
}
