// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who may use the admin site (admin studio spec sections 4 and 9): local mode (one user on
//! this PC, every right) or server mode (logins with the game accounts, ranks, HTTPS).

pub mod api;
pub mod discord;
pub mod session;
pub mod store;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use axum::http::{HeaderMap, header};
use nw_common::time::unix_ms;
use nw_db::AuditEntry;
use nw_login::keys::token_hash;
use nw_login::password::PasswordHasher;
use serde::Deserialize;

use crate::access::discord::AdminDiscord;
use crate::access::session::{Actor, Sessions};
use crate::access::store::Store;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "blighted_session";

/// How the admin site is used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// One user on this PC: loopback only, no login, every right.
    #[default]
    Local,
    /// GMs and admins over HTTPS: login, ranks, audit log.
    Server,
}

/// The access state every route shares.
#[derive(Debug, Clone)]
pub struct Access {
    mode: AccessMode,
    store: Option<Store>,
    sessions: Arc<Mutex<Sessions>>,
    hasher: Option<PasswordHasher>,
    discord: Option<AdminDiscord>,
}

impl Access {
    /// Access in `mode`, with the store of logins, ranks and the audit log, and the password
    /// hasher (both required in server mode; in local mode the store only keeps the audit log).
    pub fn new(mode: AccessMode, store: Option<Store>, hasher: Option<PasswordHasher>) -> Self {
        Self {
            mode,
            store,
            sessions: Arc::default(),
            hasher,
            discord: None,
        }
    }

    /// The same access with Discord login (server mode).
    #[must_use]
    pub fn with_discord(self, discord: AdminDiscord) -> Self {
        Self {
            discord: Some(discord),
            ..self
        }
    }

    /// The mode.
    pub fn mode(&self) -> AccessMode {
        self.mode
    }

    /// Who sends a request with these headers: the local user, the account of a live session
    /// cookie, or nobody.
    pub fn actor(&self, headers: &HeaderMap) -> Option<Actor> {
        match self.mode {
            AccessMode::Local => Some(Actor::local()),
            AccessMode::Server => {
                let token = session_token(headers)?;
                self.sessions().actor(&token_hash(&token), unix_ms())
            }
        }
    }

    /// Writes an audit row of `actor`; without a store the row only goes to the log.
    pub async fn record(
        &self,
        actor: &Actor,
        command: &str,
        target: &str,
        detail: serde_json::Value,
        result: &str,
    ) {
        tracing::info!(
            actor = actor.username,
            command,
            target,
            %detail,
            result,
            "audit"
        );
        let entry = AuditEntry {
            account: actor.account,
            rank: Some(actor.rank),
            command: command.to_owned(),
            target: target.to_owned(),
            detail,
            result: result.to_owned(),
        };
        if let Some(store) = &self.store
            && let Err(error) = store.audit(&entry).await
        {
            tracing::warn!(%error, "audit row not stored");
        }
    }

    pub(crate) fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    pub(crate) fn hasher(&self) -> Option<&PasswordHasher> {
        self.hasher.as_ref()
    }

    /// Discord login; only in server mode.
    /// Whether the site takes a name and password: server mode without Discord login. A site
    /// has one login method only.
    pub(crate) fn password_login(&self) -> bool {
        self.mode == AccessMode::Server && self.discord().is_none()
    }

    pub(crate) fn discord(&self) -> Option<&AdminDiscord> {
        self.discord
            .as_ref()
            .filter(|_| self.mode == AccessMode::Server)
    }

    pub(crate) fn sessions(&self) -> MutexGuard<'_, Sessions> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The session token of the `Cookie` header.
pub(crate) fn session_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; blighted_session=abc; other=1"),
        );
        assert_eq!(session_token(&headers).as_deref(), Some("abc"));
        assert_eq!(session_token(&HeaderMap::new()), None);
    }
}
