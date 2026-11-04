// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gameplay values of the remote config sets that `edge-http` gives the client (study note
//! `docs/re/2026-09-18-remote-config-layers.md`): the sim reads the same files through the same
//! loader and checks ([`RemoteConfigSets::load`]), so server and client share one source.
//!
//! The layers are those the client asks for ([`requested_layers`], lowest first: `ProductId`,
//! `RegionId`, `CognitoId`, then this sim's own `WorldId`), of both applications, `public` first
//! (Inferred; the two applications set no key to different values, see the tests).

use std::path::{Path, PathBuf};

use nw_content::gameplay_config::{GameplayConfig, GameplayConfigError, KeyStack};
use nw_login::config::{EdgeConfigError, load_edge_config};
use nw_login::remote_config::{
    APPLICATIONS, Dimension, RemoteConfigError, RemoteConfigSets, VERSIONLESS, object_key,
    requested_layers,
};
use serde_json::Value;
use uuid::Uuid;

/// The gameplay values cannot be loaded.
#[derive(Debug, thiserror::Error)]
pub enum GameplayError {
    /// The `edge-http` config cannot be read.
    #[error("edge config {}: {source}", .path.display())]
    EdgeConfig {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        #[source]
        source: EdgeConfigError,
    },
    /// The edge config has no `[[world]]` with this sim's world id.
    #[error("edge config {} has no world {world}", .path.display())]
    UnknownWorld {
        /// The file.
        path: PathBuf,
        /// The sim's world.
        world: Uuid,
    },
    /// The config sets cannot be loaded.
    #[error(transparent)]
    Sets(#[from] RemoteConfigError),
    /// A layer is not a JSON object.
    #[error("remote config {key} is not a JSON object")]
    NotObject {
        /// The key.
        key: String,
    },
    /// A gameplay key has a value the sim cannot read.
    #[error(transparent)]
    Values(#[from] GameplayConfigError),
}

/// The gameplay values of world `world` in the config sets that the `edge-http` config at
/// `edge_config` names.
pub fn load_gameplay(edge_config: &Path, world: Uuid) -> Result<GameplayConfig, GameplayError> {
    let config = load_edge_config(edge_config).map_err(|source| GameplayError::EdgeConfig {
        path: edge_config.to_path_buf(),
        source,
    })?;
    if config.world(world).is_none() {
        return Err(GameplayError::UnknownWorld {
            path: edge_config.to_path_buf(),
            world,
        });
    }
    let sets = RemoteConfigSets::load(&config)?;
    let own_world = world.to_string();
    let layers = requested_layers(&config);
    let mut objects = Vec::new();
    for application in APPLICATIONS {
        for (dimension, id) in &layers {
            if *dimension == Dimension::WorldId && *id != own_world {
                continue;
            }
            let key = object_key(application, *dimension, id, VERSIONLESS);
            // `RemoteConfigSets::load` checked that every requested layer exists and is an
            // object; a missing one is refused the same way.
            let object = sets
                .body(&key)
                .and_then(|body| serde_json::from_slice::<Value>(body).ok())
                .and_then(|value| match value {
                    Value::Object(object) => Some(object),
                    _ => None,
                })
                .ok_or_else(|| GameplayError::NotObject { key: key.clone() })?;
            objects.push(object);
        }
    }
    Ok(GameplayConfig::from_stack(&KeyStack::merge(&objects))?)
}

#[cfg(test)]
mod tests {
    use nw_content::gameplay_config::NEUTRAL_MULTIPLIER;
    use nw_login::config::NORMAL_EXPERIENCE_MULTIPLIER;
    use serde_json::Map;

    use super::*;

    const COMMITTED_EDGE_CONFIG: &str = "config/edge.toml";
    const COMMITTED_WORLD: &str = "8d0f4c2e-0000-4000-8000-000000000001";

    /// The edge config of the repository, with its relative config set folder made absolute and
    /// this experience multiplier.
    fn committed_edge_config(folder: &Path, experience_multiplier: u32) -> PathBuf {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let text = std::fs::read_to_string(root.join(COMMITTED_EDGE_CONFIG)).expect("edge.toml");
        let sets = root.join("config/remote-config");
        let text = text
            .replace(
                "directory = \"config/remote-config\"",
                &format!("directory = {:?}", sets.display().to_string()),
            )
            .replace(
                &format!("experience_multiplier = {NORMAL_EXPERIENCE_MULTIPLIER}"),
                &format!("experience_multiplier = {experience_multiplier}"),
            );
        let path = folder.join("edge.toml");
        std::fs::write(&path, text).expect("written");
        path
    }

    fn world() -> Uuid {
        Uuid::parse_str(COMMITTED_WORLD).expect("uuid")
    }

    #[test]
    fn the_committed_sets_give_normal_experience_and_the_retail_gameplay_values() {
        let folder = std::env::temp_dir().join(format!("sim-gameplay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("folder");
        let config = load_gameplay(
            &committed_edge_config(&folder, NORMAL_EXPERIENCE_MULTIPLIER),
            world(),
        )
        .expect("gameplay");
        assert!((config.xp.character - NEUTRAL_MULTIPLIER).abs() < f64::EPSILON);
        for id in ["UmbralCurrency", "17", "Riding", "Musician"] {
            assert!(
                (config.xp.progression(id) - NEUTRAL_MULTIPLIER).abs() < f64::EPSILON,
                "{id}"
            );
        }
        assert_eq!(config.disabled_item_classes, ["Dagger"]);
        assert_eq!(config.wallet_cap, Some(1_000_000_000));
        assert_eq!(config.paid_loadouts, Some(27));
        assert_eq!(config.territory_entry_requirement, Some(false));
        assert_eq!(config.house_level_reqs, [15, 35, 55, 55, 55]);
        assert_eq!(config.transmog_own_all, Some(true));
        assert_eq!(config.datatable_overrides.len(), 12);
        assert_eq!(config.schedule_overrides.len(), 13);
        assert_eq!(config.red_button.traffic_mitigation_disabled, Some(false));
        assert_eq!(config.twitch_system, Some(false));
        assert_eq!(config.game_modes.matchmaking_modes.len(), 6);
        std::fs::remove_dir_all(&folder).expect("removed");
    }

    #[test]
    fn a_test_server_s_experience_multiplier_reaches_the_sim() {
        /// Double experience.
        const DOUBLE: u32 = 2;
        let folder = std::env::temp_dir().join(format!("sim-gameplay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("folder");
        let config =
            load_gameplay(&committed_edge_config(&folder, DOUBLE), world()).expect("gameplay");
        let doubled = NEUTRAL_MULTIPLIER * f64::from(DOUBLE);
        assert!((config.xp.character - doubled).abs() < f64::EPSILON);
        assert!((config.xp.progression("Riding") - doubled).abs() < f64::EPSILON);
        assert!((config.xp.progression("UmbralCurrency") - doubled).abs() < f64::EPSILON);
        std::fs::remove_dir_all(&folder).expect("removed");
    }

    #[test]
    fn the_two_applications_set_no_key_to_different_values() {
        let folder = std::env::temp_dir().join(format!("sim-gameplay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("folder");
        let edge = load_edge_config(&committed_edge_config(
            &folder,
            NORMAL_EXPERIENCE_MULTIPLIER,
        ))
        .expect("edge config");
        let sets = RemoteConfigSets::load(&edge).expect("sets");
        let layers_of = |application: &str| -> Vec<Map<String, Value>> {
            requested_layers(&edge)
                .iter()
                .filter_map(|(dimension, id)| {
                    let body = sets.body(&object_key(application, *dimension, id, VERSIONLESS))?;
                    serde_json::from_slice(body).ok()
                })
                .collect()
        };
        let [public, gameplay] = APPLICATIONS.map(layers_of);
        assert_eq!(
            KeyStack::merge(public.iter().chain(&gameplay)),
            KeyStack::merge(gameplay.iter().chain(&public)),
            "the order of the applications changes no key"
        );
        std::fs::remove_dir_all(&folder).expect("removed");
    }

    #[test]
    fn a_world_the_edge_config_does_not_name_is_refused() {
        let folder = std::env::temp_dir().join(format!("sim-gameplay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&folder).expect("folder");
        let error = load_gameplay(
            &committed_edge_config(&folder, NORMAL_EXPERIENCE_MULTIPLIER),
            Uuid::nil(),
        )
        .expect_err("refused");
        assert!(
            matches!(error, GameplayError::UnknownWorld { .. }),
            "{error}"
        );
        std::fs::remove_dir_all(&folder).expect("removed");
    }
}
