// SPDX-License-Identifier: AGPL-3.0-or-later
//! Our own remote config sets, answered as the S3 bucket the client reads them from (study note
//! `docs/re/2026-09-18-remote-config-layers.md`).
//!
//! The client asks for `GET /applications/<application>/configuration-sets/<dimension>/<id>/
//! <version>` on `<bucket>.s3.amazonaws.com`, for two applications and four dimensions, each
//! once `versionless` and once for its build. It polls each key again with `If-None-Match`. The
//! `[remote_config] directory` holds one file per `versionless` layer, named after its key:
//! `<application>-<dimension>.json`, and `<application>-WorldId-<world id>.json` for each world
//! ([`layer_file_name`]). A key with no layer gets the S3 `NoSuchKey` reply (retail answers
//! every build key that way; [`crate::s3`]).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use axum::Router;
use serde_json::{Map, Value};

use crate::config::EdgeConfig;
use crate::game::CLUSTER_ID;
use crate::s3::{S3Objects, s3_router};

/// The two applications the client reads.
pub const APPLICATIONS: [&str; 2] = ["public", "publicGameplay"];
/// The product id of the Steam client (`ProductId` layers; seen in the client log).
pub const PRODUCT_ID: &str = "STEAM_APP_ID.1063730";
/// The version part of the key that holds the values of every build.
pub const VERSIONLESS: &str = "versionless";
/// The application whose `RegionId` layer holds the service endpoints this server fills.
pub const ENDPOINTS_APPLICATION: &str = "publicGameplay";
/// The service endpoints this server fills with `game.<domain>` (and the public HTTPS port when it
/// is not the default, [`EdgeConfig::game_authority`]): the path of their object in
/// that layer, and their key. The tracked files leave them out, because the domain is the
/// server's own. The stats service host (leaderboards note), the shop, entitlement and
/// purchase service hosts (`omni.fcs.endpoints.*`, read by `FUN_1416190f0`; shop note), and the
/// privacy settings service host (`omni.privacy-settings.client-endpoint`, read by
/// `FUN_14160ad60`; [`crate::privacy`]).
pub const GAME_HOST_ENDPOINTS: [(&[&str], &str); 5] = [
    (&["omni", "leaderboards"], "client-endpoint"),
    (&["omni", "privacy-settings"], "client-endpoint"),
    (&["omni", "fcs", "endpoints"], "catalog-service"),
    (&["omni", "fcs", "endpoints"], "entitlement-service"),
    (&["omni", "fcs", "endpoints"], "transaction-service"),
];
/// Between the parts of a key path in an error message.
const KEY_PATH_SEPARATOR: &str = ".";
/// The application of the layer that gets the keys the server fills from `config/edge.toml`
/// (the experience multipliers of `[experience]` and the free fast travel switch of
/// `[fast_travel]`): its lowest layer, `ProductId` (retail sets the multipliers there, and again
/// in `CognitoId`).
pub const SERVER_KEYS_APPLICATION: &str = "publicGameplay";
/// The object that holds the keys the server fills.
const JAVELIN: &str = "javelin";
/// Its key of the experience multipliers.
const XP_MULTIPLIERS: &str = "xp-multipliers";
/// Its key of the free fast travel switch, a bool (client cvar `FUN_140169720`).
const FAST_TRAVEL_FREE: &str = "playtest-disable-fast-travel-costs";
/// The keys of [`JAVELIN`] the server fills, which no file may set.
const SERVER_KEYS: [&str; 2] = [XP_MULTIPLIERS, FAST_TRAVEL_FREE];
/// Its character experience multiplier, a number.
const XP_CHARACTER: &str = "character";
/// Its list of `<CategoricalProgressionId>:<multiplier>` entries.
const XP_PROGRESSION: &str = "progression";
/// Between the id and the multiplier of a progression entry (client parser `FUN_143cad9b0`).
const PROGRESSION_ENTRY_SEPARATOR: char = ':';

/// First part of every key.
const KEY_ROOT: &str = "applications";
/// Second part of every key after the application.
const CONFIGURATION_SETS: &str = "configuration-sets";
const KEY_SEPARATOR: char = '/';
/// Between the parts of a layer file name.
const FILE_NAME_SEPARATOR: &str = "-";
/// The extension of a layer file.
const LAYER_FILE_EXTENSION: &str = "json";

/// The dimensions of a configuration set, in the client's order (`ProductId` lowest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dimension {
    /// The Steam product.
    ProductId,
    /// The cluster.
    RegionId,
    /// The Cognito pool.
    CognitoId,
    /// One world.
    WorldId,
}

impl Dimension {
    /// The key part.
    pub fn name(self) -> &'static str {
        match self {
            Self::ProductId => "ProductId",
            Self::RegionId => "RegionId",
            Self::CognitoId => "CognitoId",
            Self::WorldId => "WorldId",
        }
    }
}

/// `applications/<application>/configuration-sets/<dimension>/<id>/<version>`.
pub fn object_key(application: &str, dimension: Dimension, id: &str, version: &str) -> String {
    [
        KEY_ROOT,
        application,
        CONFIGURATION_SETS,
        dimension.name(),
        id,
        version,
    ]
    .join(&KEY_SEPARATOR.to_string())
}

/// The file of the `versionless` layer `application` / `dimension` / `id`: one file per layer in
/// one folder; only a world's file names its id, the other ids come from the configuration.
pub fn layer_file_name(application: &str, dimension: Dimension, id: &str) -> String {
    let stem = match dimension {
        Dimension::WorldId => [application, dimension.name(), id].join(FILE_NAME_SEPARATOR),
        Dimension::ProductId | Dimension::RegionId | Dimension::CognitoId => {
            [application, dimension.name()].join(FILE_NAME_SEPARATOR)
        }
    };
    format!("{stem}.{LAYER_FILE_EXTENSION}")
}

/// The layers our client asks for with this configuration: dimension and id.
pub fn requested_layers(config: &EdgeConfig) -> Vec<(Dimension, String)> {
    let mut layers = vec![
        (Dimension::ProductId, PRODUCT_ID.to_owned()),
        (Dimension::RegionId, CLUSTER_ID.to_owned()),
        (
            Dimension::CognitoId,
            config.remote_config.cognito_pool_id.to_string(),
        ),
    ];
    layers.extend(
        config
            .worlds
            .iter()
            .map(|world| (Dimension::WorldId, world.id.to_string())),
    );
    layers
}

/// The config sets could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum RemoteConfigError {
    /// A file or folder cannot be read.
    #[error("cannot read {}: {source}", .path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The folder holds a file that is no layer of this configuration.
    #[error("remote config file {} is no layer this server serves", .0.display())]
    Unexpected(PathBuf),
    /// A file is not a JSON object.
    #[error("remote config {key} is not a JSON object")]
    NotObject {
        /// The key.
        key: String,
    },
    /// A layer the client asks for has no file.
    #[error("remote config {key} is missing: no file {}", .path.display())]
    Missing {
        /// The key.
        key: String,
        /// The file it needs.
        path: PathBuf,
    },
    /// The endpoints layer lacks the object of a [`GAME_HOST_ENDPOINTS`] key, or sets that key.
    #[error("remote config {key}: {reason}")]
    Endpoint {
        /// The key.
        key: String,
        /// What is wrong.
        reason: String,
    },
    /// A file sets a key the server fills (`javelin.xp-multipliers` from `[experience]`,
    /// `javelin.playtest-disable-fast-travel-costs` from `[fast_travel]`), or the server keys
    /// layer's `javelin` is no object.
    #[error("remote config {key}: {reason}")]
    ServerKey {
        /// The key.
        key: String,
        /// What is wrong.
        reason: String,
    },
}

/// The loaded config sets, by key.
#[derive(Debug, Clone)]
pub struct RemoteConfigSets {
    objects: S3Objects,
}

impl RemoteConfigSets {
    /// Loads the `versionless` layer of every application and layer of [`requested_layers`]
    /// from its file in `[remote_config] directory` ([`layer_file_name`]). Each must be a JSON
    /// object, and the folder may hold no other file. The [`GAME_HOST_ENDPOINTS`] of the
    /// [`ENDPOINTS_APPLICATION`] `RegionId` layer are set to `game.<domain>`, and the
    /// [`SERVER_KEYS_APPLICATION`] `ProductId` layer gets `javelin.xp-multipliers` from
    /// `[experience]` and `javelin.playtest-disable-fast-travel-costs` from `[fast_travel]`,
    /// which no file may set; other layers are served byte for byte.
    pub fn load(config: &EdgeConfig) -> Result<Self, RemoteConfigError> {
        let root = &config.remote_config.directory;
        let server_keys_key = object_key(
            SERVER_KEYS_APPLICATION,
            Dimension::ProductId,
            PRODUCT_ID,
            VERSIONLESS,
        );
        let endpoints_key = object_key(
            ENDPOINTS_APPLICATION,
            Dimension::RegionId,
            CLUSTER_ID,
            VERSIONLESS,
        );
        let mut objects = HashMap::new();
        let mut layer_files = HashSet::new();
        for application in APPLICATIONS {
            for (dimension, id) in requested_layers(config) {
                let key = object_key(application, dimension, &id, VERSIONLESS);
                let path = root.join(layer_file_name(application, dimension, &id));
                let bytes = std::fs::read(&path).map_err(|source| {
                    if source.kind() == std::io::ErrorKind::NotFound {
                        RemoteConfigError::Missing {
                            key: key.clone(),
                            path: path.clone(),
                        }
                    } else {
                        RemoteConfigError::Io {
                            path: path.clone(),
                            source,
                        }
                    }
                })?;
                let Ok(Value::Object(object)) = serde_json::from_slice::<Value>(&bytes) else {
                    return Err(RemoteConfigError::NotObject { key });
                };
                if let Some(name) = server_key_set(&object) {
                    return Err(RemoteConfigError::ServerKey {
                        key,
                        reason: format!(
                            "sets {JAVELIN}{KEY_PATH_SEPARATOR}{name}, which the server fills from config/edge.toml"
                        ),
                    });
                }
                let body = if key == endpoints_key {
                    with_game_host_endpoints(object, &config.game_authority(), &key)?
                } else if key == server_keys_key {
                    with_server_keys(object, config, &key)?
                } else {
                    bytes
                };
                objects.insert(key, body);
                layer_files.insert(path);
            }
        }
        let io_error = |source| RemoteConfigError::Io {
            path: root.clone(),
            source,
        };
        for entry in std::fs::read_dir(root).map_err(io_error)? {
            let path = entry.map_err(io_error)?.path();
            if !layer_files.contains(&path) {
                return Err(RemoteConfigError::Unexpected(path));
            }
        }
        Ok(Self {
            objects: S3Objects::new(objects),
        })
    }

    /// The body of an object, if it exists.
    pub fn body(&self, key: &str) -> Option<&[u8]> {
        self.objects.body(key)
    }

    /// Every key, sorted.
    pub fn keys(&self) -> Vec<&str> {
        self.objects.keys()
    }
}

/// `object` with each [`GAME_HOST_ENDPOINTS`] key set to `game_authority`, as JSON text. The
/// object of each key must exist, and the key must not.
fn with_game_host_endpoints(
    mut object: Map<String, Value>,
    game_authority: &str,
    key: &str,
) -> Result<Vec<u8>, RemoteConfigError> {
    let fail = |reason: String| RemoteConfigError::Endpoint {
        key: key.to_owned(),
        reason,
    };
    for (path, endpoint) in GAME_HOST_ENDPOINTS {
        let name = path.join(KEY_PATH_SEPARATOR);
        let section = path
            .iter()
            .try_fold(&mut object, |parent, part| {
                parent.get_mut(*part).and_then(Value::as_object_mut)
            })
            .ok_or_else(|| fail(format!("has no {name} object")))?;
        if section.contains_key(endpoint) {
            return Err(fail(format!(
                "sets {name}{KEY_PATH_SEPARATOR}{endpoint}, which the server fills from its domain"
            )));
        }
        section.insert(
            endpoint.to_owned(),
            Value::String(game_authority.to_owned()),
        );
    }
    serde_json::to_vec_pretty(&Value::Object(object)).map_err(|error| fail(error.to_string()))
}

/// The first of the [`SERVER_KEYS`] that `object` sets, itself or a key in it, nested or written
/// flat (the key stack of the sims reads both).
fn server_key_set(object: &Map<String, Value>) -> Option<&'static str> {
    let names = |name: &str, prefix: &str| {
        name == prefix || name.starts_with(&format!("{prefix}{KEY_PATH_SEPARATOR}"))
    };
    let nested = object.get(JAVELIN).and_then(Value::as_object);
    SERVER_KEYS.into_iter().find(|server_key| {
        let flat = format!("{JAVELIN}{KEY_PATH_SEPARATOR}{server_key}");
        object.keys().any(|name| names(name, &flat))
            || nested.is_some_and(|parent| parent.keys().any(|name| names(name, server_key)))
    })
}

/// `object` with the [`SERVER_KEYS`] from `config`, as JSON text: `javelin.xp-multipliers` from
/// `[experience]` (the `character` number and one `<CategoricalProgressionId>:<multiplier>` entry
/// for each progression) and `javelin.playtest-disable-fast-travel-costs` from `[fast_travel]`.
fn with_server_keys(
    mut object: Map<String, Value>,
    config: &EdgeConfig,
    key: &str,
) -> Result<Vec<u8>, RemoteConfigError> {
    let experience = &config.experience;
    let fail = |reason: String| RemoteConfigError::ServerKey {
        key: key.to_owned(),
        reason,
    };
    let progression = experience
        .progressions()
        .into_iter()
        .map(|(id, value)| Value::String(format!("{id}{PROGRESSION_ENTRY_SEPARATOR}{value}")))
        .collect();
    let mut multipliers = Map::new();
    multipliers.insert(XP_CHARACTER.to_owned(), Value::from(experience.character));
    multipliers.insert(XP_PROGRESSION.to_owned(), Value::Array(progression));
    let javelin = object
        .entry(JAVELIN)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| fail(format!("{JAVELIN} is not an object")))?;
    javelin.insert(XP_MULTIPLIERS.to_owned(), Value::Object(multipliers));
    javelin.insert(
        FAST_TRAVEL_FREE.to_owned(),
        Value::Bool(config.fast_travel.free),
    );
    serde_json::to_vec_pretty(&Value::Object(object)).map_err(|error| fail(error.to_string()))
}

/// The S3 route: `GET /<key>` (and `HEAD`).
pub fn remote_config_router(sets: RemoteConfigSets) -> Router {
    s3_router(sets.objects)
}

#[cfg(test)]
pub(crate) mod tests {
    use axum::body::{Body, Bytes};
    use axum::http::header::IF_NONE_MATCH;
    use axum::http::{HeaderMap, Request, StatusCode};
    use data_encoding::HEXLOWER;
    use md5::{Digest, Md5};
    use std::collections::BTreeSet;

    use super::*;
    use crate::config::tests::valid;
    use crate::experience::{ExperienceConfig, NORMAL_EXPERIENCE};
    use crate::s3::{
        ANY_ETAG, ERROR_CONTENT_TYPE, HOST_ID_HEADER, OBJECT_CONTENT_TYPE, REQUEST_ID_BYTES,
        REQUEST_ID_HEADER,
    };
    use crate::test_http;

    /// The tracked config sets.
    fn committed_directory() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../config/remote-config")
    }

    /// The tracked config sets, loaded for `config`.
    pub(crate) fn committed_sets(config: &EdgeConfig) -> RemoteConfigSets {
        let mut config = config.clone();
        config.remote_config.directory = committed_directory();
        RemoteConfigSets::load(&config).expect("tracked config sets")
    }

    fn sets() -> RemoteConfigSets {
        committed_sets(&valid())
    }

    const COGNITO_ID: &str = "b04fbfcd-942c-472d-88c0-c762c4076b2f";
    const WORLD_ID: &str = "8d0f4c2e-0000-4000-8000-000000000001";
    const BUILD_VERSION: &str = "6033;6024067";
    const ESCAPED_BUILD_VERSION: &str = "6033%3B6024067";

    /// The key paths (nested object keys joined with `/`) our layers serve: the retail key names,
    /// less the four Amazon-only stats keys (`server-endpoint`, `ingestion-endpoint`,
    /// `stats-assume-role`, `ingestion-assume-role`), plus the switches the leaderboards study
    /// asks for (`analytics/enable` and seven `javelin` keys of the publicGameplay `RegionId`
    /// layer; leaderboards note section 1), plus the shop service keys `omni/fcs/*` and the
    /// privacy settings keys `omni/privacy-settings/*` that retail leaves to the client's defaults
    /// (shop note). The experience multipliers and the free fast travel switch are served once,
    /// in the `ProductId` layer, from `[experience]` and `[fast_travel]`.
    const SERVED_KEY_PATHS: [(&str, Dimension, &[&str]); 8] = [
        (
            "public",
            Dimension::ProductId,
            &[
                "javelin/landing-screen-character-transfer",
                "javelin/landing-screen-character-transfer-direct",
                "javelin/red-button-disable-traffic-mitigation",
            ],
        ),
        (
            "public",
            Dimension::RegionId,
            &[
                "analytics/enable",
                "analytics/event-types-to-disable",
                "analytics/kinesis-put-role",
                "analytics/kinesis-region-name",
                "analytics/kinesis-stream-name",
                "analytics/overflow-kinesis-put-role",
                "analytics/overflow-kinesis-stream-name",
                "analytics/region-name",
            ],
        ),
        (
            "public",
            Dimension::CognitoId,
            &["javelin/dynamic-datatable-config"],
        ),
        ("public", Dimension::WorldId, &[]),
        (
            "publicGameplay",
            Dimension::ProductId,
            &[
                "javelin/playtest-disable-fast-travel-costs",
                "javelin/xp-multipliers/character",
                "javelin/xp-multipliers/progression",
            ],
        ),
        (
            "publicGameplay",
            Dimension::RegionId,
            &[
                "javelin-server/enable-twitch-system",
                "javelin/enable-leaderboard-rotations",
                "javelin/enable-leaderboards",
                "javelin/enable-leaderboards-stat-queries",
                "javelin/enable-leaderboards-ui",
                "javelin/leaderboard-rotation-startDate",
                "javelin/leaderboard-season-rotation",
                "javelin/query-leaderboards-service-from-client",
                "javelin/use-omni-penalties",
                "javelin/use-persona-penalties",
                "javelin/use-season-end-time-config",
                "javelin/weekly-leaderboard-reset-hour",
                "omni/fcs/endpoints/catalog-service",
                "omni/fcs/endpoints/entitlement-service",
                "omni/fcs/endpoints/transaction-service",
                "omni/fcs/region",
                "omni/leaderboards/client-endpoint",
                "omni/leaderboards/region",
                "omni/privacy-settings/client-endpoint",
                "omni/privacy-settings/region",
            ],
        ),
        (
            "publicGameplay",
            Dimension::CognitoId,
            &[
                "javelin/disabled-item-classes",
                "javelin/dynamic-datatable-config",
                "javelin/game-mode-matchmaking-service-enabled-modes",
                "javelin/gamemode-activity-type-penalty-overrides",
                "javelin/gamemode-dungeon-mutated-max-runs",
                "javelin/gamemode-group-trial-max-refresh-seconds",
                "javelin/gamemode-mutations/forceRefresh",
                "javelin/gamemode-mutations/seedModifier",
                "javelin/gamemode-scheduler/cadenceInSec",
                "javelin/gamemode-scheduler/maxToSchedule",
                "javelin/gear-set-storage-max-num-paid-loadouts",
                "javelin/housing/house-level-reqs",
                "javelin/leaderboard-season-end-datetime",
                "javelin/red-button-catch-all-achievement-correction",
                "javelin/red-button-disable-spawn-slice-list",
                "javelin/red-button-npa-override",
                "javelin/schedule-overrides",
                "javelin/seasons-rewards.season-end-datetime",
                "javelin/social/enable-company-only-war-slots",
                "javelin/storage-search.crafting-enabled",
                "javelin/storage-search.enabled",
                "javelin/storage-search.housing-enabled",
                "javelin/territory-entry-requirement-enabled",
                "javelin/transmog-own-all-appearances",
                "javelin/wallet-cap-player",
            ],
        ),
        (
            "publicGameplay",
            Dimension::WorldId,
            &["services/matchmaking/disable-instance-polling"],
        ),
    ];

    fn layer_id(dimension: Dimension) -> &'static str {
        match dimension {
            Dimension::ProductId => PRODUCT_ID,
            Dimension::RegionId => CLUSTER_ID,
            Dimension::CognitoId => COGNITO_ID,
            Dimension::WorldId => WORLD_ID,
        }
    }

    fn key_paths(value: &Value, prefix: &str, out: &mut BTreeSet<String>) {
        if let Value::Object(object) = value {
            for (name, child) in object {
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}{KEY_SEPARATOR}{name}")
                };
                match child {
                    Value::Object(inner) if !inner.is_empty() => key_paths(child, &path, out),
                    _ => {
                        out.insert(path);
                    }
                }
            }
        }
    }

    fn served_json(sets: &RemoteConfigSets, key: &str) -> Value {
        serde_json::from_slice(sets.body(key).expect(key)).expect("json")
    }

    #[test]
    fn tracked_sets_have_the_retail_layers_and_the_served_key_names() {
        let sets = sets();
        let mut expected_keys = Vec::new();
        for (application, dimension, paths) in SERVED_KEY_PATHS {
            let key = object_key(application, dimension, layer_id(dimension), VERSIONLESS);
            let mut found = BTreeSet::new();
            key_paths(&served_json(&sets, &key), "", &mut found);
            let wanted: BTreeSet<String> = paths.iter().map(|path| (*path).to_owned()).collect();
            assert_eq!(found, wanted, "{key}");
            expected_keys.push(key);
        }
        expected_keys.sort_unstable();
        assert_eq!(sets.keys(), expected_keys);
    }

    #[test]
    fn tracked_sets_name_no_amazon_host_or_account() {
        let sets = sets();
        for key in sets.keys() {
            let text = String::from_utf8_lossy(sets.body(key).expect("body")).to_lowercase();
            for forbidden in [
                "amazon",
                "cloudfront",
                "iad-prod",
                "040969890792",
                "295811826248",
            ] {
                assert!(!text.contains(forbidden), "{key} names {forbidden}");
            }
        }
    }

    fn served_endpoints(sets: &RemoteConfigSets) -> Value {
        let key = object_key(
            ENDPOINTS_APPLICATION,
            Dimension::RegionId,
            CLUSTER_ID,
            VERSIONLESS,
        );
        served_json(sets, &key)
    }

    fn assert_service_endpoints(served: &Value, authority: &str) {
        for (path, endpoint) in GAME_HOST_ENDPOINTS {
            let section = path.iter().fold(served, |parent, part| &parent[*part]);
            assert_eq!(section[endpoint], authority, "{path:?} {endpoint}");
        }
    }

    #[test]
    fn the_service_endpoints_are_the_servers_game_host() {
        let served = served_endpoints(&sets());
        assert_service_endpoints(&served, "game.example.org");
        assert_eq!(served["omni"]["leaderboards"]["region"], "us-east-1");
        assert_eq!(served["omni"]["fcs"]["region"], "us-east-1");
        assert_eq!(served["omni"]["privacy-settings"]["region"], "us-east-1");
    }

    #[test]
    fn the_service_endpoints_name_another_public_port() {
        const PUBLIC_HTTPS_PORT: u16 = 8443;
        let mut config = valid();
        config.public_https_port = PUBLIC_HTTPS_PORT;
        let served = served_endpoints(&committed_sets(&config));
        assert_service_endpoints(&served, "game.example.org:8443");
    }

    #[test]
    fn other_files_are_served_byte_for_byte() {
        let sets = sets();
        let key = object_key("public", Dimension::ProductId, PRODUCT_ID, VERSIONLESS);
        let file = std::fs::read(committed_directory().join(layer_file_name(
            "public",
            Dimension::ProductId,
            PRODUCT_ID,
        )))
        .expect("file");
        assert_eq!(sets.body(&key), Some(file.as_slice()));
    }

    /// A copy of the tracked sets in a temporary folder, and a config that reads it.
    fn copied_sets() -> (tempfile::TempDir, EdgeConfig) {
        let dir = tempfile::tempdir().expect("temp dir");
        for entry in std::fs::read_dir(committed_directory()).expect("folder") {
            let file = entry.expect("entry").path();
            std::fs::copy(&file, dir.path().join(file.file_name().expect("name"))).expect("copy");
        }
        let mut config = valid();
        config.remote_config.directory = dir.path().to_path_buf();
        (dir, config)
    }

    #[test]
    fn a_missing_layer_is_an_error() {
        let (dir, mut config) = copied_sets();
        let missing = object_key("public", Dimension::WorldId, WORLD_ID, VERSIONLESS);
        std::fs::remove_file(dir.path().join(layer_file_name(
            "public",
            Dimension::WorldId,
            WORLD_ID,
        )))
        .expect("remove");
        assert!(matches!(
            RemoteConfigSets::load(&config),
            Err(RemoteConfigError::Missing { key, .. }) if key == missing
        ));
        config.remote_config.directory = dir.path().join("absent");
        assert!(matches!(
            RemoteConfigSets::load(&config),
            Err(RemoteConfigError::Missing { .. })
        ));
    }

    #[test]
    fn a_file_must_be_a_json_object() {
        let (dir, config) = copied_sets();
        let key = object_key("public", Dimension::ProductId, PRODUCT_ID, VERSIONLESS);
        std::fs::write(
            dir.path()
                .join(layer_file_name("public", Dimension::ProductId, PRODUCT_ID)),
            "[1]",
        )
        .expect("write");
        assert!(matches!(
            RemoteConfigSets::load(&config),
            Err(RemoteConfigError::NotObject { key: found }) if found == key
        ));
    }

    #[test]
    fn the_folder_holds_only_layer_files() {
        let (dir, config) = copied_sets();
        let stray = dir.path().join("publicGameplay-RegionId-old.json");
        std::fs::write(&stray, "{}").expect("write");
        assert!(matches!(
            RemoteConfigSets::load(&config),
            Err(RemoteConfigError::Unexpected(path)) if path == stray
        ));
    }

    #[test]
    fn the_files_must_not_set_a_service_endpoint() {
        let key = object_key(
            ENDPOINTS_APPLICATION,
            Dimension::RegionId,
            CLUSTER_ID,
            VERSIONLESS,
        );
        let tracked: Value = serde_json::from_slice(
            &std::fs::read(committed_directory().join(layer_file_name(
                ENDPOINTS_APPLICATION,
                Dimension::RegionId,
                CLUSTER_ID,
            )))
            .expect("read"),
        )
        .expect("json");
        let refused = |value: &Value| {
            let (dir, config) = copied_sets();
            let path = dir.path().join(layer_file_name(
                ENDPOINTS_APPLICATION,
                Dimension::RegionId,
                CLUSTER_ID,
            ));
            std::fs::write(&path, serde_json::to_vec(value).expect("json")).expect("write");
            matches!(
                RemoteConfigSets::load(&config),
                Err(RemoteConfigError::Endpoint { key: found, .. }) if found == key
            )
        };
        for (path, endpoint) in GAME_HOST_ENDPOINTS {
            let mut value = tracked.clone();
            let section = path
                .iter()
                .fold(&mut value, |parent, part| &mut parent[*part]);
            section[endpoint] = Value::from("elsewhere.example.net");
            assert!(refused(&value), "{path:?} {endpoint} set");
            let mut value = tracked.clone();
            let (last, parents) = path.split_last().expect("path");
            let parent = parents
                .iter()
                .fold(&mut value, |parent, part| &mut parent[*part]);
            parent[*last] = Value::from("no object");
            assert!(refused(&value), "{path:?} not an object");
        }
    }

    /// The served `javelin` object of the server keys layer of `sets`.
    fn served_javelin(sets: &RemoteConfigSets) -> Value {
        let key = object_key(
            SERVER_KEYS_APPLICATION,
            Dimension::ProductId,
            PRODUCT_ID,
            VERSIONLESS,
        );
        served_json(sets, &key)[JAVELIN].clone()
    }

    /// The served `javelin.xp-multipliers` object of `sets`.
    fn served_experience(sets: &RemoteConfigSets) -> Value {
        served_javelin(sets)[XP_MULTIPLIERS].clone()
    }

    /// The served progression entries.
    fn progression_entries(experience: &Value) -> Vec<String> {
        experience["progression"]
            .as_array()
            .expect("progression")
            .iter()
            .map(|entry| entry.as_str().expect("entry").to_owned())
            .collect()
    }

    #[test]
    fn normal_experience_is_served_for_every_progression_by_default() {
        let experience = served_experience(&sets());
        assert_eq!(experience["character"].as_f64(), Some(NORMAL_EXPERIENCE));
        let entries = progression_entries(&experience);
        assert_eq!(
            entries.len(),
            ExperienceConfig::default().progressions().len()
        );
        for id in [
            "UmbralCurrency",
            "Riding",
            "Musician",
            "SwordAbilityTable",
            "2",
            "17",
        ] {
            assert!(entries.contains(&format!("{id}:1")), "{id}: {entries:?}");
        }
    }

    #[test]
    fn each_experience_setting_reaches_its_own_progression() {
        /// The live event of 2026-11: character, trade skills and masteries double, territory
        /// standing triple.
        const DOUBLE: f64 = 2.0;
        const TRIPLE: f64 = 3.0;
        let (_dir, mut config) = copied_sets();
        config.experience.character = DOUBLE;
        config.experience.umbral_crystals = TRIPLE;
        config
            .experience
            .trade_skills
            .insert("riding".to_owned(), DOUBLE);
        config
            .experience
            .weapon_masteries
            .insert("life_staff".to_owned(), DOUBLE);
        config
            .experience
            .territory_standing
            .insert("nighthaven".to_owned(), TRIPLE);
        let experience = served_experience(&RemoteConfigSets::load(&config).expect("sets"));
        assert_eq!(experience["character"].as_f64(), Some(DOUBLE));
        let entries = progression_entries(&experience);
        for entry in [
            "UmbralCurrency:3",
            "Riding:2",
            "LifeMagicAbilityTable:2",
            "17:3",
            "Mining:1",
            "2:1",
        ] {
            assert!(entries.iter().any(|served| served == entry), "{entry}");
        }
    }

    #[test]
    fn fast_travel_is_paid_by_default_and_free_when_set() {
        assert_eq!(
            served_javelin(&sets())[FAST_TRAVEL_FREE],
            Value::Bool(false)
        );
        let (_dir, mut config) = copied_sets();
        config.fast_travel.free = true;
        let served = served_javelin(&RemoteConfigSets::load(&config).expect("sets"));
        assert_eq!(served[FAST_TRAVEL_FREE], Value::Bool(true));
    }

    #[test]
    fn a_file_must_not_set_a_server_key() {
        for (application, dimension, id) in [
            (SERVER_KEYS_APPLICATION, Dimension::ProductId, PRODUCT_ID),
            (SERVER_KEYS_APPLICATION, Dimension::CognitoId, COGNITO_ID),
            ("public", Dimension::WorldId, WORLD_ID),
        ] {
            for javelin in [
                serde_json::json!({"xp-multipliers": {"character": 2}}),
                serde_json::json!({"xp-multipliers.progression": ["Riding:2"]}),
                serde_json::json!({"playtest-disable-fast-travel-costs": true}),
            ] {
                let (dir, config) = copied_sets();
                let path = dir.path().join(layer_file_name(application, dimension, id));
                let mut layer: Value =
                    serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
                layer["javelin"] = javelin;
                std::fs::write(&path, serde_json::to_vec(&layer).expect("json")).expect("write");
                let key = object_key(application, dimension, id, VERSIONLESS);
                assert!(
                    matches!(
                        RemoteConfigSets::load(&config),
                        Err(RemoteConfigError::ServerKey { key: found, .. }) if found == key
                    ),
                    "{key}"
                );
            }
        }
    }

    async fn get(path: &str, if_none_match: Option<&str>) -> (StatusCode, HeaderMap, Bytes) {
        let mut request = Request::builder().method("GET").uri(path);
        if let Some(etag) = if_none_match {
            request = request.header(IF_NONE_MATCH, etag);
        }
        let request = request.body(Body::empty()).expect("request");
        let response = tower::ServiceExt::oneshot(remote_config_router(sets()), request)
            .await
            .expect("response");
        let status = response.status();
        let headers = response.headers().clone();
        let body = http_body_util::BodyExt::collect(response.into_body())
            .await
            .expect("body")
            .to_bytes();
        (status, headers, body)
    }

    fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn an_object_has_its_body_type_and_etag() {
        let key = object_key("public", Dimension::ProductId, PRODUCT_ID, VERSIONLESS);
        let (status, headers, body) = get(&format!("/{key}"), None).await;
        assert_eq!(status, StatusCode::OK);
        let file = std::fs::read(committed_directory().join(layer_file_name(
            "public",
            Dimension::ProductId,
            PRODUCT_ID,
        )))
        .expect("file");
        assert_eq!(body.as_ref(), file.as_slice());
        assert_eq!(header(&headers, "content-type"), OBJECT_CONTENT_TYPE);
        let etag = format!("\"{}\"", HEXLOWER.encode(&Md5::digest(&file)));
        assert_eq!(header(&headers, "etag"), etag);
        assert_eq!(
            header(&headers, REQUEST_ID_HEADER).len(),
            REQUEST_ID_BYTES * 2
        );
        assert!(!header(&headers, HOST_ID_HEADER).is_empty());
    }

    #[tokio::test]
    async fn a_known_etag_gets_not_modified() {
        let key = object_key("publicGameplay", Dimension::WorldId, WORLD_ID, VERSIONLESS);
        let path = format!("/{key}");
        let (_, headers, _) = get(&path, None).await;
        let etag = header(&headers, "etag").to_owned();
        let (status, headers, body) = get(&path, Some(&etag)).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        assert!(body.is_empty());
        assert_eq!(header(&headers, "etag"), etag);
        let (status, _, _) = get(&path, Some(&format!("\"other\", {etag}"))).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        let (status, _, _) = get(&path, Some(ANY_ETAG)).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED);
        let (status, _, body) = get(&path, Some("\"0123\"")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.is_empty());
    }

    #[tokio::test]
    async fn a_build_key_gets_no_such_key() {
        for (dimension, id) in [
            (Dimension::ProductId, PRODUCT_ID),
            (Dimension::RegionId, CLUSTER_ID),
            (Dimension::CognitoId, COGNITO_ID),
            (Dimension::WorldId, WORLD_ID),
        ] {
            for application in APPLICATIONS {
                let escaped = object_key(application, dimension, id, ESCAPED_BUILD_VERSION);
                let (status, headers, body) = get(&format!("/{escaped}"), None).await;
                assert_eq!(status, StatusCode::NOT_FOUND, "{escaped}");
                assert_eq!(header(&headers, "content-type"), ERROR_CONTENT_TYPE);
                let text = String::from_utf8(body.to_vec()).expect("text");
                let key = object_key(application, dimension, id, BUILD_VERSION);
                assert!(
                    text.starts_with(
                        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>NoSuchKey</Code>"
                    ),
                    "{text}"
                );
                assert!(text.contains(&format!("<Key>{key}</Key>")), "{text}");
                assert!(text.contains(&format!(
                    "<RequestId>{}</RequestId>",
                    header(&headers, REQUEST_ID_HEADER)
                )));
                assert!(text.contains(&format!(
                    "<HostId>{}</HostId>",
                    header(&headers, HOST_ID_HEADER)
                )));
            }
        }
    }

    #[tokio::test]
    async fn other_methods_are_refused() {
        let key = object_key("public", Dimension::ProductId, PRODUCT_ID, VERSIONLESS);
        let request = test_http::request("PUT", &format!("/{key}"), "{}");
        let (status, _) = test_http::send(&remote_config_router(sets()), request).await;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn requested_layers_follow_the_configuration() {
        let layers = requested_layers(&valid());
        assert_eq!(
            layers,
            [
                (Dimension::ProductId, PRODUCT_ID.to_owned()),
                (Dimension::RegionId, CLUSTER_ID.to_owned()),
                (Dimension::CognitoId, COGNITO_ID.to_owned()),
                (Dimension::WorldId, WORLD_ID.to_owned()),
            ]
        );
    }
}
