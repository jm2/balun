//! Fail-closed observation of the durable routed-approval store.
//!
//! The state file is atomically replaced, so observing that inode would lose
//! coverage at the first successful commit. This component instead observes
//! the store's private directory and classifies only the three permanent final
//! entry names. Temporary-file traffic is ignored; any create, delete, move,
//! modification, close-after-write, or attribute change involving a permanent
//! entry invalidates authority.
//!
//! Observation is deliberately separate from store I/O. A caller establishes
//! a baseline by subscribing first, draining pending events, reading and
//! revalidating the store, and then completing a second drain-to-`EAGAIN`
//! barrier. A permanent-entry event during that sandwich rejects the baseline.
//! Balun's own atomic publications are not silently exempted: after such a
//! write the caller must establish a new baseline and reread the exact record
//! it expects. This prevents an external writer from hiding behind an
//! "expected self-write" suppression window.
//!
//! A production controller must keep polling the returned observer on its
//! dedicated worker for the lifetime of any healthy authority epoch. Dropping
//! the observer, a read failure, queue overflow, watch removal, unmount,
//! malformed event, unsupported event, or bounded-drain exhaustion invalidates
//! immediately and permanently fails that observer closed.
//!
//! # Not an authority boundary yet
//!
//! This module is an unwired Linux building block. It subscribes through the
//! exact private-directory descriptor retained by [`ApprovalStore`](super::store::ApprovalStore),
//! and all Unix store I/O is relative to that descriptor. Pathname replacement
//! or a mount over the injected path therefore cannot split the watched and
//! accessed authority topologies. Requiring one link before and after each
//! permanent-file read rejects persistent hard-link aliases. Linux metadata and
//! a directory-only inotify watch still cannot detect every mutation by a
//! hostile same-UID process, including writes through a retained writable
//! mapping or through an external hard-link alias recreated from a retained
//! descriptor after the baseline. That actor, and privileged mount-namespace
//! replacement, remain outside this cooperative per-user storage boundary.
//!
//! The monitored runner owns a distinct store observer session, cancels and
//! joins its old actor after every publication of its own, performs the exact
//! post-publication reread through a fresh sandwich, and only then activates
//! combined route-and-store health. Nothing outside that runner may treat a
//! file notification as anything but a reason to rebaseline.

#![allow(
    dead_code,
    reason = "the monitored routed runner has no production caller until the approval UX lands"
)]

use std::fmt;
use std::sync::{Arc, Weak};

use super::store::{KEY_FILE_NAME, LOCK_FILE_NAME, STATE_FILE_NAME};

/// A deliberately small, topology-free failure surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StoreWatchError {
    /// The private directory could not be subscribed safely.
    Subscribe,
    /// Observation was lost or an event could not be interpreted safely.
    FailedClosed,
    /// A permanent store entry changed during the revalidation sandwich.
    ChangedDuringBaseline,
    /// A baseline token came from another observer incarnation.
    ForeignBaseline,
}

impl fmt::Display for StoreWatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Subscribe => "the approval-store directory could not be observed safely",
            Self::FailedClosed => "approval-store observation failed closed",
            Self::ChangedDuringBaseline => {
                "the approval store changed while its baseline was being revalidated"
            }
            Self::ForeignBaseline => "the approval-store baseline belongs to another observer",
        })
    }
}

impl std::error::Error for StoreWatchError {}

/// Error from the complete subscribe/drain/revalidate/drain sandwich.
///
/// Debug and display deliberately omit the injected revalidation error so an
/// accidentally topology-bearing backend error cannot enter ordinary logs
/// through this boundary. The caller can still pattern-match and handle it.
pub(super) enum StoreBaselineError<E> {
    Observation(StoreWatchError),
    Revalidation(E),
}

impl<E> fmt::Debug for StoreBaselineError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(error) => formatter.debug_tuple("Observation").field(error).finish(),
            Self::Revalidation(_) => formatter.write_str("Revalidation(<redacted>)"),
        }
    }
}

impl<E> fmt::Display for StoreBaselineError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(error) => fmt::Display::fmt(error, formatter),
            Self::Revalidation(_) => {
                formatter.write_str("approval-store baseline revalidation failed")
            }
        }
    }
}

impl<E> std::error::Error for StoreBaselineError<E> where E: 'static {}

/// Result of one complete nonblocking drain to `EAGAIN`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StoreWatchPoll {
    Quiet,
    Invalidated,
}

/// Non-cloneable proof that the pre-revalidation drain happened on this
/// observer incarnation at one exact generation.
pub(super) struct StoreBaselineToken {
    source: Weak<ObserverIdentity>,
    generation: u64,
}

impl fmt::Debug for StoreBaselineToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreBaselineToken(<redacted>)")
    }
}

/// Non-cloneable proof of a successful subscribe/read/drain ordering barrier.
///
/// This is not standalone network authority. The controller must use it only
/// to install a healthy approval-store invalidation epoch, and must keep the
/// originating observer alive and continuously polled while that epoch exists.
pub(super) struct StoreBaselineProof {
    source: Weak<ObserverIdentity>,
    generation: u64,
}

impl fmt::Debug for StoreBaselineProof {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreBaselineProof(<redacted>)")
    }
}

struct ObserverIdentity;

/// Synchronous fail-closed notification for one exact observer incarnation.
///
/// Implementations must be nonblocking, nonpanicking, idempotent, and scoped to
/// the observer incarnation which owns them. A sink can be invoked repeatedly,
/// including from event draining, terminal failure, and observer drop.
pub(super) trait StoreInvalidationSink: Send + Sync {
    fn invalidate(&self);
}

impl<F> StoreInvalidationSink for F
where
    F: Fn() + Send + Sync,
{
    fn invalidate(&self) {
        self();
    }
}

/// Backend contract used by both Linux inotify and deterministic tests.
///
/// One call must read until the nonblocking source reports `EAGAIN`. A backend
/// must return `DrainLimit` instead of waiting forever under continuous event
/// injection. Every event read before an error is delivered to `emit` first.
pub(super) trait DirectoryWatchBackend: Send {
    fn drain(&mut self, emit: &mut dyn FnMut(StoreDirectoryEvent)) -> Result<(), BackendFailure>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BackendFailure {
    Read,
    DrainLimit,
}

/// One kernel event normalized without retaining or formatting a path.
pub(super) struct StoreDirectoryEvent {
    expected_watch: bool,
    name: Option<Vec<u8>>,
    flags: StoreEventFlags,
}

impl fmt::Debug for StoreDirectoryEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreDirectoryEvent(<redacted>)")
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct StoreEventFlags(u32);

impl StoreEventFlags {
    const NONE: Self = Self(0);
    const ATTRIB: Self = Self(1 << 0);
    const CLOSE_WRITE: Self = Self(1 << 1);
    const CREATE: Self = Self(1 << 2);
    const DELETE: Self = Self(1 << 3);
    const MODIFY: Self = Self(1 << 4);
    const MOVED_FROM: Self = Self(1 << 5);
    const MOVED_TO: Self = Self(1 << 6);
    const DELETE_SELF: Self = Self(1 << 7);
    const MOVE_SELF: Self = Self(1 << 8);
    const QUEUE_OVERFLOW: Self = Self(1 << 9);
    const IGNORED: Self = Self(1 << 10);
    const UNMOUNT: Self = Self(1 << 11);
    const IS_DIRECTORY: Self = Self(1 << 12);
    const UNSUPPORTED: Self = Self(1 << 13);

    const ENTRY_CHANGES: Self = Self(
        Self::ATTRIB.0
            | Self::CLOSE_WRITE.0
            | Self::CREATE.0
            | Self::DELETE.0
            | Self::MODIFY.0
            | Self::MOVED_FROM.0
            | Self::MOVED_TO.0,
    );
    const DIRECTORY_FAILURES: Self = Self(Self::DELETE_SELF.0 | Self::MOVE_SELF.0);
    const TERMINAL_FAILURES: Self =
        Self(Self::QUEUE_OVERFLOW.0 | Self::IGNORED.0 | Self::UNMOUNT.0 | Self::UNSUPPORTED.0);
    const KNOWN: Self = Self(
        Self::ENTRY_CHANGES.0
            | Self::DIRECTORY_FAILURES.0
            | Self::TERMINAL_FAILURES.0
            | Self::IS_DIRECTORY.0,
    );

    const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    const fn has_unknown_bits(self) -> bool {
        self.0 & !Self::KNOWN.0 != 0
    }
}

impl fmt::Debug for StoreEventFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StoreEventFlags(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EventDisposition {
    Ignore,
    Invalidate,
    Poison,
}

fn classify_event(event: &StoreDirectoryEvent) -> EventDisposition {
    if event.flags.intersects(StoreEventFlags::TERMINAL_FAILURES)
        || event.flags.intersects(StoreEventFlags::DIRECTORY_FAILURES)
        || event.flags.has_unknown_bits()
        || !event.expected_watch
    {
        return EventDisposition::Poison;
    }

    if !event.flags.intersects(StoreEventFlags::ENTRY_CHANGES) {
        // An annotation such as IS_DIRECTORY without an action is malformed
        // for the event stream this backend requested.
        return EventDisposition::Poison;
    }

    let Some(name) = event.name.as_deref() else {
        // Entry actions must name the entry when they originate from a
        // directory watch. Refuse an event that cannot be classified exactly.
        return EventDisposition::Poison;
    };

    // Cooperative store transactions open the permanent lock read/write for
    // cross-process locking but never write its contents. Linux reports a
    // CLOSE_WRITE when that descriptor closes even though no authority bytes
    // changed. MODIFY, ATTRIB, or any other action remains invalidating.
    if name == LOCK_FILE_NAME.as_bytes() && event.flags == StoreEventFlags::CLOSE_WRITE {
        EventDisposition::Ignore
    } else if is_permanent_store_entry(name) {
        EventDisposition::Invalidate
    } else {
        EventDisposition::Ignore
    }
}

fn is_permanent_store_entry(name: &[u8]) -> bool {
    name == STATE_FILE_NAME.as_bytes()
        || name == KEY_FILE_NAME.as_bytes()
        || name == LOCK_FILE_NAME.as_bytes()
}

/// One subscribed private-directory observer.
///
/// This type is intentionally non-cloneable. Its invalidation sink normally
/// targets a dedicated approval-store hub rather than the independent route
/// observer hub. Dropping it always invalidates, including orderly shutdown.
pub(super) struct ApprovalStoreObserver<B, I>
where
    B: DirectoryWatchBackend,
    I: StoreInvalidationSink,
{
    backend: B,
    invalidator: I,
    identity: Arc<ObserverIdentity>,
    generation: u64,
    failed_closed: bool,
}

impl<B, I> ApprovalStoreObserver<B, I>
where
    B: DirectoryWatchBackend,
    I: StoreInvalidationSink,
{
    fn with_backend(backend: B, invalidator: I) -> Self {
        Self {
            backend,
            invalidator,
            identity: Arc::new(ObserverIdentity),
            generation: 0,
            failed_closed: false,
        }
    }

    /// Drain all currently queued notifications without blocking.
    ///
    /// Relevant events invoke the sink while they are being drained, before
    /// this method returns. Backend or classification faults poison the
    /// observer and invoke the sink before returning an error.
    fn poll(&mut self) -> Result<StoreWatchPoll, StoreWatchError> {
        if self.failed_closed {
            return Err(StoreWatchError::FailedClosed);
        }

        let starting_generation = self.generation;
        let generation = &mut self.generation;
        let failed_closed = &mut self.failed_closed;
        let invalidator = &self.invalidator;

        let result = self.backend.drain(&mut |event| {
            if *failed_closed {
                return;
            }
            match classify_event(&event) {
                EventDisposition::Ignore => {}
                EventDisposition::Invalidate => {
                    note_invalidation(generation, failed_closed, invalidator);
                }
                EventDisposition::Poison => {
                    poison_observer(generation, failed_closed, invalidator);
                }
            }
        });

        if result.is_err() {
            poison_observer(
                &mut self.generation,
                &mut self.failed_closed,
                &self.invalidator,
            );
        }
        if self.failed_closed {
            return Err(StoreWatchError::FailedClosed);
        }

        Ok(if self.generation == starting_generation {
            StoreWatchPoll::Quiet
        } else {
            StoreWatchPoll::Invalidated
        })
    }

    /// Drain events queued since subscription, then start one revalidation
    /// sandwich. The caller performs its store read only after this succeeds.
    fn begin_baseline(&mut self) -> Result<StoreBaselineToken, StoreWatchError> {
        self.poll()?;
        Ok(StoreBaselineToken {
            source: Arc::downgrade(&self.identity),
            generation: self.generation,
        })
    }

    /// Finish the post-read drain barrier.
    ///
    /// An ordinary permanent-entry change rejects only this baseline; the
    /// observer remains usable for a fresh read/retry. Observation loss is
    /// terminal and returns `FailedClosed` instead.
    fn finish_baseline(
        &mut self,
        token: StoreBaselineToken,
    ) -> Result<StoreBaselineProof, StoreWatchError> {
        if !token.source.ptr_eq(&Arc::downgrade(&self.identity)) {
            poison_observer(
                &mut self.generation,
                &mut self.failed_closed,
                &self.invalidator,
            );
            return Err(StoreWatchError::ForeignBaseline);
        }

        self.poll()?;
        if token.generation != self.generation {
            return Err(StoreWatchError::ChangedDuringBaseline);
        }

        Ok(StoreBaselineProof {
            source: Arc::downgrade(&self.identity),
            generation: self.generation,
        })
    }

    /// Establish a coherent store baseline around one caller-supplied reread.
    ///
    /// This is the production entry point for baseline creation: it makes it
    /// impossible to accidentally perform the store read before subscription
    /// or omit the post-read drain barrier. If the revalidation callback
    /// panics, a local guard poisons this observer before unwinding.
    pub(super) fn revalidate_with<R, E>(
        &mut self,
        revalidate: impl FnOnce() -> Result<R, E>,
    ) -> Result<(R, StoreBaselineProof), StoreBaselineError<E>> {
        let token = self
            .begin_baseline()
            .map_err(StoreBaselineError::Observation)?;

        let result = {
            let mut panic_guard = BaselinePanicGuard {
                generation: &mut self.generation,
                failed_closed: &mut self.failed_closed,
                invalidator: &self.invalidator,
                armed: true,
            };
            let result = revalidate();
            panic_guard.armed = false;
            result
        };

        let value = match result {
            Ok(value) => value,
            Err(error) => {
                self.poll().map_err(StoreBaselineError::Observation)?;
                return Err(StoreBaselineError::Revalidation(error));
            }
        };
        let proof = self
            .finish_baseline(token)
            .map_err(StoreBaselineError::Observation)?;
        Ok((value, proof))
    }

    /// Test whether a proof still belongs to this locally observed generation.
    ///
    /// This does not poll the backend and therefore must not be used as a
    /// pre-send authority check. Continuous polling drives the invalidation
    /// hub; the future runner must also retain that hub registration.
    fn proof_is_current(&self, proof: &StoreBaselineProof) -> bool {
        !self.failed_closed
            && proof.source.ptr_eq(&Arc::downgrade(&self.identity))
            && proof.generation == self.generation
    }
}

struct BaselinePanicGuard<'a, I>
where
    I: StoreInvalidationSink,
{
    generation: &'a mut u64,
    failed_closed: &'a mut bool,
    invalidator: &'a I,
    armed: bool,
}

impl<I> Drop for BaselinePanicGuard<'_, I>
where
    I: StoreInvalidationSink,
{
    fn drop(&mut self) {
        if self.armed {
            poison_observer(self.generation, self.failed_closed, self.invalidator);
        }
    }
}

impl<B, I> fmt::Debug for ApprovalStoreObserver<B, I>
where
    B: DirectoryWatchBackend,
    I: StoreInvalidationSink,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApprovalStoreObserver(<redacted>)")
    }
}

impl<B, I> Drop for ApprovalStoreObserver<B, I>
where
    B: DirectoryWatchBackend,
    I: StoreInvalidationSink,
{
    fn drop(&mut self) {
        // Observer exit is itself an observation failure. Do not rely on the
        // worker/controller to remember a separate shutdown invalidation.
        self.invalidator.invalidate();
        self.failed_closed = true;
    }
}

fn note_invalidation<I>(generation: &mut u64, failed_closed: &mut bool, invalidator: &I)
where
    I: StoreInvalidationSink,
{
    match generation.checked_add(1) {
        Some(next) => {
            *generation = next;
            invalidator.invalidate();
        }
        None => poison_observer(generation, failed_closed, invalidator),
    }
}

fn poison_observer<I>(generation: &mut u64, failed_closed: &mut bool, invalidator: &I)
where
    I: StoreInvalidationSink,
{
    if *failed_closed {
        return;
    }
    if let Some(next) = generation.checked_add(1) {
        *generation = next;
    }
    *failed_closed = true;
    invalidator.invalidate();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::convert::Infallible;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, BorrowedFd};

    use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
    use rustix::io::Errno;
    use tokio::io::Interest;
    use tokio::io::unix::AsyncFd;

    use super::{
        ApprovalStoreObserver, BackendFailure, DirectoryWatchBackend, StoreBaselineProof,
        StoreDirectoryEvent, StoreEventFlags, StoreInvalidationSink, StoreWatchError,
        StoreWatchPoll,
    };
    use crate::discovery::approval::store::StoreDirectoryWatchAnchor;

    // A kernel event name is bounded far below this buffer. Refill until
    // EAGAIN, but poison instead of spinning indefinitely under active abuse.
    const READ_BUFFER_BYTES: usize = 16 * 1024;
    const MAX_EVENTS_PER_DRAIN: usize = 4_096;

    enum LinuxWatchDescriptor {
        Synchronous(rustix::fd::OwnedFd),
        Async(AsyncFd<rustix::fd::OwnedFd>),
        Poisoned,
    }

    pub(in crate::discovery::approval) struct LinuxDirectoryWatch {
        descriptor: LinuxWatchDescriptor,
        watch_descriptor: i32,
        _directory: StoreDirectoryWatchAnchor,
        buffer: [MaybeUninit<u8>; READ_BUFFER_BYTES],
    }

    impl std::fmt::Debug for LinuxDirectoryWatch {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("LinuxDirectoryWatch(<redacted>)")
        }
    }

    impl LinuxDirectoryWatch {
        fn subscribe(directory: StoreDirectoryWatchAnchor) -> Result<Self, StoreWatchError> {
            let watch_path = directory.proc_fd_path();
            if !directory.path_matches_pinned_identity(&watch_path) {
                return Err(StoreWatchError::Subscribe);
            }
            let descriptor = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
                .map_err(|_| StoreWatchError::Subscribe)?;
            let watch_descriptor = inotify::add_watch(
                &descriptor,
                &watch_path,
                WatchFlags::ATTRIB
                    | WatchFlags::CLOSE_WRITE
                    | WatchFlags::CREATE
                    | WatchFlags::DELETE
                    | WatchFlags::DELETE_SELF
                    | WatchFlags::MODIFY
                    | WatchFlags::MOVED_FROM
                    | WatchFlags::MOVED_TO
                    | WatchFlags::MOVE_SELF
                    | WatchFlags::DONT_FOLLOW
                    | WatchFlags::EXCL_UNLINK
                    | WatchFlags::ONLYDIR,
            )
            .map_err(|_| StoreWatchError::Subscribe)?;
            if !directory.path_matches_pinned_identity(&watch_path) {
                return Err(StoreWatchError::Subscribe);
            }

            Ok(Self {
                descriptor: LinuxWatchDescriptor::Synchronous(descriptor),
                watch_descriptor,
                _directory: directory,
                buffer: [MaybeUninit::uninit(); READ_BUFFER_BYTES],
            })
        }

        fn arm_async_readiness(&mut self) -> Result<(), BackendFailure> {
            if matches!(self.descriptor, LinuxWatchDescriptor::Async(_)) {
                return Ok(());
            }
            let LinuxWatchDescriptor::Synchronous(descriptor) =
                std::mem::replace(&mut self.descriptor, LinuxWatchDescriptor::Poisoned)
            else {
                return Err(BackendFailure::Read);
            };

            // Avoid AsyncFd's documented no-runtime panic. This method is
            // first called while the consuming actor future is being polled.
            tokio::runtime::Handle::try_current().map_err(|_| BackendFailure::Read)?;
            let descriptor = match AsyncFd::try_with_interest(descriptor, Interest::READABLE) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    let (_descriptor, _cause) = error.into_parts();
                    return Err(BackendFailure::Read);
                }
            };
            self.descriptor = LinuxWatchDescriptor::Async(descriptor);
            Ok(())
        }

        async fn wait_and_drain(
            &mut self,
            emit: &mut (dyn FnMut(StoreDirectoryEvent) + Send),
        ) -> Result<(), BackendFailure> {
            self.wait_and_drain_after_ready(|| {}, emit).await
        }

        async fn wait_and_drain_after_ready(
            &mut self,
            after_ready: impl FnOnce() + Send,
            emit: &mut (dyn FnMut(StoreDirectoryEvent) + Send),
        ) -> Result<(), BackendFailure> {
            let LinuxWatchDescriptor::Async(descriptor) = &self.descriptor else {
                return Err(BackendFailure::Read);
            };
            let mut readiness = descriptor
                .readable()
                .await
                .map_err(|_| BackendFailure::Read)?;

            // Keep the readiness guard across the complete nonblocking read.
            // `drain_descriptor` returns Ok only after the kernel reports
            // EAGAIN, so clearing here cannot discard a readable state which
            // has not yet been observed. The test hook injects the exact race
            // which previously existed between clearing and draining.
            after_ready();
            let result = Self::drain_descriptor(
                descriptor.as_fd(),
                self.watch_descriptor,
                &mut self.buffer,
                emit,
            );
            if result.is_ok() {
                readiness.clear_ready();
            }
            result
        }

        fn drain_descriptor(
            descriptor: BorrowedFd<'_>,
            watch_descriptor: i32,
            buffer: &mut [MaybeUninit<u8>; READ_BUFFER_BYTES],
            emit: &mut dyn FnMut(StoreDirectoryEvent),
        ) -> Result<(), BackendFailure> {
            let mut reader = inotify::Reader::new(descriptor, buffer);
            let mut event_count = 0_usize;

            loop {
                match reader.next() {
                    Ok(event) => {
                        event_count = event_count
                            .checked_add(1)
                            .ok_or(BackendFailure::DrainLimit)?;
                        emit(StoreDirectoryEvent {
                            expected_watch: event.wd() == watch_descriptor,
                            name: event.file_name().map(|name| name.to_bytes().to_vec()),
                            flags: normalize_flags(event.events()),
                        });
                        if event_count > MAX_EVENTS_PER_DRAIN {
                            // Preserve the backend contract: even the first
                            // event crossing the bound is delivered before the
                            // observer is poisoned for bounded-drain exhaustion.
                            return Err(BackendFailure::DrainLimit);
                        }
                    }
                    Err(Errno::AGAIN) => return Ok(()),
                    Err(_) => return Err(BackendFailure::Read),
                }
            }
        }
    }

    impl DirectoryWatchBackend for LinuxDirectoryWatch {
        fn drain(
            &mut self,
            emit: &mut dyn FnMut(StoreDirectoryEvent),
        ) -> Result<(), BackendFailure> {
            let descriptor = match &self.descriptor {
                LinuxWatchDescriptor::Synchronous(descriptor) => descriptor.as_fd(),
                LinuxWatchDescriptor::Async(descriptor) => descriptor.as_fd(),
                LinuxWatchDescriptor::Poisoned => return Err(BackendFailure::Read),
            };
            Self::drain_descriptor(descriptor, self.watch_descriptor, &mut self.buffer, emit)
        }
    }

    fn normalize_flags(kernel: ReadFlags) -> StoreEventFlags {
        let mut normalized = StoreEventFlags::NONE;
        let mut recognized = ReadFlags::empty();

        macro_rules! map_flag {
            ($kernel_flag:ident, $normalized_flag:ident) => {
                if kernel.contains(ReadFlags::$kernel_flag) {
                    normalized = normalized.union(StoreEventFlags::$normalized_flag);
                    recognized |= ReadFlags::$kernel_flag;
                }
            };
        }

        map_flag!(ATTRIB, ATTRIB);
        map_flag!(CLOSE_WRITE, CLOSE_WRITE);
        map_flag!(CREATE, CREATE);
        map_flag!(DELETE, DELETE);
        map_flag!(MODIFY, MODIFY);
        map_flag!(MOVED_FROM, MOVED_FROM);
        map_flag!(MOVED_TO, MOVED_TO);
        map_flag!(DELETE_SELF, DELETE_SELF);
        map_flag!(MOVE_SELF, MOVE_SELF);
        map_flag!(QUEUE_OVERFLOW, QUEUE_OVERFLOW);
        map_flag!(IGNORED, IGNORED);
        map_flag!(UNMOUNT, UNMOUNT);
        map_flag!(ISDIR, IS_DIRECTORY);

        if kernel.bits() & !recognized.bits() != 0 {
            normalized = normalized.union(StoreEventFlags::UNSUPPORTED);
        }
        normalized
    }

    pub(in crate::discovery::approval) fn subscribe<I>(
        directory: StoreDirectoryWatchAnchor,
        invalidator: I,
    ) -> Result<ApprovalStoreObserver<LinuxDirectoryWatch, I>, StoreWatchError>
    where
        I: StoreInvalidationSink,
    {
        let backend = LinuxDirectoryWatch::subscribe(directory)?;
        Ok(ApprovalStoreObserver::with_backend(backend, invalidator))
    }

    impl<I> ApprovalStoreObserver<LinuxDirectoryWatch, I>
    where
        I: StoreInvalidationSink,
    {
        async fn poll_after_readable(&mut self) -> Result<StoreWatchPoll, StoreWatchError> {
            if self.failed_closed {
                return Err(StoreWatchError::FailedClosed);
            }

            let starting_generation = self.generation;
            let generation = &mut self.generation;
            let failed_closed = &mut self.failed_closed;
            let invalidator = &self.invalidator;
            let result = self
                .backend
                .wait_and_drain(&mut |event| {
                    if *failed_closed {
                        return;
                    }
                    match super::classify_event(&event) {
                        super::EventDisposition::Ignore => {}
                        super::EventDisposition::Invalidate => {
                            super::note_invalidation(generation, failed_closed, invalidator);
                        }
                        super::EventDisposition::Poison => {
                            super::poison_observer(generation, failed_closed, invalidator);
                        }
                    }
                })
                .await;

            if result.is_err() {
                super::poison_observer(
                    &mut self.generation,
                    &mut self.failed_closed,
                    &self.invalidator,
                );
            }
            if self.failed_closed {
                return Err(StoreWatchError::FailedClosed);
            }

            Ok(if self.generation == starting_generation {
                StoreWatchPoll::Quiet
            } else {
                StoreWatchPoll::Invalidated
            })
        }

        /// Consume this observer into a continuously readiness-driven actor.
        ///
        /// `proof` must come from `revalidate_with` on this exact observer.
        /// The callback is invoked synchronously, after AsyncFd registration
        /// and one final drain-to-EAGAIN barrier, with no await in between. Its
        /// sink argument remains owned by the actor for the actor's complete
        /// lifetime; production wiring uses that sink to own the dedicated
        /// approval-store observer session and install its healthy epoch.
        ///
        /// The returned future owns the observer. Cancellation, panic, backend
        /// failure, or unexpected completion therefore drops the observer and
        /// invalidates its sink. After Balun publishes its own store mutation,
        /// the controller must stop this actor, subscribe a replacement, and
        /// reread/match that publication through a fresh baseline before
        /// installing new health.
        pub(in crate::discovery::approval) async fn run_continuously<F>(
            mut self,
            proof: StoreBaselineProof,
            activate: F,
        ) -> Result<Infallible, StoreWatchError>
        where
            F: FnOnce(&I) -> Result<(), ()>,
        {
            if self.backend.arm_async_readiness().is_err() {
                super::poison_observer(
                    &mut self.generation,
                    &mut self.failed_closed,
                    &self.invalidator,
                );
                return Err(StoreWatchError::FailedClosed);
            }

            // Close the handoff window between the synchronous baseline and
            // activation of the observer-owned hub epoch.
            self.poll()?;
            if !self.proof_is_current(&proof) {
                return Err(StoreWatchError::ChangedDuringBaseline);
            }
            if activate(&self.invalidator).is_err() {
                super::poison_observer(
                    &mut self.generation,
                    &mut self.failed_closed,
                    &self.invalidator,
                );
                return Err(StoreWatchError::FailedClosed);
            }

            loop {
                self.poll_after_readable().await?;
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::time::Duration;

        use super::*;
        use crate::discovery::approval::store::{ApprovalStore, ApprovalStoreStatus, StorePaths};

        fn test_store(temporary: &tempfile::TempDir) -> (ApprovalStore, PathBuf) {
            let directory = temporary.path().join("private");
            let store = ApprovalStore::new(StorePaths::new(directory.clone()));
            (store, directory)
        }

        #[test]
        fn kernel_flags_are_mapped_without_silently_accepting_extras() {
            let mapped =
                normalize_flags(ReadFlags::CREATE | ReadFlags::MOVED_TO | ReadFlags::ISDIR);
            assert!(mapped.intersects(StoreEventFlags::CREATE));
            assert!(mapped.intersects(StoreEventFlags::MOVED_TO));
            assert!(mapped.intersects(StoreEventFlags::IS_DIRECTORY));
            assert!(!mapped.intersects(StoreEventFlags::UNSUPPORTED));

            let unsupported = normalize_flags(ReadFlags::OPEN);
            assert!(unsupported.intersects(StoreEventFlags::UNSUPPORTED));
        }

        #[test]
        fn linux_backend_is_send_and_redacts_debug_output() {
            fn assert_send<T: Send>() {}
            assert_send::<LinuxDirectoryWatch>();
            assert_eq!(format!("{:?}", StoreWatchError::Subscribe), "Subscribe");
        }

        #[test]
        fn real_store_revalidation_ignores_the_cooperative_lock_close() {
            let temporary = tempfile::tempdir().unwrap();
            let (store, _directory) = test_store(&temporary);
            assert_eq!(
                store.load().unwrap(),
                ApprovalStoreStatus::Missing {
                    key_initialized: false,
                }
            );

            let invalidations = Arc::new(AtomicUsize::new(0));
            let sink_count = Arc::clone(&invalidations);
            let mut observer = subscribe(store.watch_anchor().unwrap(), move || {
                sink_count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();

            let (status, proof) = observer.revalidate_with(|| store.load()).unwrap();
            assert_eq!(
                status,
                ApprovalStoreStatus::Missing {
                    key_initialized: false,
                }
            );
            assert!(observer.proof_is_current(&proof));
            assert_eq!(invalidations.load(Ordering::SeqCst), 0);
        }

        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn continuously_driven_actor_observes_changes_and_invalidates_on_abort() {
            async fn wait_until(predicate: impl Fn() -> bool) {
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !predicate() {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .expect("observer actor did not make bounded progress");
            }

            let temporary = tempfile::tempdir().unwrap();
            let (store, directory) = test_store(&temporary);
            let invalidations = Arc::new(AtomicUsize::new(0));
            let sink_count = Arc::clone(&invalidations);
            let mut observer = subscribe(store.watch_anchor().unwrap(), move || {
                sink_count.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
            let (_, proof) = observer.revalidate_with(|| Ok::<_, ()>(())).unwrap();

            let activated = Arc::new(AtomicBool::new(false));
            let actor_activated = Arc::clone(&activated);
            let actor = tokio::spawn(observer.run_continuously(proof, move |_sink| {
                actor_activated.store(true, Ordering::SeqCst);
                Ok(())
            }));
            wait_until(|| activated.load(Ordering::SeqCst)).await;
            assert_eq!(invalidations.load(Ordering::SeqCst), 0);

            std::fs::write(directory.join(super::super::STATE_FILE_NAME), b"synthetic").unwrap();
            wait_until(|| invalidations.load(Ordering::SeqCst) > 0).await;
            let before_abort = invalidations.load(Ordering::SeqCst);

            actor.abort();
            let join_error = actor.await.unwrap_err();
            assert!(join_error.is_cancelled());
            assert!(invalidations.load(Ordering::SeqCst) > before_abort);
        }

        #[tokio::test]
        async fn readiness_guard_covers_events_racing_the_kernel_drain() {
            let temporary = tempfile::tempdir().unwrap();
            let (store, directory) = test_store(&temporary);
            let mut watch = LinuxDirectoryWatch::subscribe(store.watch_anchor().unwrap()).unwrap();
            watch.arm_async_readiness().unwrap();

            // Make the descriptor readable, then inject the authority-bearing
            // event only after readiness has been acquired and while its guard
            // remains held. Both events must be drained before readiness clears.
            std::fs::write(directory.join("unrelated"), b"synthetic").unwrap();
            let raced_state = directory.join(super::super::STATE_FILE_NAME);
            let mut events = Vec::new();
            tokio::time::timeout(
                Duration::from_secs(2),
                watch.wait_and_drain_after_ready(
                    || std::fs::write(raced_state, b"synthetic").unwrap(),
                    &mut |event| events.push(event),
                ),
            )
            .await
            .expect("initial readiness")
            .unwrap();
            assert!(events.iter().any(|event| {
                event.name.as_deref() == Some(super::super::STATE_FILE_NAME.as_bytes())
            }));

            // Clearing after the observed EAGAIN must re-arm the next edge.
            std::fs::write(directory.join(super::super::KEY_FILE_NAME), b"synthetic").unwrap();
            events.clear();
            tokio::time::timeout(
                Duration::from_secs(2),
                watch.wait_and_drain(&mut |event| events.push(event)),
            )
            .await
            .expect("subsequent readiness")
            .unwrap();
            assert!(events.iter().any(|event| {
                event.name.as_deref() == Some(super::super::KEY_FILE_NAME.as_bytes())
            }));
        }

        #[test]
        fn replacing_the_injected_path_cannot_retarget_the_watch() {
            let temporary = tempfile::tempdir().unwrap();
            let (store, directory) = test_store(&temporary);
            let anchor = store.watch_anchor().unwrap();
            let mut observer = subscribe(anchor.clone(), || {}).unwrap();

            let renamed = temporary.path().join("renamed-pinned-store");
            std::fs::rename(&directory, &renamed).unwrap();
            std::fs::create_dir(&directory).unwrap();

            // The procfd path remains bound to the original inode, while the
            // directory rename itself is terminal for the active baseline.
            assert!(anchor.path_matches_pinned_identity(&anchor.proc_fd_path()));
            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
            std::fs::write(
                directory.join(super::super::STATE_FILE_NAME),
                b"replacement",
            )
            .unwrap();
            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) type LinuxApprovalStoreObserver<I> =
    ApprovalStoreObserver<linux::LinuxDirectoryWatch, I>;

#[cfg(target_os = "linux")]
pub(super) fn subscribe_linux<I>(
    store: &super::store::ApprovalStore,
    invalidator: I,
) -> Result<LinuxApprovalStoreObserver<I>, StoreWatchError>
where
    I: StoreInvalidationSink,
{
    let directory = store
        .watch_anchor()
        .map_err(|_| StoreWatchError::Subscribe)?;
    linux::subscribe(directory, invalidator)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        drains: VecDeque<Result<Vec<StoreDirectoryEvent>, BackendFailure>>,
    }

    impl FakeBackend {
        fn push_events(&mut self, events: impl IntoIterator<Item = StoreDirectoryEvent>) {
            self.drains.push_back(Ok(events.into_iter().collect()));
        }

        fn push_failure(&mut self, failure: BackendFailure) {
            self.drains.push_back(Err(failure));
        }
    }

    impl DirectoryWatchBackend for FakeBackend {
        fn drain(
            &mut self,
            emit: &mut dyn FnMut(StoreDirectoryEvent),
        ) -> Result<(), BackendFailure> {
            match self.drains.pop_front().unwrap_or_else(|| Ok(Vec::new())) {
                Ok(events) => {
                    for event in events {
                        emit(event);
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    }

    fn event(name: Option<&[u8]>, flags: StoreEventFlags) -> StoreDirectoryEvent {
        StoreDirectoryEvent {
            expected_watch: true,
            name: name.map(<[u8]>::to_vec),
            flags,
        }
    }

    fn observer() -> (
        ApprovalStoreObserver<FakeBackend, impl StoreInvalidationSink>,
        Arc<AtomicUsize>,
    ) {
        let invalidations = Arc::new(AtomicUsize::new(0));
        let sink_counter = Arc::clone(&invalidations);
        let observer = ApprovalStoreObserver::with_backend(FakeBackend::default(), move || {
            sink_counter.fetch_add(1, Ordering::SeqCst);
        });
        (observer, invalidations)
    }

    #[test]
    fn every_mutating_event_on_every_permanent_name_invalidates() {
        let (mut observer, invalidations) = observer();
        let actions = [
            StoreEventFlags::ATTRIB,
            StoreEventFlags::CLOSE_WRITE,
            StoreEventFlags::CREATE,
            StoreEventFlags::DELETE,
            StoreEventFlags::MODIFY,
            StoreEventFlags::MOVED_FROM,
            StoreEventFlags::MOVED_TO,
        ];
        let names = [STATE_FILE_NAME, KEY_FILE_NAME, LOCK_FILE_NAME];
        observer
            .backend
            .push_events(names.into_iter().flat_map(|name| {
                actions
                    .into_iter()
                    .map(move |action| event(Some(name.as_bytes()), action))
            }));

        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Invalidated);
        // A lock-only CLOSE_WRITE is cooperative locking noise; all other
        // permanent-entry actions, including CLOSE_WRITE on state/key, count.
        assert_eq!(invalidations.load(Ordering::SeqCst), 20);
        assert_eq!(observer.generation, 20);
    }

    #[test]
    fn lock_close_is_ignored_only_when_it_is_the_sole_action() {
        let (mut observer, invalidations) = observer();
        observer.backend.push_events([event(
            Some(LOCK_FILE_NAME.as_bytes()),
            StoreEventFlags::CLOSE_WRITE,
        )]);
        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Quiet);
        assert_eq!(invalidations.load(Ordering::SeqCst), 0);

        observer.backend.push_events([event(
            Some(LOCK_FILE_NAME.as_bytes()),
            StoreEventFlags::CLOSE_WRITE.union(StoreEventFlags::MODIFY),
        )]);
        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Invalidated);
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn unrelated_and_temporary_entry_noise_is_ignored() {
        let (mut observer, invalidations) = observer();
        observer.backend.push_events([
            event(Some(b".routed-approvals.json.tmp"), StoreEventFlags::CREATE),
            event(
                Some(b"routed-approvals.json.partial"),
                StoreEventFlags::MODIFY,
            ),
            event(Some(b"unrelated"), StoreEventFlags::DELETE),
            event(
                Some(b"temporary-directory"),
                StoreEventFlags::CREATE.union(StoreEventFlags::IS_DIRECTORY),
            ),
        ]);

        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Quiet);
        assert_eq!(invalidations.load(Ordering::SeqCst), 0);
        assert_eq!(observer.generation, 0);
    }

    #[test]
    fn fatal_kernel_conditions_poison_immediately_and_permanently() {
        for fatal in [
            StoreEventFlags::QUEUE_OVERFLOW,
            StoreEventFlags::IGNORED,
            StoreEventFlags::UNMOUNT,
            StoreEventFlags::DELETE_SELF,
            StoreEventFlags::MOVE_SELF,
            StoreEventFlags::UNSUPPORTED,
        ] {
            let (mut observer, invalidations) = observer();
            observer.backend.push_events([event(None, fatal)]);

            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
            assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn wrong_watch_missing_name_and_actionless_events_fail_closed() {
        let fixtures = [
            StoreDirectoryEvent {
                expected_watch: false,
                name: Some(STATE_FILE_NAME.as_bytes().to_vec()),
                flags: StoreEventFlags::MODIFY,
            },
            event(None, StoreEventFlags::MODIFY),
            event(
                Some(STATE_FILE_NAME.as_bytes()),
                StoreEventFlags::IS_DIRECTORY,
            ),
            event(Some(STATE_FILE_NAME.as_bytes()), StoreEventFlags(1 << 31)),
        ];

        for fixture in fixtures {
            let (mut observer, invalidations) = observer();
            observer.backend.push_events([fixture]);
            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
            assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn read_and_bounded_drain_failures_poison() {
        for failure in [BackendFailure::Read, BackendFailure::DrainLimit] {
            let (mut observer, invalidations) = observer();
            observer.backend.push_failure(failure);
            assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
            assert_eq!(invalidations.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn subscribe_read_drain_sandwich_rejects_a_racing_change() {
        let (mut observer, invalidations) = observer();
        let token = observer.begin_baseline().unwrap();
        observer.backend.push_events([event(
            Some(STATE_FILE_NAME.as_bytes()),
            StoreEventFlags::MOVED_TO,
        )]);

        assert_eq!(
            observer.finish_baseline(token).unwrap_err(),
            StoreWatchError::ChangedDuringBaseline
        );
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_quiet_sandwich_yields_a_source_bound_current_proof() {
        let (mut observer, invalidations) = observer();
        let token = observer.begin_baseline().unwrap();
        let proof = observer.finish_baseline(token).unwrap();
        assert!(observer.proof_is_current(&proof));

        observer.backend.push_events([event(
            Some(KEY_FILE_NAME.as_bytes()),
            StoreEventFlags::ATTRIB,
        )]);
        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Invalidated);
        assert!(!observer.proof_is_current(&proof));
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_foreign_baseline_token_poison_the_receiver() {
        let (mut first, _first_invalidations) = observer();
        let (mut second, second_invalidations) = observer();
        let token = first.begin_baseline().unwrap();

        assert_eq!(
            second.finish_baseline(token).unwrap_err(),
            StoreWatchError::ForeignBaseline
        );
        assert_eq!(second.poll(), Err(StoreWatchError::FailedClosed));
        assert_eq!(second_invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn an_observed_self_publication_requires_and_allows_a_fresh_matched_baseline() {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        struct SyntheticReservation {
            run_id: u128,
            commit_counter: u64,
        }

        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        struct ReservationMismatch;

        let (mut observer, invalidations) = observer();
        observer.backend.push_events([
            event(Some(STATE_FILE_NAME.as_bytes()), StoreEventFlags::MOVED_TO),
            event(
                Some(STATE_FILE_NAME.as_bytes()),
                StoreEventFlags::CLOSE_WRITE,
            ),
        ]);
        assert_eq!(observer.poll().unwrap(), StoreWatchPoll::Invalidated);

        let expected = SyntheticReservation {
            run_id: 0x1234,
            commit_counter: 9,
        };
        let mut durable_record = SyntheticReservation {
            run_id: 0x9999,
            commit_counter: 8,
        };

        let mismatch = observer.revalidate_with(|| {
            if durable_record == expected {
                Ok(durable_record)
            } else {
                Err(ReservationMismatch)
            }
        });
        assert!(matches!(
            mismatch,
            Err(StoreBaselineError::Revalidation(ReservationMismatch))
        ));

        // A caller may install new health only after a fresh reread matches
        // the exact reservation that its durable commit claimed to publish.
        durable_record = expected;
        let (matched, proof) = observer
            .revalidate_with(|| {
                if durable_record == expected {
                    Ok(durable_record)
                } else {
                    Err(ReservationMismatch)
                }
            })
            .unwrap();
        assert_eq!(matched, expected);
        assert!(observer.proof_is_current(&proof));
        assert_eq!(invalidations.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn generation_overflow_fails_closed_instead_of_reusing_a_proof() {
        let (mut observer, invalidations) = observer();
        observer.generation = u64::MAX;
        observer.backend.push_events([event(
            Some(LOCK_FILE_NAME.as_bytes()),
            StoreEventFlags::CREATE,
        )]);

        assert_eq!(observer.poll(), Err(StoreWatchError::FailedClosed));
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn observer_exit_invalidates_even_after_a_quiet_baseline() {
        let (mut observer, invalidations) = observer();
        let token = observer.begin_baseline().unwrap();
        let _proof = observer.finish_baseline(token).unwrap();
        assert_eq!(invalidations.load(Ordering::SeqCst), 0);

        drop(observer);
        assert_eq!(invalidations.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn debug_and_errors_do_not_disclose_names_or_topology() {
        let (observer, _invalidations) = observer();
        let debug = format!("{observer:?}");
        assert_eq!(debug, "ApprovalStoreObserver(<redacted>)");

        for error in [
            StoreWatchError::Subscribe,
            StoreWatchError::FailedClosed,
            StoreWatchError::ChangedDuringBaseline,
            StoreWatchError::ForeignBaseline,
        ] {
            let rendered = error.to_string();
            assert!(!rendered.contains(STATE_FILE_NAME));
            assert!(!rendered.contains(KEY_FILE_NAME));
            assert!(!rendered.contains(LOCK_FILE_NAME));
            assert!(!rendered.contains('/'));
        }

        let baseline_error = StoreBaselineError::Revalidation("/private/site/authority");
        assert_eq!(format!("{baseline_error:?}"), "Revalidation(<redacted>)");
        assert!(!baseline_error.to_string().contains("/private"));
    }

    #[test]
    fn authority_types_are_not_cloneable() {
        fn assert_send<T: Send>() {}
        assert_send::<StoreBaselineToken>();
        assert_send::<StoreBaselineProof>();

        // Compile-time API checks: there is intentionally no Clone assertion.
        let (mut observer, _invalidations) = observer();
        let token = observer.begin_baseline().unwrap();
        let proof = observer.finish_baseline(token).unwrap();
        assert!(observer.proof_is_current(&proof));
    }
}
