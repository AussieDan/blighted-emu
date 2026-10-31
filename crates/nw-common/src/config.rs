// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loads a program's configuration from one TOML file, with environment variable overrides.
//!
//! Environment variables use the program prefix, and a double underscore between nested keys.
//! Example: with the prefix `NW_EDGE_`, `NW_EDGE_DATABASE__URL` overrides `url` in `[database]`.
//! Config structs must use `#[serde(deny_unknown_fields)]`, so a typo in a key is an error.
//!
//! Environment values are parsed as TOML-like values, so `123456` becomes a number and
//! `true` becomes a bool. To give such a value to a string field, quote it inside the value:
//! `NW_EDGE_DATABASE__PASSWORD='"123456"'`. A list of tables is one value:
//! `NW_DIRECTOR_START_POSITIONS='[{x = 1.0, y = 2.0}]'`.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Toml};
use serde::de::DeserializeOwned;

/// Separator between nested keys in environment variable names.
pub const ENV_NESTING_SEPARATOR: &str = "__";
/// Prefix of every secret environment variable. Secrets never come from a config file.
pub const SECRET_ENV_PREFIX: &str = "NW_SECRET_";

/// Error returned by [`load_config`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The configuration file does not exist.
    #[error("configuration file not found: {}", .0.display())]
    FileNotFound(PathBuf),
    /// The file or an environment variable has a missing, unknown or wrongly typed value.
    #[error("invalid configuration: {0}")]
    Invalid(Box<figment::Error>),
}

/// Loads `T` from the TOML file `file`, then applies overrides from environment
/// variables that start with `env_prefix`.
pub fn load_config<T: DeserializeOwned>(file: &Path, env_prefix: &str) -> Result<T, ConfigError> {
    // figment ignores a missing file, so check it first.
    if !file.is_file() {
        return Err(ConfigError::FileNotFound(file.to_path_buf()));
    }
    Figment::new()
        // file_exact: do not search parent folders for the file.
        .merge(Toml::file_exact(file))
        .merge(Env::prefixed(env_prefix).split(ENV_NESTING_SEPARATOR))
        .extract()
        .map_err(|error| ConfigError::Invalid(Box::new(error)))
}

#[cfg(test)]
#[expect(
    clippy::result_large_err,
    reason = "figment::Jail requires its closures to return figment::Result"
)]
mod tests {
    use super::*;
    use figment::Jail;
    use serde::Deserialize;

    const TEST_PREFIX: &str = "NW_TEST_";
    const CONFIG_FILE: &str = "program.toml";
    const FILE_DATABASE_URL: &str = "postgres://localhost/file";
    const FILE_MAX_CONNECTIONS: u32 = 8;
    const ENV_MAX_CONNECTIONS: u32 = 32;
    const NUMERIC_TEXT: &str = "123456";

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct TestConfig {
        log_filter: String,
        database: DatabaseSection,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct DatabaseSection {
        url: String,
        max_connections: u32,
    }

    fn valid_toml() -> String {
        format!(
            "log_filter = \"info\"\n[database]\nurl = \"{FILE_DATABASE_URL}\"\nmax_connections = {FILE_MAX_CONNECTIONS}\n"
        )
    }

    fn load(path: &str) -> Result<TestConfig, ConfigError> {
        load_config(Path::new(path), TEST_PREFIX)
    }

    #[test]
    fn loads_values_from_file() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            let config = load(CONFIG_FILE).expect("valid config");
            assert_eq!(
                config,
                TestConfig {
                    log_filter: "info".to_owned(),
                    database: DatabaseSection {
                        url: FILE_DATABASE_URL.to_owned(),
                        max_connections: FILE_MAX_CONNECTIONS,
                    },
                }
            );
            Ok(())
        });
    }

    #[test]
    fn environment_overrides_top_level_key_with_underscore() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            jail.set_env("NW_TEST_LOG_FILTER", "debug");
            let config = load(CONFIG_FILE).expect("valid config");
            assert_eq!(config.log_filter, "debug");
            Ok(())
        });
    }

    #[test]
    fn environment_overrides_nested_key() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            jail.set_env("NW_TEST_DATABASE__MAX_CONNECTIONS", ENV_MAX_CONNECTIONS);
            let config = load(CONFIG_FILE).expect("valid config");
            assert_eq!(config.database.max_connections, ENV_MAX_CONNECTIONS);
            assert_eq!(config.database.url, FILE_DATABASE_URL);
            Ok(())
        });
    }

    #[test]
    fn numeric_environment_value_for_string_field_is_an_error() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            jail.set_env("NW_TEST_DATABASE__URL", NUMERIC_TEXT);
            let error = load(CONFIG_FILE).expect_err("unquoted digits parse as a number");
            assert!(
                matches!(error, ConfigError::Invalid(_)),
                "unexpected error: {error}"
            );
            Ok(())
        });
    }

    #[test]
    fn quoted_numeric_environment_value_loads_as_string() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            jail.set_env("NW_TEST_DATABASE__URL", format!("\"{NUMERIC_TEXT}\""));
            let config = load(CONFIG_FILE).expect("quoted digits stay a string");
            assert_eq!(config.database.url, NUMERIC_TEXT);
            Ok(())
        });
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PointsConfig {
        points: Vec<Point>,
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Point {
        x: f32,
        y: f32,
    }

    #[test]
    fn environment_value_replaces_a_list_of_tables() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, "points = [{ x = 1.0, y = 2.0 }]\n")?;
            jail.set_env("NW_TEST_POINTS", "[{x = 3.5, y = 4.5}, {x = 5.0, y = 6.0}]");
            let config: PointsConfig =
                load_config(Path::new(CONFIG_FILE), TEST_PREFIX).expect("valid config");
            assert_eq!(
                config.points,
                vec![Point { x: 3.5, y: 4.5 }, Point { x: 5.0, y: 6.0 }]
            );
            Ok(())
        });
    }

    #[test]
    fn missing_file_is_an_error() {
        Jail::expect_with(|_jail| {
            let error = load(CONFIG_FILE).expect_err("file does not exist");
            assert!(
                matches!(&error, ConfigError::FileNotFound(path) if path == Path::new(CONFIG_FILE)),
                "unexpected error: {error}"
            );
            Ok(())
        });
    }

    #[test]
    fn wrong_type_is_an_error() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &valid_toml())?;
            jail.set_env("NW_TEST_DATABASE__MAX_CONNECTIONS", "many");
            let error = load(CONFIG_FILE).expect_err("max_connections is not a number");
            assert!(
                matches!(error, ConfigError::Invalid(_)),
                "unexpected error: {error}"
            );
            Ok(())
        });
    }

    #[test]
    fn unknown_key_is_an_error() {
        Jail::expect_with(|jail| {
            jail.create_file(CONFIG_FILE, &format!("{}lgo_level = \"x\"\n", valid_toml()))?;
            let error = load(CONFIG_FILE).expect_err("lgo_level is a typo");
            assert!(
                matches!(error, ConfigError::Invalid(_)),
                "unexpected error: {error}"
            );
            Ok(())
        });
    }
}
