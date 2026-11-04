// SPDX-License-Identifier: AGPL-3.0-or-later
//! Routing by host name (M2 spec section 14, parent spec 4.2). `login.<domain>` serves the
//! launcher API and the channel document; `game.<domain>` serves the game routes, the stats
//! service and the shop services. The Kinesis and STS host names of the client's analytics get our analytics stream
//! when they lead here, the remote-config bucket host gets our own config sets, and the news host
//! and its image host get our own news. A request for any other host gets `421`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;

use nw_db::PgPool;
use nw_protocol::aws_mirror::{CMS_HOST, CMS_IMAGE_HOST, remote_config_host};
use nw_protocol::store_api::StoreTable;

use crate::analytics::{AnalyticsState, is_kinesis_host, is_sts_host, kinesis_router, sts_router};
use crate::cms::CmsRouters;
use crate::config::ShopConfig;
use crate::discord::DiscordApi;
use crate::error::ApiError;
use crate::game::{GameState, channel_router, game_router, unknown_route};
use crate::launcher::{LauncherState, launcher_router};
use crate::pg::{PgEntitlements, PgStats};
use crate::queue::QueueStore;
use crate::shop::{ShopState, shop_router};
use crate::stats::{StatsState, stats_router};
use crate::store::{
    AccountStore, CharacterStore, DiscordStateStore, GameSessionStore, RateLimiter,
};

const PORT_SEPARATOR: char = ':';
/// A fully qualified host name may end in a dot.
const ROOT_LABEL: char = '.';

#[derive(Clone)]
struct HostRoutes {
    login_host: Arc<str>,
    game_host: Arc<str>,
    login: Router,
    game: Router,
    kinesis: Router,
    sts: Router,
    bucket_host: Arc<str>,
    remote_config: Router,
    cms: Router,
    cms_images: Router,
}

/// The routes of the services outside the login logic.
pub struct ServiceRouters {
    /// The stats service ([`crate::stats::stats_router`]), on `game.<domain>`.
    pub stats: Router,
    /// The shop and entitlement services ([`crate::shop::shop_router`]), on `game.<domain>`.
    pub shop: Router,
    /// The analytics stream ([`crate::analytics::kinesis_router`]).
    pub kinesis: Router,
    /// The credentials of the analytics stream ([`crate::analytics::sts_router`]).
    pub sts: Router,
    /// Our own remote config sets ([`crate::remote_config::remote_config_router`]), on the
    /// bucket host.
    pub remote_config: Router,
    /// Our own news ([`crate::cms::CmsObjects::routers`]), on the news host and its image host.
    pub cms: CmsRouters,
}

impl ServiceRouters {
    /// The services over the migrated global database `global`, the store table `store` and the
    /// store settings `settings`, the remote config route
    /// ([`crate::remote_config::remote_config_router`]) and the news routes.
    pub fn on_postgres(
        global: &PgPool,
        store: Arc<StoreTable>,
        settings: Arc<ShopConfig>,
        remote_config: Router,
        cms: CmsRouters,
    ) -> Self {
        let stats = PgStats::new(global.clone());
        Self {
            shop: shop_router(ShopState {
                table: store,
                settings,
                entitlements: PgEntitlements::new(global.clone()),
            }),
            stats: stats_router(StatsState {
                stats: stats.clone(),
            }),
            kinesis: kinesis_router(AnalyticsState { analytics: stats }),
            sts: sts_router(),
            remote_config,
            cms,
        }
    }
}

/// The host a request names: the URI authority (HTTP/2) or the `Host` header (HTTP/1.1),
/// without port and final dot.
pub(crate) fn request_host(request: &Request) -> Option<&str> {
    let host = match request.uri().authority() {
        Some(authority) => authority.host(),
        None => {
            let header = request.headers().get(HOST)?.to_str().ok()?;
            header
                .rsplit_once(PORT_SEPARATOR)
                .filter(|(_, port)| port.bytes().all(|byte| byte.is_ascii_digit()))
                .map_or(header, |(host, _)| host)
        }
    };
    Some(host.trim_end_matches(ROOT_LABEL))
}

async fn by_host(State(routes): State<HostRoutes>, request: Request) -> Response {
    let host = request_host(&request).map(str::to_ascii_lowercase);
    let target = match host.as_deref() {
        Some(host) if host.eq_ignore_ascii_case(&routes.login_host) => routes.login,
        Some(host) if host.eq_ignore_ascii_case(&routes.game_host) => routes.game,
        Some(host) if is_kinesis_host(host) => routes.kinesis,
        Some(host) if is_sts_host(host) => routes.sts,
        Some(host) if host == &*routes.bucket_host => routes.remote_config,
        Some(CMS_HOST) => routes.cms,
        Some(CMS_IMAGE_HOST) => routes.cms_images,
        _ => return ApiError::UnknownHost.into_response(),
    };
    match target.oneshot(request).await {
        Ok(response) => response,
        Err(never) => match never {},
    }
}

/// Every route of `edge-http`, chosen by host name.
pub fn edge_router<A, G, R, D, X, C, Q>(
    launcher: LauncherState<A, G, R, D, X>,
    game: GameState<G, C, R, Q>,
    services: ServiceRouters,
) -> Router
where
    A: AccountStore,
    G: GameSessionStore,
    R: RateLimiter,
    D: DiscordStateStore,
    X: DiscordApi,
    C: CharacterStore,
    Q: QueueStore,
{
    let routes = HostRoutes {
        bucket_host: remote_config_host(&game.config.remote_config.bucket).into(),
        login_host: game.config.login_host().into(),
        game_host: game.config.game_host().into(),
        login: launcher_router(launcher)
            .merge(channel_router(game.clone()))
            .fallback(unknown_route)
            .method_not_allowed_fallback(unknown_route),
        game: game_router(game).merge(services.stats).merge(services.shop),
        kinesis: services.kinesis,
        sts: services.sts,
        remote_config: services.remote_config,
        cms: services.cms.news,
        cms_images: services.cms.images,
    };
    Router::new().fallback(by_host).with_state(routes)
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, Bytes};
    use axum::http::{Request as HttpRequest, StatusCode};
    use nw_protocol::game_api::{AUTH_STACK_STAGE, STEAM_AUTH_PATH};
    use nw_protocol::launcher_api::{AuthMode, INFO_PATH, LOGIN_PATH};

    use super::*;
    use crate::cms::CmsDocument;
    use crate::cms::tests::committed_objects;
    use crate::game::SESSION_PREFIX;
    use crate::game::tests::fixture;
    use crate::launcher::tests::state;
    use crate::memory::{MemoryAnalytics, MemoryEntitlements, MemoryStats};
    use crate::remote_config::remote_config_router;
    use crate::remote_config::tests::committed_sets;
    use crate::test_http::{self, error_code};

    const LOGIN_HOST: &str = "login.example.org";
    const GAME_HOST: &str = "game.example.org";

    async fn app_and_session() -> (Router, String) {
        let game = fixture().await;
        let mut launcher = state(AuthMode::Password);
        launcher.game_sessions = game.state.game_sessions.clone();
        let stats = MemoryStats::default();
        let services = ServiceRouters {
            shop: shop_router(ShopState {
                table: Arc::new(StoreTable::default()),
                settings: Arc::new(ShopConfig::default()),
                entitlements: MemoryEntitlements::default(),
            }),
            stats: stats_router(StatsState { stats }),
            kinesis: kinesis_router(AnalyticsState {
                analytics: MemoryAnalytics::default(),
            }),
            sts: sts_router(),
            remote_config: remote_config_router(committed_sets(&game.state.config)),
            cms: committed_objects(&game.state.config).routers(),
        };
        (edge_router(launcher, game.state, services), game.session)
    }

    async fn get(app: &Router, host: &str, path: &str) -> (StatusCode, Bytes) {
        let request = HttpRequest::builder()
            .method("GET")
            .uri(path)
            .header(HOST, host)
            .body(Body::empty())
            .expect("request");
        test_http::send(app, request).await
    }

    #[tokio::test]
    async fn login_host_serves_launcher_and_channel() {
        let (app, session) = app_and_session().await;
        let (status, _) = get(&app, LOGIN_HOST, INFO_PATH).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, &format!("{LOGIN_HOST}:443"), INFO_PATH).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, "LOGIN.example.org.", &format!("/{session}.json")).await;
        assert_eq!(status, StatusCode::OK);
        for path in ["/favicon.ico", LOGIN_PATH] {
            let (status, body) = get(&app, LOGIN_HOST, path).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (StatusCode::NOT_FOUND, "unknown_route"),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn game_host_serves_only_game_routes() {
        let (app, session) = app_and_session().await;
        let steam = format!("{SESSION_PREFIX}/{session}{AUTH_STACK_STAGE}{STEAM_AUTH_PATH}");
        let request = HttpRequest::builder()
            .method("POST")
            .uri(format!("https://{GAME_HOST}{steam}"))
            .body(Body::empty())
            .expect("request");
        let (status, _) = test_http::send(&app, request).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, GAME_HOST, INFO_PATH).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&app, GAME_HOST, &format!("/{session}.json")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let board = "/new-world/pc/leaderboards/top-x.w1?pageNumber=1&pageSize=100";
        let (status, _) = get(&app, GAME_HOST, board).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, LOGIN_HOST, board).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let catalog = "/players/p/games/new-world/platforms/steam/catalog?type=MTX";
        let (status, _) = get(&app, GAME_HOST, catalog).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, LOGIN_HOST, catalog).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn aws_hosts_get_the_analytics_routes() {
        let (app, _) = app_and_session().await;
        for host in [
            "KINESIS.us-east-1.amazonaws.com",
            "sts.us-east-1.amazonaws.com",
        ] {
            let request = HttpRequest::builder()
                .method("POST")
                .uri("/")
                .header(HOST, host)
                .body(Body::empty())
                .expect("request");
            let (status, _) = test_http::send(&app, request).await;
            assert!(
                status != StatusCode::MISDIRECTED_REQUEST && status.is_client_error(),
                "{host}: {status}"
            );
        }
    }

    #[tokio::test]
    async fn the_bucket_host_gets_the_remote_config_sets() {
        let (app, _) = app_and_session().await;
        let key =
            "/applications/public/configuration-sets/ProductId/STEAM_APP_ID.1063730/versionless";
        let (status, _) = get(&app, "ags-javelin-remote-config.s3.amazonaws.com", key).await;
        assert_eq!(status, StatusCode::OK);
        for host in [
            "ags-javelin-dev-remote-config.s3.amazonaws.com",
            "s3.amazonaws.com",
        ] {
            let (status, _) = get(&app, host, key).await;
            assert_eq!(status, StatusCode::MISDIRECTED_REQUEST, "{host}");
        }
        let (status, _) = get(&app, GAME_HOST, key).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_news_hosts_get_our_news() {
        let (app, _) = app_and_session().await;
        let key = format!("/{}", CmsDocument::MessageOfTheDay.key());
        let (status, _) = get(&app, "D1HKBWZM1BKTGO.cloudfront.net.", &key).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = get(&app, CMS_IMAGE_HOST, "/images/none.dds").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&app, GAME_HOST, &key).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = get(&app, "d2c74t4zimux3r.cloudfront.net", &key).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    }

    #[tokio::test]
    async fn other_hosts_are_refused() {
        let (app, _) = app_and_session().await;
        for host in ["example.org", "login.example.org.evil.net", "10.0.0.1"] {
            let (status, body) = get(&app, host, INFO_PATH).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (StatusCode::MISDIRECTED_REQUEST, "unknown_host"),
                "{host}"
            );
        }
        let request = HttpRequest::builder()
            .uri(INFO_PATH)
            .body(Body::empty())
            .expect("request");
        let (status, _) = test_http::send(&app, request).await;
        assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    }
}
