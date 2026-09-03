//! Versioned, atomic, GTK-free persistence of Balun's user settings.
//!
//! The settings file holds only reviewed preferences: remembered exact-address
//! discovery targets, user-assigned device names, and window state. It never
//! holds credentials, `DeviceAuth`, stream URLs, lineups, or incidental
//! network topology, and the types here cannot represent them.
//!
//! Reads fail closed. A malformed, oversized, symlinked, or newer-schema file
//! is reported with a fixed, path-free error and left untouched, so a later
//! save cannot destroy settings written by a newer Balun or edited by hand.
//! Writes go through a temporary sibling that is flushed and renamed over the
//! previous file, so a crash never leaves a partial document. On Unix the
//! file is readable and writable by its owner only.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::discovery::{ExactDiscoveryTarget, HostnameTarget};
use crate::domain::DeviceId;

/// Schema version written by this build and the newest version it can read.
pub const SCHEMA_VERSION: u32 = 2;
/// File name inside the settings directory.
pub const SETTINGS_FILE_NAME: &str = "settings.json";
/// Largest settings document that will be read or written.
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
/// Most remembered exact-address targets; matches the per-session probe cap.
pub const MAX_REMEMBERED_TARGETS: usize = 32;
/// Most user-assigned device names.
pub const MAX_DEVICE_NAMES: usize = 64;
/// Longest user-assigned device name in bytes.
pub const MAX_DEVICE_NAME_BYTES: usize = 128;
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

/// Why a user-assigned device name was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidDeviceName {
    #[error("device names are limited to {MAX_DEVICE_NAME_BYTES} bytes")]
    TooLong,
    #[error("device names cannot contain control characters")]
    ControlCharacter,
    #[error("at most {MAX_DEVICE_NAMES} devices can be named")]
    TooMany,
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
    device_names: BTreeMap<DeviceId, String>,
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

    /// The user-assigned name for a device, if any.
    #[must_use]
    pub fn device_name(&self, device: DeviceId) -> Option<&str> {
        self.device_names.get(&device).map(String::as_str)
    }

    /// Assign a name to a device. Surrounding whitespace is trimmed and an
    /// empty name clears the assignment. Returns whether anything changed.
    pub fn set_device_name(
        &mut self,
        device: DeviceId,
        name: &str,
    ) -> Result<bool, InvalidDeviceName> {
        let Some(name) = validate_device_name(name)? else {
            return Ok(self.clear_device_name(device));
        };
        if self.device_names.get(&device).map(String::as_str) == Some(name) {
            return Ok(false);
        }
        if !self.device_names.contains_key(&device) && self.device_names.len() >= MAX_DEVICE_NAMES {
            return Err(InvalidDeviceName::TooMany);
        }
        self.device_names.insert(device, name.to_owned());
        Ok(true)
    }

    /// Remove a user-assigned device name; returns whether one existed.
    pub fn clear_device_name(&mut self, device: DeviceId) -> bool {
        self.device_names.remove(&device).is_some()
    }

    /// Number of user-assigned device names.
    #[must_use]
    pub fn device_name_count(&self) -> usize {
        self.device_names.len()
    }
}

fn validate_device_name(name: &str) -> Result<Option<&str>, InvalidDeviceName> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.len() > MAX_DEVICE_NAME_BYTES {
        return Err(InvalidDeviceName::TooLong);
    }
    if name.chars().any(char::is_control) {
        return Err(InvalidDeviceName::ControlCharacter);
    }
    Ok(Some(name))
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
    #[error("a device identifier is invalid")]
    DeviceId,
    #[error("a device identifier is listed twice")]
    DuplicateDeviceId,
    #[error("a device name is invalid")]
    DeviceName,
    #[error("more than {MAX_DEVICE_NAMES} device names")]
    TooManyDeviceNames,
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

/// Reads and atomically writes one settings document in a directory.
#[derive(Clone, Debug)]
pub struct SettingsStore {
    directory: PathBuf,
}

impl SettingsStore {
    /// Use an explicit directory; it is created on the first save.
    #[must_use]
    pub const fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    /// Use the platform default directory, if the environment names one.
    #[must_use]
    pub fn at_default_location() -> Option<Self> {
        default_directory().map(Self::new)
    }

    /// The directory holding the settings file.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn path(&self) -> PathBuf {
        self.directory.join(SETTINGS_FILE_NAME)
    }

    /// Load the settings, or `Ok(None)` when no file has been written yet.
    ///
    /// Every failure leaves the file untouched.
    pub fn load(&self) -> Result<Option<Settings>, SettingsError> {
        let path = self.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SettingsError::io(SettingsOperation::Inspect, &error)),
        };
        if metadata.file_type().is_symlink() {
            return Err(SettingsError::Symlink);
        }
        if !metadata.is_file() {
            return Err(SettingsError::NotRegularFile);
        }
        if metadata.len() > MAX_SETTINGS_BYTES {
            return Err(SettingsError::TooLarge);
        }

        let file = fs::File::open(&path)
            .map_err(|error| SettingsError::io(SettingsOperation::Read, &error))?;
        let mut bytes = Vec::new();
        file.take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| SettingsError::io(SettingsOperation::Read, &error))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(SettingsError::TooLarge);
        }

        parse_document(&bytes).map(Some)
    }

    /// Atomically replace the settings file, creating the directory first.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        let bytes = serialize_document(settings)?;
        fs::create_dir_all(&self.directory)
            .map_err(|error| SettingsError::io(SettingsOperation::CreateDirectory, &error))?;

        let mut builder = tempfile::Builder::new();
        builder.prefix(TEMPORARY_PREFIX).suffix(TEMPORARY_SUFFIX);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Remembered addresses and names belong to this user alone; the
            // published file keeps the mode of the temporary it was renamed from.
            builder.permissions(fs::Permissions::from_mode(0o600));
        }
        let mut temporary = builder
            .tempfile_in(&self.directory)
            .map_err(|error| SettingsError::io(SettingsOperation::CreateTemporary, &error))?;
        temporary
            .write_all(&bytes)
            .and_then(|()| temporary.flush())
            .map_err(|error| SettingsError::io(SettingsOperation::Write, &error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| SettingsError::io(SettingsOperation::Sync, &error))?;
        temporary
            .persist(self.path())
            .map_err(|error| SettingsError::io(SettingsOperation::Publish, &error.error))?;
        sync_directory(&self.directory)
            .map_err(|error| SettingsError::io(SettingsOperation::Sync, &error))
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    fs::File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> io::Result<()> {
    // The standard library exposes no reliable directory flush here; the
    // renamed file itself has already been flushed.
    Ok(())
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
    #[serde(default)]
    device_names: BTreeMap<String, String>,
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
    #[serde(default)]
    device_names: BTreeMap<String, String>,
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
            device_names: stored.device_names,
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
            device_names: settings
                .device_names
                .iter()
                .map(|(device, name)| (device.to_string(), name.clone()))
                .collect(),
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

        if stored.device_names.len() > MAX_DEVICE_NAMES {
            return Err(MalformedSettings::TooManyDeviceNames);
        }
        let mut device_names = BTreeMap::new();
        for (key, name) in &stored.device_names {
            let device = parse_device_id(key).ok_or(MalformedSettings::DeviceId)?;
            let name = validate_device_name(name)
                .ok()
                .flatten()
                .filter(|trimmed| *trimmed == name)
                .ok_or(MalformedSettings::DeviceName)?;
            // The key parser accepts either hexadecimal case, so two stored
            // keys can name one device; merging them would drop a name.
            if device_names.insert(device, name.to_owned()).is_some() {
                return Err(MalformedSettings::DuplicateDeviceId);
            }
        }

        Ok(Self {
            window,
            remembered_targets,
            device_names,
        })
    }
}

/// Parse the eight-hex-digit form produced by `DeviceId`'s `Display`.
fn parse_device_id(text: &str) -> Option<DeviceId> {
    if text.len() != 8 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let raw = u32::from_str_radix(text, 16).ok()?;
    DeviceId::new(raw).ok()
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

    fn device(raw: u32) -> DeviceId {
        DeviceId::new(raw).expect("valid device id")
    }

    fn populated() -> Settings {
        let mut settings = Settings::default();
        settings.set_window(WindowState::new(1_600, 900, true).expect("valid window"));
        assert!(settings.remember_target(RememberedTarget::Address(target(1))));
        assert!(settings.remember_target(RememberedTarget::Address(target(2))));
        assert!(
            settings
                .set_device_name(device(0x105A_1232), "Living room")
                .expect("valid name")
        );
        settings
    }

    fn write_raw(store: &SettingsStore, bytes: &[u8]) {
        fs::create_dir_all(store.directory()).expect("create directory");
        fs::write(store.path(), bytes).expect("write raw document");
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

        let entries: Vec<_> = fs::read_dir(store.directory())
            .expect("read directory")
            .map(|entry| entry.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![OsString::from(SETTINGS_FILE_NAME)]);
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
        assert_eq!(
            keys,
            [
                "device_names",
                "remembered_targets",
                "schema_version",
                "window"
            ]
        );
        assert_eq!(value["remembered_targets"][0]["address"], "192.0.2.1");
        assert_eq!(value["device_names"]["105A1232"], "Living room");
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
        let cases: [(&[u8], MalformedSettings); 10] = [
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
            (
                b"{\"schema_version\":1,\"device_names\":{\"nothex!\":\"Name\"}}",
                MalformedSettings::DeviceId,
            ),
            (
                b"{\"schema_version\":1,\"device_names\":{\"105A1232\":\"Bad\\u0007name\"}}",
                MalformedSettings::DeviceName,
            ),
            (
                b"{\"schema_version\":1,\"device_names\":{\"105A1232\":\"One\",\"105a1232\":\"Two\"}}",
                MalformedSettings::DuplicateDeviceId,
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
    fn too_many_targets_or_names_are_rejected() {
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

        let names: Vec<String> = (0..=MAX_DEVICE_NAMES)
            .map(|index| {
                let mut raw = 0x1000_0000_u32 + (index as u32) * 16;
                while DeviceId::new(raw).is_err() {
                    raw += 1;
                }
                format!("\"{}\":\"Name\"", device(raw))
            })
            .collect();
        write_raw(
            &store,
            format!(
                "{{\"schema_version\":1,\"device_names\":{{{}}}}}",
                names.join(",")
            )
            .as_bytes(),
        );
        assert_eq!(
            store.load(),
            Err(SettingsError::Malformed(
                MalformedSettings::TooManyDeviceNames
            ))
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
    fn device_names_are_trimmed_bounded_and_clearable() {
        let mut settings = Settings::default();
        let id = device(0x105A_1232);

        assert!(settings.set_device_name(id, "  Basement  ").expect("valid"));
        assert_eq!(settings.device_name(id), Some("Basement"));
        assert!(!settings.set_device_name(id, "Basement").expect("valid"));
        assert_eq!(
            settings.set_device_name(id, "x".repeat(MAX_DEVICE_NAME_BYTES + 1).as_str()),
            Err(InvalidDeviceName::TooLong)
        );
        assert_eq!(
            settings.set_device_name(id, "tab\tname"),
            Err(InvalidDeviceName::ControlCharacter)
        );
        assert!(settings.set_device_name(id, "   ").expect("blank clears"));
        assert_eq!(settings.device_name(id), None);
        assert!(!settings.clear_device_name(id));

        let mut raw = 0x1000_0000_u32;
        for _ in 0..MAX_DEVICE_NAMES {
            while DeviceId::new(raw).is_err() {
                raw += 1;
            }
            settings
                .set_device_name(device(raw), "Name")
                .expect("within limit");
            raw += 1;
        }
        while DeviceId::new(raw).is_err() {
            raw += 1;
        }
        assert_eq!(
            settings.set_device_name(device(raw), "Name"),
            Err(InvalidDeviceName::TooMany)
        );
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

    #[test]
    fn device_id_text_round_trips_through_display() {
        let id = device(0x105A_1232);
        assert_eq!(parse_device_id(&id.to_string()), Some(id));
        assert_eq!(parse_device_id("1051123"), None);
        assert_eq!(parse_device_id("00000000"), None);
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
