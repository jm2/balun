//! Fail-closed Linux rtnetlink change observation.
//!
//! This module deliberately uses neli's low-level socket rather than
//! `NlRouter`'s multicast receiver. In neli 0.7.4 the router can route a
//! socket-level overflow only to an outstanding request, which is unsuitable
//! for a passive authority-invalidation source.
//!
//! A controller creates a fresh monitor before taking a route snapshot, calls
//! [`LinuxRouteEventMonitor::post_snapshot_barrier`] after that snapshot, and
//! starts [`LinuxRouteEventMonitor::run`] only after a clean barrier. A changed
//! barrier means the snapshot must be discarded. Reconciliation after a live
//! notification should replace this monitor with a fresh subscribed instance
//! and repeat the same sequence.

#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;
use std::io::{self, IoSliceMut};
use std::os::fd::AsRawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use neli::consts::nl::Nlmsg;
use neli::consts::rtnl::{RtAddrFamily, Rtm};
use neli::consts::socket::NlFamily;
use neli::socket::NlSocket;
use neli::utils::Groups;
use nix::sys::socket::{MsgFlags, NetlinkAddr, recvmsg};
use thiserror::Error;
use tokio::io::{Interest, unix::AsyncFd};
use tokio::runtime::Handle;
use tokio::sync::mpsc;

// Numeric RTNLGRP_* identifiers from Linux UAPI <linux/rtnetlink.h>. These are
// group numbers, not the legacy RTMGRP_* bit masks.
const RTNLGRP_LINK: u32 = 1;
const RTNLGRP_IPV4_IFADDR: u32 = 5;
const RTNLGRP_IPV4_ROUTE: u32 = 7;
const RTNLGRP_IPV4_RULE: u32 = 8;
const RTNLGRP_IPV6_IFADDR: u32 = 9;
const MONITORED_GROUPS: [u32; 5] = [
    RTNLGRP_LINK,
    RTNLGRP_IPV4_IFADDR,
    RTNLGRP_IPV4_ROUTE,
    RTNLGRP_IPV4_RULE,
    RTNLGRP_IPV6_IFADDR,
];

const RECEIVE_BUFFER_BYTES: usize = 1024 * 1024;
const SOCKET_RECEIVE_BUFFER_BYTES: usize = RECEIVE_BUFFER_BYTES;
const MAX_DATAGRAMS_PER_TURN: usize = 64;
const MAX_BYTES_PER_TURN: usize = 4 * 1024 * 1024;
const MAX_BARRIER_DATAGRAMS: usize = 256;
const MAX_BARRIER_BYTES: usize = 16 * 1024 * 1024;
const RECONCILIATION_CAPACITY: usize = 1;

/// Synchronous, topology-free hooks owned by one route-observer incarnation.
///
/// Both methods must be idempotent and must not panic. `poison` must prevent a
/// later stale observer from publishing a healthy epoch. Dropping the monitor
/// calls `poison`, including when its task is aborted.
pub(super) trait RouteMonitorObserver: Send + Sync {
    fn invalidate(&self);
    fn poison(&self);
}

/// A coalesced request for the controller to debounce and rebuild its baseline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RouteReconciliationRequired;

/// Result of draining all notifications queued after a route snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PostSnapshotBarrier {
    /// The subscribed socket reached `EAGAIN` without observing a change.
    Clean,
    /// At least one change was queued; the caller must discard its snapshot.
    Changed,
}

/// A topology-redacted terminal monitor failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(super) enum LinuxRouteMonitorError {
    #[error("the Linux route-event socket could not be opened")]
    SocketUnavailable,
    #[error("the Linux route-event receive buffer could not be bounded")]
    ReceiveBufferUnavailable,
    #[error("the Linux route-event socket could not be made nonblocking")]
    NonblockingUnavailable,
    #[error("the Linux route-event groups could not be subscribed")]
    SubscriptionUnavailable,
    #[error("the Linux route-event group subscription could not be verified")]
    MembershipMismatch,
    #[error("the Linux route-event socket could not join the async runtime")]
    RuntimeRegistrationFailed,
    #[error("the Linux route-event receive queue overflowed")]
    ReceiveOverflow,
    #[error("the Linux route-event socket closed")]
    SocketClosed,
    #[error("the Linux route-event socket failed")]
    ReceiveFailed,
    #[error("a Linux route-event datagram was malformed")]
    InvalidDatagram,
    #[error("a Linux route-event notification was unsupported")]
    UnsupportedNotification,
    #[error("the Linux route-event reconciler is unavailable")]
    ReconcilerUnavailable,
    #[error("the Linux route-event post-snapshot barrier exceeded its bound")]
    BarrierLimitExceeded,
    #[error("the Linux route-event post-snapshot barrier was not completed")]
    BarrierRequired,
}

/// One subscribed, non-cloneable rtnetlink observer.
///
/// Construction must occur on a Tokio runtime with I/O enabled. Successful
/// construction is the subscription point: callers take their route snapshot
/// only after this value has been returned.
pub(super) struct LinuxRouteEventMonitor {
    socket: AsyncFd<NlSocket>,
    core: MonitorCore,
    receive_buffer: Box<[u8]>,
    barrier_complete: bool,
}

impl LinuxRouteEventMonitor {
    /// Subscribe to every kernel source which can alter Balun's Linux route
    /// fingerprint, returning a capacity-one reconciliation receiver.
    pub(super) fn subscribe(
        observer: Arc<dyn RouteMonitorObserver>,
    ) -> Result<(Self, mpsc::Receiver<RouteReconciliationRequired>), LinuxRouteMonitorError> {
        // AsyncFd's public constructors panic without a current reactor. Check
        // before opening the socket so this API fails closed and without even
        // briefly subscribing when called from synchronous code.
        if Handle::try_current().is_err() {
            observer.poison();
            return Err(LinuxRouteMonitorError::RuntimeRegistrationFailed);
        }
        let socket = match subscribed_socket() {
            Ok(socket) => socket,
            Err(error) => {
                observer.poison();
                return Err(error);
            }
        };
        let socket = match register_readable(socket) {
            Ok(socket) => socket,
            Err(_) => {
                observer.poison();
                return Err(LinuxRouteMonitorError::RuntimeRegistrationFailed);
            }
        };
        let (reconciliation, receiver) = mpsc::channel(RECONCILIATION_CAPACITY);
        Ok((
            Self {
                socket,
                core: MonitorCore::new(observer, reconciliation),
                receive_buffer: vec![0_u8; RECEIVE_BUFFER_BYTES].into_boxed_slice(),
                barrier_complete: false,
            },
            receiver,
        ))
    }

    /// Drain to `EAGAIN` after the caller's snapshot.
    ///
    /// Work is yielded in bounded turns. A continuing stream which prevents a
    /// quiescent boundary from being established is terminal and poisons this
    /// observer incarnation.
    pub(super) async fn post_snapshot_barrier(
        &mut self,
    ) -> Result<PostSnapshotBarrier, LinuxRouteMonitorError> {
        self.barrier_complete = false;
        let mut source = NeliDatagramSource::new(self.socket.get_ref());
        let result =
            drain_post_snapshot(&mut source, &mut self.core, &mut self.receive_buffer).await;
        self.barrier_complete = matches!(result, Ok(PostSnapshotBarrier::Clean));
        result
    }

    /// Observe notifications until a terminal failure or task cancellation.
    ///
    /// A caller must first establish a clean post-snapshot barrier. Each
    /// scheduler turn is bounded, but a busy socket remains invalidated and is
    /// drained again after yielding.
    pub(super) async fn run(mut self) -> Result<(), LinuxRouteMonitorError> {
        if !self.barrier_complete {
            return self.core.fail(LinuxRouteMonitorError::BarrierRequired);
        }

        loop {
            let reconciliation = self.core.reconciliation.clone();
            let readiness = tokio::select! {
                _ = reconciliation.closed() => {
                    return self.core.fail(LinuxRouteMonitorError::ReconcilerUnavailable);
                }
                readiness = self.socket.readable() => readiness,
            };
            let mut readiness = match readiness {
                Ok(readiness) => readiness,
                Err(_) => return self.core.fail(LinuxRouteMonitorError::ReceiveFailed),
            };

            let mut source = NeliDatagramSource::new(self.socket.get_ref());
            let outcome = drain_available(
                &mut source,
                &mut self.core,
                &mut self.receive_buffer,
                MAX_DATAGRAMS_PER_TURN,
                MAX_BYTES_PER_TURN,
            );
            match outcome {
                Ok(DrainOutcome {
                    stop: DrainStop::Quiescent,
                    ..
                }) => readiness.clear_ready(),
                Ok(DrainOutcome {
                    stop: DrainStop::BudgetExhausted,
                    ..
                }) => {
                    drop(readiness);
                    tokio::task::yield_now().await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for LinuxRouteEventMonitor {
    fn drop(&mut self) {
        self.core.poison();
    }
}

impl fmt::Debug for LinuxRouteEventMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LinuxRouteEventMonitor(<redacted>)")
    }
}

fn subscribed_socket() -> Result<NlSocket, LinuxRouteMonitorError> {
    let socket =
        NlSocket::new(NlFamily::Route).map_err(|_| LinuxRouteMonitorError::SocketUnavailable)?;
    socket
        .set_recv_buffer_size(SOCKET_RECEIVE_BUFFER_BYTES)
        .map_err(|_| LinuxRouteMonitorError::ReceiveBufferUnavailable)?;
    socket
        .nonblock()
        .map_err(|_| LinuxRouteMonitorError::NonblockingUnavailable)?;
    socket
        .bind(None, Groups::new_groups(&MONITORED_GROUPS))
        .map_err(|_| LinuxRouteMonitorError::SubscriptionUnavailable)?;

    let memberships = socket
        .list_mcast_membership()
        .map_err(|_| LinuxRouteMonitorError::MembershipMismatch)?;
    if memberships.to_vec() != MONITORED_GROUPS {
        return Err(LinuxRouteMonitorError::MembershipMismatch);
    }
    Ok(socket)
}

/// Register an fd for readable readiness without allowing Tokio's documented
/// missing-I/O-driver panic to escape this security boundary.
fn register_readable<T: AsRawFd>(inner: T) -> Result<AsyncFd<T>, LinuxRouteMonitorError> {
    if Handle::try_current().is_err() {
        return Err(LinuxRouteMonitorError::RuntimeRegistrationFailed);
    }

    match catch_unwind(AssertUnwindSafe(|| {
        AsyncFd::try_with_interest(inner, Interest::READABLE)
    })) {
        Ok(Ok(registered)) => Ok(registered),
        Ok(Err(_)) | Err(_) => Err(LinuxRouteMonitorError::RuntimeRegistrationFailed),
    }
}

struct MonitorCore {
    observer: Arc<dyn RouteMonitorObserver>,
    reconciliation: mpsc::Sender<RouteReconciliationRequired>,
    poisoned: bool,
    notification_seen: bool,
}

impl MonitorCore {
    fn new(
        observer: Arc<dyn RouteMonitorObserver>,
        reconciliation: mpsc::Sender<RouteReconciliationRequired>,
    ) -> Self {
        Self {
            observer,
            reconciliation,
            poisoned: false,
            notification_seen: false,
        }
    }

    fn notification(
        &mut self,
        bytes: &[u8],
        source_pid: u32,
        source_groups: &Groups,
    ) -> Result<(), LinuxRouteMonitorError> {
        if let Err(error) = validate_notification(bytes, source_pid, source_groups) {
            return self.fail(error);
        }

        // This is deliberately sticky for the lifetime of the subscription.
        // A cancelled or repeated baseline barrier can therefore never turn a
        // previously invalidated incarnation healthy again.
        self.notification_seen = true;
        // Invalidate before notification. A full channel only coalesces work;
        // it never delays or suppresses authority invalidation.
        self.observer.invalidate();
        match self.reconciliation.try_send(RouteReconciliationRequired) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.fail(LinuxRouteMonitorError::ReconcilerUnavailable)
            }
        }
    }

    fn fail<T>(&mut self, error: LinuxRouteMonitorError) -> Result<T, LinuxRouteMonitorError> {
        self.poison();
        Err(error)
    }

    fn poison(&mut self) {
        if !self.poisoned {
            self.poisoned = true;
            self.observer.poison();
        }
    }
}

struct ReceivedDatagram {
    length: usize,
    source_pid: u32,
    source_groups: Groups,
}

trait NonblockingDatagramSource {
    fn receive(&mut self, buffer: &mut [u8]) -> io::Result<ReceivedDatagram>;
}

struct NeliDatagramSource<'a> {
    socket: &'a NlSocket,
}

impl<'a> NeliDatagramSource<'a> {
    const fn new(socket: &'a NlSocket) -> Self {
        Self { socket }
    }
}

impl NonblockingDatagramSource for NeliDatagramSource<'_> {
    fn receive(&mut self, buffer: &mut [u8]) -> io::Result<ReceivedDatagram> {
        let mut vectors = [IoSliceMut::new(buffer)];
        let message = recvmsg::<NetlinkAddr>(
            self.socket.as_raw_fd(),
            &mut vectors,
            None,
            MsgFlags::MSG_TRUNC | MsgFlags::MSG_DONTWAIT,
        )
        .map_err(|error| io::Error::from_raw_os_error(error as i32))?;
        let length = message.bytes;
        let address = message.address;
        let address = address
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing netlink source"))?;
        Ok(ReceivedDatagram {
            length,
            source_pid: address.pid(),
            source_groups: Groups::new_bitmask(address.groups()),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DrainStop {
    Quiescent,
    BudgetExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DrainOutcome {
    stop: DrainStop,
    datagrams: usize,
    bytes: usize,
    changed: bool,
}

fn drain_available<S: NonblockingDatagramSource + ?Sized>(
    source: &mut S,
    core: &mut MonitorCore,
    receive_buffer: &mut [u8],
    max_datagrams: usize,
    max_bytes: usize,
) -> Result<DrainOutcome, LinuxRouteMonitorError> {
    let mut datagrams = 0_usize;
    let mut bytes = 0_usize;
    let mut changed = false;

    loop {
        if datagrams >= max_datagrams || bytes >= max_bytes {
            return Ok(DrainOutcome {
                stop: DrainStop::BudgetExhausted,
                datagrams,
                bytes,
                changed,
            });
        }
        let remaining_bytes = max_bytes - bytes;
        if bytes > 0 && remaining_bytes < receive_buffer.len() {
            // Yield before reading when this turn no longer has space for the
            // largest accepted datagram. Never consume an event that cannot be
            // accounted within the advertised byte budget.
            return Ok(DrainOutcome {
                stop: DrainStop::BudgetExhausted,
                datagrams,
                bytes,
                changed,
            });
        }
        let receive_capacity = receive_buffer.len().min(remaining_bytes);
        let received = match source.receive(&mut receive_buffer[..receive_capacity]) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(DrainOutcome {
                    stop: DrainStop::Quiescent,
                    datagrams,
                    bytes,
                    changed,
                });
            }
            Err(error)
                if error.raw_os_error() == Some(rustix::io::Errno::NOBUFS.raw_os_error()) =>
            {
                return core.fail(LinuxRouteMonitorError::ReceiveOverflow);
            }
            Err(_) => return core.fail(LinuxRouteMonitorError::ReceiveFailed),
        };

        if received.length == 0 {
            return core.fail(LinuxRouteMonitorError::SocketClosed);
        }
        if received.length > receive_capacity {
            // MSG_TRUNC reports the complete datagram length even though only
            // the fixed prefix was copied. Never parse that incomplete prefix.
            return core.fail(LinuxRouteMonitorError::ReceiveOverflow);
        }

        core.notification(
            &receive_buffer[..received.length],
            received.source_pid,
            &received.source_groups,
        )?;
        datagrams = match datagrams.checked_add(1) {
            Some(datagrams) => datagrams,
            None => return core.fail(LinuxRouteMonitorError::ReceiveOverflow),
        };
        bytes = match bytes.checked_add(received.length) {
            Some(bytes) => bytes,
            None => return core.fail(LinuxRouteMonitorError::ReceiveOverflow),
        };
        changed = true;
    }
}

async fn drain_post_snapshot<S: NonblockingDatagramSource + ?Sized>(
    source: &mut S,
    core: &mut MonitorCore,
    receive_buffer: &mut [u8],
) -> Result<PostSnapshotBarrier, LinuxRouteMonitorError> {
    if core.reconciliation.is_closed() {
        return core.fail(LinuxRouteMonitorError::ReconcilerUnavailable);
    }

    let mut total_datagrams = 0_usize;
    let mut total_bytes = 0_usize;
    let mut changed = core.notification_seen;
    loop {
        if total_datagrams >= MAX_BARRIER_DATAGRAMS || total_bytes >= MAX_BARRIER_BYTES {
            return core.fail(LinuxRouteMonitorError::BarrierLimitExceeded);
        }
        let turn_datagrams = MAX_DATAGRAMS_PER_TURN.min(MAX_BARRIER_DATAGRAMS - total_datagrams);
        let turn_bytes = MAX_BYTES_PER_TURN.min(MAX_BARRIER_BYTES - total_bytes);
        let outcome = drain_available(source, core, receive_buffer, turn_datagrams, turn_bytes)?;
        total_datagrams = match total_datagrams.checked_add(outcome.datagrams) {
            Some(total) => total,
            None => return core.fail(LinuxRouteMonitorError::BarrierLimitExceeded),
        };
        total_bytes = match total_bytes.checked_add(outcome.bytes) {
            Some(total) => total,
            None => return core.fail(LinuxRouteMonitorError::BarrierLimitExceeded),
        };
        changed |= outcome.changed;

        match outcome.stop {
            DrainStop::Quiescent => {
                return Ok(if changed {
                    PostSnapshotBarrier::Changed
                } else {
                    PostSnapshotBarrier::Clean
                });
            }
            DrainStop::BudgetExhausted
                if total_datagrams == MAX_BARRIER_DATAGRAMS || total_bytes == MAX_BARRIER_BYTES =>
            {
                return core.fail(LinuxRouteMonitorError::BarrierLimitExceeded);
            }
            DrainStop::BudgetExhausted => tokio::task::yield_now().await,
        }
    }
}

fn validate_notification(
    bytes: &[u8],
    source_pid: u32,
    source_groups: &Groups,
) -> Result<(), LinuxRouteMonitorError> {
    // Only the sender sockaddr is kernel-authenticated. The nlmsg_pid header is
    // ordinary datagram content and can be forged by a userspace netlink peer.
    if source_pid != 0 {
        return Err(LinuxRouteMonitorError::InvalidDatagram);
    }
    let groups = source_groups.as_groups();
    if groups.is_empty() || groups.iter().any(|group| !MONITORED_GROUPS.contains(group)) {
        return Err(LinuxRouteMonitorError::UnsupportedNotification);
    }

    validate_netlink_frames(bytes, groups.as_slice())
}

const NETLINK_HEADER_BYTES: usize = 16;
const ROUTE_ATTRIBUTE_HEADER_BYTES: usize = 4;

fn aligned_netlink_length(length: usize) -> Option<usize> {
    length.checked_add(3).map(|value| value & !3)
}

/// Validate all attacker-controlled lengths before neli sees the datagram.
///
/// neli 0.7.4 subtracts the header size from `nl_len` while decoding. Feeding
/// it a short length can panic in debug builds or wrap into an enormous
/// allocation in optimized builds, so this pass is a required parser boundary.
fn validate_netlink_frames(
    bytes: &[u8],
    source_groups: &[u32],
) -> Result<(), LinuxRouteMonitorError> {
    let mut offset = 0_usize;
    let mut messages = 0_usize;

    while offset < bytes.len() {
        let header_end = offset
            .checked_add(NETLINK_HEADER_BYTES)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let header = bytes
            .get(offset..header_end)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let declared = u32::from_ne_bytes(
            header[0..4]
                .try_into()
                .map_err(|_| LinuxRouteMonitorError::InvalidDatagram)?,
        ) as usize;
        if declared < NETLINK_HEADER_BYTES {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
        let message_end = offset
            .checked_add(declared)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        if message_end > bytes.len() {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
        let message_type = u16::from_ne_bytes(
            header[4..6]
                .try_into()
                .map_err(|_| LinuxRouteMonitorError::InvalidDatagram)?,
        );
        if message_type != u16::from(Nlmsg::Overrun) && !is_supported_notification(message_type) {
            // Reject neli's specially decoded ERROR/DONE forms, as well as all
            // unrelated types, before third-party payload parsing begins.
            return Err(LinuxRouteMonitorError::UnsupportedNotification);
        }
        // nlmsg_seq and nlmsg_pid are unauthenticated message content. Kernel
        // multicast reports may copy them from the request which caused a
        // notification, so neither is an origin check. The recvmsg sockaddr
        // PID validated above is the only sender-authentication boundary.
        if message_type == u16::from(Nlmsg::Overrun) {
            return Err(LinuxRouteMonitorError::ReceiveOverflow);
        }
        let payload_start = offset
            .checked_add(NETLINK_HEADER_BYTES)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let payload = bytes
            .get(payload_start..message_end)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let expected_group = validate_rtnl_payload(message_type, payload)?;
        if source_groups != [expected_group] {
            return Err(LinuxRouteMonitorError::UnsupportedNotification);
        }
        let padded =
            aligned_netlink_length(declared).ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        offset = offset
            .checked_add(padded)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        if offset > bytes.len() {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
        messages = messages
            .checked_add(1)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
    }

    if messages == 0 || offset != bytes.len() {
        return Err(LinuxRouteMonitorError::InvalidDatagram);
    }
    Ok(())
}

fn preflight_route_attributes(
    bytes: &[u8],
    fixed_header_bytes: usize,
) -> Result<(), LinuxRouteMonitorError> {
    if bytes.len() < fixed_header_bytes {
        return Err(LinuxRouteMonitorError::InvalidDatagram);
    }
    let mut offset = fixed_header_bytes;
    while offset < bytes.len() {
        let header_end = offset
            .checked_add(ROUTE_ATTRIBUTE_HEADER_BYTES)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let header = bytes
            .get(offset..header_end)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        let declared = u16::from_ne_bytes(
            header[0..2]
                .try_into()
                .map_err(|_| LinuxRouteMonitorError::InvalidDatagram)?,
        ) as usize;
        if declared < ROUTE_ATTRIBUTE_HEADER_BYTES {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
        let attribute_end = offset
            .checked_add(declared)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        if attribute_end > bytes.len() {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
        let padded =
            aligned_netlink_length(declared).ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        offset = offset
            .checked_add(padded)
            .ok_or(LinuxRouteMonitorError::InvalidDatagram)?;
        if offset > bytes.len() {
            return Err(LinuxRouteMonitorError::InvalidDatagram);
        }
    }
    if offset != bytes.len() {
        return Err(LinuxRouteMonitorError::InvalidDatagram);
    }
    Ok(())
}

fn validate_rtnl_payload(message_type: u16, bytes: &[u8]) -> Result<u32, LinuxRouteMonitorError> {
    if message_type == u16::from(Rtm::Newlink) || message_type == u16::from(Rtm::Dellink) {
        preflight_route_attributes(bytes, 16)?;
        Ok(RTNLGRP_LINK)
    } else if message_type == u16::from(Rtm::Newaddr) || message_type == u16::from(Rtm::Deladdr) {
        preflight_route_attributes(bytes, 8)?;
        match bytes.first().copied() {
            Some(family) if family == u8::from(RtAddrFamily::Inet) => Ok(RTNLGRP_IPV4_IFADDR),
            Some(family) if family == u8::from(RtAddrFamily::Inet6) => Ok(RTNLGRP_IPV6_IFADDR),
            Some(_) | None => Err(LinuxRouteMonitorError::UnsupportedNotification),
        }
    } else {
        // Linux fib_rule_hdr and rtmsg are both a 12-byte fixed header
        // followed by rtattrs. Their first byte is the address family.
        preflight_route_attributes(bytes, 12)?;
        if bytes.first().copied() != Some(u8::from(RtAddrFamily::Inet)) {
            return Err(LinuxRouteMonitorError::UnsupportedNotification);
        }
        if message_type == u16::from(Rtm::Newroute) || message_type == u16::from(Rtm::Delroute) {
            Ok(RTNLGRP_IPV4_ROUTE)
        } else {
            Ok(RTNLGRP_IPV4_RULE)
        }
    }
}

fn is_supported_notification(message_type: u16) -> bool {
    matches!(
        message_type,
        value if value == u16::from(Rtm::Newlink)
            || value == u16::from(Rtm::Dellink)
            || value == u16::from(Rtm::Newaddr)
            || value == u16::from(Rtm::Deladdr)
            || value == u16::from(Rtm::Newroute)
            || value == u16::from(Rtm::Delroute)
            || value == u16::from(Rtm::Newrule)
            || value == u16::from(Rtm::Delrule)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::os::unix::net::UnixDatagram;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use neli::Size;
    use neli::ToBytes;
    use neli::consts::nl::NlmF;
    use neli::nl::{NlPayload, NlmsghdrBuilder};
    use neli::types::{Buffer, NlBuffer};

    use super::*;

    #[derive(Default)]
    struct FakeObserver {
        invalidations: AtomicUsize,
        poisons: AtomicUsize,
    }

    impl RouteMonitorObserver for FakeObserver {
        fn invalidate(&self) {
            self.invalidations.fetch_add(1, Ordering::SeqCst);
        }

        fn poison(&self) {
            self.poisons.fetch_add(1, Ordering::SeqCst);
        }
    }

    enum FakeReceive {
        Datagram {
            bytes: Vec<u8>,
            reported_length: usize,
            source_pid: u32,
            groups: Vec<u32>,
        },
        Error(io::Error),
    }

    struct FakeSource {
        steps: VecDeque<FakeReceive>,
    }

    impl FakeSource {
        fn new(steps: impl IntoIterator<Item = FakeReceive>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
            }
        }
    }

    impl NonblockingDatagramSource for FakeSource {
        fn receive(&mut self, buffer: &mut [u8]) -> io::Result<ReceivedDatagram> {
            match self
                .steps
                .pop_front()
                .unwrap_or_else(|| FakeReceive::Error(io::ErrorKind::WouldBlock.into()))
            {
                FakeReceive::Datagram {
                    bytes,
                    reported_length,
                    source_pid,
                    groups,
                } => {
                    let copied = bytes.len().min(buffer.len());
                    buffer[..copied].copy_from_slice(&bytes[..copied]);
                    Ok(ReceivedDatagram {
                        length: reported_length,
                        source_pid,
                        source_groups: Groups::new_groups(&groups),
                    })
                }
                FakeReceive::Error(error) => Err(error),
            }
        }
    }

    fn datagram(message_type: u16, group: u32) -> Vec<u8> {
        let payload_len =
            if message_type == u16::from(Rtm::Newaddr) || message_type == u16::from(Rtm::Deladdr) {
                8
            } else if message_type == u16::from(Rtm::Newroute)
                || message_type == u16::from(Rtm::Delroute)
                || message_type == u16::from(Rtm::Newrule)
                || message_type == u16::from(Rtm::Delrule)
            {
                12
            } else {
                16
            };
        let mut payload = vec![0_u8; payload_len];
        if message_type == u16::from(Rtm::Newaddr) || message_type == u16::from(Rtm::Deladdr) {
            payload[0] = u8::from(if group == RTNLGRP_IPV6_IFADDR {
                RtAddrFamily::Inet6
            } else {
                RtAddrFamily::Inet
            });
        } else if message_type == u16::from(Rtm::Newroute)
            || message_type == u16::from(Rtm::Delroute)
            || message_type == u16::from(Rtm::Newrule)
            || message_type == u16::from(Rtm::Delrule)
        {
            payload[0] = u8::from(RtAddrFamily::Inet);
        }
        datagram_with_payload(message_type, &payload)
    }

    fn datagram_with_payload(message_type: u16, payload: &[u8]) -> Vec<u8> {
        let message = NlmsghdrBuilder::default()
            .nl_type(message_type)
            .nl_flags(NlmF::empty())
            .nl_seq(0)
            .nl_pid(0)
            .nl_payload(NlPayload::Payload(Buffer::from(payload)))
            .build()
            .expect("valid synthetic netlink message");
        let messages = [message].into_iter().collect::<NlBuffer<_, _>>();
        let mut bytes = Cursor::new(vec![0_u8; messages.padded_size()]);
        messages
            .to_bytes(&mut bytes)
            .expect("serialize synthetic netlink datagram");
        bytes.into_inner()
    }

    fn raw_netlink_frame(declared_length: u32, message_type: u16) -> Vec<u8> {
        let mut bytes = vec![0_u8; NETLINK_HEADER_BYTES];
        bytes[0..4].copy_from_slice(&declared_length.to_ne_bytes());
        bytes[4..6].copy_from_slice(&message_type.to_ne_bytes());
        bytes
    }

    fn event(message_type: Rtm, group: u32) -> FakeReceive {
        let bytes = datagram(u16::from(message_type), group);
        let reported_length = bytes.len();
        FakeReceive::Datagram {
            bytes,
            reported_length,
            source_pid: 0,
            groups: vec![group],
        }
    }

    fn core() -> (
        MonitorCore,
        Arc<FakeObserver>,
        mpsc::Receiver<RouteReconciliationRequired>,
    ) {
        let observer = Arc::new(FakeObserver::default());
        let (sender, receiver) = mpsc::channel(RECONCILIATION_CAPACITY);
        let trait_observer: Arc<dyn RouteMonitorObserver> = observer.clone();
        (MonitorCore::new(trait_observer, sender), observer, receiver)
    }

    #[test]
    fn all_required_groups_and_message_types_are_accepted_and_coalesced() {
        assert_eq!(MONITORED_GROUPS, [1, 5, 7, 8, 9]);
        let events = [
            (Rtm::Newlink, RTNLGRP_LINK),
            (Rtm::Dellink, RTNLGRP_LINK),
            (Rtm::Newaddr, RTNLGRP_IPV4_IFADDR),
            (Rtm::Deladdr, RTNLGRP_IPV6_IFADDR),
            (Rtm::Newroute, RTNLGRP_IPV4_ROUTE),
            (Rtm::Delroute, RTNLGRP_IPV4_ROUTE),
            (Rtm::Newrule, RTNLGRP_IPV4_RULE),
            (Rtm::Delrule, RTNLGRP_IPV4_RULE),
        ];
        let (mut core, observer, mut receiver) = core();

        for (message_type, group) in events {
            let bytes = datagram(u16::from(message_type), group);
            core.notification(&bytes, 0, &Groups::new_groups(&[group]))
                .unwrap();
        }

        assert_eq!(observer.invalidations.load(Ordering::SeqCst), events.len());
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 0);
        assert_eq!(receiver.try_recv(), Ok(RouteReconciliationRequired));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[test]
    fn overrun_malformed_unicast_and_unsupported_notifications_poison() {
        let cases = [
            (
                datagram(u16::from(Nlmsg::Overrun), RTNLGRP_LINK),
                Groups::new_groups(&[RTNLGRP_LINK]),
                LinuxRouteMonitorError::ReceiveOverflow,
            ),
            (
                vec![1, 2, 3],
                Groups::new_groups(&[RTNLGRP_LINK]),
                LinuxRouteMonitorError::InvalidDatagram,
            ),
            (
                datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK),
                Groups::empty(),
                LinuxRouteMonitorError::UnsupportedNotification,
            ),
            (
                datagram(u16::from(Rtm::Newneigh), RTNLGRP_LINK),
                Groups::new_groups(&[RTNLGRP_LINK]),
                LinuxRouteMonitorError::UnsupportedNotification,
            ),
        ];

        for (bytes, groups, expected) in cases {
            let (mut core, observer, _receiver) = core();
            assert_eq!(core.notification(&bytes, 0, &groups), Err(expected));
            assert_eq!(observer.invalidations.load(Ordering::SeqCst), 0);
            assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn sockaddr_sender_pid_must_identify_the_kernel() {
        let bytes = datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK);
        let (mut core, observer, _receiver) = core();

        assert_eq!(
            core.notification(&bytes, 41, &Groups::new_groups(&[RTNLGRP_LINK])),
            Err(LinuxRouteMonitorError::InvalidDatagram)
        );
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 0);
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn authenticated_kernel_reports_may_carry_request_header_metadata() {
        let mut bytes = datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK);
        bytes[8..12].copy_from_slice(&73_u32.to_ne_bytes());
        bytes[12..16].copy_from_slice(&91_u32.to_ne_bytes());
        let (mut core, observer, mut receiver) = core();

        assert_eq!(
            core.notification(&bytes, 0, &Groups::new_groups(&[RTNLGRP_LINK])),
            Ok(())
        );
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 1);
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 0);
        assert_eq!(receiver.try_recv(), Ok(RouteReconciliationRequired));
    }

    #[test]
    fn hostile_top_level_lengths_and_special_types_never_reach_neli() {
        let cases = [
            raw_netlink_frame(0, u16::from(Rtm::Newlink)),
            raw_netlink_frame(15, u16::from(Rtm::Newlink)),
            raw_netlink_frame(17, u16::from(Rtm::Newlink)),
            raw_netlink_frame(u32::MAX, u16::from(Rtm::Newlink)),
            raw_netlink_frame(NETLINK_HEADER_BYTES as u32, u16::from(Nlmsg::Error)),
            raw_netlink_frame(NETLINK_HEADER_BYTES as u32, u16::from(Nlmsg::Done)),
        ];

        for bytes in cases {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                let (mut core, observer, _receiver) = core();
                let result = core.notification(&bytes, 0, &Groups::new_groups(&[RTNLGRP_LINK]));
                (result, observer.poisons.load(Ordering::SeqCst))
            }));
            assert!(outcome.is_ok(), "untrusted framing must not unwind");
            let (result, poisons) = outcome.expect("framing validation must return");
            assert!(matches!(
                result,
                Err(LinuxRouteMonitorError::InvalidDatagram)
                    | Err(LinuxRouteMonitorError::UnsupportedNotification)
            ));
            assert_eq!(poisons, 1);
        }
    }

    #[test]
    fn undersized_and_structurally_malformed_payloads_poison() {
        let cases = [
            (Rtm::Newlink, RTNLGRP_LINK, vec![0_u8; 8]),
            (Rtm::Newaddr, RTNLGRP_IPV4_IFADDR, vec![0_u8; 4]),
            (Rtm::Newroute, RTNLGRP_IPV4_ROUTE, vec![0_u8; 8]),
            (Rtm::Newrule, RTNLGRP_IPV4_RULE, vec![0_u8; 8]),
        ];

        for (message_type, group, payload) in cases {
            let bytes = datagram_with_payload(u16::from(message_type), &payload);
            let (mut core, observer, _receiver) = core();
            assert_eq!(
                core.notification(&bytes, 0, &Groups::new_groups(&[group])),
                Err(LinuxRouteMonitorError::InvalidDatagram)
            );
            assert_eq!(observer.invalidations.load(Ordering::SeqCst), 0);
            assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
        }

        let mut payload = vec![0_u8; 16];
        // A trailing rtattr whose declared length is smaller than its header.
        payload.extend_from_slice(&[3, 0, 0, 0]);
        let bytes = datagram_with_payload(u16::from(Rtm::Newlink), &payload);
        let (mut core, observer, _receiver) = core();
        assert_eq!(
            core.notification(&bytes, 0, &Groups::new_groups(&[RTNLGRP_LINK])),
            Err(LinuxRouteMonitorError::InvalidDatagram)
        );
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn source_group_must_be_exact_and_match_the_address_family() {
        let cases = [
            (
                datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK),
                vec![RTNLGRP_LINK, RTNLGRP_IPV4_ROUTE],
            ),
            (
                datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK),
                vec![RTNLGRP_IPV4_ROUTE],
            ),
            (
                datagram(u16::from(Rtm::Newaddr), RTNLGRP_IPV4_IFADDR),
                vec![RTNLGRP_IPV6_IFADDR],
            ),
            (
                datagram(u16::from(Rtm::Newaddr), RTNLGRP_IPV6_IFADDR),
                vec![RTNLGRP_IPV4_IFADDR],
            ),
        ];

        for (bytes, groups) in cases {
            let (mut core, observer, _receiver) = core();
            assert_eq!(
                core.notification(&bytes, 0, &Groups::new_groups(&groups)),
                Err(LinuxRouteMonitorError::UnsupportedNotification)
            );
            assert_eq!(observer.invalidations.load(Ordering::SeqCst), 0);
            assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn subscribe_without_a_runtime_fails_closed_without_panicking() {
        let observer = Arc::new(FakeObserver::default());
        let trait_observer: Arc<dyn RouteMonitorObserver> = observer.clone();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            LinuxRouteEventMonitor::subscribe(trait_observer)
        }));

        assert!(outcome.is_ok());
        assert!(matches!(
            outcome.expect("subscription must not unwind"),
            Err(LinuxRouteMonitorError::RuntimeRegistrationFailed)
        ));
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registration_with_io_disabled_is_converted_to_an_error() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("build runtime without I/O");
        let (socket, _peer) = UnixDatagram::pair().expect("create synthetic fd");
        let result = runtime.block_on(async move { register_readable(socket) });

        assert!(matches!(
            result,
            Err(LinuxRouteMonitorError::RuntimeRegistrationFailed)
        ));
    }

    #[test]
    fn production_monitor_api_typechecks_while_deliberately_unwired() {
        let _subscribe = LinuxRouteEventMonitor::subscribe;
        let _barrier = LinuxRouteEventMonitor::post_snapshot_barrier;
        let _run = LinuxRouteEventMonitor::run;
    }

    #[test]
    fn zero_oversize_enobufs_and_other_read_failures_poison() {
        let valid = datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK);
        let cases = [
            (
                FakeReceive::Datagram {
                    bytes: Vec::new(),
                    reported_length: 0,
                    source_pid: 0,
                    groups: vec![RTNLGRP_LINK],
                },
                LinuxRouteMonitorError::SocketClosed,
            ),
            (
                FakeReceive::Datagram {
                    bytes: valid,
                    reported_length: RECEIVE_BUFFER_BYTES + 1,
                    source_pid: 0,
                    groups: vec![RTNLGRP_LINK],
                },
                LinuxRouteMonitorError::ReceiveOverflow,
            ),
            (
                FakeReceive::Error(io::Error::from_raw_os_error(
                    rustix::io::Errno::NOBUFS.raw_os_error(),
                )),
                LinuxRouteMonitorError::ReceiveOverflow,
            ),
            (
                FakeReceive::Error(io::Error::new(io::ErrorKind::BrokenPipe, "synthetic")),
                LinuxRouteMonitorError::ReceiveFailed,
            ),
        ];

        for (step, expected) in cases {
            let (mut core, observer, _receiver) = core();
            let mut source = FakeSource::new([step]);
            let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
            assert_eq!(
                drain_available(
                    &mut source,
                    &mut core,
                    &mut buffer,
                    MAX_DATAGRAMS_PER_TURN,
                    MAX_BYTES_PER_TURN,
                ),
                Err(expected)
            );
            assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn each_regular_drain_turn_is_bounded_and_remains_lossless() {
        let steps = (0..=MAX_DATAGRAMS_PER_TURN)
            .map(|_| event(Rtm::Newlink, RTNLGRP_LINK))
            .chain([FakeReceive::Error(io::ErrorKind::WouldBlock.into())]);
        let mut source = FakeSource::new(steps);
        let (mut core, observer, mut receiver) = core();
        let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];

        let first = drain_available(
            &mut source,
            &mut core,
            &mut buffer,
            MAX_DATAGRAMS_PER_TURN,
            MAX_BYTES_PER_TURN,
        )
        .unwrap();
        assert_eq!(first.stop, DrainStop::BudgetExhausted);
        assert_eq!(first.datagrams, MAX_DATAGRAMS_PER_TURN);

        let second = drain_available(
            &mut source,
            &mut core,
            &mut buffer,
            MAX_DATAGRAMS_PER_TURN,
            MAX_BYTES_PER_TURN,
        )
        .unwrap();
        assert_eq!(second.stop, DrainStop::Quiescent);
        assert_eq!(second.datagrams, 1);
        assert_eq!(
            observer.invalidations.load(Ordering::SeqCst),
            MAX_DATAGRAMS_PER_TURN + 1
        );
        assert_eq!(receiver.try_recv(), Ok(RouteReconciliationRequired));
        assert_eq!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty));
    }

    #[test]
    fn a_drain_never_consumes_past_its_exact_byte_budget() {
        let first = datagram(u16::from(Rtm::Newlink), RTNLGRP_LINK);
        let exact_budget = first.len();
        let second = first.clone();
        let mut source = FakeSource::new([
            FakeReceive::Datagram {
                bytes: first,
                reported_length: exact_budget,
                source_pid: 0,
                groups: vec![RTNLGRP_LINK],
            },
            FakeReceive::Datagram {
                bytes: second,
                reported_length: exact_budget,
                source_pid: 0,
                groups: vec![RTNLGRP_LINK],
            },
            FakeReceive::Error(io::ErrorKind::WouldBlock.into()),
        ]);
        let (mut core, observer, _receiver) = core();
        let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];

        let first_turn = drain_available(
            &mut source,
            &mut core,
            &mut buffer,
            MAX_DATAGRAMS_PER_TURN,
            exact_budget,
        )
        .unwrap();
        assert_eq!(first_turn.stop, DrainStop::BudgetExhausted);
        assert_eq!(first_turn.datagrams, 1);
        assert_eq!(first_turn.bytes, exact_budget);
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 1);

        let second_turn = drain_available(
            &mut source,
            &mut core,
            &mut buffer,
            MAX_DATAGRAMS_PER_TURN,
            exact_budget,
        )
        .unwrap();
        assert_eq!(second_turn.stop, DrainStop::BudgetExhausted);
        assert_eq!(second_turn.datagrams, 1);
        assert_eq!(second_turn.bytes, exact_budget);
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn post_snapshot_barrier_requires_quiescence_and_reports_changes() {
        let (mut clean_core, clean_observer, _receiver) = core();
        let mut clean_source =
            FakeSource::new([FakeReceive::Error(io::ErrorKind::WouldBlock.into())]);
        let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
        assert_eq!(
            drain_post_snapshot(&mut clean_source, &mut clean_core, &mut buffer).await,
            Ok(PostSnapshotBarrier::Clean)
        );
        assert_eq!(clean_observer.invalidations.load(Ordering::SeqCst), 0);

        let (mut changed_core, changed_observer, mut receiver) = core();
        let mut changed_source = FakeSource::new([
            event(Rtm::Newaddr, RTNLGRP_IPV4_IFADDR),
            event(Rtm::Newaddr, RTNLGRP_IPV6_IFADDR),
            FakeReceive::Error(io::ErrorKind::WouldBlock.into()),
        ]);
        assert_eq!(
            drain_post_snapshot(&mut changed_source, &mut changed_core, &mut buffer).await,
            Ok(PostSnapshotBarrier::Changed)
        );
        assert_eq!(changed_observer.invalidations.load(Ordering::SeqCst), 2);
        assert_eq!(receiver.try_recv(), Ok(RouteReconciliationRequired));
    }

    #[tokio::test]
    async fn a_changed_subscription_can_never_become_clean_on_retry() {
        let (mut core, observer, _receiver) = core();
        let mut changed_source = FakeSource::new([
            event(Rtm::Newlink, RTNLGRP_LINK),
            FakeReceive::Error(io::ErrorKind::WouldBlock.into()),
        ]);
        let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];
        assert_eq!(
            drain_post_snapshot(&mut changed_source, &mut core, &mut buffer).await,
            Ok(PostSnapshotBarrier::Changed)
        );

        let mut quiescent_source =
            FakeSource::new([FakeReceive::Error(io::ErrorKind::WouldBlock.into())]);
        assert_eq!(
            drain_post_snapshot(&mut quiescent_source, &mut core, &mut buffer).await,
            Ok(PostSnapshotBarrier::Changed)
        );
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn post_snapshot_barrier_has_a_total_storm_bound() {
        let steps = (0..MAX_BARRIER_DATAGRAMS).map(|_| event(Rtm::Newroute, RTNLGRP_IPV4_ROUTE));
        let mut source = FakeSource::new(steps);
        let (mut core, observer, _receiver) = core();
        let mut buffer = vec![0_u8; RECEIVE_BUFFER_BYTES];

        assert_eq!(
            drain_post_snapshot(&mut source, &mut core, &mut buffer).await,
            Err(LinuxRouteMonitorError::BarrierLimitExceeded)
        );
        assert_eq!(
            observer.invalidations.load(Ordering::SeqCst),
            MAX_BARRIER_DATAGRAMS
        );
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_closed_reconciler_invalidates_first_and_then_poisons() {
        let (mut core, observer, receiver) = core();
        drop(receiver);
        let bytes = datagram(u16::from(Rtm::Newrule), RTNLGRP_IPV4_RULE);

        assert_eq!(
            core.notification(&bytes, 0, &Groups::new_groups(&[RTNLGRP_IPV4_RULE]),),
            Err(LinuxRouteMonitorError::ReconcilerUnavailable)
        );
        assert_eq!(observer.invalidations.load(Ordering::SeqCst), 1);
        assert_eq!(observer.poisons.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn errors_and_debug_output_are_topology_redacted() {
        let rendered = format!(
            "{:?} {}",
            LinuxRouteMonitorError::InvalidDatagram,
            LinuxRouteMonitorError::InvalidDatagram
        );
        assert!(!rendered.contains("192.168"));
        assert!(!rendered.contains("wg-test"));
    }
}
