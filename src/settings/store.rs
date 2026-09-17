//! Pinned private-profile transactions. Native I/O itself is not cancellable.

use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use cap_fs_ext::{
    FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirBuilder, File, Metadata, OpenOptions};

use super::{
    MAX_SETTINGS_BYTES, SETTINGS_FILE_NAME, Settings, SettingsError, SettingsOperation,
    TEMPORARY_PREFIX, TEMPORARY_SUFFIX, parse_document, serialize_document,
};

const LOCK_NAME: &str = ".settings.lock";

/// A private settings profile pinned at its first access and shared by clones.
///
/// The existing configuration parent is trusted. Documents and siblings are
/// opened relative to the pinned directory, without following leaf aliases.
/// Windows inherits the parent DACL; this type does not attest that ACL.
#[derive(Clone)]
pub struct SettingsStore {
    directory: PathBuf,
    profile: Arc<OnceLock<Result<Profile, SettingsError>>>,
}

impl std::fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsStore").finish_non_exhaustive()
    }
}

struct Profile {
    directory: Dir,
    // Retain parent handles too. On Windows they deny directory rename/delete,
    // including the pathname fallback used by cap-std's rename implementation.
    _parents: Vec<Dir>,
    #[cfg(test)]
    hooks: TestHooks,
}

impl SettingsStore {
    /// Use an absolute directory, created privately on the first access.
    #[must_use]
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            profile: Arc::new(OnceLock::new()),
        }
    }

    /// Use the platform default directory, if the environment names one.
    #[must_use]
    pub fn at_default_location() -> Option<Self> {
        super::default_directory().map(Self::new)
    }

    /// The originally selected profile path, not a capability for I/O.
    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[cfg(test)]
    pub(super) fn path(&self) -> PathBuf {
        self.directory.join(SETTINGS_FILE_NAME)
    }

    fn profile(&self) -> Result<&Profile, SettingsError> {
        self.profile
            .get_or_init(|| Profile::open(&self.directory))
            .as_ref()
            .map_err(|error| *error)
    }

    /// Load under the cooperative transaction lock; leave invalid bytes intact.
    pub fn load(&self) -> Result<Option<Settings>, SettingsError> {
        let profile = self.profile()?;
        let lock = profile.lock()?;
        let (settings, _) = profile.read()?;
        profile.check_lock(&lock)?;
        Ok(settings)
    }

    /// Recheck the current schema under lock, then publish a complete document.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        self.save_unless_cancelled(settings, &AtomicBool::new(false))
    }

    /// As [`Self::save`], with cancellation checkpoints before publication.
    ///
    /// A publication already inside the OS can finish after cancellation. The
    /// caller must not describe a timeout as proof that nothing was written.
    pub fn save_unless_cancelled(
        &self,
        settings: &Settings,
        cancelled: &AtomicBool,
    ) -> Result<(), SettingsError> {
        check_cancelled(cancelled)?;
        let bytes = serialize_document(settings)?;
        let profile = self.profile()?;
        let lock = profile.lock()?;
        let (_, previous) = profile.read()?;
        check_cancelled(cancelled)?;
        let mut temporary = profile.temporary()?;
        #[cfg(test)]
        profile.hooks.run(TestStage::Write);
        check_cancelled(cancelled)?;
        temporary
            .file
            .write_all(&bytes)
            .and_then(|()| temporary.file.flush())
            .map_err(|e| SettingsError::io(SettingsOperation::Write, &e))?;
        #[cfg(test)]
        profile.hooks.run(TestStage::Sync);
        check_cancelled(cancelled)?;
        temporary
            .file
            .sync_all()
            .map_err(|e| SettingsError::io(SettingsOperation::Sync, &e))?;
        check_cancelled(cancelled)?;
        profile.check_lock(&lock)?;
        #[cfg(test)]
        profile.hooks.run(TestStage::Publish);
        let current = profile.inspect(SETTINGS_FILE_NAME)?;
        if !same_optional_snapshot(previous.as_ref(), current.as_ref()) {
            return Err(SettingsError::Changed);
        }
        temporary.check_identity()?;
        check_cancelled(cancelled)?;
        profile
            .directory
            .rename(&temporary.name, &profile.directory, SETTINGS_FILE_NAME)
            .map_err(|e| SettingsError::io(SettingsOperation::Publish, &e))?;
        temporary.published = true;
        let published = profile
            .inspect(SETTINGS_FILE_NAME)?
            .ok_or(SettingsError::Changed)?;
        let written = temporary
            .file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        if !same_identity(&published, &written) {
            return Err(SettingsError::Changed);
        }
        #[cfg(unix)]
        profile
            .directory
            .try_clone()
            .and_then(|dir| dir.into_std_file().sync_all())
            .map_err(|e| SettingsError::io(SettingsOperation::Sync, &e))?;
        Ok(())
    }
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), SettingsError> {
    if cancelled.load(Ordering::Acquire) {
        Err(SettingsError::Cancelled)
    } else {
        Ok(())
    }
}

fn options(write: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        // Stable read snapshots and lock files may not be replaced while open.
        options.share_mode(FILE_SHARE_READ);
    }
    options
}

fn open_directory(parent: &Dir, name: &Path) -> Result<Dir, SettingsError> {
    let mut opts = options(false);
    opts.maybe_dir(true);
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
    }
    let file = parent
        .open_with(name, &opts)
        .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
    let metadata = file
        .metadata()
        .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
    check_directory(&metadata, true)?;
    Ok(Dir::from_std_file(file.into_std()))
}

impl Profile {
    fn open(path: &Path) -> Result<Self, SettingsError> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(SettingsError::InvalidDirectory);
        }
        let mut missing = vec![
            path.file_name()
                .ok_or(SettingsError::InvalidDirectory)?
                .to_os_string(),
        ];
        let mut existing = path.parent().ok_or(SettingsError::InvalidDirectory)?;
        let mut initial = options(false);
        // Aliases in the account-selected existing parent are trusted only at
        // this initial acquisition. Every descendant open is no-follow.
        initial.follow(FollowSymlinks::Yes).maybe_dir(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
            initial.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
        let parent_file = loop {
            match File::open_ambient_with(existing, &initial, ambient_authority()) {
                Ok(file) => break file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    missing.push(
                        existing
                            .file_name()
                            .ok_or(SettingsError::InvalidDirectory)?
                            .to_os_string(),
                    );
                    existing = existing.parent().ok_or(SettingsError::InvalidDirectory)?;
                }
                Err(error) => return Err(SettingsError::io(SettingsOperation::Inspect, &error)),
            }
        };
        check_directory(
            &parent_file
                .metadata()
                .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?,
            false,
        )?;
        let mut parent = Dir::from_std_file(parent_file.into_std());
        let mut parents = Vec::new();
        for name in missing.into_iter().rev() {
            let builder = &mut DirBuilder::new();
            #[cfg(unix)]
            {
                use cap_std::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match parent.create_dir_with(&name, builder) {
                Ok(()) => (),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => {
                    return Err(SettingsError::io(
                        SettingsOperation::CreateDirectory,
                        &error,
                    ));
                }
            }
            let next = open_directory(&parent, Path::new(&name))?;
            // Older Balun created owned 0755 profile directories. Tightening
            // read/search permission is safe after rejecting foreign owners and
            // any group/other write permission. No file bytes are changed.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                next.try_clone()
                    .and_then(|dir| {
                        dir.into_std_file()
                            .set_permissions(std::fs::Permissions::from_mode(0o700))
                    })
                    .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
            }
            parents.push(parent);
            parent = next;
        }
        Ok(Self {
            directory: parent,
            _parents: parents,
            #[cfg(test)]
            hooks: TestHooks::default(),
        })
    }

    fn inspect(&self, name: &str) -> Result<Option<Metadata>, SettingsError> {
        match self.directory.symlink_metadata(name) {
            Ok(metadata) => {
                check_file(&metadata)?;
                Ok(Some(metadata))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(SettingsError::io(SettingsOperation::Inspect, &error)),
        }
    }

    fn lock(&self) -> Result<File, SettingsError> {
        check_directory(
            &self
                .directory
                .dir_metadata()
                .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?,
            true,
        )?;
        #[cfg(unix)]
        {
            use cap_std::fs::MetadataExt;
            if self
                .directory
                .dir_metadata()
                .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?
                .mode()
                & 0o077
                != 0
            {
                return Err(SettingsError::Permissions);
            }
        }
        let mut opts = options(true);
        opts.create(true);
        #[cfg(windows)]
        {
            use cap_std::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
            opts.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
        let file = self
            .directory
            .open_with(LOCK_NAME, &opts)
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        check_file(
            &file
                .metadata()
                .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?,
        )?;
        let locking = file.into_std();
        locking.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => SettingsError::Busy,
            std::fs::TryLockError::Error(error) => {
                SettingsError::io(SettingsOperation::Inspect, &error)
            }
        })?;
        // Retain the exact handle that acquired the lock for the transaction.
        let file = File::from_std(locking);
        self.check_lock(&file)?;
        Ok(file)
    }

    fn check_lock(&self, file: &File) -> Result<(), SettingsError> {
        let expected = file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        let actual = self.inspect(LOCK_NAME)?.ok_or(SettingsError::Changed)?;
        if same_identity(&expected, &actual) {
            Ok(())
        } else {
            Err(SettingsError::Changed)
        }
    }

    fn read(&self) -> Result<(Option<Settings>, Option<Metadata>), SettingsError> {
        let Some(before) = self.inspect(SETTINGS_FILE_NAME)? else {
            return Ok((None, None));
        };
        #[cfg(test)]
        self.hooks.run(TestStage::ReadOpen);
        let file = self
            .directory
            .open_with(SETTINGS_FILE_NAME, &options(false))
            .map_err(|e| SettingsError::io(SettingsOperation::Read, &e))?;
        let opened = file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        check_file(&opened)?;
        if !same_snapshot(&before, &opened) {
            return Err(SettingsError::Changed);
        }
        let mut bytes = Vec::new();
        (&file)
            .take(MAX_SETTINGS_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| SettingsError::io(SettingsOperation::Read, &e))?;
        if bytes.len() as u64 > MAX_SETTINGS_BYTES {
            return Err(SettingsError::TooLarge);
        }
        let after = file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        let named = self
            .inspect(SETTINGS_FILE_NAME)?
            .ok_or(SettingsError::Changed)?;
        if !same_snapshot(&opened, &after) || !same_snapshot(&after, &named) {
            return Err(SettingsError::Changed);
        }
        Ok((Some(parse_document(&bytes)?), Some(after)))
    }

    fn temporary(&self) -> Result<Temporary<'_>, SettingsError> {
        for _ in 0..8 {
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| SettingsError::Io {
                operation: SettingsOperation::CreateTemporary,
                kind: io::ErrorKind::Other,
            })?;
            let name = format!(
                "{TEMPORARY_PREFIX}{}{TEMPORARY_SUFFIX}",
                random
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let mut opts = options(true);
            opts.create_new(true);
            #[cfg(windows)]
            {
                use cap_std::fs::OpenOptionsExt;
                use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_DELETE, FILE_SHARE_READ};
                opts.share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE);
            }
            match self.directory.open_with(&name, &opts) {
                Ok(file) => {
                    return Ok(Temporary {
                        directory: &self.directory,
                        file,
                        name,
                        published: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => (),
                Err(error) => {
                    return Err(SettingsError::io(
                        SettingsOperation::CreateTemporary,
                        &error,
                    ));
                }
            }
        }
        Err(SettingsError::Busy)
    }
}

struct Temporary<'a> {
    directory: &'a Dir,
    file: File,
    name: String,
    published: bool,
}

impl Temporary<'_> {
    fn check_identity(&self) -> Result<(), SettingsError> {
        let opened = self
            .file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        let named = self
            .directory
            .symlink_metadata(&self.name)
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        check_file(&opened)?;
        check_file(&named)?;
        if same_snapshot(&opened, &named) {
            Ok(())
        } else {
            Err(SettingsError::Changed)
        }
    }
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if !self.published {
            let _ = self.directory.remove_file(&self.name);
        }
    }
}

fn check_directory(metadata: &Metadata, owned: bool) -> Result<(), SettingsError> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() || is_reparse(metadata) {
        return Err(SettingsError::InvalidDirectory);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        let owner = rustix::process::geteuid().as_raw();
        if metadata.mode() & 0o022 != 0 || (owned && metadata.uid() != owner) {
            return Err(SettingsError::Permissions);
        }
    }
    #[cfg(not(unix))]
    let _ = owned;
    Ok(())
}

fn check_file(metadata: &Metadata) -> Result<(), SettingsError> {
    if metadata.file_type().is_symlink() || is_reparse(metadata) {
        return Err(SettingsError::Symlink);
    }
    if !metadata.is_file() {
        return Err(SettingsError::NotRegularFile);
    }
    if metadata.nlink() != 1 {
        return Err(SettingsError::HardLink);
    }
    if metadata.len() > MAX_SETTINGS_BYTES {
        return Err(SettingsError::TooLarge);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(SettingsError::Permissions);
        }
    }
    Ok(())
}

fn is_reparse(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

fn same_identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}
fn same_snapshot(a: &Metadata, b: &Metadata) -> bool {
    same_identity(a, b) && a.len() == b.len() && a.modified().ok() == b.modified().ok()
}
fn same_optional_snapshot(a: Option<&Metadata>, b: Option<&Metadata>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_snapshot(a, b),
        _ => false,
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum TestStage {
    ReadOpen,
    Write,
    Sync,
    Publish,
}

#[cfg(test)]
type TestHook = (TestStage, Box<dyn FnOnce() + Send>);

#[cfg(test)]
#[derive(Default)]
struct TestHooks(std::sync::Mutex<Option<TestHook>>);

#[cfg(test)]
impl TestHooks {
    fn run(&self, stage: TestStage) {
        let callback = {
            let mut hook = self.0.lock().unwrap();
            if hook.as_ref().is_some_and(|(point, _)| *point == stage) {
                hook.take().map(|(_, callback)| callback)
            } else {
                None
            }
        };
        if let Some(callback) = callback {
            callback();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(root.path().join("profile"));
        store.save(&Settings::default()).unwrap();
        (root, store)
    }

    fn hook(store: &SettingsStore, stage: TestStage, callback: impl FnOnce() + Send + 'static) {
        *store.profile().unwrap().hooks.0.lock().unwrap() = Some((stage, Box::new(callback)));
    }

    #[test]
    fn newer_or_malformed_document_after_startup_is_never_overwritten() {
        let (_root, store) = store();
        assert!(store.load().unwrap().is_some());
        for bytes in [
            b"{\"schema_version\":99,\"future\":true}".as_slice(),
            b"malformed-secret-marker",
        ] {
            fs::write(store.path(), bytes).unwrap();
            assert!(store.save(&Settings::default()).is_err());
            assert_eq!(fs::read(store.path()).unwrap(), bytes);
        }
    }

    #[test]
    fn transactions_are_exclusive_and_keep_a_stable_lock_identity() {
        let (_root, store) = store();
        let profile = store.profile().unwrap();
        let lock = profile.lock().unwrap();
        let other = SettingsStore::new(store.directory().to_owned());
        assert_eq!(other.save(&Settings::default()), Err(SettingsError::Busy));
        drop(lock);
        other.save(&Settings::default()).unwrap();
        store.save(&Settings::default()).unwrap();
        assert!(store.directory().join(LOCK_NAME).exists());
    }

    #[test]
    fn hard_linked_settings_or_lock_are_rejected() {
        let (root, store) = store();
        fs::hard_link(store.path(), root.path().join("settings-alias")).unwrap();
        assert_eq!(store.load(), Err(SettingsError::HardLink));
        fs::remove_file(root.path().join("settings-alias")).unwrap();
        fs::hard_link(
            store.directory().join(LOCK_NAME),
            root.path().join("lock-alias"),
        )
        .unwrap();
        assert_eq!(
            store.save(&Settings::default()),
            Err(SettingsError::HardLink)
        );
    }

    #[test]
    fn a_hard_link_substituted_after_inspection_is_rejected_before_reading() {
        let (root, store) = store();
        let outside = root.path().join("outside");
        let sentinel = b"not-settings-secret-marker";
        fs::write(&outside, sentinel).unwrap();
        let target = outside.clone();
        let path = store.path();
        hook(&store, TestStage::ReadOpen, move || {
            fs::remove_file(&path).unwrap();
            fs::hard_link(target, path).unwrap();
        });
        assert_eq!(store.load(), Err(SettingsError::HardLink));
        assert_eq!(fs::read(outside).unwrap(), sentinel);
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_substituted_after_inspection_cannot_block_or_be_read() {
        let (_root, store) = store();
        let path = store.path();
        hook(&store, TestStage::ReadOpen, move || {
            fs::remove_file(&path).unwrap();
            rustix::fs::mkfifoat(rustix::fs::CWD, path, rustix::fs::Mode::RUSR).unwrap();
        });
        assert_eq!(store.load(), Err(SettingsError::NotRegularFile));
    }

    #[cfg(unix)]
    #[test]
    fn losing_the_named_lock_prevents_publication() {
        let (_root, store) = store();
        let before = fs::read(store.path()).unwrap();
        let path = store.directory().join(LOCK_NAME);
        hook(&store, TestStage::Sync, move || {
            fs::remove_file(path).unwrap();
        });
        assert_eq!(
            store.save(&Settings::default()),
            Err(SettingsError::Changed)
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
    }

    #[cfg(windows)]
    fn assert_windows_sharing_denial(error: io::Error) {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

        // Rust does not necessarily map ERROR_SHARING_VIOLATION to
        // PermissionDenied. Check the native denial, then prove release below.
        assert!(
            matches!(
                error
                    .raw_os_error()
                    .and_then(|code| u32::try_from(code).ok()),
                Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
            ),
            "unexpected sharing error: {error}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_held_lock_cannot_be_removed_or_replaced() {
        let (_root, store) = store();
        let lock = store.profile().unwrap().lock().unwrap();
        assert_windows_sharing_denial(
            fs::remove_file(store.directory().join(LOCK_NAME)).unwrap_err(),
        );
        store.profile().unwrap().check_lock(&lock).unwrap();
        drop(lock);
        fs::remove_file(store.directory().join(LOCK_NAME)).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn a_reparse_profile_is_rejected_without_touching_its_target() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let sentinel = b"{\"schema_version\":99}";
        fs::write(outside.join(SETTINGS_FILE_NAME), sentinel).unwrap();
        let profile = root.path().join("profile-junction");
        let result = std::process::Command::new("cmd.exe")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(&profile)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "native junction fixture creation failed"
        );
        let store = SettingsStore::new(profile);
        assert!(store.load().is_err());
        assert!(store.save(&Settings::default()).is_err());
        assert_eq!(
            fs::read(outside.join(SETTINGS_FILE_NAME)).unwrap(),
            sentinel
        );
        assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replacing_document_after_inspection_cannot_follow_an_outside_symlink() {
        let (root, store) = store();
        let outside = root.path().join("outside.json");
        let sentinel = b"{\"schema_version\":99,\"secret\":\"must-not-load\"}";
        fs::write(&outside, sentinel).unwrap();
        let path = store.path();
        let target = outside.clone();
        hook(&store, TestStage::ReadOpen, move || {
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(target, path).unwrap();
        });
        assert!(store.load().is_err());
        assert_eq!(fs::read(outside).unwrap(), sentinel);
    }

    #[test]
    fn a_regular_replacement_between_inspection_and_open_is_detected() {
        let (root, store) = store();
        let replacement = root.path().join("replacement");
        fs::write(
            &replacement,
            serialize_document(&Settings::default()).unwrap(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let path = store.path();
        hook(&store, TestStage::ReadOpen, move || {
            fs::rename(replacement, path).unwrap();
        });
        assert_eq!(store.load(), Err(SettingsError::Changed));
    }

    #[test]
    fn a_pinned_profile_cannot_follow_a_replacement_directory() {
        let (root, store) = store();
        let moved = root.path().join("moved-profile");
        match fs::rename(store.directory(), &moved) {
            Ok(()) => {
                fs::create_dir(store.directory()).unwrap();
                let sentinel = b"replacement directory";
                fs::write(store.path(), sentinel).unwrap();
                store.save(&Settings::default()).unwrap();
                assert_eq!(fs::read(store.path()).unwrap(), sentinel);
                assert_eq!(
                    parse_document(&fs::read(moved.join(SETTINGS_FILE_NAME)).unwrap()).unwrap(),
                    Settings::default()
                );
            }
            Err(error) => {
                // Windows denies renaming directories while pinned no-delete
                // handles are retained. This is also an admitted safe outcome.
                #[cfg(windows)]
                {
                    assert_windows_sharing_denial(error);
                    store.save(&Settings::default()).unwrap();
                    let directory = store.directory().to_path_buf();
                    drop(store);
                    fs::rename(directory, moved).unwrap();
                }
                #[cfg(not(windows))]
                panic!("profile rename failed unexpectedly: {error}");
            }
        }
    }

    #[test]
    fn cancellation_after_writing_keeps_the_prior_complete_document() {
        let (_root, store) = store();
        let before = fs::read(store.path()).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&cancelled);
        hook(&store, TestStage::Sync, move || {
            signal.store(true, Ordering::Release)
        });
        assert_eq!(
            store.save_unless_cancelled(&Settings::default(), &cancelled),
            Err(SettingsError::Cancelled)
        );
        assert_eq!(fs::read(store.path()).unwrap(), before);
        assert_eq!(fs::read_dir(store.directory()).unwrap().count(), 2);
    }

    #[test]
    fn cancelled_stalls_at_write_and_flush_boundaries_cannot_publish_late() {
        for stage in [TestStage::Write, TestStage::Sync] {
            let (_root, store) = store();
            let before = fs::read(store.path()).unwrap();
            let (entered, blocked) = std::sync::mpsc::channel();
            let (release, wait) = std::sync::mpsc::channel();
            hook(&store, stage, move || {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            });
            let cancelled = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&cancelled);
            let writer = store.clone();
            let worker = std::thread::spawn(move || {
                writer.save_unless_cancelled(&Settings::default(), &signal)
            });
            blocked
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            assert_eq!(fs::read(store.path()).unwrap(), before);
            cancelled.store(true, Ordering::Release);
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap(), Err(SettingsError::Cancelled));
            assert_eq!(fs::read(store.path()).unwrap(), before);
            assert_eq!(fs::read_dir(store.directory()).unwrap().count(), 2);
        }
    }

    #[cfg(unix)]
    #[test]
    fn foreign_owned_directory_metadata_is_rejected() {
        use cap_std::fs::MetadataExt;
        let root = Dir::open_ambient_dir("/", ambient_authority()).unwrap();
        let metadata = root.dir_metadata().unwrap();
        // A root-run environment cannot supply a naturally foreign-owned
        // system directory; unprivileged CI exercises the actual owner check.
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            assert_eq!(
                check_directory(&metadata, true),
                Err(SettingsError::Permissions)
            );
        }
    }

    #[test]
    fn replacement_before_publication_preserves_the_newer_document() {
        let (_root, store) = store();
        let path = store.path();
        let sentinel = b"{\"schema_version\":99}";
        hook(&store, TestStage::Publish, move || {
            fs::write(path, sentinel).unwrap()
        });
        assert_eq!(
            store.save(&Settings::default()),
            Err(SettingsError::Changed)
        );
        assert_eq!(fs::read(store.path()).unwrap(), sentinel);
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_are_enforced_and_legacy_owned_directories_are_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, store) = store();
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(store.load(), Err(SettingsError::Permissions));
        fs::set_permissions(store.path(), fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o777)).unwrap();
        assert_eq!(store.load(), Err(SettingsError::Permissions));
        fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(store.load(), Err(SettingsError::Permissions));
        let reopened = SettingsStore::new(store.directory().to_owned());
        assert!(reopened.load().unwrap().is_some());
        assert_eq!(
            fs::metadata(store.directory())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
