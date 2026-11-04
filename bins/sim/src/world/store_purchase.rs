// SPDX-License-Identifier: AGPL-3.0-or-later
//! Store purchases (898). While `[shop] enabled` is on, the sim sells each mount of the store
//! table: for 0 while `[shop] free` is on (retail's last weeks, `mount-menu-buying-all-store-horses`,
//! 2026-11-04: 35 mounts for 0 Marks of Fortune), else for its price from the account's wallet.
//! The sim answers 5936 at once; the loop runs the [`StoreGrant`] (pays and saves the entitlement
//! to the player's account in the global database, in one transaction); then the sim answers 1285
//! with the entitlement and the wallet (retail: 0.12 s and 0.24 s after the call). The account
//! then holds the mount on every sim and every later login, and the store service lists it.

use bevy_ecs::prelude::Entity;
use nw_common::time::iso8601_millis;
use nw_db::{AccountEntitlement, DbError, Purchase, Wallet};
use nw_messages::store::{
    DURABLE_ENTITLEMENT_KIND, EntitlementRecord, EntitlementRequest, RETAIL_REQUEST_STATUS,
    RequestInitializedMsg, RequestSuccessMsg, StorePurchase, WALLET_ENTITLEMENT_KIND,
};
use nw_messages::{FacetHeader, RAW_ID_LEN, ServerMessage};
use nw_protocol::store_api::{STORE_CURRENCY, STORE_CURRENCY_NAME};
use uuid::Uuid;

use super::economy::count_call;
use super::{Player, SimWorld, send_now};

/// Nanoseconds in a millisecond: a transaction number is the time in nanoseconds (retail's are
/// 19 digits).
const NANOS_PER_MILLI: u64 = 1_000_000;
/// The amount of a mount entitlement.
const ONE: u32 = 1;

/// A store purchase that the loop pays and saves to the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreGrant {
    /// The buyer's entity number.
    pub number: u64,
    /// The buyer's account.
    pub account: Uuid,
    /// The entitlement it buys.
    pub entitlement: String,
    /// Its price in Marks of Fortune: 0 in a free store.
    pub price: u32,
    /// The header of the purchase call; the answers carry it.
    header: FacetHeader,
    /// The request id of the purchase call.
    request: [u8; RAW_ID_LEN],
    /// The transaction number of the answers.
    transaction: Vec<u8>,
}

impl StoreGrant {
    /// The answer to the grant's purchase that lists `entitlements`.
    fn answer(&self, entitlements: Vec<EntitlementRecord>) -> EntitlementRequest {
        EntitlementRequest {
            header: self.header,
            request: self.request,
            transaction: self.transaction.clone(),
            status: RETAIL_REQUEST_STATUS,
            byte_a: 0,
            entitlements,
            done: true,
        }
    }
}

/// ISO 8601 text of Unix milliseconds `ms`.
fn iso_time(ms: i64) -> Vec<u8> {
    iso8601_millis(u64::try_from(ms).unwrap_or_default()).into_bytes()
}

/// The 1285 record of the entitlement `held`, named `name`.
fn record(held: &AccountEntitlement, name: &str) -> EntitlementRecord {
    EntitlementRecord {
        id: held.entitlement.as_bytes().to_vec(),
        alias: held.entitlement.as_bytes().to_vec(),
        name: name.as_bytes().to_vec(),
        kind: DURABLE_ENTITLEMENT_KIND,
        amounts: [0; 3],
        total: ONE,
        word_a: 0,
        instances: vec![held.instance.to_string().into_bytes()],
        word_b: 0,
        updated: iso_time(held.granted_at_ms),
        byte_c: 0,
    }
}

/// The 1285 record of `wallet`: every Mark of this server is free (none is bought with money).
fn wallet_record(wallet: Wallet) -> EntitlementRecord {
    let marks = u32::try_from(wallet.marks).unwrap_or(u32::MAX);
    EntitlementRecord {
        id: STORE_CURRENCY.as_bytes().to_vec(),
        alias: STORE_CURRENCY.as_bytes().to_vec(),
        name: STORE_CURRENCY_NAME.as_bytes().to_vec(),
        kind: WALLET_ENTITLEMENT_KIND,
        amounts: [marks, 0, 0],
        total: marks,
        word_a: 0,
        instances: Vec::new(),
        word_b: 0,
        updated: iso_time(wallet.updated_at_ms),
        byte_c: 0,
    }
}

impl SimWorld {
    /// Player `entity` buys the offer of `purchase` (898). While the store sells, an offer of the
    /// store table (its id is the entitlement) gets 5936 now and a grant for the loop, at its
    /// price. A closed store or another offer is refused with no answer (retail's refusal message
    /// 3113 `OnRequestFail` has no capture; its layout is Unknown).
    pub(super) fn store_purchase(
        &mut self,
        entity: Entity,
        header: FacetHeader,
        purchase: &StorePurchase,
    ) {
        let offer = String::from_utf8_lossy(&purchase.offer).into_owned();
        if !self.content.shop.enabled {
            count_call("store_purchase", false);
            tracing::info!(offer, "store purchase refused: the store is closed");
            return;
        }
        if self.content.store.mount_of(&offer).is_none() {
            count_call("store_purchase", false);
            tracing::info!(offer, "store purchase refused: no such store offer");
            return;
        }
        let Some(player) = self.world.get::<Player>(entity) else {
            return;
        };
        let (number, account, connection) =
            (player.number, player.account, player.connection.clone());
        let transaction = (self.last_now_ms.saturating_mul(NANOS_PER_MILLI))
            .max(self.economy.last_transaction.saturating_add(1));
        self.economy.last_transaction = transaction;
        let grant = StoreGrant {
            number,
            account,
            price: self.content.shop.price(&offer),
            entitlement: offer,
            header,
            request: purchase.transaction,
            transaction: transaction.to_string().into_bytes(),
        };
        let started =
            ServerMessage::RequestInitialized(RequestInitializedMsg(grant.answer(Vec::new())));
        send_now(&mut self.combat_messages, &connection, &[started]);
        count_call("store_purchase", true);
        self.economy.grants.push(grant);
    }

    /// The store purchases that the loop pays and saves now.
    pub fn take_store_grants(&mut self) -> Vec<StoreGrant> {
        std::mem::take(&mut self.economy.grants)
    }

    /// The loop ran `grant` (`answer`): when the account holds the entitlement now, the buyer,
    /// if it is still on this sim, may use the mount and gets 1285 with it and the wallet. A
    /// wallet below the price or a failed save only logs: the buyer holds nothing new, and its
    /// client waits.
    pub fn store_granted(&mut self, grant: &StoreGrant, answer: Result<Purchase, DbError>) {
        let (held, wallet) = match answer {
            Ok(Purchase::Bought { held, wallet } | Purchase::AlreadyHeld { held, wallet }) => {
                (held, wallet)
            }
            Ok(Purchase::TooPoor { wallet }) => {
                tracing::info!(
                    entitlement = grant.entitlement,
                    price = grant.price,
                    marks = wallet.marks,
                    "store purchase refused: the wallet holds too few Marks of Fortune"
                );
                return;
            }
            Err(error) => {
                tracing::error!(%error, entitlement = grant.entitlement, account = %grant.account, "store purchase not saved");
                return;
            }
        };
        let Some(entity) = self.player_by_number(grant.number) else {
            return;
        };
        let name = self.content.store.mount_of(&held.entitlement).map_or_else(
            || held.entitlement.clone(),
            |mount| mount.unlock.name.clone(),
        );
        let Some(mut player) = self.world.get_mut::<Player>(entity) else {
            return;
        };
        player.entitlements.insert(held.entitlement.clone());
        let connection = player.connection.clone();
        let done = ServerMessage::RequestSuccess(RequestSuccessMsg(
            grant.answer(vec![record(&held, &name), wallet_record(wallet)]),
        ));
        send_now(&mut self.combat_messages, &connection, &[done]);
    }
}
