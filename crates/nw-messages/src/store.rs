// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store and account rewards: the store flag of the player, the entitlement calls and the
//! PvP reward track (economy note `local-data/scratch-work/run15/economy/REPORT.md`, section 6).
//!
//! - 4908 `PlayerComponentServerFacet_RequestSetIsInStore` (a flag): retail answers with the
//!   `isInStore` child of the player's 3935 state (`crate::player_components::put_is_in_store`),
//!   no message (`unlock_store_content`, `menus-linux-2026-09-12`, both new characters).
//! - 898 `EntitlementComponentServerFacet_ClientInitializeTransaction`: a store purchase (retail
//!   `unlock_store_content`: three purchases of `Marks of Fortune`).
//! - 5936 `EntitlementComponentClientFacet_OnRequestInitialized` and 1285 `OnRequestSuccess`: the
//!   server's answers to a purchase, 0.12 s and 0.24 s after it (retail
//!   `mount-menu-buying-all-store-horses`, 35 free mounts): the call's request id, a transaction
//!   number, and the entitlements it changed (none, then the bought one and the wallet).
//! - 5107 `EntitlementComponentServerFacet_RequestClaimEntitlement`: the claim of an external
//!   reward (retail `ExternalRewards`, `menus-linux-2026-09-12`: a Twitch drop container).
//! - 4932 `RewardTrackComponentServerFacet_RequestPurchaseReward` (slot, item, cost) and 4910
//!   `RequestResetRewardTrack`: the PvP reward track (`pvp_track_unlocked`); retail answers
//!   through the track's replicated state and the bag, no message.

use crate::BodyCodec;
use crate::combat::{read_string, write_string};
use crate::facet::FacetHeader;
use crate::wire::{MessageError, RAW_ID_LEN, Reader, Writer};

/// `PlayerComponentServerFacet_RequestSetIsInStore`.
pub const SET_IS_IN_STORE_TYPE_INDEX: u64 = 4908;
/// `EntitlementComponentServerFacet_ClientInitializeTransaction`.
pub const INITIALIZE_TRANSACTION_TYPE_INDEX: u64 = 898;
/// `EntitlementComponentServerFacet_RequestClaimEntitlement`.
pub const CLAIM_ENTITLEMENT_TYPE_INDEX: u64 = 5107;
/// `RewardTrackComponentServerFacet_RequestPurchaseReward`.
pub const PURCHASE_REWARD_TYPE_INDEX: u64 = 4932;
/// `RewardTrackComponentServerFacet_RequestResetRewardTrack`.
pub const RESET_REWARD_TRACK_TYPE_INDEX: u64 = 4910;
/// `EntitlementComponentClientFacet_OnRequestInitialized`.
pub const REQUEST_INITIALIZED_TYPE_INDEX: u64 = 5936;
/// `EntitlementComponentClientFacet_OnRequestSuccess`.
pub const REQUEST_SUCCESS_TYPE_INDEX: u64 = 1285;
/// The request status of both answers (every retail answer: 2; its meaning is Unknown).
pub const RETAIL_REQUEST_STATUS: u32 = 2;
/// The kind of a durable entitlement, such as a mount (retail: 0).
pub const DURABLE_ENTITLEMENT_KIND: u32 = 0;
/// The kind of the store currency's wallet (retail: 2, the `Marks of Fortune` record).
pub const WALLET_ENTITLEMENT_KIND: u32 = 2;

/// A store purchase (898): the client's locale, the offer, and its price.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorePurchase {
    /// The language (retail `en-US`).
    pub language: Vec<u8>,
    /// The currency (retail `EUR`).
    pub currency: Vec<u8>,
    /// The country (retail `IT`).
    pub country: Vec<u8>,
    /// A `u32` (retail 1).
    pub word_a: u32,
    /// The transaction id.
    pub transaction: [u8; RAW_ID_LEN],
    /// The offer id.
    pub offer: Vec<u8>,
    /// The offer id again (retail: the same text).
    pub offer_again: Vec<u8>,
    /// A byte (retail 0).
    pub byte_b: u8,
    /// The product name (retail `Marks of Fortune`).
    pub product: Vec<u8>,
    /// The price (retail 0 and 5000).
    pub price: u32,
    /// A byte (retail 1).
    pub byte_c: u8,
}

/// One entitlement of a store answer (retail 1285: the bought mount, and the Marks of Fortune
/// wallet).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementRecord {
    /// The service's entitlement id.
    pub id: Vec<u8>,
    /// The entitlement alias: the game data's `UniqueTagID` (`Ent_Mount_MTX_7_bear_Ronin`).
    pub alias: Vec<u8>,
    /// Its display name (`Ronin Bear Mount`).
    pub name: Vec<u8>,
    /// [`DURABLE_ENTITLEMENT_KIND`] for a mount; [`WALLET_ENTITLEMENT_KIND`] for the wallet.
    pub kind: u32,
    /// Three amounts: a wallet's free, paid and pending Marks (Inferred from retail 8000, 5000,
    /// 0 beside a total of 13000, and the store service's `freeAmount`, `paidAmount`,
    /// `pendingAmount`); a mount: 0.
    pub amounts: [u32; 3],
    /// The total amount (a mount: 1; the wallet: 13000, the balance the store showed).
    pub total: u32,
    /// A `u64` (retail 0).
    pub word_a: u64,
    /// The ids of its instances (a mount: one UUID text).
    pub instances: Vec<Vec<u8>>,
    /// A `u64` (retail 0).
    pub word_b: u64,
    /// When it changed, ISO 8601 text (`2026-11-04T12:32:26.000Z`).
    pub updated: Vec<u8>,
    /// A byte (retail 0).
    pub byte_c: u8,
}

/// The body of a store answer (5936 and 1285).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitlementRequest {
    /// The header: the purchase call's own.
    pub header: FacetHeader,
    /// The request id of the purchase call ([`StorePurchase::transaction`]).
    pub request: [u8; RAW_ID_LEN],
    /// The transaction number, decimal text (retail 19 digits).
    pub transaction: Vec<u8>,
    /// [`RETAIL_REQUEST_STATUS`].
    pub status: u32,
    /// A byte (retail 0).
    pub byte_a: u8,
    /// The entitlements the request changed: none in 5936.
    pub entitlements: Vec<EntitlementRecord>,
    /// A flag (retail true).
    pub done: bool,
}

impl EntitlementRequest {
    fn write(&self, writer: &mut Writer) -> Result<(), MessageError> {
        self.header.write(writer);
        writer.bytes(&self.request);
        write_string(writer, &self.transaction)?;
        writer.u32(self.status).u8(self.byte_a);
        writer.prefixed(self.entitlements.len() as u64)?;
        for record in &self.entitlements {
            write_string(writer, &record.id)?;
            write_string(writer, &record.alias)?;
            write_string(writer, &record.name)?;
            writer.u32(record.kind);
            for amount in record.amounts {
                writer.u32(amount);
            }
            writer.u32(record.total).u64(record.word_a);
            writer.prefixed(record.instances.len() as u64)?;
            for instance in &record.instances {
                write_string(writer, instance)?;
            }
            writer.u64(record.word_b);
            write_string(writer, &record.updated)?;
            writer.u8(record.byte_c);
        }
        writer.boolean(self.done);
        Ok(())
    }

    fn read(reader: &mut Reader<'_>) -> Result<Self, MessageError> {
        let header = FacetHeader::read(reader)?;
        let request = reader.raw_id()?;
        let transaction = read_string(reader)?;
        let status = reader.u32()?;
        let byte_a = reader.u8()?;
        let count = reader.prefixed()?;
        let mut entitlements = Vec::new();
        for _ in 0..count {
            let id = read_string(reader)?;
            let alias = read_string(reader)?;
            let name = read_string(reader)?;
            let kind = reader.u32()?;
            let amounts = [reader.u32()?, reader.u32()?, reader.u32()?];
            let total = reader.u32()?;
            let word_a = reader.u64()?;
            let instance_count = reader.prefixed()?;
            let mut instances = Vec::new();
            for _ in 0..instance_count {
                instances.push(read_string(reader)?);
            }
            entitlements.push(EntitlementRecord {
                id,
                alias,
                name,
                kind,
                amounts,
                total,
                word_a,
                instances,
                word_b: reader.u64()?,
                updated: read_string(reader)?,
                byte_c: reader.u8()?,
            });
        }
        Ok(Self {
            header,
            request,
            transaction,
            status,
            byte_a,
            entitlements,
            done: reader.boolean()?,
        })
    }
}

/// Defines a store answer: an [`EntitlementRequest`] under its own type index.
macro_rules! entitlement_answer {
    ($(#[$doc:meta])* $name:ident, $type_index:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(pub EntitlementRequest);

        impl BodyCodec for $name {
            const TYPE_INDEX: u64 = $type_index;

            fn encode(&self) -> Result<Vec<u8>, MessageError> {
                let mut writer = Writer::new();
                self.0.write(&mut writer)?;
                Ok(writer.finish())
            }

            fn decode(body: &[u8]) -> Result<Self, MessageError> {
                let mut reader = Reader::new(body);
                let request = EntitlementRequest::read(&mut reader)?;
                reader.finish()?;
                Ok(Self(request))
            }
        }
    };
}

entitlement_answer!(
    /// 5936: the purchase is taken.
    RequestInitializedMsg,
    REQUEST_INITIALIZED_TYPE_INDEX
);
entitlement_answer!(
    /// 1285: the purchase is done, with the entitlements it changed.
    RequestSuccessMsg,
    REQUEST_SUCCESS_TYPE_INDEX
);

/// One store call of the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreCall {
    /// 4908: the player opened (true) or closed the store.
    SetIsInStore {
        /// The header (the player's `PlayerComponent`).
        header: FacetHeader,
        /// Whether the store is open.
        in_store: bool,
    },
    /// 898: a store purchase.
    Purchase {
        /// The header (the player's `EntitlementComponent`).
        header: FacetHeader,
        /// The purchase.
        purchase: StorePurchase,
    },
    /// 5107: the claim of an entitlement.
    ClaimEntitlement {
        /// The header.
        header: FacetHeader,
        /// The entitlement: the CRC-32 of its lowercase id.
        entitlement: u32,
    },
    /// 4932: buy one reward of the PvP track.
    PurchaseReward {
        /// The header (the player's `RewardTrackComponent`).
        header: FacetHeader,
        /// The reward slot (retail 1 and 7).
        slot: u8,
        /// The reward item: the CRC-32 of its lowercase id.
        item: u32,
        /// The cost the client shows.
        cost: u32,
    },
    /// 4910: reroll the PvP track.
    ResetRewardTrack {
        /// The header.
        header: FacetHeader,
    },
}

impl StoreCall {
    /// Decodes `body` when `type_index` is a store call; `None` otherwise.
    pub fn decode(type_index: u64, body: &[u8]) -> Option<Result<Self, MessageError>> {
        let read = |reader: &mut Reader<'_>| -> Result<Self, MessageError> {
            let header = FacetHeader::read(reader)?;
            Ok(match type_index {
                SET_IS_IN_STORE_TYPE_INDEX => Self::SetIsInStore {
                    header,
                    in_store: reader.boolean()?,
                },
                INITIALIZE_TRANSACTION_TYPE_INDEX => Self::Purchase {
                    header,
                    purchase: StorePurchase {
                        language: read_string(reader)?,
                        currency: read_string(reader)?,
                        country: read_string(reader)?,
                        word_a: reader.u32()?,
                        transaction: reader.raw_id()?,
                        offer: read_string(reader)?,
                        offer_again: read_string(reader)?,
                        byte_b: reader.u8()?,
                        product: read_string(reader)?,
                        price: reader.u32()?,
                        byte_c: reader.u8()?,
                    },
                },
                CLAIM_ENTITLEMENT_TYPE_INDEX => Self::ClaimEntitlement {
                    header,
                    entitlement: reader.u32()?,
                },
                PURCHASE_REWARD_TYPE_INDEX => Self::PurchaseReward {
                    header,
                    slot: reader.u8()?,
                    item: reader.u32()?,
                    cost: reader.u32()?,
                },
                _ => Self::ResetRewardTrack { header },
            })
        };
        match type_index {
            SET_IS_IN_STORE_TYPE_INDEX
            | INITIALIZE_TRANSACTION_TYPE_INDEX
            | CLAIM_ENTITLEMENT_TYPE_INDEX
            | PURCHASE_REWARD_TYPE_INDEX
            | RESET_REWARD_TRACK_TYPE_INDEX => {
                let mut reader = Reader::new(body);
                Some(read(&mut reader).and_then(|call| {
                    reader.finish()?;
                    Ok(call)
                }))
            }
            _ => None,
        }
    }

    /// The body bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MessageError::CountTooLarge`] for a text that is too long.
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        let mut writer = Writer::new();
        match self {
            Self::SetIsInStore { header, in_store } => {
                header.write(&mut writer);
                writer.boolean(*in_store);
            }
            Self::Purchase { header, purchase } => {
                header.write(&mut writer);
                write_string(&mut writer, &purchase.language)?;
                write_string(&mut writer, &purchase.currency)?;
                write_string(&mut writer, &purchase.country)?;
                writer.u32(purchase.word_a).bytes(&purchase.transaction);
                write_string(&mut writer, &purchase.offer)?;
                write_string(&mut writer, &purchase.offer_again)?;
                writer.u8(purchase.byte_b);
                write_string(&mut writer, &purchase.product)?;
                writer.u32(purchase.price).u8(purchase.byte_c);
            }
            Self::ClaimEntitlement {
                header,
                entitlement,
            } => {
                header.write(&mut writer);
                writer.u32(*entitlement);
            }
            Self::PurchaseReward {
                header,
                slot,
                item,
                cost,
            } => {
                header.write(&mut writer);
                writer.u8(*slot).u32(*item).u32(*cost);
            }
            Self::ResetRewardTrack { header } => header.write(&mut writer),
        }
        Ok(writer.finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::hex_bytes;

    /// Retail calls: `unlock_store_content` 12633 ms (4908), 23700 ms (898), `ExternalRewards`
    /// 66150 ms (5107), `pvp_track_unlocked` 31868 ms (4932) and 56565 ms (4910).
    const CALLS: [(u64, &str); 5] = [
        (
            SET_IS_IN_STORE_TYPE_INDEX,
            "07a3b18acf378b0b2896c8c14210296e01",
        ),
        (
            INITIALIZE_TRANSACTION_TYPE_INDEX,
            "eb5c1ce8c5cdbff72896c8c14210296e05656e2d55530345555202495400000001e58f4711d7e74ac483462fb23806808c16366745374b715a53724251564c31724c4c42785a6e4116366745374b715a53724251564c31724c4c42785a6e4100104d61726b73206f6620466f7274756e650000000001",
        ),
        (
            CLAIM_ENTITLEMENT_TYPE_INDEX,
            "a3f72a79438587bbe131d651bfc7f132f938e156",
        ),
        (
            PURCHASE_REWARD_TYPE_INDEX,
            "67e3d016f476590a2896c8c14210296e07fe1498030000c350",
        ),
        (
            RESET_REWARD_TRACK_TYPE_INDEX,
            "67e3d016f476590a2896c8c14210296e",
        ),
    ];

    #[test]
    fn retail_calls_read_and_write_back() {
        for (type_index, hex) in CALLS {
            let call = StoreCall::decode(type_index, &hex_bytes(hex))
                .expect("a call")
                .expect("decodes");
            assert_eq!(
                call.encode().expect("encode"),
                hex_bytes(hex),
                "{type_index}"
            );
        }
        let Some(Ok(StoreCall::Purchase { purchase, .. })) =
            StoreCall::decode(CALLS[1].0, &hex_bytes(CALLS[1].1))
        else {
            panic!("purchase");
        };
        assert_eq!(purchase.product, b"Marks of Fortune");
        let Some(Ok(StoreCall::PurchaseReward { slot, cost, .. })) =
            StoreCall::decode(CALLS[3].0, &hex_bytes(CALLS[3].1))
        else {
            panic!("reward");
        };
        assert_eq!((slot, cost), (7, 50_000));
    }

    /// Retail answers to the first purchase of `mount-menu-buying-all-store-horses` (the Ronin
    /// Bear Mount, 545.93 s and 546.05 s).
    const INITIALIZED: &str = "dbff741f993e0651193a1ff815806fc8606425b0a4464137b37787b0d13b7dd71336323130363032383339393533353939353933000000020000\
        01";
    const SUCCESS: &str = "dbff741f993e0651193a1ff815806fc8606425b0a4464137b37787b0d13b7dd71336323130363032383339393533353939353933000000020002153045443359\
        61427a536c5845504b485649316d35371a456e745f4d6f756e745f4d54585f375f626561725f526f6e696e10526f6e696e2042656172204d6f756e74\
        00000000000000000000000000000000000000010000000000000000012438616661623433322d313434352d343236392d623135392d383034393165\
        303666663238000000000000000018323032362d31312d30345431323a33323a32362e3030305a00163168436c77424a7846326568346d44634359\
        56343562104d61726b73206f6620466f7274756e651c4e657720576f726c64202d204d61726b73206f6620466f7274756e650000000200001f4000\
        00138800000000000032c8000000000000000000000000000000000018323032362d31312d30345431323a33323a32362e3030305a0001";

    #[test]
    fn retail_store_answers_read_and_write_back() {
        let initialized = RequestInitializedMsg::decode(&hex_bytes(INITIALIZED)).expect("5936");
        assert_eq!(initialized.0.status, RETAIL_REQUEST_STATUS);
        assert!(initialized.0.entitlements.is_empty());
        assert_eq!(
            initialized.encode().expect("encode"),
            hex_bytes(INITIALIZED)
        );
        let success = RequestSuccessMsg::decode(&hex_bytes(SUCCESS)).expect("1285");
        assert_eq!(success.encode().expect("encode"), hex_bytes(SUCCESS));
        assert_eq!(success.0.request, initialized.0.request);
        assert_eq!(success.0.transaction, b"6210602839953599593");
        let [mount, wallet] = &success.0.entitlements[..] else {
            panic!("two entitlements");
        };
        assert_eq!(mount.alias, b"Ent_Mount_MTX_7_bear_Ronin");
        assert_eq!((mount.kind, mount.total), (DURABLE_ENTITLEMENT_KIND, 1));
        assert_eq!(
            mount.instances,
            [b"8afab432-1445-4269-b159-80491e06ff28".to_vec()]
        );
        assert_eq!(
            (wallet.alias.as_slice(), wallet.total),
            (&b"Marks of Fortune"[..], 13_000)
        );
        assert_eq!(wallet.kind, WALLET_ENTITLEMENT_KIND);
    }

    #[test]
    fn other_types_are_not_store_calls() {
        assert!(StoreCall::decode(SET_IS_IN_STORE_TYPE_INDEX + 1, &[]).is_none());
    }
}
