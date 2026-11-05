// SPDX-License-Identifier: AGPL-3.0-or-later
//! Helpers for tests in this and other crates (`test-util` feature).

use nw_protocol::store_api::StoreTable;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::extract::connect_info::MockConnectInfo;
use nw_db::PgPool;
use uuid::Uuid;

use crate::cms::CmsRouters;
use crate::config::{EdgeConfig, ShopConfig};
use crate::discord::DiscordHttp;
use crate::game::GameState;
use crate::host::{ServiceRouters, edge_router};
use crate::launcher::LauncherState;
use crate::password::PasswordHasher;
use crate::pg::{PgAccounts, PgCharacters};
use crate::secrets::SigningKey;
use crate::valkey::Valkey;

/// Environment variable with the Valkey URL for infrastructure tests.
pub const TEST_VALKEY_URL_ENV: &str = "NW_TEST_VALKEY_URL";
/// The Docker Compose Valkey.
pub const DEFAULT_TEST_VALKEY_URL: &str = "redis://127.0.0.1:6379";
/// Entry ticket key of tests.
pub const TEST_SIGNING_KEY_HEX: &str =
    "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
/// Stands for the `[aws_mirror]` certificate; the launcher API passes it on as text.
pub const TEST_AWS_MIRROR_CERTIFICATE_PEM: &str =
    "-----BEGIN CERTIFICATE-----\ntest\n-----END CERTIFICATE-----\n";
const ONE_HASH_AT_A_TIME: usize = 1;
const PEER_PORT: u16 = 50_000;
const PRIVATE_NETWORK: u8 = 10;

/// Valkey URL for infrastructure tests (`NW_TEST_VALKEY_URL` or the Docker Compose default).
pub fn test_valkey_url() -> String {
    std::env::var(TEST_VALKEY_URL_ENV).unwrap_or_else(|_| DEFAULT_TEST_VALKEY_URL.to_owned())
}

/// The key of [`TEST_SIGNING_KEY_HEX`].
///
/// # Panics
///
/// Never: the constant is a valid key.
pub fn test_signing_key() -> SigningKey {
    SigningKey::from_hex(TEST_SIGNING_KEY_HEX).expect("test signing key")
}

/// A peer address no other test run uses, so the per-IP limits in the shared Valkey stay low.
pub fn random_peer() -> SocketAddr {
    let bytes = Uuid::new_v4().into_bytes();
    SocketAddr::from(([PRIVATE_NETWORK, bytes[0], bytes[1], bytes[2]], PEER_PORT))
}

/// Every `edge-http` route over the real stores, as `edge-http` builds them, for the first world
/// of `config`. Discord is off, the key is [`test_signing_key`], the mirror certificate is
/// [`TEST_AWS_MIRROR_CERTIFICATE_PEM`], the bucket host has no objects, and every request comes
/// from one [`random_peer`].
///
/// # Panics
///
/// When the password worker pool cannot start.
pub fn edge_app(
    config: Arc<EdgeConfig>,
    global: &PgPool,
    world: &PgPool,
    valkey: Valkey,
) -> Router {
    let world_id = config.worlds[0].id;
    let launcher = LauncherState {
        config: Arc::clone(&config),
        accounts: PgAccounts::new(global.clone()),
        game_sessions: valkey.clone(),
        limits: valkey.clone(),
        discord_states: valkey.clone(),
        discord: None::<DiscordHttp>,
        hasher: PasswordHasher::new(ONE_HASH_AT_A_TIME).expect("hasher"),
        aws_mirror_certificate_pem: TEST_AWS_MIRROR_CERTIFICATE_PEM.into(),
    };
    let game = GameState {
        config,
        game_sessions: valkey.clone(),
        characters: PgCharacters::new(global.clone(), HashMap::from([(world_id, world.clone())])),
        limits: valkey.clone(),
        queue: valkey,
        signing_key: test_signing_key(),
    };
    edge_router(
        launcher,
        game,
        ServiceRouters::on_postgres(
            global,
            Arc::new(StoreTable::default()),
            Arc::new(ShopConfig::default()),
            crate::analytics::AnalyticsConfig::default(),
            Router::new(),
            CmsRouters {
                news: Router::new(),
                images: Router::new(),
            },
        ),
    )
    .layer(MockConnectInfo(random_peer()))
}
