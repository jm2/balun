//! Pinned private-profile transactions. Native I/O itself is not cancellable.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use cap_fs_ext::{
    FollowSymlinks, MetadataExt, OpenOptionsFollowExt, OpenOptionsMaybeDirExt, OpenOptionsSyncExt,
};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, DirBuilder, File, Metadata, OpenOptions};

use super::{
    MAX_SETTINGS_BYTES, MAX_SUBNET_PREFIX_BYTES, SETTINGS_FILE_NAME, SUBNET_PREFIX_FILE_NAME,
    Settings, SettingsError, SettingsOperation, TEMPORARY_PREFIX, TEMPORARY_SUFFIX, parse_document,
    parse_subnet_prefix, serialize_document, serialize_subnet_prefix,
};
use crate::discovery::TypedSubnetScope;

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

/// Unlock explicitly before closing the acquiring handle. On Unix a child
/// spawned by another thread may temporarily inherit the open file description;
/// closing only our descriptor would otherwise leave its lock held until exec.
struct TransactionLock(std::fs::File);

impl Drop for TransactionLock {
    fn drop(&mut self) {
        // Closing remains the fallback if the OS reports an unlock error.
        let _ = self.0.unlock();
    }
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
        profile.publish(
            &lock,
            SETTINGS_FILE_NAME,
            &bytes,
            previous.as_ref(),
            cancelled,
        )
    }

    /// The subnet last entered for subnet search. A file that is not one
    /// canonical private `/23`–`/32` prefix is ignored and left untouched; an
    /// unsafe or oversized file is an error the caller may ignore likewise.
    pub fn load_subnet_prefix(&self) -> Result<Option<TypedSubnetScope>, SettingsError> {
        let profile = self.profile()?;
        let lock = profile.lock()?;
        let read = profile.read_file(SUBNET_PREFIX_FILE_NAME, MAX_SUBNET_PREFIX_BYTES)?;
        profile.check_lock(&lock)?;
        Ok(read.and_then(|(bytes, _)| parse_subnet_prefix(&bytes)))
    }

    /// Remember `prefix` with the settings document's atomic, private
    /// publication, or with `None` remove the file.
    pub fn save_subnet_prefix_unless_cancelled(
        &self,
        prefix: Option<TypedSubnetScope>,
        cancelled: &AtomicBool,
    ) -> Result<(), SettingsError> {
        check_cancelled(cancelled)?;
        let profile = self.profile()?;
        let lock = profile.lock()?;
        let previous = profile.inspect(SUBNET_PREFIX_FILE_NAME)?;
        let Some(prefix) = prefix else {
            if previous.is_some() {
                #[cfg(test)]
                profile.hooks.run(TestStage::Publish);
                profile.check_lock(&lock)?;
                profile
                    .directory
                    .remove_file(SUBNET_PREFIX_FILE_NAME)
                    .map_err(|e| SettingsError::io(SettingsOperation::Publish, &e))?;
            }
            return Ok(());
        };
        let bytes = serialize_subnet_prefix(prefix);
        profile.publish(
            &lock,
            SUBNET_PREFIX_FILE_NAME,
            &bytes,
            previous.as_ref(),
            cancelled,
        )
    }
}

impl Profile {
    /// Publish `bytes` as `name` through a flushed private temporary sibling,
    /// unless `name` changed since `previous` was observed.
    fn publish(
        &self,
        lock: &TransactionLock,
        name: &str,
        bytes: &[u8],
        previous: Option<&Metadata>,
        cancelled: &AtomicBool,
    ) -> Result<(), SettingsError> {
        check_cancelled(cancelled)?;
        let mut temporary = self.temporary()?;
        #[cfg(test)]
        self.hooks.run(TestStage::Write);
        check_cancelled(cancelled)?;
        temporary
            .file
            .write_all(bytes)
            .and_then(|()| temporary.file.flush())
            .map_err(|e| SettingsError::io(SettingsOperation::Write, &e))?;
        #[cfg(test)]
        self.hooks.run(TestStage::Sync);
        check_cancelled(cancelled)?;
        temporary
            .file
            .sync_all()
            .map_err(|e| SettingsError::io(SettingsOperation::Sync, &e))?;
        check_cancelled(cancelled)?;
        self.check_lock(lock)?;
        #[cfg(test)]
        self.hooks.run(TestStage::Publish);
        let current = self.inspect(name)?;
        if !same_optional_snapshot(previous, current.as_ref()) {
            return Err(SettingsError::Changed);
        }
        temporary.check_identity()?;
        check_cancelled(cancelled)?;
        self.directory
            .rename(&temporary.name, &self.directory, name)
            .map_err(|e| SettingsError::io(SettingsOperation::Publish, &e))?;
        temporary.published = true;
        let published = self.inspect(name)?.ok_or(SettingsError::Changed)?;
        let written = temporary
            .file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        if !same_identity(&published, &written) {
            return Err(SettingsError::Changed);
        }
        #[cfg(unix)]
        self.directory
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

fn open_directory(parent: &Dir, name: &Path) -> Result<(Dir, Metadata), SettingsError> {
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
    Ok((Dir::from_std_file(file.into_std()), metadata))
}

impl Profile {
    fn open(path: &Path) -> Result<Self, SettingsError> {
        if !path.is_absolute() {
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
        // this initial acquisition. Let the OS resolve parent components too:
        // lexical removal of `..` would change meaning after a trusted alias.
        // Missing suffixes must have ordinary file names; `file_name` rejects
        // a trailing parent component before it can enter capability operations.
        // Every descendant open is no-follow.
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
            let (next, metadata) = open_directory(&parent, Path::new(&name))?;
            // Older Balun created owned 0755 profile directories, and a 002
            // umask can add user-private-group write. Clearing group/other bits
            // is safe after admission; owner bits are never added, so a
            // read-only profile stays read-only. No file bytes are changed.
            #[cfg(unix)]
            {
                use cap_std::fs::MetadataExt;
                use std::os::unix::fs::PermissionsExt;
                let mode = metadata.mode();
                if mode & 0o077 != 0 {
                    next.try_clone()
                        .and_then(|dir| {
                            dir.into_std_file()
                                .set_permissions(std::fs::Permissions::from_mode(mode & 0o700))
                        })
                        .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
                }
            }
            #[cfg(not(unix))]
            let _ = metadata;
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

    fn lock(&self) -> Result<TransactionLock, SettingsError> {
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
        let lock = TransactionLock(locking);
        self.check_lock(&lock)?;
        Ok(lock)
    }

    fn check_lock(&self, lock: &TransactionLock) -> Result<(), SettingsError> {
        let expected = Metadata::from_file(&lock.0)
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        let actual = self.inspect(LOCK_NAME)?.ok_or(SettingsError::Changed)?;
        if same_identity(&expected, &actual) {
            Ok(())
        } else {
            Err(SettingsError::Changed)
        }
    }

    fn read(&self) -> Result<(Option<Settings>, Option<Metadata>), SettingsError> {
        match self.read_file(SETTINGS_FILE_NAME, MAX_SETTINGS_BYTES)? {
            None => Ok((None, None)),
            Some((bytes, metadata)) => Ok((Some(parse_document(&bytes)?), Some(metadata))),
        }
    }

    /// Read at most `limit` bytes of `name` without following aliases,
    /// rejecting a file that changes while it is read.
    fn read_file(
        &self,
        name: &str,
        limit: u64,
    ) -> Result<Option<(Vec<u8>, Metadata)>, SettingsError> {
        let Some(before) = self.inspect(name)? else {
            return Ok(None);
        };
        #[cfg(test)]
        self.hooks.run(TestStage::ReadOpen);
        let file = self
            .directory
            .open_with(name, &options(false))
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
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| SettingsError::io(SettingsOperation::Read, &e))?;
        if bytes.len() as u64 > limit {
            return Err(SettingsError::TooLarge);
        }
        let after = file
            .metadata()
            .map_err(|e| SettingsError::io(SettingsOperation::Inspect, &e))?;
        let named = self.inspect(name)?.ok_or(SettingsError::Changed)?;
        if !same_snapshot(&opened, &after) || !same_snapshot(&after, &named) {
            return Err(SettingsError::Changed);
        }
        Ok(Some((bytes, after)))
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
        check_unix_directory(
            metadata.mode(),
            metadata.uid(),
            metadata.gid(),
            owned,
            user_private_group,
        )?;
    }
    #[cfg(not(unix))]
    let _ = owned;
    Ok(())
}

/// Refuse a world-writable directory and, when `owned`, a foreign owner.
///
/// Group write is admitted only for a directory the effective user owns whose
/// group is that account's user-private group, as a 002 umask creates. The
/// group lookup runs only for such a directory; a failed lookup refuses.
#[cfg(unix)]
fn check_unix_directory(
    mode: u32,
    uid: u32,
    gid: u32,
    owned: bool,
    account_group: impl FnOnce() -> Option<u32>,
) -> Result<(), SettingsError> {
    let foreign = uid != rustix::process::geteuid().as_raw();
    if mode & 0o002 != 0 || (owned && foreign) {
        return Err(SettingsError::Permissions);
    }
    if mode & 0o020 != 0 && (foreign || account_group() != Some(gid)) {
        return Err(SettingsError::Permissions);
    }
    Ok(())
}

/// The effective group ID when it is the account's user-private group (UPG).
///
/// Conservative UPG convention: `getgrgid_r(getegid())` names the group exactly
/// as `getpwuid_r(geteuid())` names the user, and the group lists no member
/// other than that user. A failed lookup or unrepresentable name is `None`.
#[cfg(unix)]
fn user_private_group() -> Option<u32> {
    use nix::unistd::{Group, User, getegid, geteuid};
    let user = User::from_uid(geteuid()).ok().flatten();
    let group = Group::from_gid(getegid()).ok().flatten();
    private_group(user.map(|user| user.name).as_deref(), group.as_ref())
}

#[cfg(unix)]
fn private_group(user: Option<&str>, group: Option<&nix::unistd::Group>) -> Option<u32> {
    let (user, group) = (user?, group?);
    let named = !user.is_empty() && !user.contains(char::REPLACEMENT_CHARACTER);
    (named && group.name == user && group.mem.iter().all(|member| member == user))
        .then_some(group.gid.as_raw())
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
    fn existing_parent_navigation_preserves_the_selected_profile() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("work")).unwrap();
        let selected = root.path().join("work").join("..").join("config/profile");
        let store = SettingsStore::new(selected.clone());
        store.save(&Settings::default()).unwrap();
        assert!(store.load().unwrap().is_some());
        assert_eq!(store.directory(), selected);
        assert!(
            root.path()
                .join("config/profile")
                .join(SETTINGS_FILE_NAME)
                .is_file()
        );
        let other = SettingsStore::new(root.path().join("config/profile"));
        assert_eq!(store.load().unwrap(), other.load().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn unresolved_parent_navigation_cannot_create_a_different_profile() {
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(root.path().join("missing/../profile"));
        assert!(store.save(&Settings::default()).is_err());
        assert!(!root.path().join("missing").exists());
        assert!(!root.path().join("profile").exists());
    }

    #[cfg(unix)]
    #[test]
    fn parent_navigation_uses_filesystem_alias_semantics_before_pinning() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("actual/nested")).unwrap();
        std::os::unix::fs::symlink(root.path().join("actual/nested"), root.path().join("alias"))
            .unwrap();
        let store = SettingsStore::new(root.path().join("alias/../config/profile"));
        store.save(&Settings::default()).unwrap();
        assert!(store.load().unwrap().is_some());
        assert!(
            root.path()
                .join("actual/config/profile")
                .join(SETTINGS_FILE_NAME)
                .is_file()
        );
        assert!(!root.path().join("config").exists());
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

    #[cfg(unix)]
    #[test]
    fn releasing_a_transaction_unlocks_even_with_an_inherited_descriptor() {
        let (_root, store) = store();
        let profile = store.profile().unwrap();
        let lock = profile.lock().unwrap();
        // A concurrent process spawn can briefly inherit the open file
        // description until exec closes CLOEXEC descriptors. A clone models
        // that lifetime deterministically, without spawning another process.
        let inherited = lock.0.try_clone().unwrap();
        let other = SettingsStore::new(store.directory().to_owned());
        assert_eq!(other.save(&Settings::default()), Err(SettingsError::Busy));
        drop(lock);
        other.save(&Settings::default()).unwrap();
        drop(inherited);
    }

    /// The subnet-prefix file uses the profile's pinned directory, its
    /// cooperative lock, and the same fail-closed checks as the settings.
    #[test]
    fn subnet_prefix_operations_fail_closed_like_the_settings_document() {
        use super::super::SUBNET_PREFIX_FILE_NAME;

        let prefix: TypedSubnetScope = "10.0.0.0/24".parse().unwrap();
        let idle = AtomicBool::new(false);

        // No admissible profile.
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("not-a-profile"), b"").unwrap();
        let unusable = SettingsStore::new(root.path().join("not-a-profile"));
        assert!(unusable.load_subnet_prefix().is_err());
        assert!(
            unusable
                .save_subnet_prefix_unless_cancelled(Some(prefix), &idle)
                .is_err()
        );

        // A hard-linked lock is refused before anything is read or written.
        let (root, store) = store();
        let lock_alias = root.path().join("lock-alias");
        fs::hard_link(store.directory().join(LOCK_NAME), &lock_alias).unwrap();
        assert_eq!(store.load_subnet_prefix(), Err(SettingsError::HardLink));
        assert_eq!(
            store.save_subnet_prefix_unless_cancelled(Some(prefix), &idle),
            Err(SettingsError::HardLink)
        );
        fs::remove_file(lock_alias).unwrap();

        // Something other than a regular file is never replaced.
        let file = store.directory().join(SUBNET_PREFIX_FILE_NAME);
        fs::create_dir(&file).unwrap();
        assert_eq!(
            store.save_subnet_prefix_unless_cancelled(Some(prefix), &idle),
            Err(SettingsError::NotRegularFile)
        );
        fs::remove_dir(&file).unwrap();

        store
            .save_subnet_prefix_unless_cancelled(Some(prefix), &idle)
            .unwrap();
        // Losing the lock (Windows denies removing a held one) discards what
        // was read and removes nothing.
        #[cfg(unix)]
        {
            let lock = store.directory().join(LOCK_NAME);
            hook(&store, TestStage::ReadOpen, move || {
                fs::remove_file(lock).unwrap();
            });
            assert_eq!(store.load_subnet_prefix(), Err(SettingsError::Changed));
            assert_eq!(store.load_subnet_prefix(), Ok(Some(prefix)));
            let lock = store.directory().join(LOCK_NAME);
            hook(&store, TestStage::Publish, move || {
                fs::remove_file(lock).unwrap();
            });
            assert_eq!(
                store.save_subnet_prefix_unless_cancelled(None, &idle),
                Err(SettingsError::Changed)
            );
            assert!(file.exists(), "nothing was removed without the lock");
        }

        // A file that vanishes while it is being forgotten is reported.
        let removed = file.clone();
        hook(&store, TestStage::Publish, move || {
            fs::remove_file(removed).unwrap();
        });
        assert!(matches!(
            store.save_subnet_prefix_unless_cancelled(None, &idle),
            Err(SettingsError::Io {
                operation: SettingsOperation::Publish,
                ..
            })
        ));
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
            // rustix's mkfifoat is unavailable on Apple targets. nix exposes
            // the same safe fixture operation on both macOS and Linux.
            nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).unwrap();
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

    #[cfg(unix)]
    fn group(name: &str, gid: u32, members: &[&str]) -> nix::unistd::Group {
        nix::unistd::Group {
            name: name.to_owned(),
            passwd: std::ffi::CString::default(),
            gid: nix::unistd::Gid::from_raw(gid),
            mem: members.iter().map(|member| (*member).to_owned()).collect(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_user_private_group_needs_the_user_name_and_no_other_members() {
        let upg = group("alice", 1_000, &[]);
        assert_eq!(private_group(Some("alice"), Some(&upg)), Some(1_000));
        let listed = group("alice", 1_000, &["alice"]);
        assert_eq!(private_group(Some("alice"), Some(&listed)), Some(1_000));
        for (user, group) in [
            (None, Some(upg.clone())),
            (Some("alice"), None),
            (Some("bob"), Some(upg.clone())),
            (Some("alice"), Some(group("users", 1_000, &[]))),
            (
                Some("alice"),
                Some(group("alice", 1_000, &["alice", "bob"])),
            ),
            (Some(""), Some(group("", 1_000, &[]))),
            (Some("\u{FFFD}"), Some(group("\u{FFFD}", 1_000, &[]))),
        ] {
            assert_eq!(private_group(user, group.as_ref()), None, "{user:?}");
        }
        // The account lookup itself only ever reports the effective group.
        if let Some(gid) = user_private_group() {
            assert_eq!(gid, rustix::process::getegid().as_raw());
        }
    }

    #[cfg(unix)]
    #[test]
    fn group_write_is_admitted_only_for_an_owned_user_private_group() {
        let me = rustix::process::geteuid().as_raw();
        let other = me.wrapping_add(1);
        for owned in [false, true] {
            let upg = || Some(1_000);
            assert_eq!(check_unix_directory(0o775, me, 1_000, owned, upg), Ok(()));
            assert_eq!(check_unix_directory(0o770, me, 1_000, owned, upg), Ok(()));
            // Private modes never consult the account database.
            let unused = || -> Option<u32> { panic!("group lookup for a private mode") };
            assert_eq!(
                check_unix_directory(0o755, me, 1_000, owned, unused),
                Ok(())
            );
            for (mode, uid, gid, lookup) in [
                (0o775, me, 1_001, Some(1_000)),
                (0o775, me, 1_000, None),
                (0o775, other, 1_000, Some(1_000)),
                (0o777, me, 1_000, Some(1_000)),
                (0o757, me, 1_000, Some(1_000)),
            ] {
                assert_eq!(
                    check_unix_directory(mode, uid, gid, owned, || lookup),
                    Err(SettingsError::Permissions),
                    "{mode:o} {owned}"
                );
            }
        }
        // A foreign but unwritable configuration parent stays trusted; the
        // Balun directory itself must be owned.
        let unused = || -> Option<u32> { panic!("group lookup for a private mode") };
        assert_eq!(check_unix_directory(0o755, other, 0, false, unused), Ok(()));
        assert_eq!(
            check_unix_directory(0o700, other, 0, true, unused),
            Err(SettingsError::Permissions)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_group_writable_configuration_follows_the_account_group_policy() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("config");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o770)).unwrap();
        // The result depends on this account's group database, so compare the
        // real lookup with the created directory rather than assuming a host.
        let private = user_private_group() == Some(fs::metadata(&parent).unwrap().gid());
        let store = SettingsStore::new(parent.join("balun"));
        if private {
            store.save(&Settings::default()).unwrap();
            fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o770)).unwrap();
            let reopened = SettingsStore::new(store.directory().to_owned());
            assert!(reopened.load().unwrap().is_some());
            let mode = fs::metadata(store.directory()).unwrap().mode();
            assert_eq!(mode & 0o777, 0o700, "user-private group write is removed");
        } else {
            assert_eq!(store.load(), Err(SettingsError::Permissions));
            assert!(!store.directory().exists());
        }
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        let shared = SettingsStore::new(parent.join("balun"));
        assert_eq!(shared.load(), Err(SettingsError::Permissions));
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_profile_is_tightened_without_becoming_writable() {
        use std::os::unix::fs::PermissionsExt;
        let (_root, store) = store();
        let before = fs::read(store.path()).unwrap();
        fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o550)).unwrap();
        let reopened = SettingsStore::new(store.directory().to_owned());
        assert!(reopened.load().unwrap().is_some());
        let mode = || {
            fs::metadata(store.directory())
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(), 0o500, "only group/other bits are cleared");
        // An unprivileged owner cannot create the temporary sibling; root can.
        if !rustix::process::geteuid().is_root() {
            assert!(reopened.save(&Settings::default()).is_err());
            assert_eq!(fs::read(store.path()).unwrap(), before);
        }
        assert_eq!(mode(), 0o500);
        fs::set_permissions(store.directory(), fs::Permissions::from_mode(0o700)).unwrap();
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
