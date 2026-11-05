// SPDX-License-Identifier: AGPL-3.0-or-later
//! Login queue and entry tickets (M2 spec section 8; review findings C1, C2, C3, I3).
//!
//! One entry per account and world. Waiting entries that stop refreshing are removed. Entries are
//! admitted from the front while the live entry records are fewer than both `capacity` and the
//! free player slots that `world-director` reports (M5 spec section 9); an entry record lives
//! [`ENTRY_TICKET_SECONDS`]. With no report, nobody is admitted. The Steam ticket is never stored.

use std::future::Future;

use nw_protocol::{LoginQueueReply, LoginQueueResponse, LoginQueueToken};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::WorldConfig;
use crate::secrets::SigningKey;
use crate::signature::{EntryClaims, sign_entry};
use crate::store::StoreError;
use crate::time::MILLIS_PER_SECOND;

/// Lifetime of an entry record (parent spec 4.2).
pub const ENTRY_TICKET_SECONDS: u64 = 60;
/// A waiting entry is removed after this many missed refresh intervals.
pub const STALE_REFRESH_INTERVALS: u64 = 3;
/// The queue never offers a transfer to another world.
const ALLOW_QUEUE_TRANSFER: bool = false;

/// Queue limits of one world.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueSettings {
    /// World of the queue.
    pub world_id: Uuid,
    /// Live entry records allowed at one time (the entry ticket limit).
    pub capacity: u32,
    /// A waiting entry not refreshed for this long is removed.
    pub stale_after_ms: u64,
}

impl QueueSettings {
    /// The settings of a configured world.
    pub fn for_world(world: &WorldConfig) -> Self {
        Self {
            world_id: world.id,
            capacity: world.capacity,
            stale_after_ms: world
                .refresh_interval_seconds
                .saturating_mul(STALE_REFRESH_INTERVALS)
                .saturating_mul(MILLIS_PER_SECOND),
        }
    }
}

/// An admitted entry: what `rep-gateway` checks and deletes in M3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRecord {
    /// Ticket id.
    pub ticket_id: String,
    /// Account that may enter.
    pub account_id: Uuid,
    /// World to enter.
    pub world_id: Uuid,
    /// Character that enters.
    pub character_id: Uuid,
    /// Expiry, Unix seconds.
    pub expires_at: i64,
}

impl EntryRecord {
    /// The fields the signature covers.
    pub fn claims(&self) -> EntryClaims {
        EntryClaims {
            ticket_id: self.ticket_id.clone(),
            account_id: self.account_id,
            world_id: self.world_id,
            character_id: self.character_id,
            expires_at: self.expires_at,
        }
    }
}

/// Where an entry stands after a join or refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueStatus {
    /// Still waiting.
    Waiting {
        /// Ticket id the client refreshes with.
        ticket_id: String,
        /// Place in the queue, 1 = next.
        position: u64,
    },
    /// Admitted; the record is live.
    Admitted(EntryRecord),
}

/// A queue request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueAction {
    /// Join (or rejoin) with a character. `new_ticket_id` is used when a new entry is made.
    Join {
        /// The character to enter with.
        character_id: Uuid,
        /// Ticket id for a new entry.
        new_ticket_id: String,
    },
    /// Refresh an entry of this account.
    Refresh {
        /// Ticket id from an earlier reply.
        ticket_id: String,
    },
}

/// The time a queue update runs at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTime {
    /// The store's own clock (Valkey `TIME`), so that several `edge-http` copies agree.
    StoreClock,
    /// A fixed time in Unix milliseconds (tests).
    At(u64),
}

/// The login queues (Valkey; an in-memory version for tests).
pub trait QueueStore: Clone + Send + Sync + 'static {
    /// Applies `action` for `account_id` at `time` and admits what fits. `None` means the ticket
    /// is unknown or belongs to another account.
    fn update(
        &self,
        settings: &QueueSettings,
        account_id: Uuid,
        action: QueueAction,
        time: QueueTime,
    ) -> impl Future<Output = Result<Option<QueueStatus>, StoreError>> + Send;

    /// Sets the free player slots of `world_id` for `ttl_ms`; after that the world has none.
    fn set_free_slots(
        &self,
        world_id: Uuid,
        slots: u32,
        ttl_ms: u64,
    ) -> impl Future<Output = Result<(), StoreError>> + Send;

    /// The free player slots `world-director` last reported for `world_id`; 0 when its report
    /// is older than its time to live, or none came.
    fn free_slots(&self, world_id: Uuid) -> impl Future<Output = Result<u32, StoreError>> + Send;

    /// Returns the entry record of `ticket_id` one time and deletes it, with its queue entry and
    /// its admission slot (M3 spec section 7). `None` when the record is absent or already
    /// taken. The caller checks `expires_at`.
    fn take_entry(
        &self,
        ticket_id: &str,
    ) -> impl Future<Output = Result<Option<EntryRecord>, StoreError>> + Send;
}

/// The reply body for a queue status. A waiting reply has no `Token`, so the client waits and
/// refreshes after `RefreshInterval` (protocol note 1.3). An admitted reply carries the signed
/// `Token` with the `RepAddress` of the world's own `rep-gateway`.
pub fn queue_reply(
    status: &QueueStatus,
    world: &WorldConfig,
    key: &SigningKey,
    now: i64,
) -> LoginQueueReply {
    let refresh_interval = i64::try_from(world.refresh_interval_seconds).unwrap_or(i64::MAX);
    let base = LoginQueueResponse {
        allow_queue_transfer: Some(ALLOW_QUEUE_TRANSFER),
        queue_name: Some(world.region.clone()),
        refresh_interval: Some(refresh_interval),
        ..LoginQueueResponse::default()
    };
    let response = match status {
        QueueStatus::Waiting {
            ticket_id,
            position,
        } => {
            let position = i64::try_from(*position).unwrap_or(i64::MAX);
            LoginQueueResponse {
                position: Some(position),
                estimated_time: Some(position.saturating_mul(refresh_interval)),
                ticket_id: Some(ticket_id.clone()),
                ..base
            }
        }
        QueueStatus::Admitted(record) => {
            // Unix seconds are exact in f64 until the year 285 million.
            let now = now as f64;
            LoginQueueResponse {
                position: Some(0),
                ticket_id: Some(record.ticket_id.clone()),
                token: Some(LoginQueueToken {
                    signature: Some(sign_entry(key, &record.claims())),
                    rep_address: Some(world.rep_address.clone()),
                    ticket_id: Some(record.ticket_id.clone()),
                    world_id: Some(record.world_id.to_string()),
                    character_id: Some(record.character_id.to_string()),
                    persona_id: Some(record.account_id.to_string()),
                    generate_time: Some(now),
                    issue_time: Some(now),
                    ..LoginQueueToken::default()
                }),
                ..base
            }
        }
    };
    LoginQueueReply {
        login_queue_response: response,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::tests::valid;
    use crate::signature::verify_entry;

    const KEY_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
    const NOW: i64 = 1_760_400_000;
    const POSITION: u64 = 3;

    fn record() -> EntryRecord {
        EntryRecord {
            ticket_id: "ticket-1".to_owned(),
            account_id: Uuid::from_u128(1),
            world_id: Uuid::from_u128(2),
            character_id: Uuid::from_u128(3),
            expires_at: NOW + i64::try_from(ENTRY_TICKET_SECONDS).expect("small"),
        }
    }

    #[test]
    fn settings_follow_the_world() {
        let world = &valid().worlds[0];
        let settings = QueueSettings::for_world(world);
        assert_eq!(settings.capacity, world.capacity);
        assert_eq!(
            settings.stale_after_ms,
            world.refresh_interval_seconds * STALE_REFRESH_INTERVALS * MILLIS_PER_SECOND
        );
    }

    #[test]
    fn huge_refresh_interval_does_not_overflow() {
        let mut world = valid().worlds[0].clone();
        world.refresh_interval_seconds = u64::MAX;
        assert_eq!(QueueSettings::for_world(&world).stale_after_ms, u64::MAX);
    }

    #[test]
    fn waiting_reply_has_no_token() {
        let config = valid();
        let key = SigningKey::from_hex(KEY_HEX).expect("key");
        let status = QueueStatus::Waiting {
            ticket_id: "ticket-1".to_owned(),
            position: POSITION,
        };
        let reply = queue_reply(&status, &config.worlds[0], &key, NOW);
        let response = reply.login_queue_response;
        let refresh = i64::try_from(config.worlds[0].refresh_interval_seconds).expect("small");
        assert_eq!(
            response.position,
            Some(i64::try_from(POSITION).expect("small"))
        );
        assert_eq!(response.ticket_id.as_deref(), Some("ticket-1"));
        assert_eq!(response.refresh_interval, Some(refresh));
        assert!(response.token.is_none());
    }

    #[test]
    fn admitted_reply_carries_a_verifiable_token() {
        let config = valid();
        let key = SigningKey::from_hex(KEY_HEX).expect("key");
        let status = QueueStatus::Admitted(record());
        let reply = queue_reply(&status, &config.worlds[0], &key, NOW);
        let token = reply.login_queue_response.token.expect("token");
        assert_eq!(token.rep_address.as_deref(), Some("127.0.0.1:33435"));
        assert_eq!(token.persona_id, Some(Uuid::from_u128(1).to_string()));
        assert_eq!(token.generate_time, Some(NOW as f64));
        let signature = token.signature.expect("signature");
        assert!(verify_entry(&key, &record().claims(), &signature));
    }
}
