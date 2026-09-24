//! Versioned, atomic, GTK-free persistence of Balun's user settings.
//!
//! The settings file holds only reviewed preferences: remembered discovery
//! targets and window state. It never holds credentials, `DeviceAuth`, stream
//! URLs, lineups, or incidental network topology, and the types here cannot
//! represent them.
//!
//! Reads fail closed. A malformed, oversized, symlinked, or newer-schema file
//! is reported with a fixed, path-free error and left untouched, so a later
//! save cannot destroy settings written by a newer Balun or edited by hand.
//! Writes go through a temporary sibling that is flushed and renamed over the
//! previous file, so a crash never leaves a partial document. On Unix the
//! file is readable and writable by its owner only.

use std::ffi::OsString;
#[cfg(test)]
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};

use crate::discovery::{ExactDiscoveryTarget, HostnameTarget};

mod store;
pub use store::SettingsStore;

/// Schema version written by this build and the newest version it can read.
pub const SCHEMA_VERSION: u32 = 2;
/// File name inside the settings directory.
pub const SETTINGS_FILE_NAME: &str = "settings.json";
/// Largest settings document that will be read or written.
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
/// Most remembered exact-address targets; matches the per-session probe cap.
pub const MAX_REMEMBERED_TARGETS: usize = 32;
/// Smallest persisted window dimension in logical pixels.
pub const MIN_WINDOW_DIMENSION: u32 = 200;
/// Largest persisted window dimension in logical pixels.
pub const MAX_WINDOW_DIMENSION: u32 = 16_384;
/// Window size used until the user has resized the window.
pub const DEFAULT_WINDOW_WIDTH: u32 = 1_200;
/// Window height used until the user has resized the window.
pub const DEFAULT_WINDOW_HEIGHT: u32 = 720;

const TEMPORARY_PREFIX: &str = ".settings.";
const TEMPORARY_SUFFIX: &str = ".tmp";

/// Persisted main-window geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowState {
    width: u32,
    height: u32,
    maximized: bool,
}

/// Why a window state was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidWindowState {
    #[error(
        "window dimensions must be between {MIN_WINDOW_DIMENSION} and {MAX_WINDOW_DIMENSION} pixels"
    )]
    DimensionOutOfRange,
}

impl WindowState {
    /// Validate a window size in logical pixels.
    pub const fn new(width: u32, height: u32, maximized: bool) -> Result<Self, InvalidWindowState> {
        if width < MIN_WINDOW_DIMENSION
            || width > MAX_WINDOW_DIMENSION
            || height < MIN_WINDOW_DIMENSION
            || height > MAX_WINDOW_DIMENSION
        {
            return Err(InvalidWindowState::DimensionOutOfRange);
        }
        Ok(Self {
            width,
            height,
            maximized,
        })
    }

    /// Window width in logical pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Window height in logical pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// Whether the window was maximized.
    #[must_use]
    pub const fn maximized(self) -> bool {
        self.maximized
    }
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
            maximized: false,
        }
    }
}

/// One remembered discovery entry: a numeric address or a hostname that is
/// resolved again at each launch.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum RememberedTarget {
    Address(ExactDiscoveryTarget),
    Hostname(HostnameTarget),
}

/// The complete in-memory settings document.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Settings {
    window: WindowState,
    remembered_targets: Vec<RememberedTarget>,
}

impl Settings {
    /// Persisted window geometry.
    #[must_use]
    pub const fn window(&self) -> WindowState {
        self.window
    }

    /// Replace the window geometry; returns whether anything changed.
    pub fn set_window(&mut self, window: WindowState) -> bool {
        if self.window == window {
            return false;
        }
        self.window = window;
        true
    }

    /// Remembered discovery entries, oldest first.
    #[must_use]
    pub fn remembered_targets(&self) -> &[RememberedTarget] {
        &self.remembered_targets
    }

    /// Remember a validated target as the most recent entry.
    ///
    /// A repeated target moves to the most recent position. When the list is
    /// full, the oldest entry is forgotten. Returns whether the list changed.
    pub fn remember_target(&mut self, target: RememberedTarget) -> bool {
        if self.remembered_targets.last() == Some(&target) {
            return false;
        }
        self.remembered_targets.retain(|known| *known != target);
        while self.remembered_targets.len() >= MAX_REMEMBERED_TARGETS {
            self.remembered_targets.remove(0);
        }
        self.remembered_targets.push(target);
        true
    }

    /// Forget a remembered target; returns whether it was present.
    pub fn forget_target(&mut self, target: &RememberedTarget) -> bool {
        let before = self.remembered_targets.len();
        self.remembered_targets.retain(|known| known != target);
        self.remembered_targets.len() != before
    }
}

/// The step of a store operation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettingsOperation {
    /// Inspecting the settings file.
    Inspect,
    /// Reading the settings file.
    Read,
    /// Creating the settings directory.
    CreateDirectory,
    /// Creating the temporary sibling for an atomic write.
    CreateTemporary,
    /// Writing the temporary sibling.
    Write,
    /// Flushing the temporary sibling or its directory.
    Sync,
    /// Renaming the temporary sibling over the settings file.
    Publish,
}

impl std::fmt::Display for SettingsOperation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Inspect => "inspecting the settings file",
            Self::Read => "reading the settings file",
            Self::CreateDirectory => "creating the settings directory",
            Self::CreateTemporary => "creating the temporary settings file",
            Self::Write => "writing the temporary settings file",
            Self::Sync => "flushing the settings file",
            Self::Publish => "replacing the settings file",
        })
    }
}

/// Why a stored document was rejected. No value from the file is echoed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MalformedSettings {
    #[error("the document is not the expected JSON shape")]
    Json,
    #[error("schema version 0 is not valid")]
    ZeroSchemaVersion,
    #[error("the window state is out of range")]
    WindowState,
    #[error("a remembered target is not a usable numeric address")]
    RememberedTarget,
    #[error("a remembered target is listed twice")]
    DuplicateTarget,
    #[error("more than {MAX_REMEMBERED_TARGETS} remembered targets")]
    TooManyTargets,
}

/// A settings load or save failure. Paths and file contents are never carried.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsError {
    #[error("the settings file is a symbolic link")]
    Symlink,
    #[error("the settings path is not a regular file")]
    NotRegularFile,
    #[error("the settings file exceeds {MAX_SETTINGS_BYTES} bytes")]
    TooLarge,
    #[error("the settings file is malformed: {0}")]
    Malformed(MalformedSettings),
    #[error("settings schema version {found} is newer than the supported version {SCHEMA_VERSION}")]
    UnsupportedSchema { found: u32 },
    #[error("{operation} failed: {kind:?}")]
    Io {
        operation: SettingsOperation,
        kind: io::ErrorKind,
    },
    #[error("the settings could not be serialized")]
    Serialization,
    #[error("the settings profile is not a private local directory")]
    InvalidDirectory,
    #[error("the settings owner or permissions are not private")]
    Permissions,
    #[error("the settings file has multiple links")]
    HardLink,
    #[error("the settings identity changed during the operation")]
    Changed,
    #[error("another settings transaction is active")]
    Busy,
    #[error("the settings operation was cancelled")]
    Cancelled,
}

impl SettingsError {
    fn io(operation: SettingsOperation, error: &io::Error) -> Self {
        Self::Io {
            operation,
            kind: error.kind(),
        }
    }
}

/// Resolve the platform settings directory from the process environment.
///
/// Windows uses `%APPDATA%\Balun`, macOS uses
/// `~/Library/Application Support/Balun`, and other Unix systems use
/// `$XDG_CONFIG_HOME/balun` or `~/.config/balun`. Relative and empty values
/// are ignored. Returns `None` when the environment does not name a directory.
#[must_use]
pub fn default_directory() -> Option<PathBuf> {
    default_directory_from(|key| std::env::var_os(key))
}

fn absolute_directory(value: Option<OsString>) -> Option<PathBuf> {
    let path = PathBuf::from(value?);
    path.is_absolute().then_some(path)
}

#[cfg(windows)]
fn default_directory_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute_directory(env("APPDATA")).map(|base| base.join("Balun"))
}

#[cfg(target_os = "macos")]
fn default_directory_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute_directory(env("HOME")).map(|home| {
        home.join("Library")
            .join("Application Support")
            .join("Balun")
    })
}

#[cfg(all(unix, not(target_os = "macos")))]
fn default_directory_from(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    absolute_directory(env("XDG_CONFIG_HOME"))
        .or_else(|| absolute_directory(env("HOME")).map(|home| home.join(".config")))
        .map(|base| base.join("balun"))
}

#[cfg(not(any(windows, unix)))]
fn default_directory_from(_env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    None
}

/// Only the version is read before choosing a stored shape, so a document
/// written by a newer Balun is reported as unsupported rather than malformed.
#[derive(Deserialize)]
struct SchemaHeader {
    schema_version: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSettingsV1 {
    schema_version: u32,
    #[serde(default)]
    window: StoredWindowV1,
    #[serde(default)]
    remembered_targets: Vec<StoredTargetV1>,
    #[serde(default, rename = "device_names", skip_serializing)]
    _retired_device_names: IgnoredAny,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredWindowV1 {
    width: u32,
    height: u32,
    maximized: bool,
}

impl Default for StoredWindowV1 {
    fn default() -> Self {
        let window = WindowState::default();
        Self {
            width: window.width,
            height: window.height,
            maximized: window.maximized,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTargetV1 {
    address: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredSettingsV2 {
    schema_version: u32,
    #[serde(default)]
    window: StoredWindowV1,
    #[serde(default)]
    remembered_targets: Vec<StoredTargetV2>,
    /// Retired friendly names. Earlier builds always wrote this map, so it is
    /// still accepted, then ignored and no longer written.
    #[serde(default, rename = "device_names", skip_serializing)]
    _retired_device_names: IgnoredAny,
}

/// Exactly one of `address` or `host` is present.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTargetV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    host: Option<String>,
}

impl From<StoredSettingsV1> for StoredSettingsV2 {
    fn from(stored: StoredSettingsV1) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            window: stored.window,
            remembered_targets: stored
                .remembered_targets
                .into_iter()
                .map(|target| StoredTargetV2 {
                    address: Some(target.address),
                    host: None,
                })
                .collect(),
            _retired_device_names: IgnoredAny,
        }
    }
}

fn parse_document(bytes: &[u8]) -> Result<Settings, SettingsError> {
    let header: SchemaHeader = serde_json::from_slice(bytes)
        .map_err(|_| SettingsError::Malformed(MalformedSettings::Json))?;
    match header.schema_version {
        0 => Err(SettingsError::Malformed(
            MalformedSettings::ZeroSchemaVersion,
        )),
        1 => {
            let stored: StoredSettingsV1 = serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Malformed(MalformedSettings::Json))?;
            Settings::try_from(StoredSettingsV2::from(stored)).map_err(SettingsError::Malformed)
        }
        2 => {
            let stored: StoredSettingsV2 = serde_json::from_slice(bytes)
                .map_err(|_| SettingsError::Malformed(MalformedSettings::Json))?;
            Settings::try_from(stored).map_err(SettingsError::Malformed)
        }
        found => Err(SettingsError::UnsupportedSchema { found }),
    }
}

fn serialize_document(settings: &Settings) -> Result<Vec<u8>, SettingsError> {
    let stored = StoredSettingsV2::from(settings);
    let mut bytes = serde_json::to_vec_pretty(&stored).map_err(|_| SettingsError::Serialization)?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return Err(SettingsError::Serialization);
    }
    Ok(bytes)
}

impl From<&Settings> for StoredSettingsV2 {
    fn from(settings: &Settings) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            window: StoredWindowV1 {
                width: settings.window.width,
                height: settings.window.height,
                maximized: settings.window.maximized,
            },
            remembered_targets: settings
                .remembered_targets
                .iter()
                .map(|target| match target {
                    RememberedTarget::Address(address) => StoredTargetV2 {
                        address: Some(address.ip_addr().to_string()),
                        host: None,
                    },
                    RememberedTarget::Hostname(host) => StoredTargetV2 {
                        address: None,
                        host: Some(host.name().to_owned()),
                    },
                })
                .collect(),
            _retired_device_names: IgnoredAny,
        }
    }
}

impl TryFrom<StoredSettingsV2> for Settings {
    type Error = MalformedSettings;

    fn try_from(stored: StoredSettingsV2) -> Result<Self, Self::Error> {
        let window = WindowState::new(
            stored.window.width,
            stored.window.height,
            stored.window.maximized,
        )
        .map_err(|_| MalformedSettings::WindowState)?;

        if stored.remembered_targets.len() > MAX_REMEMBERED_TARGETS {
            return Err(MalformedSettings::TooManyTargets);
        }
        let mut remembered_targets = Vec::with_capacity(stored.remembered_targets.len());
        for stored_target in &stored.remembered_targets {
            let target = match (&stored_target.address, &stored_target.host) {
                (Some(address), None) => ExactDiscoveryTarget::parse(address)
                    .map(RememberedTarget::Address)
                    .map_err(|_| MalformedSettings::RememberedTarget)?,
                (None, Some(host)) => HostnameTarget::parse(host)
                    .map(RememberedTarget::Hostname)
                    .map_err(|_| MalformedSettings::RememberedTarget)?,
                _ => return Err(MalformedSettings::RememberedTarget),
            };
            if remembered_targets.contains(&target) {
                return Err(MalformedSettings::DuplicateTarget);
            }
            remembered_targets.push(target);
        }

        Ok(Self {
            window,
            remembered_targets,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use tempfile::TempDir;

    use super::*;

    fn test_store() -> (TempDir, SettingsStore) {
        let temporary = tempfile::tempdir().expect("test directory");
        let store = SettingsStore::new(temporary.path().join("balun"));
        (temporary, store)
    }

    fn target(last_octet: u8) -> ExactDiscoveryTarget {
        ExactDiscoveryTarget::parse(&format!("192.0.2.{last_octet}")).expect("valid target")
    }

    fn populated() -> Settings {
        let mut settings = Settings::default();
        settings.set_window(WindowState::new(1_600, 900, true).expect("valid window"));
        assert!(settings.remember_target(RememberedTarget::Address(target(1))));
        assert!(settings.remember_target(RememberedTarget::Address(target(2))));
        settings
    }

    fn write_raw(store: &SettingsStore, bytes: &[u8]) {
        fs::create_dir_all(store.directory()).expect("create directory");
        fs::write(store.path(), bytes).expect("write raw document");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn raw_bytes(store: &SettingsStore) -> Vec<u8> {
        fs::read(store.path()).expect("read raw document")
    }

    #[test]
    fn missing_file_loads_as_none() {
        let (_directory, store) = test_store();
        assert_eq!(store.load(), Ok(None));
    }

    #[test]
    fn save_creates_the_directory_and_round_trips() {
        let (_directory, store) = test_store();
        let settings = populated();

        store.save(&settings).expect("save");

        assert_eq!(store.load(), Ok(Some(settings)));
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_directory, store) = test_store();

        store.save(&populated()).expect("save");

        let mode = fs::metadata(store.path())
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn defaults_round_trip_and_match_the_window_constants() {
        let (_directory, store) = test_store();
        store.save(&Settings::default()).expect("save");

        let loaded = store.load().expect("load").expect("document");

        assert_eq!(loaded, Settings::default());
        assert_eq!(loaded.window().width(), DEFAULT_WINDOW_WIDTH);
        assert_eq!(loaded.window().height(), DEFAULT_WINDOW_HEIGHT);
        assert!(!loaded.window().maximized());
    }

    #[test]
    fn save_replaces_the_previous_document_and_leaves_no_temporaries() {
        let (_directory, store) = test_store();
        store.save(&Settings::default()).expect("first save");
        store.save(&populated()).expect("second save");

        let mut entries: Vec<_> = fs::read_dir(store.directory())
            .expect("read directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        entries.sort();
        assert_eq!(
            entries,
            vec![
                OsString::from(".settings.lock"),
                OsString::from(SETTINGS_FILE_NAME)
            ]
        );
        assert_eq!(store.load(), Ok(Some(populated())));
    }

    #[test]
    fn serialized_document_is_versioned_and_carries_no_endpoints_or_secrets() {
        let bytes = serialize_document(&populated()).expect("serialize");
        let text = std::str::from_utf8(&bytes).expect("utf-8");
        let value: serde_json::Value = serde_json::from_str(text).expect("json");

        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        let keys: Vec<_> = value
            .as_object()
            .expect("object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["remembered_targets", "schema_version", "window"]);
        assert_eq!(value["remembered_targets"][0]["address"], "192.0.2.1");
        let lowered = text.to_ascii_lowercase();
        for forbidden in ["http", "url", "deviceauth", "auth", "lineup", "5004"] {
            assert!(
                !lowered.contains(forbidden),
                "settings must not serialize {forbidden:?}: {text}"
            );
        }
    }

    #[test]
    fn minimal_current_schema_document_loads_defaults() {
        let (_directory, store) = test_store();
        write_raw(&store, b"{\"schema_version\":1}\n");

        assert_eq!(store.load(), Ok(Some(Settings::default())));
    }

    #[test]
    fn newer_schema_is_reported_and_left_untouched() {
        let (_directory, store) = test_store();
        let raw = b"{\"schema_version\":3,\"future\":{\"unknown\":true}}\n";
        write_raw(&store, raw);

        assert_eq!(
            store.load(),
            Err(SettingsError::UnsupportedSchema { found: 3 })
        );
        assert_eq!(raw_bytes(&store), raw);
    }

    #[test]
    fn malformed_documents_are_reported_and_left_untouched() {
        let cases: [(&[u8], MalformedSettings); 7] = [
            (b"{\"schema_version\":0}", MalformedSettings::ZeroSchemaVersion),
            (b"{\"schema_version\":1,\"extra\":1}", MalformedSettings::Json),
            (b"{\"schema_version\":1,", MalformedSettings::Json),
            (b"not json", MalformedSettings::Json),
            (
                b"{\"schema_version\":1,\"window\":{\"width\":10,\"height\":700,\"maximized\":false}}",
                MalformedSettings::WindowState,
            ),
            (
                b"{\"schema_version\":1,\"remembered_targets\":[{\"address\":\"tuner.example\"}]}",
                MalformedSettings::RememberedTarget,
            ),
            (
                b"{\"schema_version\":1,\"remembered_targets\":[{\"address\":\"192.0.2.1\"},{\"address\":\"192.0.2.1\"}]}",
                MalformedSettings::DuplicateTarget,
            ),
        ];

        for (raw, expected) in cases {
            let (_directory, store) = test_store();
            write_raw(&store, raw);
            assert_eq!(
                store.load(),
                Err(SettingsError::Malformed(expected)),
                "document {:?}",
                String::from_utf8_lossy(raw)
            );
            assert_eq!(raw_bytes(&store), raw);
        }
    }

    #[test]
    fn too_many_targets_are_rejected() {
        let (_directory, store) = test_store();
        let targets: Vec<String> = (1..=MAX_REMEMBERED_TARGETS + 1)
            .map(|index| format!("{{\"address\":\"10.0.{}.{}\"}}", index / 256, index % 256))
            .collect();
        write_raw(
            &store,
            format!(
                "{{\"schema_version\":1,\"remembered_targets\":[{}]}}",
                targets.join(",")
            )
            .as_bytes(),
        );
        assert_eq!(
            store.load(),
            Err(SettingsError::Malformed(MalformedSettings::TooManyTargets))
        );
    }

    #[test]
    fn oversized_file_is_rejected_without_reading_it() {
        let (_directory, store) = test_store();
        let mut raw = b"{\"schema_version\":1,\"device_names\":{}".to_vec();
        raw.resize(
            usize::try_from(MAX_SETTINGS_BYTES).expect("usize") + 1,
            b' ',
        );
        write_raw(&store, &raw);

        assert_eq!(store.load(), Err(SettingsError::TooLarge));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_settings_file_is_rejected() {
        let (directory, store) = test_store();
        let real = directory.path().join("real.json");
        fs::write(&real, b"{\"schema_version\":1}\n").expect("write real file");
        fs::create_dir_all(store.directory()).expect("create directory");
        std::os::unix::fs::symlink(&real, store.path()).expect("create symlink");

        assert_eq!(store.load(), Err(SettingsError::Symlink));
    }

    #[test]
    fn directory_in_place_of_the_file_is_rejected() {
        let (_directory, store) = test_store();
        fs::create_dir_all(store.path()).expect("create directory at file path");

        assert_eq!(store.load(), Err(SettingsError::NotRegularFile));
    }

    #[test]
    fn remembered_targets_deduplicate_reorder_and_evict_oldest() {
        let mut settings = Settings::default();
        for index in 1..=MAX_REMEMBERED_TARGETS {
            assert!(settings.remember_target(RememberedTarget::Address(target(
                u8::try_from(index).expect("u8")
            ))));
        }
        assert_eq!(settings.remembered_targets().len(), MAX_REMEMBERED_TARGETS);
        assert!(
            !settings.remember_target(RememberedTarget::Address(target(32))),
            "repeating the newest is inert"
        );

        assert!(
            settings.remember_target(RememberedTarget::Address(target(1))),
            "an older repeat moves to the end"
        );
        assert_eq!(
            settings.remembered_targets().last(),
            Some(&RememberedTarget::Address(target(1)))
        );
        assert_eq!(settings.remembered_targets().len(), MAX_REMEMBERED_TARGETS);

        assert!(settings.remember_target(RememberedTarget::Address(target(33))));
        assert_eq!(settings.remembered_targets().len(), MAX_REMEMBERED_TARGETS);
        assert!(
            !settings
                .remembered_targets()
                .contains(&RememberedTarget::Address(target(2))),
            "the oldest entry is evicted"
        );

        assert!(settings.forget_target(&RememberedTarget::Address(target(33))));
        assert!(!settings.forget_target(&RememberedTarget::Address(target(33))));
    }

    #[test]
    fn window_state_validates_its_range() {
        assert!(WindowState::new(MIN_WINDOW_DIMENSION, MIN_WINDOW_DIMENSION, false).is_ok());
        assert!(WindowState::new(MAX_WINDOW_DIMENSION, MAX_WINDOW_DIMENSION, true).is_ok());
        assert_eq!(
            WindowState::new(MIN_WINDOW_DIMENSION - 1, 700, false),
            Err(InvalidWindowState::DimensionOutOfRange)
        );
        assert_eq!(
            WindowState::new(1_200, MAX_WINDOW_DIMENSION + 1, false),
            Err(InvalidWindowState::DimensionOutOfRange)
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn default_directory_prefers_absolute_xdg_config_home() {
        let env = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some(OsString::from("/tmp/xdg")),
            "HOME" => Some(OsString::from("/home/user")),
            _ => None,
        };
        assert_eq!(
            default_directory_from(env),
            Some(PathBuf::from("/tmp/xdg/balun"))
        );

        let relative = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some(OsString::from("relative/config")),
            "HOME" => Some(OsString::from("/home/user")),
            _ => None,
        };
        assert_eq!(
            default_directory_from(relative),
            Some(PathBuf::from("/home/user/.config/balun"))
        );

        assert_eq!(default_directory_from(|_| None), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn default_directory_uses_application_support() {
        let env = |key: &str| (key == "HOME").then(|| OsString::from("/Users/user"));
        assert_eq!(
            default_directory_from(env),
            Some(PathBuf::from(
                "/Users/user/Library/Application Support/Balun"
            ))
        );
        assert_eq!(default_directory_from(|_| None), None);
    }

    #[cfg(windows)]
    #[test]
    fn default_directory_uses_appdata() {
        let env = |key: &str| {
            (key == "APPDATA").then(|| OsString::from(r"C:\Users\user\AppData\Roaming"))
        };
        assert_eq!(
            default_directory_from(env),
            Some(PathBuf::from(r"C:\Users\user\AppData\Roaming\Balun"))
        );
        assert_eq!(default_directory_from(|_| None), None);
    }

    #[test]
    fn errors_are_path_free() {
        let error = SettingsError::Io {
            operation: SettingsOperation::Publish,
            kind: io::ErrorKind::PermissionDenied,
        };
        let text = error.to_string();
        assert!(text.contains("replacing the settings file"));
        assert!(!text.contains('/') && !text.contains('\\'));

        let unused = IpAddr::V4(Ipv4Addr::LOCALHOST);
        assert!(
            !SettingsError::Symlink
                .to_string()
                .contains(&unused.to_string())
        );
    }

    #[test]
    fn version_one_documents_migrate_and_are_rewritten_as_version_two() {
        let (_directory, store) = test_store();
        write_raw(
            &store,
            b"{\"schema_version\":1,\"remembered_targets\":[{\"address\":\"192.0.2.1\"}]}\n",
        );

        let loaded = store.load().expect("load").expect("document");
        assert_eq!(
            loaded.remembered_targets(),
            &[RememberedTarget::Address(target(1))]
        );

        store.save(&loaded).expect("save");
        let value: serde_json::Value = serde_json::from_slice(&raw_bytes(&store)).expect("json");
        assert_eq!(value["schema_version"], SCHEMA_VERSION);
        assert_eq!(value["remembered_targets"][0]["address"], "192.0.2.1");
        assert!(value["remembered_targets"][0].get("host").is_none());
    }

    #[test]
    fn retired_device_names_from_earlier_builds_load_and_are_not_rewritten() {
        let mut expected = Settings::default();
        assert!(expected.remember_target(RememberedTarget::Address(target(1))));
        for raw in [
            &b"{\"schema_version\":1,\"remembered_targets\":[{\"address\":\"192.0.2.1\"}],\"device_names\":{}}"[..],
            b"{\"schema_version\":2,\"remembered_targets\":[{\"address\":\"192.0.2.1\"}],\"device_names\":{\"105A1232\":\"Living room\"}}",
            b"{\"schema_version\":2,\"remembered_targets\":[{\"address\":\"192.0.2.1\"}],\"device_names\":{\"nothex!\":\"Bad\\u0007name\"}}",
        ] {
            let (_directory, store) = test_store();
            write_raw(&store, raw);
            let loaded = store.load().expect("load").expect("document");
            assert_eq!(loaded, expected, "{}", String::from_utf8_lossy(raw));
            store.save(&loaded).expect("save");
            let value: serde_json::Value =
                serde_json::from_slice(&raw_bytes(&store)).expect("json");
            assert!(value.get("device_names").is_none());
            assert_eq!(store.load(), Ok(Some(expected.clone())));
        }
    }

    #[test]
    fn remembered_hostnames_round_trip_and_normalize() {
        let (_directory, store) = test_store();
        let host = HostnameTarget::parse("tuner.example").expect("valid hostname");
        let mut settings = Settings::default();
        assert!(settings.remember_target(RememberedTarget::Hostname(host.clone())));
        assert!(settings.remember_target(RememberedTarget::Address(target(1))));
        store.save(&settings).expect("save");

        let value: serde_json::Value = serde_json::from_slice(&raw_bytes(&store)).expect("json");
        assert_eq!(value["remembered_targets"][0]["host"], "tuner.example");
        assert!(value["remembered_targets"][0].get("address").is_none());
        assert_eq!(store.load(), Ok(Some(settings)));

        write_raw(
            &store,
            b"{\"schema_version\":2,\"remembered_targets\":[{\"host\":\"Tuner.Example.\"}]}\n",
        );
        let loaded = store.load().expect("load").expect("document");
        assert_eq!(
            loaded.remembered_targets(),
            &[RememberedTarget::Hostname(host)]
        );

        for raw in [
            &b"{\"schema_version\":2,\"remembered_targets\":[{\"address\":\"192.0.2.1\",\"host\":\"t.example\"}]}"[..],
            b"{\"schema_version\":2,\"remembered_targets\":[{}]}",
            b"{\"schema_version\":2,\"remembered_targets\":[{\"host\":\"192.0.2.1\"}]}",
            b"{\"schema_version\":2,\"remembered_targets\":[{\"host\":\"http://t.example\"}]}",
        ] {
            write_raw(&store, raw);
            assert_eq!(
                store.load(),
                Err(SettingsError::Malformed(MalformedSettings::RememberedTarget)),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }
}
