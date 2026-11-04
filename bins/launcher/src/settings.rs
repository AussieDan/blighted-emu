// SPDX-License-Identifier: GPL-3.0-or-later
//! Saved launcher settings (M2 spec section 6): the saved servers (a name, an address and the
//! launcher token of each), the selected server, theme and launch options. The same file and
//! rules on Windows and Linux. The file is readable only by the user on Unix.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::options::LaunchOptions;
use crate::server::api_base_url;
use crate::theme::ThemeChoice;

/// Folder under the user config folder.
pub const SETTINGS_DIR: &str = "blighted-launcher";
/// Settings file name.
pub const SETTINGS_FILE: &str = "settings.json";
/// File mode of the settings file on Unix: owner read and write only.
#[cfg(unix)]
const OWNER_ONLY_MODE: u32 = 0o600;

/// One saved server.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerProfile {
    /// The name the player sees; at first the server's own name.
    pub name: String,
    /// The server address as typed.
    pub server: String,
    /// Launcher session token of this server, when logged in. A token belongs to one server
    /// (review finding 5), so each saved server keeps its own.
    pub launcher_token: Option<String>,
}

/// What the launcher remembers. Keys of older launchers (`steam_path`, from when the game started
/// through Steam's launcher) are ignored; their one `server` and `launcher_token` become the
/// first saved server ([`Settings::load`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The saved servers, in the order they were saved.
    pub profiles: Vec<ServerProfile>,
    /// The address of the selected saved server.
    pub selected: Option<String>,
    /// The window theme; files of older launchers have none and give night.
    pub theme: ThemeChoice,
    /// How the game starts; files of older launchers have none and give empty options.
    pub options: LaunchOptions,
    /// The one server of an older launcher's file; read, never written.
    #[serde(skip_serializing)]
    server: String,
    /// The token of an older launcher's server; read, never written.
    #[serde(skip_serializing)]
    launcher_token: Option<String>,
}

/// Whether two typed addresses name the same server: the same launcher API base URL
/// (`Example.org` and `example.org:443` are one server).
pub fn same_server(one: &str, other: &str) -> bool {
    match (api_base_url(one), api_base_url(other)) {
        (Ok(one), Ok(other)) => one == other,
        _ => one.trim() == other.trim(),
    }
}

/// The settings file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// No user config folder is known on this system.
    #[error("no user config folder found")]
    NoConfigDir,
    /// The file could not be read or written.
    #[error("cannot access {}: {source}", .path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not valid JSON settings.
    #[error("settings file {} is not valid: {source}", .path.display())]
    Parse {
        /// The file.
        path: PathBuf,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
}

/// Environment variable of the user config folder on Windows.
#[cfg(windows)]
const CONFIG_DIR_ENV: &str = "APPDATA";
/// Environment variable of the user config folder on Linux (XDG base directories).
#[cfg(not(windows))]
const CONFIG_DIR_ENV: &str = "XDG_CONFIG_HOME";
/// Home folder variable; the XDG default config folder is `$HOME/.config`.
#[cfg(not(windows))]
const HOME_ENV: &str = "HOME";
#[cfg(not(windows))]
const XDG_DEFAULT_CONFIG_DIR: &str = ".config";

/// The user config folder from environment variables.
fn config_dir(lookup: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |name: &str| {
        lookup(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    #[cfg(windows)]
    {
        set(CONFIG_DIR_ENV)
    }
    #[cfg(not(windows))]
    {
        set(CONFIG_DIR_ENV).or_else(|| set(HOME_ENV).map(|home| home.join(XDG_DEFAULT_CONFIG_DIR)))
    }
}

/// `<user config folder>/blighted-launcher/settings.json`.
pub fn default_settings_path() -> Result<PathBuf, SettingsError> {
    config_dir(|name| std::env::var(name).ok())
        .map(|dir| dir.join(SETTINGS_DIR).join(SETTINGS_FILE))
        .ok_or(SettingsError::NoConfigDir)
}

impl Settings {
    /// The saved server with address `server`.
    fn profile_mut(&mut self, server: &str) -> Option<&mut ServerProfile> {
        self.profiles
            .iter_mut()
            .find(|profile| same_server(&profile.server, server))
    }

    /// The selected saved server.
    pub fn selected_profile(&self) -> Option<&ServerProfile> {
        let selected = self.selected.as_deref()?;
        self.profiles
            .iter()
            .find(|profile| same_server(&profile.server, selected))
    }

    /// The launcher token of the selected server, when logged in.
    pub fn token(&self) -> Option<&str> {
        self.selected_profile()?.launcher_token.as_deref()
    }

    /// Sets the launcher token of the selected server (`None`: logged out).
    pub fn set_token(&mut self, token: Option<String>) {
        if let Some(selected) = self.selected.clone()
            && let Some(profile) = self.profile_mut(&selected)
        {
            profile.launcher_token = token;
        }
    }

    /// The launcher connected to `server`, whose own name is `server_name`: it is saved, when it
    /// is new, under that name, and selected. A server saved under its address alone (from an
    /// older launcher's file) takes the server's name.
    pub fn connected(&mut self, server: &str, server_name: &str) {
        match self.profile_mut(server) {
            Some(profile) => {
                if profile.name == profile.server {
                    profile.name = server_name.to_owned();
                }
            }
            None => self.profiles.push(ServerProfile {
                name: server_name.to_owned(),
                server: server.to_owned(),
                launcher_token: None,
            }),
        }
        self.selected = self
            .profile_mut(server)
            .map(|profile| profile.server.clone());
    }

    /// Renames the saved server `server`; an empty name keeps the old one.
    pub fn rename(&mut self, server: &str, name: &str) {
        let name = name.trim();
        if let Some(profile) = self.profile_mut(server)
            && !name.is_empty()
        {
            profile.name = name.to_owned();
        }
    }

    /// Forgets the saved server `server` and its login; when it was selected, none is.
    pub fn remove(&mut self, server: &str) {
        self.profiles
            .retain(|profile| !same_server(&profile.server, server));
        if self
            .selected
            .as_deref()
            .is_some_and(|selected| same_server(selected, server))
        {
            self.selected = None;
        }
    }

    /// The one server of an older launcher's file becomes the first saved server, named by its
    /// address until the launcher connects ([`Self::connected`]).
    fn adopt_older_server(&mut self) {
        let server = std::mem::take(&mut self.server);
        let token = self.launcher_token.take();
        if server.is_empty() || self.profile_mut(&server).is_some() {
            return;
        }
        self.profiles.push(ServerProfile {
            name: server.clone(),
            server: server.clone(),
            launcher_token: token,
        });
        self.selected.get_or_insert(server);
    }

    /// Reads the settings; a missing file gives the defaults.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(SettingsError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let mut settings: Self =
            serde_json::from_str(&text).map_err(|source| SettingsError::Parse {
                path: path.to_path_buf(),
                source,
            })?;
        settings.adopt_older_server();
        Ok(settings)
    }

    /// Writes the settings, creating the folder. On Unix only the owner can read the file.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let io_error = |source| SettingsError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|source| SettingsError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(OWNER_ONLY_MODE);
            // An older file may have wider rights; narrow them before writing the token.
            if path.exists() {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(OWNER_ONLY_MODE))
                    .map_err(io_error)?;
            }
        }
        let mut file = options.open(path).map_err(io_error)?;
        file.write_all(text.as_bytes()).map_err(io_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(name: &str, server: &str, token: Option<&str>) -> ServerProfile {
        ServerProfile {
            name: name.to_owned(),
            server: server.to_owned(),
            launcher_token: token.map(str::to_owned),
        }
    }

    fn sample() -> Settings {
        Settings {
            profiles: vec![profile("Test world", "example.org", Some("token"))],
            selected: Some("example.org".to_owned()),
            theme: ThemeChoice::Day,
            options: LaunchOptions {
                game_exe: "/g/BlightedNewWorld.exe".to_owned(),
                environment: "DXVK_HUD=fps".to_owned(),
                ..LaunchOptions::default()
            },
            ..Settings::default()
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn config_dir_follows_xdg() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        assert_eq!(
            config_dir(lookup(&[("XDG_CONFIG_HOME", "/cfg"), ("HOME", "/home/a")])),
            Some(PathBuf::from("/cfg"))
        );
        assert_eq!(
            config_dir(lookup(&[("HOME", "/home/a")])),
            Some(PathBuf::from("/home/a/.config"))
        );
        assert_eq!(config_dir(lookup(&[])), None);
    }

    #[test]
    fn each_server_is_saved_once_under_its_name_with_its_own_login() {
        let mut settings = sample();
        settings.connected("Example.org:443", "Renamed world");
        assert_eq!(
            settings,
            sample(),
            "the same server: no new entry, its name kept"
        );
        settings.connected("local.example.org", "Test world");
        assert_eq!(settings.profiles.len(), 2);
        assert_eq!(settings.selected.as_deref(), Some("local.example.org"));
        assert_eq!(settings.token(), None, "the new server has no login yet");
        settings.set_token(Some("local token".to_owned()));
        settings.connected("example.org", "Test world");
        assert_eq!(
            settings.token(),
            Some("token"),
            "the first server kept its login"
        );
        settings.connected("local.example.org", "Test world");
        assert_eq!(settings.token(), Some("local token"));
    }

    #[test]
    fn a_saved_server_is_renamed_and_removed() {
        let mut settings = sample();
        settings.rename("example.org", "  ");
        assert_eq!(
            settings.profiles[0].name, "Test world",
            "an empty name keeps the old one"
        );
        settings.rename("example.org", " Main ");
        assert_eq!(settings.profiles[0].name, "Main");
        settings.connected("local.example.org", "Test world");
        settings.remove("example.org");
        assert_eq!(settings.selected.as_deref(), Some("local.example.org"));
        settings.remove("local.example.org");
        assert!(settings.profiles.is_empty());
        assert_eq!(settings.selected, None);
        assert_eq!(settings.token(), None);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(
            Settings::load(&dir.path().join("none.json")).expect("load"),
            Settings::default()
        );
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(SETTINGS_DIR).join(SETTINGS_FILE);
        sample().save(&path).expect("save");
        assert_eq!(Settings::load(&path).expect("load"), sample());
    }

    /// Mode of a file that others can read.
    #[cfg(unix)]
    const WORLD_READABLE_MODE: u32 = 0o644;
    /// Permission bits of a mode.
    #[cfg(unix)]
    const PERMISSION_BITS: u32 = 0o777;

    #[cfg(unix)]
    #[test]
    fn settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(SETTINGS_FILE);
        std::fs::write(&path, "{}").expect("old file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(WORLD_READABLE_MODE))
            .expect("open rights");
        sample().save(&path).expect("save");
        let mode = std::fs::metadata(&path)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & PERMISSION_BITS, OWNER_ONLY_MODE);
    }

    #[test]
    fn an_older_file_s_server_becomes_the_first_saved_server() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(SETTINGS_FILE);
        std::fs::write(
            &path,
            r#"{"server":"example.org","launcher_token":"token","steam_path":null}"#,
        )
        .expect("write");
        let mut loaded = Settings::load(&path).expect("load");
        assert_eq!(
            loaded,
            Settings {
                profiles: vec![profile("example.org", "example.org", Some("token"))],
                theme: ThemeChoice::Night,
                options: LaunchOptions::default(),
                ..sample()
            }
        );
        loaded.connected("example.org", "Test world");
        assert_eq!(
            loaded.profiles[0].name, "Test world",
            "it takes the server's name"
        );
        loaded.save(&path).expect("save");
        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        for older in ["server", "launcher_token"] {
            assert!(written.get(older).is_none(), "{older} is not written again");
        }
        assert_eq!(Settings::load(&path).expect("load"), loaded);
    }

    #[test]
    fn broken_file_is_an_error() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join(SETTINGS_FILE);
        std::fs::write(&path, "not json").expect("write");
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Parse { .. })
        ));
    }
}
