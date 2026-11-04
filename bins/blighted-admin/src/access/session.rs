// SPDX-License-Identifier: AGPL-3.0-or-later
//! Admin site sessions and the login lockout (admin studio spec section 9.1). Only the SHA-256
//! of a session token is kept.

use std::collections::HashMap;

use nw_db::AdminRank;
use serde::Serialize;
use uuid::Uuid;

/// Life of a session.
pub const SESSION_TTL_MS: u64 = 12 * 60 * 60 * 1000;
/// Failed logins in [`FAIL_WINDOW_MS`] that lock a username.
pub const LOCK_FAILS: usize = 5;
/// Window of the failed logins that count.
pub const FAIL_WINDOW_MS: u64 = 10 * 60 * 1000;
/// How long a username stays locked.
pub const LOCK_MS: u64 = 15 * 60 * 1000;
/// Time to finish a Discord login on the Discord page.
pub const DISCORD_LOGIN_MS: u64 = 5 * 60 * 1000;
/// Most Discord logins that wait at one time; more are refused until some end.
pub const MAX_DISCORD_LOGINS: usize = 1000;

/// Who sends a request: an account with a rank, or the local user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Actor {
    /// The account; `None` for the local user.
    pub account: Option<Uuid>,
    /// The login name, or `local`.
    pub username: String,
    /// The rank.
    pub rank: AdminRank,
}

impl Actor {
    /// The user of a local-mode site: every right, no account.
    pub fn local() -> Self {
        Self {
            account: None,
            username: "local".to_owned(),
            rank: AdminRank::HeadAdmin,
        }
    }
}

#[derive(Debug, Clone)]
struct Session {
    actor: Actor,
    expires_ms: u64,
}

/// The sessions and the recent failed logins.
#[derive(Debug, Default)]
pub struct Sessions {
    by_token: HashMap<[u8; 32], Session>,
    fails: HashMap<String, Vec<u64>>,
    locked_until: HashMap<String, u64>,
    /// PKCE verifier and end time of each Discord login by its state.
    discord_logins: HashMap<String, (String, u64)>,
}

impl Sessions {
    /// Starts a session of `actor` for the token with hash `token_hash` at `now_ms`.
    pub fn open(&mut self, token_hash: [u8; 32], actor: Actor, now_ms: u64) {
        self.by_token
            .retain(|_, session| session.expires_ms > now_ms);
        self.by_token.insert(
            token_hash,
            Session {
                actor,
                expires_ms: now_ms + SESSION_TTL_MS,
            },
        );
    }

    /// The actor of a live session.
    pub fn actor(&self, token_hash: &[u8; 32], now_ms: u64) -> Option<Actor> {
        self.by_token
            .get(token_hash)
            .filter(|session| session.expires_ms > now_ms)
            .map(|session| session.actor.clone())
    }

    /// Ends a session.
    pub fn close(&mut self, token_hash: &[u8; 32]) {
        self.by_token.remove(token_hash);
    }

    /// True while `username` is locked.
    pub fn locked(&self, username: &str, now_ms: u64) -> bool {
        self.locked_until
            .get(username)
            .is_some_and(|until| *until > now_ms)
    }

    /// A failed login of `username`; the [`LOCK_FAILS`]th in the window locks it.
    pub fn failed(&mut self, username: &str, now_ms: u64) {
        let fails = self.fails.entry(username.to_owned()).or_default();
        fails.retain(|at| now_ms.saturating_sub(*at) < FAIL_WINDOW_MS);
        fails.push(now_ms);
        if fails.len() >= LOCK_FAILS {
            fails.clear();
            self.locked_until
                .insert(username.to_owned(), now_ms + LOCK_MS);
        }
    }

    /// A good login of `username` clears its failures.
    pub fn succeeded(&mut self, username: &str) {
        self.fails.remove(username);
    }

    /// Starts a Discord login of `state` with its PKCE `verifier` at `now_ms`; `false` when
    /// [`MAX_DISCORD_LOGINS`] wait already.
    pub fn begin_discord(&mut self, state: String, verifier: String, now_ms: u64) -> bool {
        self.discord_logins
            .retain(|_, (_, expires_ms)| *expires_ms > now_ms);
        if self.discord_logins.len() >= MAX_DISCORD_LOGINS {
            return false;
        }
        self.discord_logins
            .insert(state, (verifier, now_ms + DISCORD_LOGIN_MS));
        true
    }

    /// The verifier of a waiting Discord login of `state`; it can be taken once.
    pub fn take_discord(&mut self, state: &str, now_ms: u64) -> Option<String> {
        self.discord_logins
            .remove(state)
            .filter(|(_, expires_ms)| *expires_ms > now_ms)
            .map(|(verifier, _)| verifier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_000_000;
    const TOKEN: [u8; 32] = [7; 32];

    fn gm() -> Actor {
        Actor {
            account: Some(Uuid::from_u128(1)),
            username: "gm".to_owned(),
            rank: AdminRank::GameMaster,
        }
    }

    #[test]
    fn a_session_lives_for_its_ttl_and_can_be_closed() {
        let mut sessions = Sessions::default();
        sessions.open(TOKEN, gm(), NOW);
        assert_eq!(sessions.actor(&TOKEN, NOW + 1), Some(gm()));
        assert_eq!(sessions.actor(&TOKEN, NOW + SESSION_TTL_MS), None);
        assert_eq!(sessions.actor(&[0; 32], NOW), None);
        sessions.close(&TOKEN);
        assert_eq!(sessions.actor(&TOKEN, NOW + 1), None);
    }

    #[test]
    fn five_fails_in_the_window_lock_the_name() {
        let mut sessions = Sessions::default();
        for step in 0..LOCK_FAILS - 1 {
            sessions.failed("ada", NOW + step as u64);
        }
        assert!(!sessions.locked("ada", NOW + 10));
        sessions.failed("ada", NOW + 10);
        assert!(sessions.locked("ada", NOW + 11));
        assert!(sessions.locked("ada", NOW + LOCK_MS));
        assert!(!sessions.locked("ada", NOW + 10 + LOCK_MS));
        assert!(!sessions.locked("bob", NOW + 11), "other names are free");
    }

    #[test]
    fn a_discord_login_is_taken_once_before_it_expires() {
        let mut sessions = Sessions::default();
        assert!(sessions.begin_discord("s1".to_owned(), "v1".to_owned(), NOW));
        assert!(sessions.begin_discord("s2".to_owned(), "v2".to_owned(), NOW));
        assert_eq!(sessions.take_discord("s1", NOW + 1).as_deref(), Some("v1"));
        assert_eq!(sessions.take_discord("s1", NOW + 2), None, "only once");
        assert_eq!(sessions.take_discord("s2", NOW + DISCORD_LOGIN_MS), None);
        assert_eq!(sessions.take_discord("unknown", NOW), None);
    }

    #[test]
    fn too_many_waiting_discord_logins_are_refused() {
        let mut sessions = Sessions::default();
        for index in 0..MAX_DISCORD_LOGINS {
            assert!(sessions.begin_discord(index.to_string(), String::new(), NOW));
        }
        assert!(!sessions.begin_discord("one more".to_owned(), String::new(), NOW));
        assert!(
            sessions.begin_discord("later".to_owned(), String::new(), NOW + DISCORD_LOGIN_MS),
            "expired logins make room"
        );
    }

    #[test]
    fn old_fails_and_a_good_login_do_not_count() {
        let mut sessions = Sessions::default();
        for step in 0..LOCK_FAILS - 1 {
            sessions.failed("ada", NOW + step as u64);
        }
        sessions.failed("ada", NOW + FAIL_WINDOW_MS + 1);
        assert!(!sessions.locked("ada", NOW + FAIL_WINDOW_MS + 2));
        for step in 0..LOCK_FAILS - 1 {
            sessions.failed("bob", NOW + step as u64);
        }
        sessions.succeeded("bob");
        sessions.failed("bob", NOW + 10);
        assert!(!sessions.locked("bob", NOW + 11));
    }
}
