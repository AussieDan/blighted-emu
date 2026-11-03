// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scripted client (M2 spec section 11, `infra` feature). It walks the login steps of the game
//! client over the real PostgreSQL and Valkey stores and through the host router: launcher login,
//! channel document, `/steam`, `getlogininfo`, world list, validator, character create,
//! queue and entry ticket. Then it repeats every game step with a wrong session and with no
//! session.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::{AUTHORIZATION, HOST};
use axum::http::{Request, StatusCode};
use nw_db::test_db::with_test_schema;
use nw_db::{GLOBAL_MIGRATOR, PgPool, WORLD_MIGRATOR};
use nw_protocol::LoginQueueReply;
use nw_protocol::game_api::{
    AUTH_STACK_STAGE, AUTH_STACK_TAG, CHARACTERS_SEGMENT, ChannelDocument, CreateCharacterReply,
    GATEWAY_STAGE, GET_LOGIN_INFO_PATH, LIST_WORLDS_PATH, LOGIN_GATEWAY_TAG, LoginInfoReply,
    QUEUE_LOGIN_PATH, STEAM_AUTH_PATH, VALIDATOR_SEGMENT, WORLDS_PATH,
};
use nw_protocol::launcher_api::{
    LOGIN_PATH, LauncherSession, PLAY_PATH, PasswordCredentials, PlayReply, REGISTER_PATH,
};
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::config::tests::valid;
use crate::game::CHANNEL_FILE_SUFFIX;
use crate::keys::new_game_session_key;
use crate::queue::{QueueAction, QueueSettings, QueueStatus, QueueStore, QueueTime};
use crate::signature::verify_entry;
use crate::test_http::{self, error_code};
use crate::test_support::{edge_app, test_signing_key, test_valkey_url};
use crate::valkey::Valkey;

const USERNAME: &str = "scripted";
const PASSWORD: &str = "password-ok";
const CHARACTER_NAME: &str = "Scripta";
/// Free player slots that the director would report: the world has room.
const FREE_SLOTS: u32 = 10;
/// The slot count outlives the test.
const FREE_SLOTS_TTL_MS: u64 = 600_000;
const STEAM_TICKET: &str = "140000";
const HTTPS_PREFIX: &str = "https://";
const ENDPOINT_PATH_START: char = '/';
const BEARER_PREFIX: &str = "Bearer ";
/// A public HTTPS port that is not the default, so the walk sends hosts with a port, as players
/// behind a forwarded port do.
const WALK_PUBLIC_HTTPS_PORT: u16 = 8443;
/// The body the client builds by hand for `/steam` (JSON note section 3).
const STEAM_BODY: &str =
    r#"{"steamAuthTicket":"140000", "steamAppId":"1063730", "branch":"public"}"#;
const LOGIN_INFO_BODY: &str = r#"{"channelId":"live","includeNames":"true"}"#;
const LIST_WORLDS_BODY: &str = r#"{"ListWorldsRequestBody":{}}"#;

async fn call(
    app: &Router,
    method: &str,
    host: &str,
    path: &str,
    bearer: Option<&str>,
    body: &str,
) -> (StatusCode, Bytes) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, host);
    if let Some(token) = bearer {
        request = request.header(AUTHORIZATION, format!("{BEARER_PREFIX}{token}"));
    }
    let request = request.body(Body::from(body.to_owned())).expect("request");
    test_http::send(app, request).await
}

fn parse<T: DeserializeOwned>(step: &str, (status, body): (StatusCode, Bytes)) -> T {
    assert_eq!(status, StatusCode::OK, "{step}: {body:?}");
    serde_json::from_slice(&body).unwrap_or_else(|error| panic!("{step}: {error}"))
}

/// `host` and `/path` of an endpoint text such as `game.example.org/s/<key>`.
fn split_endpoint(endpoint: &str) -> (String, String) {
    let text = endpoint.strip_prefix(HTTPS_PREFIX).unwrap_or(endpoint);
    let start = text.find(ENDPOINT_PATH_START).unwrap_or(text.len());
    (text[..start].to_owned(), text[start..].to_owned())
}

/// One game step: the path after the session prefix and the body.
struct Step {
    name: &'static str,
    path: String,
    body: String,
}

#[tokio::test]
async fn a_client_walks_from_login_to_entry_ticket() {
    with_test_schema(async |global| {
        GLOBAL_MIGRATOR.run(global).await.expect("migrate global");
        with_test_schema(async |world| {
            WORLD_MIGRATOR.run(world).await.expect("migrate world");
            walk(global, world).await;
        })
        .await;
    })
    .await;
}

async fn walk(global: &PgPool, world_pool: &PgPool) {
    let mut config = valid();
    // A new world id keeps this run's queue apart from other runs in the shared Valkey.
    config.worlds[0].id = Uuid::new_v4();
    config.public_https_port = WALK_PUBLIC_HTTPS_PORT;
    let world = config.worlds[0].clone();
    let config = Arc::new(config);
    let valkey = Valkey::connect(&test_valkey_url())
        .await
        .expect("connect to Valkey");
    valkey
        .set_free_slots(world.id, FREE_SLOTS, FREE_SLOTS_TTL_MS)
        .await
        .expect("free slots");
    let app = edge_app(Arc::clone(&config), global, world_pool, valkey.clone());
    let login_host = config.login_authority();

    // Launcher: sign up, log in again, ask for a game session key.
    let credentials = serde_json::to_string(&PasswordCredentials {
        username: USERNAME.to_owned(),
        password: PASSWORD.to_owned(),
    })
    .expect("credentials");
    let _: LauncherSession = parse(
        "register",
        call(&app, "POST", &login_host, REGISTER_PATH, None, &credentials).await,
    );
    let session: LauncherSession = parse(
        "login",
        call(&app, "POST", &login_host, LOGIN_PATH, None, &credentials).await,
    );
    let play: PlayReply = parse(
        "play",
        call(
            &app,
            "POST",
            &login_host,
            PLAY_PATH,
            Some(&session.launcher_token),
            "",
        )
        .await,
    );
    let key = play.game_session_key;

    // Channel document: every endpoint carries the session.
    let channel_path = format!("/{key}{CHANNEL_FILE_SUFFIX}");
    let channel: ChannelDocument = parse(
        "channel",
        call(&app, "GET", &login_host, &channel_path, None, "").await,
    );
    let apis = &channel
        .clusters
        .values()
        .next()
        .expect("one cluster")
        .public_apis;
    let endpoint = |tag: &str| {
        apis.iter()
            .find(|api| api.client_tag == tag)
            .map(|api| api.api_endpoint.clone())
            .expect("endpoint")
    };
    assert_eq!(endpoint(AUTH_STACK_TAG), endpoint(LOGIN_GATEWAY_TAG));
    let (game_host, prefix) = split_endpoint(&endpoint(LOGIN_GATEWAY_TAG));
    assert_eq!(game_host, config.game_authority());

    let gateway = |operation: &str| format!("{GATEWAY_STAGE}{operation}");
    let world_route = |segment: &str| gateway(&format!("{WORLDS_PATH}/{}{segment}", world.id));
    let mut steps = vec![
        Step {
            name: "steam",
            path: format!("{AUTH_STACK_STAGE}{STEAM_AUTH_PATH}"),
            body: STEAM_BODY.to_owned(),
        },
        Step {
            name: "login info",
            path: gateway(GET_LOGIN_INFO_PATH),
            body: LOGIN_INFO_BODY.to_owned(),
        },
        Step {
            name: "world list",
            path: gateway(LIST_WORLDS_PATH),
            body: LIST_WORLDS_BODY.to_owned(),
        },
        Step {
            name: "validator",
            path: world_route(VALIDATOR_SEGMENT),
            body: serde_json::json!({"ValidateCharacterBody": {"Name": CHARACTER_NAME}})
                .to_string(),
        },
        Step {
            name: "create",
            path: world_route(CHARACTERS_SEGMENT),
            body: serde_json::json!({"CreateCharacterRequest": {
                "Name": CHARACTER_NAME,
                "CharacterCreationParams": "{}",
                "SteamAuthTicket": STEAM_TICKET,
            }})
            .to_string(),
        },
    ];
    let mut replies = HashMap::new();
    for step in &steps {
        let path = format!("{prefix}{}", step.path);
        let reply = call(&app, "POST", &game_host, &path, None, &step.body).await;
        assert_eq!(reply.0, StatusCode::OK, "{}: {:?}", step.name, reply.1);
        replies.insert(step.name, reply);
    }
    let created: CreateCharacterReply =
        parse("create", replies.remove("create").expect("create reply"));
    assert_eq!(created.character.world_id, world.id.to_string());

    // The new character is in the list, with its name.
    let info: LoginInfoReply = parse(
        "login info after create",
        call(
            &app,
            "POST",
            &game_host,
            &format!("{prefix}{}", gateway(GET_LOGIN_INFO_PATH)),
            None,
            LOGIN_INFO_BODY,
        )
        .await,
    );
    let list = info.login_info_list;
    assert_eq!(list.characters, vec![created.character.clone()]);
    assert_eq!(list.name_reservations[0].display_name, CHARACTER_NAME);

    // Queue: the world has room, so the join is admitted with a signed entry ticket.
    let queue_body = serde_json::json!({"LoginQueueRequest": {
        "WorldId": world.id.to_string(),
        "CharacterId": created.character.character_id,
        "SteamAuthTicket": STEAM_TICKET,
    }})
    .to_string();
    let joined: LoginQueueReply = parse(
        "queue",
        call(
            &app,
            "POST",
            &game_host,
            &format!("{prefix}{}", gateway(QUEUE_LOGIN_PATH)),
            None,
            &queue_body,
        )
        .await,
    );
    let token = joined.login_queue_response.token.expect("entry token");
    let ticket_id = token.ticket_id.clone().expect("ticket id");
    let refresh_path = gateway(&format!("{QUEUE_LOGIN_PATH}/{ticket_id}"));
    let refreshed: LoginQueueReply = parse(
        "queue refresh",
        call(
            &app,
            "POST",
            &game_host,
            &format!("{prefix}{refresh_path}"),
            None,
            "",
        )
        .await,
    );
    assert_eq!(
        refreshed.login_queue_response.ticket_id,
        Some(ticket_id.clone())
    );
    let account = Uuid::parse_str(token.persona_id.as_deref().expect("persona")).expect("uuid");
    let status = valkey
        .update(
            &QueueSettings::for_world(&world),
            account,
            QueueAction::Refresh {
                ticket_id: ticket_id.clone(),
            },
            QueueTime::StoreClock,
        )
        .await
        .expect("entry record");
    let Some(QueueStatus::Admitted(record)) = status else {
        panic!("expected an admitted entry");
    };
    let signature = token.signature.expect("signature");
    assert!(verify_entry(
        &test_signing_key(),
        &record.claims(),
        &signature
    ));

    // Every game step refuses a wrong session and a missing one.
    steps.push(Step {
        name: "queue",
        path: gateway(QUEUE_LOGIN_PATH),
        body: queue_body.clone(),
    });
    steps.push(Step {
        name: "queue refresh",
        path: refresh_path,
        body: String::new(),
    });
    let wrong_prefix = prefix.replace(&key, &new_game_session_key().expect("key"));
    for step in &steps {
        let wrong = format!("{wrong_prefix}{}", step.path);
        let (status, body) = call(&app, "POST", &game_host, &wrong, None, &step.body).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (StatusCode::UNAUTHORIZED, "no_session"),
            "{} with a wrong session",
            step.name
        );
        let (status, _) = call(&app, "POST", &game_host, &step.path, None, &step.body).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{} without a session",
            step.name
        );
    }
    let wrong_channel = format!(
        "/{}{CHANNEL_FILE_SUFFIX}",
        new_game_session_key().expect("key")
    );
    let (status, _) = call(&app, "GET", &login_host, &wrong_channel, None, "").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
