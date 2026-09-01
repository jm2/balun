//! Durable storage for route-derived discovery authority.
//!
//! The store is synchronous by design. Callers must run it away from the UI
//! and async executor threads. Every read-modify-write transaction holds an
//! exclusive lock on a permanent sibling lock file; the authority JSON itself
//! is replaced atomically and is never used as the lock object.
//!
//! The injected directory is also the lock-generation boundary. Live authority
//! removal goes through [`ApprovalStore::revoke_all`] instead of deleting it.
//! On Unix, Balun validates and pins that directory once, then resolves every
//! permanent entry, temporary file, publication, and directory durability
//! barrier relative to the same descriptor. Renaming the injected pathname or
//! mounting a different directory over it therefore cannot redirect a running
//! store to another authority topology.
//!
//! Permanent Unix files must also have exactly one link before and after they
//! are opened and read. This rejects persistent aliases, but inotify and file
//! metadata cannot make storage linearizable against every action by a hostile
//! same-UID process. In particular, a retained writable mapping or a retained
//! descriptor used to recreate an external hard-link alias can bypass a
//! directory-only watch after the baseline. Production wiring must retain the
//! sibling watcher for the full authority epoch; hostile same-UID actors and
//! privileged namespace replacement remain outside this cooperative boundary.

use std::collections::BTreeSet;
use std::fmt;
#[cfg(not(unix))]
use std::fs::OpenOptions;
use std::fs::{self, File, TryLockError};
use std::io::{self, Read, Write};
#[cfg(any(not(unix), target_os = "linux", test))]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
#[cfg(not(unix))]
use tempfile::NamedTempFile;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

use super::super::client::ProbeConfig;
use super::super::routed::RoutedScanConfig;
use super::super::routes::{RouteCandidate, RouteSnapshot};
use super::gate::{RevalidatedRoutedScan, RoutedRevalidationError, revalidate_routed_scan};
use super::{
    ActiveReservation, MAX_AUTOMATIC_COOLDOWN, MAX_EMPTY_RUN_STREAK, RESERVATION_LEASE,
    RouteFingerprint, RouteFingerprintKey, RoutedApprovalState, RoutedBeginDecision,
    RoutedCompletionDecision, RoutedPolicyTime, RoutedProposalError, RoutedProposalSummary,
    RoutedRunId, RoutedScanOutcome, RoutedScanPermit, RoutedScanProposal, RoutedScanTrigger,
};

pub(super) const STATE_FILE_NAME: &str = "routed-approvals.json";
pub(super) const KEY_FILE_NAME: &str = "routed-approvals.key";
pub(super) const LOCK_FILE_NAME: &str = "routed-approvals.lock";
const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: usize = 64 * 1024;
const MAX_APPROVALS: usize = 8;
const KEY_BYTES: usize = 32;
const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const KEY_BINDING_DOMAIN: &[u8] = b"io.github.jm2.Balun/routed-approval-store-key-v1";

/// An injected private directory. The store performs no platform path lookup.
///
/// Its immediate parent must already exist and is the caller's trusted path
/// boundary. On Unix that parent must be owned by the effective user and must
/// not be group- or world-writable. The store creates only this final private
/// directory, then durably confirms its parent entry before authority work.
///
/// On Windows this must be a per-user local-data directory with a restrictive
/// inherited DACL. Safe stable Rust can preserve that inheritance, but cannot
/// attest an owner-only Windows ACL.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct StorePaths {
    directory: PathBuf,
}

impl StorePaths {
    pub(crate) fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    fn state(&self) -> PathBuf {
        self.directory.join(STATE_FILE_NAME)
    }

    fn key(&self) -> PathBuf {
        self.directory.join(KEY_FILE_NAME)
    }

    fn lock(&self) -> PathBuf {
        self.directory.join(LOCK_FILE_NAME)
    }
}

impl fmt::Debug for StorePaths {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StorePaths(<redacted>)")
    }
}

/// The exact private-directory identity used by one running store.
///
/// Unix operations never reconstruct a child pathname from `StorePaths` after
/// this value is created. The descriptor also stays alive in the Linux watch
/// anchor, so `/proc/self/fd/<n>/.` cannot be retargeted through descriptor
/// reuse while inotify resolves it.
struct StoreDirectory {
    #[cfg(unix)]
    descriptor: File,
    #[cfg(not(unix))]
    path: PathBuf,
}

impl fmt::Debug for StoreDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreDirectory(<redacted>)")
    }
}

struct StoreParent {
    #[cfg(unix)]
    descriptor: File,
    #[cfg(not(unix))]
    _private: (),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StoreEntryKind {
    File,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StoreEntryMetadata {
    kind: StoreEntryKind,
    private_file: bool,
    #[cfg(unix)]
    device: i128,
    #[cfg(unix)]
    inode: i128,
}

impl StoreEntryMetadata {
    const fn is_file(self) -> bool {
        matches!(self.kind, StoreEntryKind::File)
    }

    const fn is_symlink(self) -> bool {
        matches!(self.kind, StoreEntryKind::Symlink)
    }

    const fn same_file(self, other: Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            let _ = other;
            true
        }
    }
}

#[cfg(unix)]
struct StoreTemporary {
    file: File,
    directory: Arc<StoreDirectory>,
    name: String,
    published: bool,
}

#[cfg(not(unix))]
struct StoreTemporary {
    named: NamedTempFile,
}

#[cfg(unix)]
impl StoreTemporary {
    fn as_file(&self) -> &File {
        &self.file
    }
}

#[cfg(not(unix))]
impl StoreTemporary {
    fn as_file(&self) -> &File {
        self.named.as_file()
    }
}

#[cfg(unix)]
impl Write for StoreTemporary {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(unix)]
impl Drop for StoreTemporary {
    fn drop(&mut self) {
        if !self.published {
            let _ = rustix::fs::unlinkat(
                &self.directory.descriptor,
                self.name.as_str(),
                rustix::fs::AtFlags::empty(),
            );
        }
    }
}

#[cfg(not(unix))]
impl Write for StoreTemporary {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.named.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.named.flush()
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(super) struct StoreDirectoryWatchAnchor(Arc<StoreDirectory>);

#[cfg(target_os = "linux")]
impl StoreDirectoryWatchAnchor {
    pub(super) fn proc_fd_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}/.", self.0.descriptor.as_raw_fd()))
    }

    pub(super) fn path_matches_pinned_identity(&self, path: &Path) -> bool {
        let Ok(path_metadata) = fs::metadata(path) else {
            return false;
        };
        let Ok(pinned_metadata) = self.0.descriptor.metadata() else {
            return false;
        };
        path_metadata.is_dir()
            && pinned_metadata.is_dir()
            && path_metadata.dev() == pinned_metadata.dev()
            && path_metadata.ino() == pinned_metadata.ino()
    }
}

#[cfg(unix)]
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl StoreDirectory {
    #[cfg(unix)]
    fn entry_metadata(&self, name: &str) -> io::Result<StoreEntryMetadata> {
        let metadata = rustix::fs::statat(
            &self.descriptor,
            name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(io::Error::from)?;
        Ok(unix_entry_metadata(&metadata))
    }

    #[cfg(not(unix))]
    fn entry_metadata(&self, name: &str) -> io::Result<StoreEntryMetadata> {
        let metadata = fs::symlink_metadata(self.path.join(name))?;
        Ok(portable_entry_metadata(&metadata))
    }

    #[cfg(unix)]
    fn opened_metadata(file: &File) -> io::Result<StoreEntryMetadata> {
        let metadata = rustix::fs::fstat(file).map_err(io::Error::from)?;
        Ok(unix_entry_metadata(&metadata))
    }

    #[cfg(not(unix))]
    fn opened_metadata(file: &File) -> io::Result<StoreEntryMetadata> {
        file.metadata()
            .map(|metadata| portable_entry_metadata(&metadata))
    }

    #[cfg(unix)]
    fn open_read(&self, name: &str) -> io::Result<File> {
        rustix::fs::openat(
            &self.descriptor,
            name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(io::Error::from)
    }

    #[cfg(not(unix))]
    fn open_read(&self, name: &str) -> io::Result<File> {
        File::open(self.path.join(name))
    }

    #[cfg(unix)]
    fn open_lock(&self, name: &str, create_new: bool) -> io::Result<File> {
        let mut flags =
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW;
        if create_new {
            flags |= rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL;
        }
        rustix::fs::openat(
            &self.descriptor,
            name,
            flags,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map(File::from)
        .map_err(io::Error::from)
    }

    #[cfg(not(unix))]
    fn open_lock(&self, name: &str, create_new: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(create_new);
        configure_lock_sharing(&mut options);
        options.open(self.path.join(name))
    }

    #[cfg(unix)]
    fn create_temporary(self: &Arc<Self>) -> io::Result<StoreTemporary> {
        const MAX_TEMPORARY_ATTEMPTS: usize = 128;
        for _ in 0..MAX_TEMPORARY_ATTEMPTS {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let name = format!(".routed-approvals.tmp.{}.{}", std::process::id(), sequence);
            match rustix::fs::openat(
                &self.descriptor,
                name.as_str(),
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NOFOLLOW,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            ) {
                Ok(descriptor) => {
                    let file = File::from(descriptor);
                    set_private_file_permissions(&file)?;
                    return Ok(StoreTemporary {
                        file,
                        directory: Arc::clone(self),
                        name,
                        published: false,
                    });
                }
                Err(rustix::io::Errno::EXIST) => {}
                Err(error) => return Err(io::Error::from(error)),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "approval-store temporary namespace is exhausted",
        ))
    }

    #[cfg(not(unix))]
    fn create_temporary(self: &Arc<Self>) -> io::Result<StoreTemporary> {
        NamedTempFile::new_in(&self.path).map(|named| StoreTemporary { named })
    }

    #[cfg(unix)]
    fn publish_replace(&self, mut temporary: StoreTemporary, name: &str) -> io::Result<()> {
        rustix::fs::renameat(
            &self.descriptor,
            temporary.name.as_str(),
            &self.descriptor,
            name,
        )
        .map_err(io::Error::from)?;
        temporary.published = true;
        Ok(())
    }

    #[cfg(not(unix))]
    fn publish_replace(&self, temporary: StoreTemporary, name: &str) -> io::Result<()> {
        temporary
            .named
            .persist(self.path.join(name))
            .map(|_| ())
            .map_err(|error| error.error)
    }

    #[cfg(unix)]
    fn publish_noclobber(&self, mut temporary: StoreTemporary, name: &str) -> io::Result<()> {
        publish_noclobber_unix(self, temporary.name.as_str(), name)?;
        temporary.published = true;
        Ok(())
    }

    #[cfg(not(unix))]
    fn publish_noclobber(&self, temporary: StoreTemporary, name: &str) -> io::Result<()> {
        temporary
            .named
            .persist_noclobber(self.path.join(name))
            .map(|_| ())
            .map_err(|error| error.error)
    }
}

#[cfg(unix)]
fn unix_entry_metadata(metadata: &rustix::fs::Stat) -> StoreEntryMetadata {
    let kind = match rustix::fs::FileType::from_raw_mode(metadata.st_mode) {
        rustix::fs::FileType::RegularFile => StoreEntryKind::File,
        rustix::fs::FileType::Symlink => StoreEntryKind::Symlink,
        _ => StoreEntryKind::Other,
    };
    StoreEntryMetadata {
        kind,
        private_file: metadata.st_uid == rustix::process::geteuid().as_raw()
            && metadata.st_mode & 0o7777 == 0o600
            && metadata.st_nlink == 1,
        device: i128::from(metadata.st_dev),
        inode: i128::from(metadata.st_ino),
    }
}

#[cfg(not(unix))]
fn portable_entry_metadata(metadata: &fs::Metadata) -> StoreEntryMetadata {
    let kind = if metadata.file_type().is_symlink() {
        StoreEntryKind::Symlink
    } else if metadata.is_file() {
        StoreEntryKind::File
    } else {
        StoreEntryKind::Other
    };
    StoreEntryMetadata {
        kind,
        private_file: secure_file_metadata(metadata),
    }
}

#[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
fn publish_noclobber_unix(
    directory: &StoreDirectory,
    temporary_name: &str,
    final_name: &str,
) -> io::Result<()> {
    rustix::fs::renameat_with(
        &directory.descriptor,
        temporary_name,
        &directory.descriptor,
        final_name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(io::Error::from)
}

#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn publish_noclobber_unix(
    directory: &StoreDirectory,
    temporary_name: &str,
    final_name: &str,
) -> io::Result<()> {
    rustix::fs::linkat(
        &directory.descriptor,
        temporary_name,
        &directory.descriptor,
        final_name,
        rustix::fs::AtFlags::empty(),
    )
    .map_err(io::Error::from)?;
    rustix::fs::unlinkat(
        &directory.descriptor,
        temporary_name,
        rustix::fs::AtFlags::empty(),
    )
    .map_err(io::Error::from)
}

/// A topology-free view of the persisted authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ApprovalStoreStatus {
    Missing {
        key_initialized: bool,
    },
    Ready {
        approval_count: usize,
        has_active_reservation: bool,
    },
    Quarantined(QuarantineReason),
}

/// Why authority bytes are being preserved but refused.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum QuarantineReason {
    #[error("the approval state is not a regular file")]
    StateNotRegular,
    #[error("the approval state path is a symbolic link")]
    StateSymlink,
    #[error("the approval state permissions or owner are unsafe")]
    UnsafeStatePermissions,
    #[error("the approval state cannot be read")]
    StateUnreadable,
    #[error("the approval state exceeds the byte limit")]
    StateTooLarge,
    #[error("the approval state is malformed, unsupported, or semantically invalid")]
    InvalidState,
    #[error("the key is missing while approval state exists")]
    MissingKey,
    #[error("the approval state is missing while an initialized key exists")]
    MissingState,
    #[error("the key is not a regular file")]
    KeyNotRegular,
    #[error("the key path is a symbolic link")]
    KeySymlink,
    #[error("the key permissions or owner are unsafe")]
    UnsafeKeyPermissions,
    #[error("the key cannot be read")]
    KeyUnreadable,
    #[error("the key does not contain exactly 32 bytes")]
    InvalidKeyLength,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum StoreError {
    #[error("the approval store is busy")]
    Busy,
    #[error("the approval store directory is unavailable")]
    DirectoryUnavailable,
    #[error("the approval store directory is not a private regular directory")]
    UnsafeDirectory,
    #[error("the approval store parent directory is not safely user-owned")]
    UnsafeParentDirectory,
    #[error("the approval store directory entry could not be durably confirmed")]
    DirectoryDurabilityUncertain,
    #[error("the permanent approval lock file is unsafe")]
    UnsafeLockFile,
    #[error("approval storage failed before publication ({operation:?}: {kind:?})")]
    BeforePublication {
        operation: StoreOperation,
        kind: io::ErrorKind,
    },
    #[error("the operating system could not provide random key material")]
    EntropyUnavailable,
    #[error("approval state is quarantined: {0}")]
    Quarantined(QuarantineReason),
    #[error("the proposal was built with a different approval-store key")]
    StaleProposal,
    #[error("the approval store has reached its eight-fingerprint limit")]
    ApprovalLimit,
    #[error("the global run counter is exhausted")]
    RunCounterExhausted,
    #[error("the proposed route-derived scan is invalid: {0}")]
    InvalidProposal(RoutedProposalError),
    #[error("the in-memory approval transition violated store invariants")]
    InvalidTransition,
    #[error("approval state serialization failed")]
    Serialization,
    #[error("serialized approval state exceeds the byte limit")]
    SerializedStateTooLarge,
}

/// A topology-redacted failure at the store-owned fresh-snapshot gate.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum StoredRevalidationError {
    #[error("approval storage rejected revalidation: {0}")]
    Store(StoreError),
    #[error("the exact routed reservation is no longer active")]
    AuthorityChanged,
    #[error("fresh routed-plan revalidation failed: {0}")]
    Revalidation(RoutedRevalidationError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreOperation {
    Lock,
    CreateTemporary,
    WriteTemporary,
    SyncTemporary,
    Publish,
}

/// Whether publication reached the platform's durable commit boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommitDurability {
    Confirmed,
    /// This platform has no safe portable parent-directory sync primitive.
    Unsupported,
    /// Replacement is visible, but a post-publication durability step failed.
    Uncertain,
}

/// The result of an approval, revoke, or other state mutation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StoreCommit {
    durability: CommitDurability,
}

impl StoreCommit {
    #[must_use]
    pub(crate) const fn durability(self) -> CommitDurability {
        self.durability
    }

    #[must_use]
    pub(crate) const fn is_confirmed(self) -> bool {
        matches!(self.durability, CommitDurability::Confirmed)
    }
}

/// A proposal coupled to the immutable key from which it was derived.
///
/// The binding prevents a proposal retained in memory from being approved
/// after the key file has been externally replaced.
pub(crate) struct StoredRoutedProposal {
    proposal: RoutedScanProposal,
    key_binding: [u8; 32],
}

impl StoredRoutedProposal {
    #[must_use]
    pub(crate) fn summary(&self) -> &RoutedProposalSummary {
        self.proposal.summary()
    }
}

impl fmt::Debug for StoredRoutedProposal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredRoutedProposal")
            .field("proposal", &self.proposal)
            .field("key_binding", &"<redacted>")
            .finish()
    }
}

/// A store-owned begin decision. A permit exists only in the confirmed case.
pub(crate) enum StoredBeginDecision {
    Permitted(RoutedScanPermit),
    NeedsApproval(RoutedProposalSummary),
    CoolingDown { remaining: Duration },
    Busy,
    PublishedWithoutPermit { durability: CommitDurability },
}

impl fmt::Debug for StoredBeginDecision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Permitted(permit) => formatter.debug_tuple("Permitted").field(permit).finish(),
            Self::NeedsApproval(summary) => formatter
                .debug_tuple("NeedsApproval")
                .field(summary)
                .finish(),
            Self::CoolingDown { remaining } => formatter
                .debug_struct("CoolingDown")
                .field("remaining", remaining)
                .finish(),
            Self::Busy => formatter.write_str("Busy"),
            Self::PublishedWithoutPermit { durability } => formatter
                .debug_struct("PublishedWithoutPermit")
                .field("durability", durability)
                .finish(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredCompletionDecision {
    Stale,
    Confirmed(RoutedCompletionDecision),
    PublishedUnconfirmed {
        decision: RoutedCompletionDecision,
        durability: CommitDurability,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredRevokeDecision {
    NotFound,
    Published(StoreCommit),
}

/// The packet-free durable owner of routed approval state.
pub(crate) struct ApprovalStore {
    paths: StorePaths,
    lock_timeout: Duration,
    backend: Arc<dyn StoreBackend>,
    directory: Mutex<Option<Arc<StoreDirectory>>>,
}

impl ApprovalStore {
    pub(crate) fn new(paths: StorePaths) -> Self {
        Self {
            paths,
            lock_timeout: DEFAULT_LOCK_TIMEOUT,
            backend: Arc::new(SystemBackend),
            directory: Mutex::new(None),
        }
    }

    #[cfg(test)]
    fn with_backend(
        paths: StorePaths,
        lock_timeout: Duration,
        backend: Arc<dyn StoreBackend>,
    ) -> Self {
        Self {
            paths,
            lock_timeout: lock_timeout.min(MAX_LOCK_TIMEOUT),
            backend,
            directory: Mutex::new(None),
        }
    }

    pub(crate) fn load(&self) -> Result<ApprovalStoreStatus, StoreError> {
        let _lock = self.acquire_lock()?;
        match self.load_locked()? {
            LockedLoad::Missing {
                key: KeyLoad::Missing,
            } => Ok(ApprovalStoreStatus::Missing {
                key_initialized: false,
            }),
            LockedLoad::Ready { ledger, .. } => Ok(ApprovalStoreStatus::Ready {
                approval_count: ledger.approvals.len(),
                has_active_reservation: ledger
                    .approvals
                    .iter()
                    .any(|approval| approval.active.is_some()),
            }),
            LockedLoad::Quarantined(reason)
            | LockedLoad::Missing {
                key: KeyLoad::Quarantined(reason),
            } => Ok(ApprovalStoreStatus::Quarantined(reason)),
            LockedLoad::Missing {
                key: KeyLoad::Ready(_),
            } => Ok(ApprovalStoreStatus::Quarantined(
                QuarantineReason::MissingState,
            )),
        }
    }

    /// Load or create the immutable key, then construct an exact proposal.
    pub(crate) fn build_proposal(
        &self,
        snapshot: &RouteSnapshot,
        candidates: &[RouteCandidate],
        probe_config: ProbeConfig,
        scan_config: RoutedScanConfig,
    ) -> Result<StoredRoutedProposal, StoreError> {
        let key = {
            let _lock = self.acquire_lock()?;
            self.load_or_create_key_locked()?
        };
        let key_binding = key_binding(&key);
        let route_key = RouteFingerprintKey::from_bytes(*key);
        let proposal = RoutedScanProposal::from_route_candidates(
            snapshot,
            candidates,
            &route_key,
            probe_config,
            scan_config,
        )
        .map_err(StoreError::InvalidProposal)?;
        Ok(StoredRoutedProposal {
            proposal,
            key_binding,
        })
    }

    /// Persist an explicit approval without retaining any topology fields.
    pub(crate) fn approve(
        &self,
        proposal: &StoredRoutedProposal,
        now: RoutedPolicyTime,
    ) -> Result<StoreCommit, StoreError> {
        let _lock = self.acquire_lock()?;
        let (mut ledger, key) = self.load_mutable_locked()?;
        verify_proposal_key(proposal, &key)?;

        if has_unexpired_active(&mut ledger, now) {
            return Err(StoreError::InvalidTransition);
        }

        let approval = RoutedApprovalState::from_user_approval(
            &proposal.proposal,
            now,
            ledger.last_issued_counter.map(RoutedRunId::from_counter),
        );
        if let Some(existing) = ledger
            .approvals
            .iter_mut()
            .find(|state| state.fingerprint == approval.fingerprint)
        {
            *existing = approval;
        } else {
            if ledger.approvals.len() >= MAX_APPROVALS {
                return Err(StoreError::ApprovalLimit);
            }
            ledger.approvals.push(approval);
        }
        ledger.sort();
        self.commit_ledger(&ledger)
    }

    /// Reserve one global run and release its permit only after a confirmed
    /// file and parent-directory commit.
    pub(super) fn reserve(
        &self,
        proposal: StoredRoutedProposal,
        trigger: RoutedScanTrigger,
        now: RoutedPolicyTime,
    ) -> Result<StoredBeginDecision, StoreError> {
        let _lock = self.acquire_lock()?;
        let (mut ledger, key) = self.load_mutable_locked()?;
        verify_proposal_key(&proposal, &key)?;

        if has_unexpired_active(&mut ledger, now) {
            return Ok(StoredBeginDecision::Busy);
        }

        let fingerprint = proposal.proposal.fingerprint;
        let Some(index) = ledger
            .approvals
            .iter()
            .position(|state| state.fingerprint == fingerprint)
        else {
            return Ok(StoredBeginDecision::NeedsApproval(
                proposal.proposal.summary,
            ));
        };

        let next_counter = ledger
            .last_issued_counter
            .map_or(Some(1), |counter| counter.checked_add(1))
            .ok_or(StoreError::RunCounterExhausted)?;
        let decision = ledger.approvals[index].plan_begin(
            proposal.proposal,
            trigger,
            now,
            RoutedRunId::from_counter(next_counter),
        );
        let pending = match decision {
            RoutedBeginDecision::Pending(pending) => pending,
            RoutedBeginDecision::NeedsApproval(summary) => {
                return Ok(StoredBeginDecision::NeedsApproval(summary));
            }
            RoutedBeginDecision::CoolingDown { remaining } => {
                return Ok(StoredBeginDecision::CoolingDown { remaining });
            }
            RoutedBeginDecision::Busy => return Ok(StoredBeginDecision::Busy),
            RoutedBeginDecision::InvalidRunId => return Err(StoreError::InvalidTransition),
        };

        ledger.approvals[index] = pending.state_after_reservation().clone();
        ledger.last_issued_counter = Some(next_counter);
        ledger.sort();
        let commit = self.commit_ledger(&ledger)?;
        if commit.is_confirmed() {
            let (_state, permit) = pending.confirm_persisted();
            Ok(StoredBeginDecision::Permitted(permit))
        } else {
            // The pending value is deliberately dropped. A visible crash
            // reservation may remain, but no network authority escapes.
            Ok(StoredBeginDecision::PublishedWithoutPermit {
                durability: commit.durability,
            })
        }
    }

    /// Apply completion only to the exact globally active fingerprint/run.
    /// The fingerprint also scopes stale completions across an explicit
    /// deletion and reinitialization of the complete installation store.
    pub(crate) fn complete(
        &self,
        fingerprint: RouteFingerprint,
        run_id: RoutedRunId,
        outcome: RoutedScanOutcome,
        now: RoutedPolicyTime,
    ) -> Result<StoredCompletionDecision, StoreError> {
        let _lock = self.acquire_lock()?;
        let (mut ledger, _key) = match self.load_mutable_if_present_locked()? {
            Some(loaded) => loaded,
            None => return Ok(StoredCompletionDecision::Stale),
        };
        let Some(index) = ledger.approvals.iter().position(|state| {
            state.fingerprint == fingerprint
                && state.active.is_some_and(|active| active.run_id == run_id)
        }) else {
            return Ok(StoredCompletionDecision::Stale);
        };

        let decision = ledger.approvals[index].complete(run_id, outcome, now);
        if decision == RoutedCompletionDecision::Stale {
            return Ok(StoredCompletionDecision::Stale);
        }
        let commit = self.commit_ledger(&ledger)?;
        if commit.is_confirmed() {
            Ok(StoredCompletionDecision::Confirmed(decision))
        } else {
            Ok(StoredCompletionDecision::PublishedUnconfirmed {
                decision,
                durability: commit.durability,
            })
        }
    }

    /// Revoke the exact proposal fingerprint. The immutable key and global
    /// run high-water are retained so an old completion can never be reused.
    pub(super) fn revoke(
        &self,
        proposal: &StoredRoutedProposal,
    ) -> Result<StoredRevokeDecision, StoreError> {
        let _lock = self.acquire_lock()?;
        let (mut ledger, key) = match self.load_mutable_if_present_locked()? {
            Some(loaded) => loaded,
            None => return Ok(StoredRevokeDecision::NotFound),
        };
        verify_proposal_key(proposal, &key)?;
        let before = ledger.approvals.len();
        ledger
            .approvals
            .retain(|state| state.fingerprint != proposal.proposal.fingerprint);
        if ledger.approvals.len() == before {
            return Ok(StoredRevokeDecision::NotFound);
        }
        Ok(StoredRevokeDecision::Published(
            self.commit_ledger(&ledger)?,
        ))
    }

    /// Revoke all remembered routed authority without requiring the old
    /// topology to still exist. The immutable key and global run high-water
    /// remain, preventing reuse of any previously issued run identity.
    pub(super) fn revoke_all(&self) -> Result<StoredRevokeDecision, StoreError> {
        let _lock = self.acquire_lock()?;
        let (mut ledger, _key) = match self.load_mutable_if_present_locked()? {
            Some(loaded) => loaded,
            None => return Ok(StoredRevokeDecision::NotFound),
        };
        if ledger.approvals.is_empty() {
            return Ok(StoredRevokeDecision::NotFound);
        }
        ledger.approvals.clear();
        Ok(StoredRevokeDecision::Published(
            self.commit_ledger(&ledger)?,
        ))
    }

    /// Consume one committed permit and revalidate it with the store-owned
    /// key only while its exact fingerprint and run remain globally active.
    ///
    /// The store lock prevents cooperative revocation during this check. The
    /// approval admission boundary must already hold a live invalidation
    /// registration, then recheck it immediately after this method releases
    /// the lock because revocation or a network change can occur at once.
    pub(super) fn revalidate_permit(
        &self,
        permit: RoutedScanPermit,
        snapshot: &RouteSnapshot,
        now: RoutedPolicyTime,
    ) -> Result<RevalidatedRoutedScan, StoredRevalidationError> {
        let _lock = self
            .acquire_lock()
            .map_err(StoredRevalidationError::Store)?;
        let (ledger, key) = self
            .load_mutable_if_present_locked()
            .map_err(StoredRevalidationError::Store)?
            .ok_or(StoredRevalidationError::AuthorityChanged)?;
        let Some(approval) = ledger.approvals.iter().find(|approval| {
            approval.fingerprint == permit.fingerprint
                && approval.active.is_some_and(|active| {
                    active.run_id == permit.run_id && active.expires_at == permit.expires_at
                })
        }) else {
            return Err(StoredRevalidationError::AuthorityChanged);
        };
        if ledger
            .approvals
            .iter()
            .filter(|approval| approval.active.is_some())
            .count()
            != 1
        {
            return Err(StoredRevalidationError::AuthorityChanged);
        }

        let effective_now = now.max(approval.last_observed_time);
        let route_key = RouteFingerprintKey::from_bytes(*key);
        revalidate_routed_scan(permit, snapshot, &route_key, effective_now)
            .map_err(StoredRevalidationError::Revalidation)
    }

    fn load_or_create_key_locked(&self) -> Result<Zeroizing<[u8; KEY_BYTES]>, StoreError> {
        match self.load_locked()? {
            LockedLoad::Quarantined(reason)
            | LockedLoad::Missing {
                key: KeyLoad::Quarantined(reason),
            } => Err(StoreError::Quarantined(reason)),
            LockedLoad::Ready { key, .. } => Ok(key),
            LockedLoad::Missing {
                key: KeyLoad::Ready(_),
            } => Err(StoreError::Quarantined(QuarantineReason::MissingState)),
            LockedLoad::Missing {
                key: KeyLoad::Missing,
            } => {
                let key = self.create_key_locked()?;
                // The empty ledger makes later key-without-state unambiguously
                // invalid. A confirmed state commit also supplies the parent
                // directory barrier for the preceding immutable-key publish.
                self.commit_ledger(&ApprovalLedger::default())?;
                Ok(key)
            }
        }
    }

    fn load_mutable_locked(&self) -> Result<MutableLoad, StoreError> {
        match self.load_locked()? {
            LockedLoad::Ready { ledger, key } => Ok((ledger, key)),
            LockedLoad::Missing {
                key: KeyLoad::Ready(_),
            } => Err(StoreError::Quarantined(QuarantineReason::MissingState)),
            LockedLoad::Missing {
                key: KeyLoad::Missing,
            } => Err(StoreError::StaleProposal),
            LockedLoad::Quarantined(reason)
            | LockedLoad::Missing {
                key: KeyLoad::Quarantined(reason),
            } => Err(StoreError::Quarantined(reason)),
        }
    }

    fn load_mutable_if_present_locked(&self) -> Result<Option<MutableLoad>, StoreError> {
        match self.load_locked()? {
            LockedLoad::Ready { ledger, key } => Ok(Some((ledger, key))),
            LockedLoad::Missing {
                key: KeyLoad::Missing,
            } => Ok(None),
            LockedLoad::Missing {
                key: KeyLoad::Ready(_),
            } => Err(StoreError::Quarantined(QuarantineReason::MissingState)),
            LockedLoad::Quarantined(reason)
            | LockedLoad::Missing {
                key: KeyLoad::Quarantined(reason),
            } => Err(StoreError::Quarantined(reason)),
        }
    }

    fn load_locked(&self) -> Result<LockedLoad, StoreError> {
        let state = self.read_state_locked()?;
        if let StateLoad::Quarantined(reason) = state {
            return Ok(LockedLoad::Quarantined(reason));
        }
        let key = self.read_key_locked()?;
        match state {
            StateLoad::Missing => Ok(LockedLoad::Missing { key }),
            StateLoad::Ready(ledger) => match key {
                KeyLoad::Ready(key) => Ok(LockedLoad::Ready { ledger, key }),
                KeyLoad::Missing => Ok(LockedLoad::Quarantined(QuarantineReason::MissingKey)),
                KeyLoad::Quarantined(reason) => Ok(LockedLoad::Quarantined(reason)),
            },
            StateLoad::Quarantined(_) => unreachable!("handled above"),
        }
    }

    fn read_state_locked(&self) -> Result<StateLoad, StoreError> {
        let directory = self.pinned_directory()?;
        let metadata = match directory.entry_metadata(STATE_FILE_NAME) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(StateLoad::Missing),
            Err(_) => return Ok(StateLoad::Quarantined(QuarantineReason::StateUnreadable)),
        };
        if metadata.is_symlink() {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateSymlink));
        }
        if !metadata.is_file() {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateNotRegular));
        }
        if !metadata.private_file {
            return Ok(StateLoad::Quarantined(
                QuarantineReason::UnsafeStatePermissions,
            ));
        }
        let file = match directory.open_read(STATE_FILE_NAME) {
            Ok(file) => file,
            Err(_) => return Ok(StateLoad::Quarantined(QuarantineReason::StateUnreadable)),
        };
        let opened = match StoreDirectory::opened_metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(StateLoad::Quarantined(QuarantineReason::StateUnreadable)),
        };
        if !opened.is_file() || !metadata.same_file(opened) {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateNotRegular));
        }
        if !opened.private_file {
            return Ok(StateLoad::Quarantined(
                QuarantineReason::UnsafeStatePermissions,
            ));
        }

        let mut bytes = Vec::with_capacity(MAX_STATE_BYTES + 1);
        if (&file)
            .take((MAX_STATE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateUnreadable));
        }
        if bytes.len() > MAX_STATE_BYTES {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateTooLarge));
        }
        let after_read = match StoreDirectory::opened_metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(StateLoad::Quarantined(QuarantineReason::StateUnreadable)),
        };
        if !after_read.is_file() || !opened.same_file(after_read) {
            return Ok(StateLoad::Quarantined(QuarantineReason::StateNotRegular));
        }
        if !after_read.private_file {
            return Ok(StateLoad::Quarantined(
                QuarantineReason::UnsafeStatePermissions,
            ));
        }
        let stored: StoredEnvelopeV1 = match serde_json::from_slice(&bytes) {
            Ok(stored) => stored,
            Err(_) => return Ok(StateLoad::Quarantined(QuarantineReason::InvalidState)),
        };
        match ApprovalLedger::try_from(stored) {
            Ok(ledger) => Ok(StateLoad::Ready(ledger)),
            Err(()) => Ok(StateLoad::Quarantined(QuarantineReason::InvalidState)),
        }
    }

    fn read_key_locked(&self) -> Result<KeyLoad, StoreError> {
        let directory = self.pinned_directory()?;
        let metadata = match directory.entry_metadata(KEY_FILE_NAME) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(KeyLoad::Missing),
            Err(_) => return Ok(KeyLoad::Quarantined(QuarantineReason::KeyUnreadable)),
        };
        if metadata.is_symlink() {
            return Ok(KeyLoad::Quarantined(QuarantineReason::KeySymlink));
        }
        if !metadata.is_file() {
            return Ok(KeyLoad::Quarantined(QuarantineReason::KeyNotRegular));
        }
        if !metadata.private_file {
            return Ok(KeyLoad::Quarantined(QuarantineReason::UnsafeKeyPermissions));
        }
        let file = match directory.open_read(KEY_FILE_NAME) {
            Ok(file) => file,
            Err(_) => return Ok(KeyLoad::Quarantined(QuarantineReason::KeyUnreadable)),
        };
        let opened = match StoreDirectory::opened_metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(KeyLoad::Quarantined(QuarantineReason::KeyUnreadable)),
        };
        if !opened.is_file() || !metadata.same_file(opened) {
            return Ok(KeyLoad::Quarantined(QuarantineReason::KeyNotRegular));
        }
        if !opened.private_file {
            return Ok(KeyLoad::Quarantined(QuarantineReason::UnsafeKeyPermissions));
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(KEY_BYTES + 1));
        if (&file)
            .take((KEY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return Ok(KeyLoad::Quarantined(QuarantineReason::KeyUnreadable));
        }
        if bytes.len() != KEY_BYTES {
            return Ok(KeyLoad::Quarantined(QuarantineReason::InvalidKeyLength));
        }
        let after_read = match StoreDirectory::opened_metadata(&file) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(KeyLoad::Quarantined(QuarantineReason::KeyUnreadable)),
        };
        if !after_read.is_file() || !opened.same_file(after_read) {
            return Ok(KeyLoad::Quarantined(QuarantineReason::KeyNotRegular));
        }
        if !after_read.private_file {
            return Ok(KeyLoad::Quarantined(QuarantineReason::UnsafeKeyPermissions));
        }
        let mut key = Zeroizing::new([0_u8; KEY_BYTES]);
        key.copy_from_slice(&bytes);
        Ok(KeyLoad::Ready(key))
    }

    fn create_key_locked(&self) -> Result<Zeroizing<[u8; KEY_BYTES]>, StoreError> {
        let directory = self.pinned_directory()?;
        let mut key = Zeroizing::new([0_u8; KEY_BYTES]);
        self.backend
            .fill_random(&mut *key)
            .map_err(|()| StoreError::EntropyUnavailable)?;

        self.backend
            .checkpoint(WriteCheckpoint::BeforeCreate)
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        let mut temporary = directory
            .create_temporary()
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        set_private_file_permissions(temporary.as_file())
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterCreate)
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        temporary
            .write_all(&*key)
            .and_then(|()| temporary.flush())
            .map_err(|error| before_publication(StoreOperation::WriteTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterWrite)
            .map_err(|error| before_publication(StoreOperation::WriteTemporary, error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| before_publication(StoreOperation::SyncTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterFileSync)
            .map_err(|error| before_publication(StoreOperation::SyncTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::BeforePublish)
            .map_err(|error| before_publication(StoreOperation::Publish, error))?;

        match directory.publish_noclobber(temporary, KEY_FILE_NAME) {
            Ok(()) => {
                // A post-publication failure cannot make key replacement safe
                // to retry. The visible immutable key remains authoritative.
                let _ = self.backend.checkpoint(WriteCheckpoint::AfterPublish);
                let _ = self.backend.sync_directory(&directory);
                Ok(key)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                key.zeroize();
                match self.read_key_locked()? {
                    KeyLoad::Ready(existing) => Ok(existing),
                    KeyLoad::Missing => Err(StoreError::BeforePublication {
                        operation: StoreOperation::Publish,
                        kind: io::ErrorKind::NotFound,
                    }),
                    KeyLoad::Quarantined(reason) => Err(StoreError::Quarantined(reason)),
                }
            }
            Err(error) => Err(before_publication(StoreOperation::Publish, error)),
        }
    }

    fn commit_ledger(&self, ledger: &ApprovalLedger) -> Result<StoreCommit, StoreError> {
        ledger
            .validate()
            .map_err(|()| StoreError::InvalidTransition)?;
        let stored = StoredEnvelopeV1::from(ledger);
        let mut bytes =
            serde_json::to_vec_pretty(&stored).map_err(|_| StoreError::Serialization)?;
        bytes.push(b'\n');
        self.publish_state_bytes(&bytes)
    }

    fn publish_state_bytes(&self, bytes: &[u8]) -> Result<StoreCommit, StoreError> {
        if bytes.len() > MAX_STATE_BYTES {
            return Err(StoreError::SerializedStateTooLarge);
        }

        let directory = self.pinned_directory()?;
        self.backend
            .checkpoint(WriteCheckpoint::BeforeCreate)
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        let mut temporary = directory
            .create_temporary()
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        set_private_file_permissions(temporary.as_file())
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterCreate)
            .map_err(|error| before_publication(StoreOperation::CreateTemporary, error))?;
        temporary
            .write_all(bytes)
            .and_then(|()| temporary.flush())
            .map_err(|error| before_publication(StoreOperation::WriteTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterWrite)
            .map_err(|error| before_publication(StoreOperation::WriteTemporary, error))?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|error| before_publication(StoreOperation::SyncTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::AfterFileSync)
            .map_err(|error| before_publication(StoreOperation::SyncTemporary, error))?;
        self.backend
            .checkpoint(WriteCheckpoint::BeforePublish)
            .map_err(|error| before_publication(StoreOperation::Publish, error))?;
        directory
            .publish_replace(temporary, STATE_FILE_NAME)
            .map_err(|error| before_publication(StoreOperation::Publish, error))?;

        if self
            .backend
            .checkpoint(WriteCheckpoint::AfterPublish)
            .is_err()
        {
            return Ok(StoreCommit {
                durability: CommitDurability::Uncertain,
            });
        }
        let durability = match self.backend.sync_directory(&directory) {
            Ok(DirectorySync::Confirmed) => CommitDurability::Confirmed,
            Ok(DirectorySync::Unsupported) => CommitDurability::Unsupported,
            Err(_) => CommitDurability::Uncertain,
        };
        Ok(StoreCommit { durability })
    }

    fn acquire_lock(&self) -> Result<StoreLock, StoreError> {
        let directory = self.pinned_directory()?;
        let file = self.open_lock_file()?;
        let started = self.backend.monotonic_now();
        loop {
            match file.try_lock() {
                Ok(()) => {
                    let opened = StoreDirectory::opened_metadata(&file)
                        .map_err(|_| StoreError::UnsafeLockFile)?;
                    // A Unix lock path can be renamed while this descriptor is
                    // waiting without changing its link count. Re-resolve the
                    // permanent name only after acquisition so a replacement
                    // inode cannot create a split lock generation.
                    let entry = directory
                        .entry_metadata(LOCK_FILE_NAME)
                        .map_err(|_| StoreError::UnsafeLockFile)?;
                    if !opened.is_file()
                        || !opened.private_file
                        || !entry.is_file()
                        || !entry.private_file
                        || !opened.same_file(entry)
                    {
                        let _ = file.unlock();
                        return Err(StoreError::UnsafeLockFile);
                    }
                    return Ok(StoreLock { file });
                }
                Err(TryLockError::WouldBlock) => {
                    if self
                        .backend
                        .monotonic_now()
                        .saturating_duration_since(started)
                        >= self.lock_timeout.min(MAX_LOCK_TIMEOUT)
                    {
                        return Err(StoreError::Busy);
                    }
                    self.backend.sleep(LOCK_RETRY_INTERVAL);
                }
                Err(TryLockError::Error(error)) => {
                    return Err(before_publication(StoreOperation::Lock, error));
                }
            }
        }
    }

    fn pinned_directory(&self) -> Result<Arc<StoreDirectory>, StoreError> {
        let mut pinned = self
            .directory
            .lock()
            .map_err(|_| StoreError::DirectoryUnavailable)?;
        if let Some(directory) = pinned.as_ref() {
            validate_pinned_directory(directory)?;
            return Ok(Arc::clone(directory));
        }
        let directory = Arc::new(self.open_store_directory()?);
        *pinned = Some(Arc::clone(&directory));
        Ok(directory)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn watch_anchor(&self) -> Result<StoreDirectoryWatchAnchor, StoreError> {
        self.pinned_directory().map(StoreDirectoryWatchAnchor)
    }

    #[cfg(unix)]
    fn open_store_directory(&self) -> Result<StoreDirectory, StoreError> {
        use std::path::Component;

        let parent = self
            .paths
            .directory
            .parent()
            .ok_or(StoreError::DirectoryUnavailable)?;
        let Some(Component::Normal(directory_name)) = self.paths.directory.components().next_back()
        else {
            return Err(StoreError::DirectoryUnavailable);
        };
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|_| StoreError::DirectoryUnavailable)?;
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || !secure_parent_directory_metadata(&parent_metadata)
        {
            return Err(StoreError::UnsafeParentDirectory);
        }

        let parent_descriptor = rustix::fs::open(
            parent,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| StoreError::DirectoryUnavailable)?;
        let parent_file = File::from(parent_descriptor);
        let opened_parent = parent_file
            .metadata()
            .map_err(|_| StoreError::DirectoryUnavailable)?;
        if !opened_parent.is_dir()
            || !same_file(&parent_metadata, &opened_parent)
            || !secure_parent_directory_metadata(&opened_parent)
        {
            return Err(StoreError::UnsafeParentDirectory);
        }

        let created =
            match rustix::fs::mkdirat(&parent_file, directory_name, rustix::fs::Mode::RWXU) {
                Ok(()) => true,
                Err(rustix::io::Errno::EXIST) => false,
                Err(_) => return Err(StoreError::DirectoryUnavailable),
            };
        let entry = rustix::fs::statat(
            &parent_file,
            directory_name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| StoreError::DirectoryUnavailable)?;
        if !rustix::fs::FileType::from_raw_mode(entry.st_mode).is_dir()
            || !secure_unix_directory_stat(&entry)
        {
            return Err(StoreError::UnsafeDirectory);
        }

        let descriptor = rustix::fs::openat(
            &parent_file,
            directory_name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| StoreError::DirectoryUnavailable)?;
        let directory_file = File::from(descriptor);
        if created {
            rustix::fs::fchmod(&directory_file, rustix::fs::Mode::RWXU)
                .map_err(|_| StoreError::DirectoryUnavailable)?;
        }
        let opened =
            rustix::fs::fstat(&directory_file).map_err(|_| StoreError::DirectoryUnavailable)?;
        if !rustix::fs::FileType::from_raw_mode(opened.st_mode).is_dir()
            || !same_unix_stat(&entry, &opened)
            || !secure_unix_directory_stat(&opened)
        {
            return Err(StoreError::UnsafeDirectory);
        }

        let store_parent = StoreParent {
            descriptor: parent_file,
        };
        match self.backend.sync_store_parent(&store_parent) {
            Ok(DirectorySync::Confirmed) => {}
            Ok(DirectorySync::Unsupported) => {
                return Err(StoreError::DirectoryDurabilityUncertain);
            }
            Err(_) => return Err(StoreError::DirectoryDurabilityUncertain),
        }
        Ok(StoreDirectory {
            descriptor: directory_file,
        })
    }

    #[cfg(not(unix))]
    fn open_store_directory(&self) -> Result<StoreDirectory, StoreError> {
        let parent = self
            .paths
            .directory
            .parent()
            .ok_or(StoreError::DirectoryUnavailable)?;
        let parent_metadata =
            fs::symlink_metadata(parent).map_err(|_| StoreError::DirectoryUnavailable)?;
        if parent_metadata.file_type().is_symlink()
            || !parent_metadata.is_dir()
            || !secure_parent_directory_metadata(&parent_metadata)
        {
            return Err(StoreError::UnsafeParentDirectory);
        }

        match fs::create_dir(&self.paths.directory) {
            Ok(()) => set_private_directory_permissions(&self.paths.directory)
                .map_err(|_| StoreError::DirectoryUnavailable)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(StoreError::DirectoryUnavailable),
        }
        let metadata = fs::symlink_metadata(&self.paths.directory)
            .map_err(|_| StoreError::DirectoryUnavailable)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || !secure_directory_metadata(&metadata)
        {
            return Err(StoreError::UnsafeDirectory);
        }
        let store_parent = StoreParent { _private: () };
        match self.backend.sync_store_parent(&store_parent) {
            Ok(DirectorySync::Confirmed | DirectorySync::Unsupported) => {}
            Err(_) => return Err(StoreError::DirectoryDurabilityUncertain),
        }
        Ok(StoreDirectory {
            path: self.paths.directory.clone(),
        })
    }

    fn open_lock_file(&self) -> Result<File, StoreError> {
        let directory = self.pinned_directory()?;
        loop {
            match directory.entry_metadata(LOCK_FILE_NAME) {
                Ok(metadata) => {
                    if metadata.is_symlink() || !metadata.is_file() || !metadata.private_file {
                        return Err(StoreError::UnsafeLockFile);
                    }
                    let file = directory
                        .open_lock(LOCK_FILE_NAME, false)
                        .map_err(|error| before_publication(StoreOperation::Lock, error))?;
                    let opened = StoreDirectory::opened_metadata(&file)
                        .map_err(|error| before_publication(StoreOperation::Lock, error))?;
                    if !opened.is_file() || !metadata.same_file(opened) || !opened.private_file {
                        return Err(StoreError::UnsafeLockFile);
                    }
                    return Ok(file);
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match directory.open_lock(LOCK_FILE_NAME, true) {
                        Ok(file) => {
                            set_private_file_permissions(&file)
                                .map_err(|error| before_publication(StoreOperation::Lock, error))?;
                            let opened = StoreDirectory::opened_metadata(&file)
                                .map_err(|error| before_publication(StoreOperation::Lock, error))?;
                            if !opened.is_file() || !opened.private_file {
                                return Err(StoreError::UnsafeLockFile);
                            }
                            return Ok(file);
                        }
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                        Err(error) => {
                            return Err(before_publication(StoreOperation::Lock, error));
                        }
                    }
                }
                Err(error) => return Err(before_publication(StoreOperation::Lock, error)),
            }
        }
    }
}

impl fmt::Debug for ApprovalStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalStore")
            .field("paths", &self.paths)
            .field("lock_timeout", &self.lock_timeout)
            .finish_non_exhaustive()
    }
}

struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WriteCheckpoint {
    BeforeCreate,
    AfterCreate,
    AfterWrite,
    AfterFileSync,
    BeforePublish,
    AfterPublish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DirectorySync {
    Confirmed,
    Unsupported,
}

trait StoreBackend: Send + Sync {
    fn fill_random(&self, output: &mut [u8]) -> Result<(), ()>;
    fn monotonic_now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
    fn checkpoint(&self, _checkpoint: WriteCheckpoint) -> io::Result<()> {
        Ok(())
    }
    fn sync_directory(&self, directory: &StoreDirectory) -> io::Result<DirectorySync>;
    fn sync_store_parent(&self, parent: &StoreParent) -> io::Result<DirectorySync>;
}

struct SystemBackend;

impl StoreBackend for SystemBackend {
    fn fill_random(&self, output: &mut [u8]) -> Result<(), ()> {
        getrandom::fill(output).map_err(|_| ())
    }

    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        thread::sleep(duration);
    }

    #[cfg(unix)]
    fn sync_directory(&self, directory: &StoreDirectory) -> io::Result<DirectorySync> {
        directory.descriptor.sync_all()?;
        Ok(DirectorySync::Confirmed)
    }

    #[cfg(not(unix))]
    fn sync_directory(&self, _directory: &StoreDirectory) -> io::Result<DirectorySync> {
        // std does not expose a reliable portable parent-directory flush on
        // Windows. Callers may observe state, but reserve never releases a
        // permit at this durability level.
        Ok(DirectorySync::Unsupported)
    }

    #[cfg(unix)]
    fn sync_store_parent(&self, parent: &StoreParent) -> io::Result<DirectorySync> {
        parent.descriptor.sync_all()?;
        Ok(DirectorySync::Confirmed)
    }

    #[cfg(not(unix))]
    fn sync_store_parent(&self, _parent: &StoreParent) -> io::Result<DirectorySync> {
        Ok(DirectorySync::Unsupported)
    }
}

enum LockedLoad {
    Missing {
        key: KeyLoad,
    },
    Ready {
        ledger: ApprovalLedger,
        key: Zeroizing<[u8; KEY_BYTES]>,
    },
    Quarantined(QuarantineReason),
}

enum StateLoad {
    Missing,
    Ready(ApprovalLedger),
    Quarantined(QuarantineReason),
}

enum KeyLoad {
    Missing,
    Ready(Zeroizing<[u8; KEY_BYTES]>),
    Quarantined(QuarantineReason),
}

type MutableLoad = (ApprovalLedger, Zeroizing<[u8; KEY_BYTES]>);

#[derive(Clone, Default, Eq, PartialEq)]
struct ApprovalLedger {
    last_issued_counter: Option<u128>,
    approvals: Vec<RoutedApprovalState>,
}

impl ApprovalLedger {
    fn sort(&mut self) {
        self.approvals
            .sort_by_key(|approval| approval.fingerprint.0);
    }

    fn validate(&self) -> Result<(), ()> {
        if self.approvals.len() > MAX_APPROVALS {
            return Err(());
        }
        if self.last_issued_counter == Some(0) {
            return Err(());
        }
        let mut fingerprints = BTreeSet::new();
        let mut active_count = 0_usize;
        for approval in &self.approvals {
            if !fingerprints.insert(approval.fingerprint.0)
                || approval.empty_run_streak > MAX_EMPTY_RUN_STREAK
            {
                return Err(());
            }
            let local_counter = approval.last_issued_run_id.map(run_counter);
            if local_counter == Some(0)
                || local_counter.is_some_and(|counter| {
                    self.last_issued_counter
                        .is_none_or(|global| counter > global)
                })
            {
                return Err(());
            }
            if let Some(active) = approval.active {
                active_count += 1;
                let expected_expiry = approval
                    .last_observed_time
                    .saturating_add(RESERVATION_LEASE);
                let minimum_automatic_not_before = approval
                    .last_observed_time
                    .saturating_add(MAX_AUTOMATIC_COOLDOWN);
                if approval.last_issued_run_id != Some(active.run_id)
                    || self.last_issued_counter != Some(run_counter(active.run_id))
                    || active.expires_at != expected_expiry
                    || active.expires_at <= approval.last_observed_time
                    || approval.automatic_not_before < minimum_automatic_not_before
                    || active.expires_at > approval.automatic_not_before
                    || active.previous_automatic_not_before > approval.automatic_not_before
                {
                    return Err(());
                }
            }
        }
        if active_count > 1 {
            return Err(());
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredEnvelopeV1 {
    schema_version: u32,
    last_issued_run_id: Option<[u8; 16]>,
    approvals: Vec<StoredApprovalV1>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredApprovalV1 {
    fingerprint: [u8; 32],
    empty_run_streak: u8,
    automatic_not_before_seconds: u64,
    last_observed_time_seconds: u64,
    last_issued_run_id: Option<[u8; 16]>,
    active: Option<StoredActiveReservationV1>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredActiveReservationV1 {
    run_id: [u8; 16],
    trigger: StoredScanTriggerV1,
    expires_at_seconds: u64,
    previous_automatic_not_before_seconds: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredScanTriggerV1 {
    Automatic,
    ExplicitRefresh,
}

impl TryFrom<StoredEnvelopeV1> for ApprovalLedger {
    type Error = ();

    fn try_from(stored: StoredEnvelopeV1) -> Result<Self, Self::Error> {
        if stored.schema_version != STATE_SCHEMA_VERSION || stored.approvals.len() > MAX_APPROVALS {
            return Err(());
        }
        let mut ledger = Self {
            last_issued_counter: stored.last_issued_run_id.map(u128::from_be_bytes),
            approvals: stored
                .approvals
                .into_iter()
                .map(|approval| RoutedApprovalState {
                    fingerprint: RouteFingerprint(approval.fingerprint),
                    empty_run_streak: approval.empty_run_streak,
                    automatic_not_before: RoutedPolicyTime::from_seconds(
                        approval.automatic_not_before_seconds,
                    ),
                    last_observed_time: RoutedPolicyTime::from_seconds(
                        approval.last_observed_time_seconds,
                    ),
                    last_issued_run_id: approval.last_issued_run_id.map(RoutedRunId),
                    active: approval.active.map(|active| ActiveReservation {
                        run_id: RoutedRunId(active.run_id),
                        trigger: match active.trigger {
                            StoredScanTriggerV1::Automatic => RoutedScanTrigger::Automatic,
                            StoredScanTriggerV1::ExplicitRefresh => {
                                RoutedScanTrigger::ExplicitRefresh
                            }
                        },
                        expires_at: RoutedPolicyTime::from_seconds(active.expires_at_seconds),
                        previous_automatic_not_before: RoutedPolicyTime::from_seconds(
                            active.previous_automatic_not_before_seconds,
                        ),
                    }),
                })
                .collect(),
        };
        ledger.validate()?;
        ledger.sort();
        Ok(ledger)
    }
}

impl From<&ApprovalLedger> for StoredEnvelopeV1 {
    fn from(ledger: &ApprovalLedger) -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            last_issued_run_id: ledger.last_issued_counter.map(u128::to_be_bytes),
            approvals: ledger
                .approvals
                .iter()
                .map(|approval| StoredApprovalV1 {
                    fingerprint: approval.fingerprint.0,
                    empty_run_streak: approval.empty_run_streak,
                    automatic_not_before_seconds: approval.automatic_not_before.as_seconds(),
                    last_observed_time_seconds: approval.last_observed_time.as_seconds(),
                    last_issued_run_id: approval.last_issued_run_id.map(|run_id| run_id.0),
                    active: approval.active.map(|active| StoredActiveReservationV1 {
                        run_id: active.run_id.0,
                        trigger: match active.trigger {
                            RoutedScanTrigger::Automatic => StoredScanTriggerV1::Automatic,
                            RoutedScanTrigger::ExplicitRefresh => {
                                StoredScanTriggerV1::ExplicitRefresh
                            }
                        },
                        expires_at_seconds: active.expires_at.as_seconds(),
                        previous_automatic_not_before_seconds: active
                            .previous_automatic_not_before
                            .as_seconds(),
                    }),
                })
                .collect(),
        }
    }
}

fn before_publication(operation: StoreOperation, error: io::Error) -> StoreError {
    StoreError::BeforePublication {
        operation,
        kind: error.kind(),
    }
}

fn run_counter(run_id: RoutedRunId) -> u128 {
    u128::from_be_bytes(run_id.0)
}

fn key_binding(key: &[u8; KEY_BYTES]) -> [u8; 32] {
    *blake3::keyed_hash(key, KEY_BINDING_DOMAIN).as_bytes()
}

fn verify_proposal_key(
    proposal: &StoredRoutedProposal,
    key: &[u8; KEY_BYTES],
) -> Result<(), StoreError> {
    if proposal.key_binding == key_binding(key) {
        Ok(())
    } else {
        Err(StoreError::StaleProposal)
    }
}

/// Clear a globally expired reservation in the tentative transaction. Return
/// true only while a reservation still blocks all new starts.
fn has_unexpired_active(ledger: &mut ApprovalLedger, now: RoutedPolicyTime) -> bool {
    for approval in &mut ledger.approvals {
        let Some(active) = approval.active else {
            continue;
        };
        let effective_now = now.max(approval.last_observed_time);
        if effective_now < active.expires_at {
            return true;
        }
        approval.last_observed_time = effective_now;
        approval.active = None;
    }
    false
}

#[cfg(unix)]
fn validate_pinned_directory(directory: &StoreDirectory) -> Result<(), StoreError> {
    let metadata =
        rustix::fs::fstat(&directory.descriptor).map_err(|_| StoreError::DirectoryUnavailable)?;
    if rustix::fs::FileType::from_raw_mode(metadata.st_mode).is_dir()
        && secure_unix_directory_stat(&metadata)
    {
        Ok(())
    } else {
        Err(StoreError::UnsafeDirectory)
    }
}

#[cfg(not(unix))]
fn validate_pinned_directory(directory: &StoreDirectory) -> Result<(), StoreError> {
    let metadata =
        fs::symlink_metadata(&directory.path).map_err(|_| StoreError::DirectoryUnavailable)?;
    if !metadata.file_type().is_symlink()
        && metadata.is_dir()
        && secure_directory_metadata(&metadata)
    {
        Ok(())
    } else {
        Err(StoreError::UnsafeDirectory)
    }
}

#[cfg(unix)]
fn secure_unix_directory_stat(metadata: &rustix::fs::Stat) -> bool {
    metadata.st_uid == rustix::process::geteuid().as_raw() && metadata.st_mode & 0o7777 == 0o700
}

#[cfg(unix)]
fn same_unix_stat(first: &rustix::fs::Stat, second: &rustix::fs::Stat) -> bool {
    first.st_dev == second.st_dev && first.st_ino == second.st_ino
}

#[cfg(not(unix))]
fn secure_file_metadata(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(not(unix))]
fn secure_directory_metadata(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn secure_parent_directory_metadata(metadata: &fs::Metadata) -> bool {
    metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode() & 0o022 == 0
}

#[cfg(not(unix))]
fn secure_parent_directory_metadata(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn same_file(before: &fs::Metadata, opened: &fs::Metadata) -> bool {
    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
fn same_file(_before: &fs::Metadata, _opened: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn set_private_file_permissions(file: &File) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_private_file_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_directory: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn configure_lock_sharing(options: &mut OpenOptions) {
    // Deny delete sharing so the stable lock path cannot be replaced while a
    // cooperative Balun process has it open.
    options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_lock_sharing(_options: &mut OpenOptions) {}

#[cfg(test)]
mod tests {
    use std::env;
    use std::fs::OpenOptions;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    use std::process::Command;
    #[cfg(target_os = "linux")]
    use std::sync::Barrier;
    use std::sync::Mutex;
    #[cfg(target_os = "linux")]
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ipnet::IpNet;
    use tempfile::TempDir;

    use super::super::super::routes::{
        InterfaceId, InterfaceKind, NetworkInterface, NetworkRoute, RouteKind, RouteScope,
        select_route_candidates,
    };
    use super::*;

    const LOCK_CHILD_ENV: &str = "BALUN_APPROVAL_STORE_LOCK_CHILD";
    const LOCK_CHILD_TEST: &str = "discovery::approval::store::tests::approval_store_lock_child";

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum TestDirectorySync {
        Confirmed,
        Unsupported,
        Fail,
    }

    struct TestBackend {
        entropy_byte: u8,
        entropy_calls: AtomicUsize,
        fail_at: Mutex<Option<WriteCheckpoint>>,
        directory_sync: Mutex<TestDirectorySync>,
        store_parent_sync: Mutex<TestDirectorySync>,
    }

    impl TestBackend {
        fn new(entropy_byte: u8) -> Self {
            Self {
                entropy_byte,
                entropy_calls: AtomicUsize::new(0),
                fail_at: Mutex::new(None),
                directory_sync: Mutex::new(TestDirectorySync::Confirmed),
                store_parent_sync: Mutex::new(TestDirectorySync::Confirmed),
            }
        }

        fn fail_once(&self, checkpoint: WriteCheckpoint) {
            *self.fail_at.lock().expect("test failpoint lock") = Some(checkpoint);
        }

        fn set_directory_sync(&self, mode: TestDirectorySync) {
            *self.directory_sync.lock().expect("test directory lock") = mode;
        }

        fn set_store_parent_sync(&self, mode: TestDirectorySync) {
            *self
                .store_parent_sync
                .lock()
                .expect("test parent directory lock") = mode;
        }
    }

    impl StoreBackend for TestBackend {
        fn fill_random(&self, output: &mut [u8]) -> Result<(), ()> {
            self.entropy_calls.fetch_add(1, Ordering::SeqCst);
            output.fill(self.entropy_byte);
            Ok(())
        }

        fn monotonic_now(&self) -> Instant {
            Instant::now()
        }

        fn sleep(&self, duration: Duration) {
            thread::sleep(duration);
        }

        fn checkpoint(&self, checkpoint: WriteCheckpoint) -> io::Result<()> {
            let mut fail_at = self.fail_at.lock().expect("test failpoint lock");
            if *fail_at == Some(checkpoint) {
                *fail_at = None;
                Err(io::Error::other("injected approval-store failure"))
            } else {
                Ok(())
            }
        }

        fn sync_directory(&self, _directory: &StoreDirectory) -> io::Result<DirectorySync> {
            match *self.directory_sync.lock().expect("test directory lock") {
                TestDirectorySync::Confirmed => Ok(DirectorySync::Confirmed),
                TestDirectorySync::Unsupported => Ok(DirectorySync::Unsupported),
                TestDirectorySync::Fail => Err(io::Error::other("injected directory-sync failure")),
            }
        }

        fn sync_store_parent(&self, _parent: &StoreParent) -> io::Result<DirectorySync> {
            match *self
                .store_parent_sync
                .lock()
                .expect("test parent directory lock")
            {
                TestDirectorySync::Confirmed => Ok(DirectorySync::Confirmed),
                TestDirectorySync::Unsupported => Ok(DirectorySync::Unsupported),
                TestDirectorySync::Fail => {
                    Err(io::Error::other("injected store-parent sync failure"))
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    struct LockWaitBackend {
        first_wait: AtomicBool,
        waiter_entered: Barrier,
        resume_waiter: Barrier,
    }

    #[cfg(target_os = "linux")]
    impl LockWaitBackend {
        fn new() -> Self {
            Self {
                first_wait: AtomicBool::new(false),
                waiter_entered: Barrier::new(2),
                resume_waiter: Barrier::new(2),
            }
        }

        fn wait_until_contended(&self) {
            self.waiter_entered.wait();
        }

        fn resume(&self) {
            self.resume_waiter.wait();
        }
    }

    #[cfg(target_os = "linux")]
    impl StoreBackend for LockWaitBackend {
        fn fill_random(&self, output: &mut [u8]) -> Result<(), ()> {
            output.fill(0x5a);
            Ok(())
        }

        fn monotonic_now(&self) -> Instant {
            Instant::now()
        }

        fn sleep(&self, duration: Duration) {
            if self
                .first_wait
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                self.waiter_entered.wait();
                self.resume_waiter.wait();
            }
            thread::sleep(duration);
        }

        fn sync_directory(&self, _directory: &StoreDirectory) -> io::Result<DirectorySync> {
            Ok(DirectorySync::Confirmed)
        }

        fn sync_store_parent(&self, _parent: &StoreParent) -> io::Result<DirectorySync> {
            Ok(DirectorySync::Confirmed)
        }
    }

    fn test_store() -> (TempDir, ApprovalStore, Arc<TestBackend>) {
        let temporary = tempfile::tempdir().expect("test directory");
        let backend = Arc::new(TestBackend::new(0x5a));
        let store = ApprovalStore::with_backend(
            StorePaths::new(temporary.path().join("private")),
            Duration::from_millis(100),
            backend.clone(),
        );
        (temporary, store, backend)
    }

    fn ipnet(value: &str) -> IpNet {
        value.parse().expect("valid test network")
    }

    fn snapshot(destination: &str, interface_id: u64) -> RouteSnapshot {
        RouteSnapshot::from_effective_routes(
            vec![NetworkInterface::new(
                InterfaceId::new(interface_id),
                "private-test-tunnel",
                InterfaceKind::Tunnel,
                true,
                [ipnet("10.250.0.2/32")],
            )],
            vec![NetworkRoute::effective(
                ipnet(destination),
                Some(InterfaceId::new(interface_id)),
                RouteKind::Unicast,
                RouteScope::OnLink,
            )],
        )
    }

    fn proposal(
        store: &ApprovalStore,
        destination: &str,
        interface_id: u64,
    ) -> StoredRoutedProposal {
        let snapshot = snapshot(destination, interface_id);
        let candidates = select_route_candidates(&snapshot, &[]).expect("route candidates");
        store
            .build_proposal(
                &snapshot,
                &candidates,
                ProbeConfig::default(),
                RoutedScanConfig::default(),
            )
            .expect("stored proposal")
    }

    fn approve(store: &ApprovalStore, proposal: &StoredRoutedProposal, second: u64) -> StoreCommit {
        store
            .approve(proposal, RoutedPolicyTime::from_seconds(second))
            .expect("approval commit")
    }

    fn write_private(path: &Path, bytes: &[u8]) {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path).expect("open private test file");
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .expect("private test permissions");
        file.write_all(bytes).expect("write private test file");
        file.sync_all().expect("sync private test file");
    }

    fn read(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read test file")
    }

    fn initialize_key(store: &ApprovalStore) {
        let _lock = store.acquire_lock().expect("store lock");
        store
            .load_or_create_key_locked()
            .expect("initialized test key");
    }

    fn commit_direct(store: &ApprovalStore, ledger: &ApprovalLedger) -> StoreCommit {
        let _lock = store.acquire_lock().expect("store lock");
        store.commit_ledger(ledger).expect("direct commit")
    }

    fn state_for(fingerprint: u8, counter: Option<u128>) -> RoutedApprovalState {
        RoutedApprovalState {
            fingerprint: RouteFingerprint([fingerprint; 32]),
            empty_run_streak: 0,
            automatic_not_before: RoutedPolicyTime::from_seconds(10),
            last_observed_time: RoutedPolicyTime::from_seconds(10),
            last_issued_run_id: counter.map(RoutedRunId::from_counter),
            active: None,
        }
    }

    fn assert_quarantined(store: &ApprovalStore, expected: QuarantineReason) {
        assert_eq!(
            store.load().expect("load quarantine status"),
            ApprovalStoreStatus::Quarantined(expected)
        );
    }

    #[test]
    fn strict_round_trip_persists_no_topology() {
        let (_temporary, store, backend) = test_store();
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        assert_eq!(proposal.summary().candidate_count(), 2);
        let commit = approve(&store, &proposal, 100);
        assert_eq!(commit.durability(), CommitDurability::Confirmed);
        assert_eq!(backend.entropy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: false,
            }
        );

        let persisted = String::from_utf8(read(&store.paths.state())).unwrap();
        assert!(!persisted.contains("private-test-tunnel"));
        assert!(!persisted.contains("172.31"));
        assert!(!persisted.contains("10.250"));
        assert!(persisted.contains("fingerprint"));
        assert_eq!(read(&store.paths.key()), vec![0x5a; KEY_BYTES]);
    }

    #[test]
    fn corrupt_state_is_preserved_and_cannot_create_a_key() {
        let (_temporary, store, backend) = test_store();
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Missing {
                key_initialized: false
            }
        );
        let corrupt = b"{ definitely-not-json";
        write_private(&store.paths.state(), corrupt);

        let _lock = store.acquire_lock().unwrap();
        assert_eq!(
            store.load_or_create_key_locked().unwrap_err(),
            StoreError::Quarantined(QuarantineReason::InvalidState)
        );
        drop(_lock);
        assert_eq!(read(&store.paths.state()), corrupt);
        assert!(!store.paths.key().exists());
        assert_eq!(backend.entropy_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn exact_size_limit_is_accepted_and_one_more_byte_is_quarantined() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let mut bytes = serde_json::to_vec(&StoredEnvelopeV1 {
            schema_version: STATE_SCHEMA_VERSION,
            last_issued_run_id: None,
            approvals: Vec::new(),
        })
        .unwrap();
        bytes.resize(MAX_STATE_BYTES, b' ');
        write_private(&store.paths.state(), &bytes);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 0,
                has_active_reservation: false,
            }
        );

        bytes.push(b' ');
        write_private(&store.paths.state(), &bytes);
        assert_quarantined(&store, QuarantineReason::StateTooLarge);
        assert_eq!(read(&store.paths.state()), bytes);
    }

    #[test]
    fn unknown_duplicate_malformed_and_future_json_are_quarantined_in_place() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let cases: &[&[u8]] = &[
            br#"{"schema_version":1,"last_issued_run_id":null,"approvals":[],"extra":true}"#,
            br#"{"schema_version":1,"schema_version":1,"last_issued_run_id":null,"approvals":[]}"#,
            br#"{"schema_version":2,"last_issued_run_id":null,"approvals":[]}"#,
            br#"{"schema_version":1,"last_issued_run_id":null,"approvals":[]"#,
        ];
        for bytes in cases {
            write_private(&store.paths.state(), bytes);
            assert_quarantined(&store, QuarantineReason::InvalidState);
            assert_eq!(read(&store.paths.state()), *bytes);
        }
    }

    #[test]
    fn nested_unknown_duplicate_and_enum_fields_are_strict() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let mut value = serde_json::to_value(StoredEnvelopeV1::from(&ApprovalLedger {
            last_issued_counter: None,
            approvals: vec![state_for(1, None)],
        }))
        .unwrap();
        value["approvals"][0]["future_field"] = serde_json::Value::Bool(true);
        let unknown_nested = serde_json::to_vec(&value).unwrap();

        let ordinary = serde_json::to_string(&StoredEnvelopeV1::from(&ApprovalLedger {
            last_issued_counter: None,
            approvals: vec![state_for(1, None)],
        }))
        .unwrap();
        let duplicate_nested = ordinary.replacen(
            "\"empty_run_streak\":0",
            "\"empty_run_streak\":0,\"empty_run_streak\":0",
            1,
        );

        let active = ApprovalLedger {
            last_issued_counter: Some(1),
            approvals: vec![RoutedApprovalState {
                fingerprint: RouteFingerprint([1; 32]),
                empty_run_streak: 0,
                automatic_not_before: RoutedPolicyTime::from_seconds(100),
                last_observed_time: RoutedPolicyTime::from_seconds(10),
                last_issued_run_id: Some(RoutedRunId::from_counter(1)),
                active: Some(ActiveReservation {
                    run_id: RoutedRunId::from_counter(1),
                    trigger: RoutedScanTrigger::Automatic,
                    expires_at: RoutedPolicyTime::from_seconds(70),
                    previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
                }),
            }],
        };
        let unknown_enum = serde_json::to_string(&StoredEnvelopeV1::from(&active))
            .unwrap()
            .replacen("\"automatic\"", "\"future_trigger\"", 1);

        for bytes in [
            unknown_nested,
            duplicate_nested.into_bytes(),
            unknown_enum.into_bytes(),
        ] {
            write_private(&store.paths.state(), &bytes);
            assert_quarantined(&store, QuarantineReason::InvalidState);
            assert_eq!(read(&store.paths.state()), bytes);
        }
    }

    #[test]
    fn duplicate_fingerprints_and_multiple_active_runs_are_semantically_invalid() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);

        let duplicate = ApprovalLedger {
            last_issued_counter: None,
            approvals: vec![state_for(1, None), state_for(1, None)],
        };
        let bytes = serde_json::to_vec(&StoredEnvelopeV1::from(&duplicate)).unwrap();
        write_private(&store.paths.state(), &bytes);
        assert_quarantined(&store, QuarantineReason::InvalidState);

        let active = |fingerprint: u8, run: u128| RoutedApprovalState {
            fingerprint: RouteFingerprint([fingerprint; 32]),
            empty_run_streak: 0,
            automatic_not_before: RoutedPolicyTime::from_seconds(100),
            last_observed_time: RoutedPolicyTime::from_seconds(10),
            last_issued_run_id: Some(RoutedRunId::from_counter(run)),
            active: Some(ActiveReservation {
                run_id: RoutedRunId::from_counter(run),
                trigger: RoutedScanTrigger::Automatic,
                expires_at: RoutedPolicyTime::from_seconds(70),
                previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
            }),
        };
        let two_active = ApprovalLedger {
            last_issued_counter: Some(2),
            approvals: vec![active(1, 1), active(2, 2)],
        };
        let bytes = serde_json::to_vec(&StoredEnvelopeV1::from(&two_active)).unwrap();
        write_private(&store.paths.state(), &bytes);
        assert_quarantined(&store, QuarantineReason::InvalidState);
    }

    #[test]
    fn invalid_streak_run_high_water_and_active_semantics_are_rejected() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let mut invalid = state_for(1, Some(2));
        invalid.empty_run_streak = MAX_EMPTY_RUN_STREAK + 1;
        let cases = [
            ApprovalLedger {
                last_issued_counter: Some(2),
                approvals: vec![invalid],
            },
            ApprovalLedger {
                last_issued_counter: Some(1),
                approvals: vec![state_for(1, Some(2))],
            },
            ApprovalLedger {
                last_issued_counter: Some(1),
                approvals: vec![RoutedApprovalState {
                    automatic_not_before: RoutedPolicyTime::from_seconds(1_810),
                    active: Some(ActiveReservation {
                        run_id: RoutedRunId::from_counter(1),
                        trigger: RoutedScanTrigger::Automatic,
                        expires_at: RoutedPolicyTime::from_seconds(10),
                        previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
                    }),
                    ..state_for(1, Some(1))
                }],
            },
            ApprovalLedger {
                last_issued_counter: Some(2),
                approvals: vec![RoutedApprovalState {
                    automatic_not_before: RoutedPolicyTime::from_seconds(1_810),
                    active: Some(ActiveReservation {
                        run_id: RoutedRunId::from_counter(1),
                        trigger: RoutedScanTrigger::Automatic,
                        expires_at: RoutedPolicyTime::from_seconds(70),
                        previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
                    }),
                    ..state_for(1, Some(1))
                }],
            },
            ApprovalLedger {
                last_issued_counter: Some(1),
                approvals: vec![RoutedApprovalState {
                    automatic_not_before: RoutedPolicyTime::from_seconds(1_810),
                    active: Some(ActiveReservation {
                        run_id: RoutedRunId::from_counter(1),
                        trigger: RoutedScanTrigger::Automatic,
                        expires_at: RoutedPolicyTime::from_seconds(69),
                        previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
                    }),
                    ..state_for(1, Some(1))
                }],
            },
            ApprovalLedger {
                last_issued_counter: Some(1),
                approvals: vec![RoutedApprovalState {
                    automatic_not_before: RoutedPolicyTime::from_seconds(70),
                    active: Some(ActiveReservation {
                        run_id: RoutedRunId::from_counter(1),
                        trigger: RoutedScanTrigger::Automatic,
                        expires_at: RoutedPolicyTime::from_seconds(70),
                        previous_automatic_not_before: RoutedPolicyTime::from_seconds(10),
                    }),
                    ..state_for(1, Some(1))
                }],
            },
        ];
        for ledger in cases {
            let bytes = serde_json::to_vec(&StoredEnvelopeV1::from(&ledger)).unwrap();
            write_private(&store.paths.state(), &bytes);
            assert_quarantined(&store, QuarantineReason::InvalidState);
        }

        let zero_global = ApprovalLedger {
            last_issued_counter: Some(0),
            approvals: Vec::new(),
        };
        let zero_local = ApprovalLedger {
            last_issued_counter: Some(1),
            approvals: vec![state_for(1, Some(0))],
        };
        for ledger in [zero_global, zero_local] {
            let bytes = serde_json::to_vec(&StoredEnvelopeV1::from(&ledger)).unwrap();
            write_private(&store.paths.state(), &bytes);
            assert_quarantined(&store, QuarantineReason::InvalidState);
        }
    }

    #[test]
    fn a_key_is_generated_once_and_files_are_private_on_unix() {
        let (_temporary, store, backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        let second = proposal(&store, "172.31.90.8/30", 7);
        assert_eq!(first.key_binding, second.key_binding);
        assert_eq!(backend.entropy_calls.load(Ordering::SeqCst), 1);
        approve(&store, &first, 10);

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&store.paths.directory).unwrap().mode() & 0o7777,
                0o700
            );
            for path in [store.paths.key(), store.paths.lock(), store.paths.state()] {
                assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o600);
            }
        }
    }

    #[test]
    fn store_directory_parent_must_be_confirmed_before_authority_work() {
        let (_temporary, store, backend) = test_store();
        backend.set_store_parent_sync(TestDirectorySync::Fail);
        assert_eq!(
            store.load().unwrap_err(),
            StoreError::DirectoryDurabilityUncertain
        );
        assert!(store.paths.directory.is_dir());
        assert!(!store.paths.lock().exists());
        assert!(!store.paths.key().exists());
        assert!(!store.paths.state().exists());

        backend.set_store_parent_sync(TestDirectorySync::Confirmed);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Missing {
                key_initialized: false,
            }
        );
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        assert!(approve(&store, &proposal, 10).is_confirmed());
        assert!(matches!(
            store
                .reserve(
                    proposal,
                    RoutedScanTrigger::Automatic,
                    RoutedPolicyTime::from_seconds(10)
                )
                .unwrap(),
            StoredBeginDecision::Permitted(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn group_or_world_writable_store_parent_is_rejected_before_child_creation() {
        let (temporary, store, _backend) = test_store();
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o770)).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::UnsafeParentDirectory);
        assert!(!store.paths.directory.exists());
        fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn invalid_key_is_quarantined_and_never_replaced() {
        let (_temporary, store, backend) = test_store();
        store.load().unwrap();
        let bytes = vec![0x44; KEY_BYTES - 1];
        write_private(&store.paths.key(), &bytes);
        assert_quarantined(&store, QuarantineReason::InvalidKeyLength);
        let _lock = store.acquire_lock().unwrap();
        assert_eq!(
            store.load_or_create_key_locked().unwrap_err(),
            StoreError::Quarantined(QuarantineReason::InvalidKeyLength)
        );
        drop(_lock);
        assert_eq!(read(&store.paths.key()), bytes);
        assert_eq!(backend.entropy_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn either_half_of_an_initialized_store_missing_is_quarantined() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let state = read(&store.paths.state());
        fs::remove_file(store.paths.key()).unwrap();
        assert_quarantined(&store, QuarantineReason::MissingKey);
        assert_eq!(read(&store.paths.state()), state);

        write_private(&store.paths.key(), &[0x5a; KEY_BYTES]);
        fs::remove_file(store.paths.state()).unwrap();
        assert_quarantined(&store, QuarantineReason::MissingState);
        assert_eq!(read(&store.paths.key()), vec![0x5a; KEY_BYTES]);
    }

    #[test]
    fn an_unconfirmed_key_publish_cannot_lead_to_a_permit() {
        let (_temporary, store, backend) = test_store();
        backend.set_directory_sync(TestDirectorySync::Fail);
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        assert_eq!(backend.entropy_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            approve(&store, &proposal, 10).durability(),
            CommitDurability::Uncertain
        );
        assert!(matches!(
            store
                .reserve(
                    proposal,
                    RoutedScanTrigger::Automatic,
                    RoutedPolicyTime::from_seconds(10)
                )
                .unwrap(),
            StoredBeginDecision::PublishedWithoutPermit {
                durability: CommitDurability::Uncertain
            }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_unsafe_permissions_are_quarantined() {
        use std::os::unix::fs::symlink;

        let (_temporary, store, _backend) = test_store();
        store.load().unwrap();
        let outside = store.paths.directory.join("outside");
        write_private(&outside, &[0x22; KEY_BYTES]);
        symlink(&outside, store.paths.key()).unwrap();
        assert_quarantined(&store, QuarantineReason::KeySymlink);
        fs::remove_file(store.paths.key()).unwrap();

        write_private(&store.paths.key(), &[0x22; KEY_BYTES]);
        fs::set_permissions(store.paths.key(), fs::Permissions::from_mode(0o640)).unwrap();
        assert_quarantined(&store, QuarantineReason::UnsafeKeyPermissions);
    }

    #[cfg(unix)]
    #[test]
    fn state_symlinks_nonregular_files_and_unsafe_permissions_are_quarantined() {
        use std::os::unix::fs::symlink;

        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let valid = read(&store.paths.state());
        fs::remove_file(store.paths.state()).unwrap();
        let outside = store.paths.directory.join("outside-state");
        write_private(&outside, &valid);
        symlink(&outside, store.paths.state()).unwrap();
        assert_quarantined(&store, QuarantineReason::StateSymlink);

        fs::remove_file(store.paths.state()).unwrap();
        write_private(&store.paths.state(), &valid);
        fs::set_permissions(store.paths.state(), fs::Permissions::from_mode(0o640)).unwrap();
        assert_quarantined(&store, QuarantineReason::UnsafeStatePermissions);

        fs::remove_file(store.paths.state()).unwrap();
        fs::create_dir(store.paths.state()).unwrap();
        assert_quarantined(&store, QuarantineReason::StateNotRegular);
    }

    #[cfg(unix)]
    #[test]
    fn permanent_store_files_with_hard_link_aliases_are_refused() {
        let (temporary, store, _backend) = test_store();
        initialize_key(&store);

        let state_alias = temporary.path().join("state-alias");
        fs::hard_link(store.paths.state(), &state_alias).unwrap();
        assert_quarantined(&store, QuarantineReason::UnsafeStatePermissions);
        fs::remove_file(state_alias).unwrap();

        let key_alias = temporary.path().join("key-alias");
        fs::hard_link(store.paths.key(), &key_alias).unwrap();
        assert_quarantined(&store, QuarantineReason::UnsafeKeyPermissions);
        fs::remove_file(key_alias).unwrap();

        let lock_alias = temporary.path().join("lock-alias");
        fs::hard_link(store.paths.lock(), &lock_alias).unwrap();
        assert_eq!(store.load().unwrap_err(), StoreError::UnsafeLockFile);
    }

    #[cfg(unix)]
    #[test]
    fn pathname_replacement_cannot_redirect_a_pinned_store() {
        let (temporary, store, _backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        assert!(approve(&store, &first, 10).is_confirmed());

        let injected_path = store.paths.directory.clone();
        let pinned_path = temporary.path().join("renamed-pinned-store");
        fs::rename(&injected_path, &pinned_path).unwrap();
        fs::create_dir(&injected_path).unwrap();
        fs::set_permissions(&injected_path, fs::Permissions::from_mode(0o700)).unwrap();
        write_private(&injected_path.join(KEY_FILE_NAME), &[0x33; KEY_BYTES]);
        let replacement_state = b"{ replacement topology }";
        write_private(&injected_path.join(STATE_FILE_NAME), replacement_state);

        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: false,
            }
        );
        let second = proposal(&store, "172.31.91.8/30", 8);
        assert!(approve(&store, &second, 20).is_confirmed());
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 2,
                has_active_reservation: false,
            }
        );

        // The replacement path is never opened, locked, or published through.
        assert_eq!(
            read(&injected_path.join(STATE_FILE_NAME)),
            replacement_state
        );
        assert!(!injected_path.join(LOCK_FILE_NAME).exists());
        assert_ne!(read(&pinned_path.join(STATE_FILE_NAME)), replacement_state);
    }

    #[test]
    fn every_prepublication_failure_preserves_the_previous_state() {
        let (_temporary, store, backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &first, 10);
        let previous = read(&store.paths.state());
        let second = proposal(&store, "172.31.91.8/30", 8);

        for checkpoint in [
            WriteCheckpoint::BeforeCreate,
            WriteCheckpoint::AfterCreate,
            WriteCheckpoint::AfterWrite,
            WriteCheckpoint::AfterFileSync,
            WriteCheckpoint::BeforePublish,
        ] {
            backend.fail_once(checkpoint);
            assert!(matches!(
                store.approve(&second, RoutedPolicyTime::from_seconds(20)),
                Err(StoreError::BeforePublication { .. })
            ));
            assert_eq!(read(&store.paths.state()), previous);
            assert_eq!(
                store.load().unwrap(),
                ApprovalStoreStatus::Ready {
                    approval_count: 1,
                    has_active_reservation: false,
                }
            );
        }
    }

    #[test]
    fn postpublication_failures_are_visible_but_never_reported_confirmed() {
        let (_temporary, store, backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &first, 10);
        let previous = read(&store.paths.state());
        let second = proposal(&store, "172.31.91.8/30", 8);

        backend.fail_once(WriteCheckpoint::AfterPublish);
        let commit = approve(&store, &second, 20);
        assert_eq!(commit.durability(), CommitDurability::Uncertain);
        assert_ne!(read(&store.paths.state()), previous);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 2,
                has_active_reservation: false,
            }
        );

        backend.set_directory_sync(TestDirectorySync::Fail);
        let third = proposal(&store, "172.31.92.8/30", 9);
        let commit = approve(&store, &third, 30);
        assert_eq!(commit.durability(), CommitDurability::Uncertain);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 3,
                has_active_reservation: false,
            }
        );
    }

    #[test]
    fn existing_state_is_atomically_replaced_without_growing_the_ledger() {
        let (_temporary, store, _backend) = test_store();
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &proposal, 10);
        let previous = read(&store.paths.state());
        approve(&store, &proposal, 20);
        let replaced = read(&store.paths.state());
        assert_ne!(replaced, previous);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: false,
            }
        );
    }

    #[test]
    fn serialized_mutations_keep_one_global_reservation_and_monotonic_runs() {
        let (temporary, store, backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        let second = proposal(&store, "172.31.91.8/30", 8);
        approve(&store, &first, 10);
        approve(&store, &second, 10);

        let first_permit = match store
            .reserve(
                first,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected first permit, got {decision:?}"),
        };
        assert_eq!(run_counter(first_permit.run_id()), 1);

        let reopened = ApprovalStore::with_backend(
            StorePaths::new(temporary.path().join("private")),
            Duration::from_millis(100),
            backend.clone(),
        );
        let blocked = proposal(&reopened, "172.31.91.8/30", 8);
        assert!(matches!(
            reopened
                .reserve(
                    blocked,
                    RoutedScanTrigger::ExplicitRefresh,
                    RoutedPolicyTime::from_seconds(11)
                )
                .unwrap(),
            StoredBeginDecision::Busy
        ));
        assert!(matches!(
            reopened
                .complete(
                    first_permit.fingerprint(),
                    first_permit.run_id(),
                    RoutedScanOutcome::Found,
                    RoutedPolicyTime::from_seconds(11)
                )
                .unwrap(),
            StoredCompletionDecision::Confirmed(RoutedCompletionDecision::Applied { .. })
        ));

        let second = proposal(&reopened, "172.31.91.8/30", 8);
        let second_permit = match reopened
            .reserve(
                second,
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(12),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected second permit, got {decision:?}"),
        };
        assert_eq!(run_counter(second_permit.run_id()), 2);
        reopened
            .complete(
                second_permit.fingerprint(),
                second_permit.run_id(),
                RoutedScanOutcome::Found,
                RoutedPolicyTime::from_seconds(13),
            )
            .unwrap();

        let reapproved = proposal(&reopened, "172.31.90.8/30", 7);
        approve(&reopened, &reapproved, 100);
        let permit = match reopened
            .reserve(
                reapproved,
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(100),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected reapproved permit, got {decision:?}"),
        };
        assert_eq!(run_counter(permit.run_id()), 3);
    }

    #[test]
    fn exhausted_global_run_counter_never_wraps_or_rewrites_state() {
        let (_temporary, store, _backend) = test_store();
        let initial = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &initial, 10);
        {
            let _lock = store.acquire_lock().unwrap();
            let LockedLoad::Ready {
                mut ledger,
                key: _key,
            } = store.load_locked().unwrap()
            else {
                panic!("initialized ledger");
            };
            ledger.last_issued_counter = Some(u128::MAX);
            ledger.approvals[0].last_issued_run_id = Some(RoutedRunId::from_counter(u128::MAX));
            assert!(store.commit_ledger(&ledger).unwrap().is_confirmed());
        }
        let previous = read(&store.paths.state());
        let current = proposal(&store, "172.31.90.8/30", 7);
        assert_eq!(
            store
                .reserve(
                    current,
                    RoutedScanTrigger::ExplicitRefresh,
                    RoutedPolicyTime::from_seconds(10)
                )
                .unwrap_err(),
            StoreError::RunCounterExhausted
        );
        assert_eq!(read(&store.paths.state()), previous);
    }

    #[test]
    fn stale_and_expired_completions_are_exact_and_persisted() {
        let (_temporary, store, _backend) = test_store();
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        let fingerprint = proposal.proposal.fingerprint;
        approve(&store, &proposal, 10);
        assert_eq!(
            store
                .complete(
                    fingerprint,
                    RoutedRunId::from_counter(99),
                    RoutedScanOutcome::Found,
                    RoutedPolicyTime::from_seconds(10)
                )
                .unwrap(),
            StoredCompletionDecision::Stale
        );
        let permit = match store
            .reserve(
                proposal,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected permit, got {decision:?}"),
        };
        assert!(matches!(
            store
                .complete(
                    permit.fingerprint(),
                    permit.run_id(),
                    RoutedScanOutcome::CompleteEmpty,
                    permit.expires_at()
                )
                .unwrap(),
            StoredCompletionDecision::Confirmed(RoutedCompletionDecision::Expired)
        ));
        assert_eq!(
            store
                .complete(
                    permit.fingerprint(),
                    permit.run_id(),
                    RoutedScanOutcome::Found,
                    permit.expires_at()
                )
                .unwrap(),
            StoredCompletionDecision::Stale
        );
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: false,
            }
        );
    }

    #[test]
    fn fingerprint_scopes_completion_across_complete_store_reinitialization() {
        let (temporary, store, _backend) = test_store();
        let old = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &old, 10);
        let old_permit = match store
            .reserve(
                old,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected old permit, got {decision:?}"),
        };

        // Simulate out-of-protocol deletion of the complete authority pair.
        // A new random installation key scopes the otherwise reused counter.
        fs::remove_file(store.paths.key()).unwrap();
        fs::remove_file(store.paths.state()).unwrap();
        let replacement_backend = Arc::new(TestBackend::new(0x6b));
        let replacement_store = ApprovalStore::with_backend(
            StorePaths::new(temporary.path().join("private")),
            Duration::from_millis(100),
            replacement_backend,
        );
        let replacement = proposal(&replacement_store, "172.31.90.8/30", 7);
        approve(&replacement_store, &replacement, 20);
        let replacement_permit = match replacement_store
            .reserve(
                replacement,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(20),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected replacement permit, got {decision:?}"),
        };
        assert_eq!(run_counter(old_permit.run_id()), 1);
        assert_eq!(run_counter(replacement_permit.run_id()), 1);
        assert!(old_permit.fingerprint() != replacement_permit.fingerprint());
        assert_eq!(
            store
                .complete(
                    old_permit.fingerprint(),
                    old_permit.run_id(),
                    RoutedScanOutcome::Found,
                    RoutedPolicyTime::from_seconds(21),
                )
                .unwrap(),
            StoredCompletionDecision::Stale
        );
        assert_eq!(
            replacement_store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: true,
            }
        );
    }

    #[test]
    fn unsupported_or_uncertain_durability_never_releases_a_permit() {
        for mode in [TestDirectorySync::Unsupported, TestDirectorySync::Fail] {
            let (_temporary, store, backend) = test_store();
            let proposal = proposal(&store, "172.31.90.8/30", 7);
            approve(&store, &proposal, 10);
            backend.set_directory_sync(mode);
            let decision = store
                .reserve(
                    proposal,
                    RoutedScanTrigger::Automatic,
                    RoutedPolicyTime::from_seconds(10),
                )
                .unwrap();
            let expected = match mode {
                TestDirectorySync::Unsupported => CommitDurability::Unsupported,
                TestDirectorySync::Fail => CommitDurability::Uncertain,
                TestDirectorySync::Confirmed => unreachable!(),
            };
            assert!(matches!(
                decision,
                StoredBeginDecision::PublishedWithoutPermit { durability }
                    if durability == expected
            ));
            assert_eq!(
                store.load().unwrap(),
                ApprovalStoreStatus::Ready {
                    approval_count: 1,
                    has_active_reservation: true,
                }
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn system_backend_withholds_permits_without_directory_durability() {
        let temporary = tempfile::tempdir().unwrap();
        let store = ApprovalStore::new(StorePaths::new(temporary.path().join("private")));
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        assert_eq!(
            approve(&store, &proposal, 10).durability(),
            CommitDurability::Unsupported
        );
        assert!(matches!(
            store
                .reserve(
                    proposal,
                    RoutedScanTrigger::Automatic,
                    RoutedPolicyTime::from_seconds(10)
                )
                .unwrap(),
            StoredBeginDecision::PublishedWithoutPermit {
                durability: CommitDurability::Unsupported
            }
        ));
    }

    #[test]
    fn approval_limit_refuses_ninth_unique_fingerprint_without_eviction() {
        let (_temporary, store, _backend) = test_store();
        for index in 0_u8..MAX_APPROVALS as u8 {
            let third_octet = 100_u8 + index;
            let destination = format!("172.31.{third_octet}.8/30");
            let proposal = proposal(&store, &destination, u64::from(index) + 1);
            approve(&store, &proposal, u64::from(index));
        }
        let previous = read(&store.paths.state());
        let ninth = proposal(&store, "172.31.200.8/30", 99);
        assert_eq!(
            store
                .approve(&ninth, RoutedPolicyTime::from_seconds(100))
                .unwrap_err(),
            StoreError::ApprovalLimit
        );
        assert_eq!(read(&store.paths.state()), previous);
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: MAX_APPROVALS,
                has_active_reservation: false,
            }
        );
    }

    #[test]
    fn revoke_is_exact_and_retains_the_global_high_water() {
        let (_temporary, store, _backend) = test_store();
        let initial = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &initial, 10);
        let permit = match store
            .reserve(
                initial,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected permit, got {decision:?}"),
        };
        store
            .complete(
                permit.fingerprint(),
                permit.run_id(),
                RoutedScanOutcome::Found,
                RoutedPolicyTime::from_seconds(11),
            )
            .unwrap();
        let current = proposal(&store, "172.31.90.8/30", 7);
        assert!(matches!(
            store.revoke(&current).unwrap(),
            StoredRevokeDecision::Published(commit) if commit.is_confirmed()
        ));
        assert_eq!(
            store.revoke(&current).unwrap(),
            StoredRevokeDecision::NotFound
        );
        approve(&store, &current, 100);
        let permit = match store
            .reserve(
                current,
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(100),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected permit after revoke, got {decision:?}"),
        };
        assert_eq!(run_counter(permit.run_id()), 2);
    }

    #[test]
    fn store_owned_revalidation_requires_the_exact_active_reservation() {
        let (_temporary, store, _backend) = test_store();
        let approved = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &approved, 10);
        let permit = match store
            .reserve(
                approved,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected permit, got {decision:?}"),
        };
        let fresh = snapshot("172.31.90.8/30", 7);
        let revalidated = store
            .revalidate_permit(permit, &fresh, RoutedPolicyTime::from_seconds(11))
            .unwrap();
        assert_eq!(run_counter(revalidated.run_id()), 1);
        assert_eq!(revalidated.targets().len(), 2);
    }

    #[test]
    fn revoke_before_revalidation_consumes_no_network_authority() {
        let (_temporary, store, _backend) = test_store();
        let approved = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &approved, 10);
        let permit = match store
            .reserve(
                approved,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected permit, got {decision:?}"),
        };
        let current = proposal(&store, "172.31.90.8/30", 7);
        assert!(matches!(
            store.revoke(&current).unwrap(),
            StoredRevokeDecision::Published(commit) if commit.is_confirmed()
        ));
        let fresh = snapshot("172.31.90.8/30", 7);
        assert_eq!(
            store
                .revalidate_permit(permit, &fresh, RoutedPolicyTime::from_seconds(11))
                .unwrap_err(),
            StoredRevalidationError::AuthorityChanged
        );
    }

    #[test]
    fn revoke_all_needs_no_topology_and_preserves_the_global_run_counter() {
        let (_temporary, store, _backend) = test_store();
        let first = proposal(&store, "172.31.90.8/30", 7);
        approve(&store, &first, 10);
        let first_permit = match store
            .reserve(
                first,
                RoutedScanTrigger::Automatic,
                RoutedPolicyTime::from_seconds(10),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected first permit, got {decision:?}"),
        };
        assert_eq!(run_counter(first_permit.run_id()), 1);
        assert!(matches!(
            store.revoke_all().unwrap(),
            StoredRevokeDecision::Published(commit) if commit.is_confirmed()
        ));
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 0,
                has_active_reservation: false,
            }
        );

        let replacement = proposal(&store, "172.31.91.8/30", 8);
        approve(&store, &replacement, 20);
        let second_permit = match store
            .reserve(
                replacement,
                RoutedScanTrigger::ExplicitRefresh,
                RoutedPolicyTime::from_seconds(20),
            )
            .unwrap()
        {
            StoredBeginDecision::Permitted(permit) => permit,
            decision => panic!("expected second permit, got {decision:?}"),
        };
        assert_eq!(run_counter(second_permit.run_id()), 2);
    }

    #[test]
    fn debug_output_redacts_paths_keys_and_topology() {
        let (_temporary, store, _backend) = test_store();
        let proposal = proposal(&store, "172.31.90.8/30", 7);
        let store_debug = format!("{store:?}");
        let proposal_debug = format!("{proposal:?}");
        assert!(!store_debug.contains("private"));
        assert!(!proposal_debug.contains("172.31"));
        assert!(!proposal_debug.contains("private-test-tunnel"));
        assert!(!proposal_debug.contains(&"5a".repeat(KEY_BYTES)));
    }

    #[test]
    fn approval_store_lock_child() {
        let Ok(root) = env::var(LOCK_CHILD_ENV) else {
            return;
        };
        let root = PathBuf::from(root);
        let store = ApprovalStore::new(StorePaths::new(root.join("private")));
        let _lock = store.acquire_lock().expect("child acquires store lock");
        fs::write(root.join("ready"), b"ready").expect("write ready signal");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("release").exists() {
            assert!(
                Instant::now() < deadline,
                "parent did not release child lock"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn permanent_lock_serializes_distinct_processes() {
        let temporary = tempfile::tempdir().unwrap();
        let mut child = Command::new(env::current_exe().unwrap())
            .args(["--exact", LOCK_CHILD_TEST, "--nocapture"])
            .env(LOCK_CHILD_ENV, temporary.path())
            .spawn()
            .expect("spawn lock child");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !temporary.path().join("ready").exists() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("lock child did not become ready");
            }
            thread::sleep(Duration::from_millis(10));
        }

        let backend = Arc::new(TestBackend::new(0x5a));
        let store = ApprovalStore::with_backend(
            StorePaths::new(temporary.path().join("private")),
            Duration::from_millis(30),
            backend,
        );
        assert_eq!(store.load().unwrap_err(), StoreError::Busy);
        fs::write(temporary.path().join("release"), b"release").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(matches!(
            store.load(),
            Ok(ApprovalStoreStatus::Missing { .. })
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn lock_replacement_while_waiting_is_refused_after_acquisition() {
        let temporary = tempfile::tempdir().unwrap();
        let backend = Arc::new(LockWaitBackend::new());
        let store = Arc::new(ApprovalStore::with_backend(
            StorePaths::new(temporary.path().join("private")),
            Duration::from_secs(2),
            backend.clone(),
        ));
        assert!(matches!(
            store.load(),
            Ok(ApprovalStoreStatus::Missing { .. })
        ));

        let lock_path = store.paths.lock();
        let holder = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        holder.try_lock().unwrap();

        let waiter_store = Arc::clone(&store);
        let waiter = thread::spawn(move || waiter_store.load());
        backend.wait_until_contended();

        let displaced_lock = store.paths.directory.join("displaced-lock");
        fs::rename(&lock_path, displaced_lock).unwrap();
        write_private(&lock_path, b"");
        drop(holder);
        backend.resume();

        assert_eq!(
            waiter.join().expect("lock waiter thread").unwrap_err(),
            StoreError::UnsafeLockFile
        );
    }

    #[test]
    fn oversized_serialization_is_rejected_before_temp_creation() {
        let (_temporary, store, backend) = test_store();
        initialize_key(&store);
        backend.fail_once(WriteCheckpoint::BeforeCreate);
        let _lock = store.acquire_lock().unwrap();
        assert_eq!(
            store
                .publish_state_bytes(&vec![b'x'; MAX_STATE_BYTES + 1])
                .unwrap_err(),
            StoreError::SerializedStateTooLarge
        );
        // The failpoint was not consumed because the cap precedes temp IO.
        assert!(backend.fail_at.lock().unwrap().is_some());
        drop(_lock);
    }

    #[test]
    fn direct_replace_helper_is_confirmed() {
        let (_temporary, store, _backend) = test_store();
        initialize_key(&store);
        let first = ApprovalLedger {
            last_issued_counter: None,
            approvals: vec![state_for(1, None)],
        };
        assert!(commit_direct(&store, &first).is_confirmed());
        let second = ApprovalLedger {
            last_issued_counter: Some(7),
            approvals: vec![state_for(2, Some(7))],
        };
        assert!(commit_direct(&store, &second).is_confirmed());
        assert_eq!(
            store.load().unwrap(),
            ApprovalStoreStatus::Ready {
                approval_count: 1,
                has_active_reservation: false,
            }
        );
    }
}
