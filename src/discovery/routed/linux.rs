//! Verified Linux interface-pinned UDP socket construction.
//!
//! This layer creates an unconnected, nonblocking standard-library socket and
//! keeps it inside an opaque capability. It never registers with an async
//! runtime, exposes a raw descriptor, or sends a packet. Production
//! route-derived execution remains deliberately unwired.
//!
//! Linux added the `SO_BINDTOIFINDEX` readback used by this proof in 5.1 and
//! made the first `SO_BINDTODEVICE` assignment unprivileged in 5.7. Balun does
//! not infer success from a kernel version: if either operation is absent or
//! denied, construction fails closed without an unpinned fallback.
//!
//! The double readback proves the pin only at construction time, so the only
//! I/O view, [`PinnedProbeSocket`], re-reads both pin views and consults the
//! runner's authority check immediately before every datagram. The monitored
//! runner arms route and store invalidation before consuming admission and
//! retains it for the whole run.

#![allow(
    dead_code,
    reason = "the monitored routed runner has no production caller until the approval UX lands"
)]

use std::fmt;
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket as StdUdpSocket};
use std::num::NonZeroU32;
use std::sync::Arc;

use socket2::{Domain, Protocol, SockAddr, SockRef, Socket, Type};
use thiserror::Error;

use crate::discovery::client::ProbeSocket;
use crate::discovery::routes::InterfaceId;

const LINUX_INTERFACE_NAME_MAX_BYTES: usize = 15;

/// A nonblocking UDP socket whose Linux interface pin was verified twice.
///
/// The socket stays private so callers cannot construct this capability around
/// an unverified descriptor, clear its pin, or perform I/O directly. The only
/// I/O view is [`Self::into_probe_socket`], which re-checks the caller's
/// authority and the pin immediately before every send. This type
/// intentionally does not implement `Clone` or a raw-descriptor trait.
pub(in crate::discovery) struct PinnedRoutedUdpSocket {
    socket: StdUdpSocket,
    interface_name: Vec<u8>,
    interface_id: NonZeroU32,
}

impl PinnedRoutedUdpSocket {
    /// Register the pinned descriptor with the current Tokio runtime for one
    /// bounded probe. `authority` is consulted before every datagram; when it
    /// returns `false`, or the pin no longer reads back, the send is refused
    /// and nothing leaves the socket.
    pub(in crate::discovery) fn into_probe_socket(
        self,
        authority: PreSendAuthority,
    ) -> Result<PinnedProbeSocket, PinnedRoutedUdpSocketError> {
        let socket = tokio::net::UdpSocket::from_std(self.socket)
            .map_err(|_| PinnedRoutedUdpSocketError::RegisterWithRuntime)?;
        Ok(PinnedProbeSocket {
            socket,
            interface_name: self.interface_name,
            interface_id: self.interface_id,
            authority,
        })
    }
}

/// Authority check consulted immediately before every datagram a pinned
/// probe socket sends. It must be cheap and must never block.
pub(crate) type PreSendAuthority = Arc<dyn Fn() -> bool + Send + Sync>;

/// Why a pinned probe socket refused to send one datagram.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(in crate::discovery) enum PinnedSendRefusal {
    #[error("routed authority is no longer current at the pre-send boundary")]
    AuthorityLost,
    #[error("the Linux socket pin could not be re-verified before sending: {0}")]
    Pin(PinnedRoutedUdpSocketError),
}

/// The only I/O view of a pinned socket. Every send re-checks the caller's
/// authority and reads both pin views back; receives need no check because
/// the pin already confines them to the interface.
pub(in crate::discovery) struct PinnedProbeSocket {
    socket: tokio::net::UdpSocket,
    interface_name: Vec<u8>,
    interface_id: NonZeroU32,
    authority: PreSendAuthority,
}

impl PinnedProbeSocket {
    fn verify_before_send(&self) -> Result<(), PinnedSendRefusal> {
        if !(self.authority)() {
            return Err(PinnedSendRefusal::AuthorityLost);
        }
        verify_interface_pin(
            &mut TokioReadbackOps,
            &self.socket,
            &self.interface_name,
            self.interface_id,
        )
        .map_err(PinnedSendRefusal::Pin)
    }
}

impl fmt::Debug for PinnedProbeSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PinnedProbeSocket(<redacted>)")
    }
}

impl ProbeSocket for PinnedProbeSocket {
    async fn send_to(&self, buffer: &[u8], target: SocketAddr) -> io::Result<usize> {
        self.verify_before_send().map_err(io::Error::other)?;
        self.socket.send_to(buffer, target).await
    }

    fn recv_from(
        &self,
        buffer: &mut [u8],
    ) -> impl Future<Output = io::Result<(usize, SocketAddr)>> + Send {
        self.socket.recv_from(buffer)
    }
}

/// Pin readback through Tokio's registered descriptor; every mutating
/// operation is refused because the socket was fully prepared before
/// registration.
struct TokioReadbackOps;

impl PinnedUdpSocketOps for TokioReadbackOps {
    type Socket = tokio::net::UdpSocket;

    fn create_ipv4_udp(&mut self) -> io::Result<Self::Socket> {
        Err(io::Error::other("readback only"))
    }

    fn bind_to_device(&mut self, _socket: &Self::Socket, _name: &[u8]) -> io::Result<()> {
        Err(io::Error::other("readback only"))
    }

    fn device_name(&mut self, socket: &Self::Socket) -> io::Result<Option<Vec<u8>>> {
        SockRef::from(socket).device()
    }

    fn device_index(&mut self, socket: &Self::Socket) -> io::Result<Option<NonZeroU32>> {
        SockRef::from(socket).device_index_v4()
    }

    fn bind_wildcard_ephemeral(&mut self, _socket: &Self::Socket) -> io::Result<()> {
        Err(io::Error::other("readback only"))
    }

    fn set_nonblocking(&mut self, _socket: &Self::Socket) -> io::Result<()> {
        Err(io::Error::other("readback only"))
    }
}

impl fmt::Debug for PinnedRoutedUdpSocket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PinnedRoutedUdpSocket(<redacted>)")
    }
}

/// Topology-redacted reason Linux socket pinning could not be proven.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(in crate::discovery) enum PinnedRoutedUdpSocketError {
    #[error("the Linux interface name is invalid for socket pinning")]
    InvalidInterfaceName,

    #[error("the Linux interface identifier is invalid for socket pinning")]
    InvalidInterfaceId,

    #[error("the Linux UDP socket could not be created")]
    CreateSocket,

    #[error("the Linux UDP socket could not be pinned to its interface")]
    PinInterface,

    #[error("the Linux UDP socket interface name could not be verified")]
    ReadInterfaceName,

    #[error("the Linux UDP socket interface name did not match")]
    InterfaceNameMismatch,

    #[error("the Linux UDP socket interface identifier could not be verified")]
    ReadInterfaceId,

    #[error("the Linux UDP socket interface identifier did not match")]
    InterfaceIdMismatch,

    #[error("the Linux UDP socket could not bind an ephemeral local port")]
    BindLocal,

    #[error("the Linux UDP socket could not be made nonblocking")]
    SetNonblocking,

    #[error("the Linux UDP socket could not be registered with the async runtime")]
    RegisterWithRuntime,
}

/// Create an IPv4 UDP socket and prove its interface pin survives local bind.
///
/// The identifier and name must come from the same fresh route snapshot. No
/// fallback to an unpinned socket is attempted. `SO_BINDTODEVICE` is written
/// exactly once, then both its name view and `SO_BINDTOIFINDEX` are checked
/// before and after binding `0.0.0.0:0`.
pub(in crate::discovery) fn open_pinned_routed_udp_socket(
    interface_id: InterfaceId,
    interface_name: &str,
) -> Result<PinnedRoutedUdpSocket, PinnedRoutedUdpSocketError> {
    let expected_name = validate_interface_name(interface_name)?.to_vec();
    let expected_id = validate_interface_id(interface_id)?;
    let mut ops = Socket2Ops;
    let socket = open_pinned_routed_udp_socket_with(&mut ops, interface_id, interface_name)?;
    Ok(PinnedRoutedUdpSocket {
        socket: StdUdpSocket::from(socket),
        interface_name: expected_name,
        interface_id: expected_id,
    })
}

trait PinnedUdpSocketOps {
    type Socket;

    fn create_ipv4_udp(&mut self) -> io::Result<Self::Socket>;
    fn bind_to_device(&mut self, socket: &Self::Socket, name: &[u8]) -> io::Result<()>;
    fn device_name(&mut self, socket: &Self::Socket) -> io::Result<Option<Vec<u8>>>;
    fn device_index(&mut self, socket: &Self::Socket) -> io::Result<Option<NonZeroU32>>;
    fn bind_wildcard_ephemeral(&mut self, socket: &Self::Socket) -> io::Result<()>;
    fn set_nonblocking(&mut self, socket: &Self::Socket) -> io::Result<()>;
}

struct Socket2Ops;

impl PinnedUdpSocketOps for Socket2Ops {
    type Socket = Socket;

    fn create_ipv4_udp(&mut self) -> io::Result<Self::Socket> {
        Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))
    }

    fn bind_to_device(&mut self, socket: &Self::Socket, name: &[u8]) -> io::Result<()> {
        socket.bind_device(Some(name))
    }

    fn device_name(&mut self, socket: &Self::Socket) -> io::Result<Option<Vec<u8>>> {
        socket.device()
    }

    fn device_index(&mut self, socket: &Self::Socket) -> io::Result<Option<NonZeroU32>> {
        socket.device_index_v4()
    }

    fn bind_wildcard_ephemeral(&mut self, socket: &Self::Socket) -> io::Result<()> {
        let address = SockAddr::from(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0));
        socket.bind(&address)
    }

    fn set_nonblocking(&mut self, socket: &Self::Socket) -> io::Result<()> {
        socket.set_nonblocking(true)
    }
}

fn open_pinned_routed_udp_socket_with<O: PinnedUdpSocketOps>(
    ops: &mut O,
    interface_id: InterfaceId,
    interface_name: &str,
) -> Result<O::Socket, PinnedRoutedUdpSocketError> {
    let interface_name = validate_interface_name(interface_name)?;
    let interface_id = validate_interface_id(interface_id)?;

    let socket = ops
        .create_ipv4_udp()
        .map_err(|_| PinnedRoutedUdpSocketError::CreateSocket)?;
    ops.bind_to_device(&socket, interface_name)
        .map_err(|_| PinnedRoutedUdpSocketError::PinInterface)?;
    verify_interface_pin(ops, &socket, interface_name, interface_id)?;

    ops.bind_wildcard_ephemeral(&socket)
        .map_err(|_| PinnedRoutedUdpSocketError::BindLocal)?;
    verify_interface_pin(ops, &socket, interface_name, interface_id)?;

    ops.set_nonblocking(&socket)
        .map_err(|_| PinnedRoutedUdpSocketError::SetNonblocking)?;
    Ok(socket)
}

fn validate_interface_name(name: &str) -> Result<&[u8], PinnedRoutedUdpSocketError> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > LINUX_INTERFACE_NAME_MAX_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(PinnedRoutedUdpSocketError::InvalidInterfaceName);
    }
    Ok(bytes)
}

fn validate_interface_id(id: InterfaceId) -> Result<NonZeroU32, PinnedRoutedUdpSocketError> {
    u32::try_from(id.get())
        .ok()
        .filter(|id| i32::try_from(*id).is_ok())
        .and_then(NonZeroU32::new)
        .ok_or(PinnedRoutedUdpSocketError::InvalidInterfaceId)
}

fn verify_interface_pin<O: PinnedUdpSocketOps>(
    ops: &mut O,
    socket: &O::Socket,
    expected_name: &[u8],
    expected_id: NonZeroU32,
) -> Result<(), PinnedRoutedUdpSocketError> {
    let actual_name = ops
        .device_name(socket)
        .map_err(|_| PinnedRoutedUdpSocketError::ReadInterfaceName)?;
    if actual_name.as_deref() != Some(expected_name) {
        return Err(PinnedRoutedUdpSocketError::InterfaceNameMismatch);
    }

    let actual_id = ops
        .device_index(socket)
        .map_err(|_| PinnedRoutedUdpSocketError::ReadInterfaceId)?;
    if actual_id != Some(expected_id) {
        return Err(PinnedRoutedUdpSocketError::InterfaceIdMismatch);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsFd, AsRawFd};
    #[cfg(feature = "desktop")]
    use std::sync::atomic::{AtomicBool, Ordering};

    #[cfg(feature = "desktop")]
    use tokio_util::sync::CancellationToken;

    use super::*;
    #[cfg(feature = "desktop")]
    use crate::discovery::client::{DiscoveryClient, DiscoveryError};
    #[cfg(feature = "desktop")]
    use crate::discovery::types::DiscoveryMethod;
    #[cfg(feature = "desktop")]
    use crate::hdhr::fake_device::FakeHdhrDevice;

    #[cfg(feature = "desktop")]
    fn loopback_interface_id() -> InterfaceId {
        let text = std::fs::read_to_string("/sys/class/net/lo/ifindex")
            .expect("Linux exposes the loopback interface index");
        InterfaceId::new(text.trim().parse().expect("numeric loopback index"))
    }

    #[cfg(feature = "desktop")]
    #[tokio::test]
    async fn pinned_probe_socket_reaches_a_loopback_responder_only_while_authority_holds() {
        let device = FakeHdhrDevice::start(1, &[]);
        let pinned = match open_pinned_routed_udp_socket(loopback_interface_id(), "lo") {
            Ok(pinned) => pinned,
            Err(PinnedRoutedUdpSocketError::PinInterface) => {
                eprintln!("skipping: this kernel refuses SO_BINDTODEVICE without privileges");
                return;
            }
            Err(error) => panic!("pin the loopback interface: {error}"),
        };
        let live = Arc::new(AtomicBool::new(true));
        let authority: PreSendAuthority = {
            let live = Arc::clone(&live);
            Arc::new(move || live.load(Ordering::SeqCst))
        };
        let socket = pinned
            .into_probe_socket(authority)
            .expect("register the pinned socket with the runtime");
        assert!(!format!("{socket:?}").contains("lo"));

        let client = DiscoveryClient::default();
        let report = client
            .discover_routed_target_through(&socket, Ipv4Addr::LOCALHOST, &CancellationToken::new())
            .await
            .expect("probe the loopback responder through the pinned socket");
        assert_eq!(report.observations.len(), 1);
        assert_eq!(report.observations[0].device_id, device.device_id());
        assert_eq!(
            report.observations[0].method,
            DiscoveryMethod::RoutedTargeted
        );
        assert!(report.stats.datagrams_sent >= 1);
        assert!(report.stats.datagrams_accepted >= 1);

        live.store(false, Ordering::SeqCst);
        let refused = client
            .discover_routed_target_through(&socket, Ipv4Addr::LOCALHOST, &CancellationToken::new())
            .await
            .expect_err("lost authority must refuse the send");
        assert!(
            matches!(
                refused,
                DiscoveryError::Io {
                    operation: "send discovery request",
                    ..
                }
            ),
            "{refused:?}"
        );
        let text = refused.to_string();
        assert!(text.contains("authority is no longer current"), "{text}");
        assert!(!text.contains("lo\""), "{text}");
    }

    const VALID_ID: u32 = 17;
    const VALID_NAME: &str = "wg-test0";

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Operation {
        Create,
        Pin,
        ReadName,
        ReadId,
        Bind,
        Nonblocking,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Failure {
        Create,
        Pin,
        FirstNameRead,
        FirstNameMissing,
        FirstNameMismatch,
        FirstIdRead,
        FirstIdMissing,
        FirstIdMismatch,
        Bind,
        SecondNameRead,
        SecondNameMissing,
        SecondNameMismatch,
        SecondIdRead,
        SecondIdMissing,
        SecondIdMismatch,
        Nonblocking,
    }

    #[derive(Debug, Eq, PartialEq)]
    struct FakeSocket;

    struct FakeOps {
        failure: Option<Failure>,
        operations: Vec<Operation>,
        name_reads: usize,
        id_reads: usize,
        pinned_name: Vec<u8>,
        reported_id: u32,
    }

    impl Default for FakeOps {
        fn default() -> Self {
            Self {
                failure: None,
                operations: Vec::new(),
                name_reads: 0,
                id_reads: 0,
                pinned_name: Vec::new(),
                reported_id: VALID_ID,
            }
        }
    }

    impl FakeOps {
        fn failing(failure: Failure) -> Self {
            Self {
                failure: Some(failure),
                ..Self::default()
            }
        }

        fn should_fail(&self, failure: Failure) -> bool {
            self.failure == Some(failure)
        }

        fn operation_count(&self, operation: Operation) -> usize {
            self.operations
                .iter()
                .filter(|candidate| **candidate == operation)
                .count()
        }
    }

    impl PinnedUdpSocketOps for FakeOps {
        type Socket = FakeSocket;

        fn create_ipv4_udp(&mut self) -> io::Result<Self::Socket> {
            self.operations.push(Operation::Create);
            if self.should_fail(Failure::Create) {
                Err(io::Error::other("sensitive create detail"))
            } else {
                Ok(FakeSocket)
            }
        }

        fn bind_to_device(&mut self, _socket: &Self::Socket, name: &[u8]) -> io::Result<()> {
            self.operations.push(Operation::Pin);
            self.pinned_name = name.to_vec();
            if self.should_fail(Failure::Pin) {
                Err(io::Error::other("sensitive pin detail"))
            } else {
                Ok(())
            }
        }

        fn device_name(&mut self, _socket: &Self::Socket) -> io::Result<Option<Vec<u8>>> {
            self.operations.push(Operation::ReadName);
            self.name_reads += 1;
            let (read_failure, missing, mismatch) = if self.name_reads == 1 {
                (
                    Failure::FirstNameRead,
                    Failure::FirstNameMissing,
                    Failure::FirstNameMismatch,
                )
            } else {
                (
                    Failure::SecondNameRead,
                    Failure::SecondNameMissing,
                    Failure::SecondNameMismatch,
                )
            };
            if self.should_fail(read_failure) {
                Err(io::Error::other("sensitive name detail"))
            } else if self.should_fail(missing) {
                Ok(None)
            } else if self.should_fail(mismatch) {
                Ok(Some(b"different-device".to_vec()))
            } else {
                Ok(Some(self.pinned_name.clone()))
            }
        }

        fn device_index(&mut self, _socket: &Self::Socket) -> io::Result<Option<NonZeroU32>> {
            self.operations.push(Operation::ReadId);
            self.id_reads += 1;
            let (read_failure, missing, mismatch) = if self.id_reads == 1 {
                (
                    Failure::FirstIdRead,
                    Failure::FirstIdMissing,
                    Failure::FirstIdMismatch,
                )
            } else {
                (
                    Failure::SecondIdRead,
                    Failure::SecondIdMissing,
                    Failure::SecondIdMismatch,
                )
            };
            if self.should_fail(read_failure) {
                Err(io::Error::other("sensitive id detail"))
            } else if self.should_fail(missing) {
                Ok(None)
            } else if self.should_fail(mismatch) {
                Ok(NonZeroU32::new(self.reported_id.saturating_sub(1)))
            } else {
                Ok(NonZeroU32::new(self.reported_id))
            }
        }

        fn bind_wildcard_ephemeral(&mut self, _socket: &Self::Socket) -> io::Result<()> {
            self.operations.push(Operation::Bind);
            if self.should_fail(Failure::Bind) {
                Err(io::Error::other("sensitive bind detail"))
            } else {
                Ok(())
            }
        }

        fn set_nonblocking(&mut self, _socket: &Self::Socket) -> io::Result<()> {
            self.operations.push(Operation::Nonblocking);
            if self.should_fail(Failure::Nonblocking) {
                Err(io::Error::other("sensitive flag detail"))
            } else {
                Ok(())
            }
        }
    }

    fn open_with(ops: &mut FakeOps) -> Result<FakeSocket, PinnedRoutedUdpSocketError> {
        open_pinned_routed_udp_socket_with(ops, InterfaceId::new(u64::from(VALID_ID)), VALID_NAME)
    }

    #[test]
    fn successful_factory_has_one_pin_and_exact_verification_order() {
        let mut ops = FakeOps::default();

        assert_eq!(open_with(&mut ops), Ok(FakeSocket));
        assert_eq!(
            ops.operations,
            [
                Operation::Create,
                Operation::Pin,
                Operation::ReadName,
                Operation::ReadId,
                Operation::Bind,
                Operation::ReadName,
                Operation::ReadId,
                Operation::Nonblocking,
            ]
        );
        assert_eq!(ops.operation_count(Operation::Pin), 1);
        assert_eq!(ops.operation_count(Operation::Bind), 1);
        assert_eq!(ops.pinned_name, VALID_NAME.as_bytes());
    }

    #[test]
    fn invalid_names_fail_before_socket_creation() {
        for name in [
            "",
            "0123456789abcdef",
            "wg\0hidden",
            "wg\nline",
            "wg\u{85}ctl",
        ] {
            let mut ops = FakeOps::default();
            assert_eq!(
                open_pinned_routed_udp_socket_with(
                    &mut ops,
                    InterfaceId::new(u64::from(VALID_ID)),
                    name,
                ),
                Err(PinnedRoutedUdpSocketError::InvalidInterfaceName)
            );
            assert!(ops.operations.is_empty());
        }
    }

    #[test]
    fn fifteen_byte_and_utf8_names_are_forwarded_exactly() {
        for name in ["0123456789abcde", "wg-éé"] {
            let mut ops = FakeOps::default();
            assert_eq!(
                open_pinned_routed_udp_socket_with(
                    &mut ops,
                    InterfaceId::new(u64::from(VALID_ID)),
                    name,
                ),
                Ok(FakeSocket)
            );
            assert_eq!(ops.pinned_name, name.as_bytes());
        }
    }

    #[test]
    fn invalid_ids_fail_before_socket_creation() {
        for id in [
            0,
            u64::try_from(i32::MAX).unwrap() + 1,
            u64::from(u32::MAX),
            u64::from(u32::MAX) + 1,
            u64::MAX,
        ] {
            let mut ops = FakeOps::default();
            assert_eq!(
                open_pinned_routed_udp_socket_with(&mut ops, InterfaceId::new(id), VALID_NAME),
                Err(PinnedRoutedUdpSocketError::InvalidInterfaceId)
            );
            assert!(ops.operations.is_empty());
        }
    }

    #[test]
    fn maximum_positive_linux_interface_id_is_accepted() {
        let maximum = u32::try_from(i32::MAX).unwrap();
        let mut ops = FakeOps {
            reported_id: maximum,
            ..FakeOps::default()
        };
        assert_eq!(
            open_pinned_routed_udp_socket_with(
                &mut ops,
                InterfaceId::new(u64::from(maximum)),
                VALID_NAME,
            ),
            Ok(FakeSocket)
        );
    }

    #[test]
    fn capability_is_not_clone_or_a_raw_socket_handoff() {
        trait AmbiguousIfClone<A> {
            fn marker() {}
        }
        impl<T> AmbiguousIfClone<()> for T {}
        impl<T: Clone> AmbiguousIfClone<u8> for T {}

        trait AmbiguousIfAsRawFd<A> {
            fn marker() {}
        }
        impl<T> AmbiguousIfAsRawFd<()> for T {}
        impl<T: AsRawFd> AmbiguousIfAsRawFd<u8> for T {}

        trait AmbiguousIfAsFd<A> {
            fn marker() {}
        }
        impl<T> AmbiguousIfAsFd<()> for T {}
        impl<T: AsFd> AmbiguousIfAsFd<u8> for T {}

        trait AmbiguousIfIntoStdSocket<A> {
            fn marker() {}
        }
        impl<T> AmbiguousIfIntoStdSocket<()> for T {}
        impl<T: Into<StdUdpSocket>> AmbiguousIfIntoStdSocket<u8> for T {}

        let _ = <PinnedRoutedUdpSocket as AmbiguousIfClone<_>>::marker;
        let _ = <PinnedRoutedUdpSocket as AmbiguousIfAsRawFd<_>>::marker;
        let _ = <PinnedRoutedUdpSocket as AmbiguousIfAsFd<_>>::marker;
        let _ = <PinnedRoutedUdpSocket as AmbiguousIfIntoStdSocket<_>>::marker;
    }

    #[test]
    fn every_failure_before_local_bind_stops_before_bind_or_handoff() {
        let cases: &[(Failure, PinnedRoutedUdpSocketError, &[Operation])] = &[
            (
                Failure::Create,
                PinnedRoutedUdpSocketError::CreateSocket,
                &[Operation::Create],
            ),
            (
                Failure::Pin,
                PinnedRoutedUdpSocketError::PinInterface,
                &[Operation::Create, Operation::Pin],
            ),
            (
                Failure::FirstNameRead,
                PinnedRoutedUdpSocketError::ReadInterfaceName,
                &[Operation::Create, Operation::Pin, Operation::ReadName],
            ),
            (
                Failure::FirstNameMissing,
                PinnedRoutedUdpSocketError::InterfaceNameMismatch,
                &[Operation::Create, Operation::Pin, Operation::ReadName],
            ),
            (
                Failure::FirstNameMismatch,
                PinnedRoutedUdpSocketError::InterfaceNameMismatch,
                &[Operation::Create, Operation::Pin, Operation::ReadName],
            ),
            (
                Failure::FirstIdRead,
                PinnedRoutedUdpSocketError::ReadInterfaceId,
                &[
                    Operation::Create,
                    Operation::Pin,
                    Operation::ReadName,
                    Operation::ReadId,
                ],
            ),
            (
                Failure::FirstIdMissing,
                PinnedRoutedUdpSocketError::InterfaceIdMismatch,
                &[
                    Operation::Create,
                    Operation::Pin,
                    Operation::ReadName,
                    Operation::ReadId,
                ],
            ),
            (
                Failure::FirstIdMismatch,
                PinnedRoutedUdpSocketError::InterfaceIdMismatch,
                &[
                    Operation::Create,
                    Operation::Pin,
                    Operation::ReadName,
                    Operation::ReadId,
                ],
            ),
        ];

        for &(failure, expected_error, expected_operations) in cases {
            let mut ops = FakeOps::failing(failure);
            assert_eq!(
                open_with(&mut ops),
                Err(expected_error),
                "failure: {failure:?}"
            );
            assert_eq!(ops.operations, expected_operations, "failure: {failure:?}");
            assert_eq!(
                ops.operation_count(Operation::Bind),
                0,
                "failure: {failure:?}"
            );
        }
    }

    #[test]
    fn every_failure_after_local_bind_stops_before_socket_handoff() {
        let through_bind = &[
            Operation::Create,
            Operation::Pin,
            Operation::ReadName,
            Operation::ReadId,
            Operation::Bind,
        ];
        let through_second_name = &[
            Operation::Create,
            Operation::Pin,
            Operation::ReadName,
            Operation::ReadId,
            Operation::Bind,
            Operation::ReadName,
        ];
        let through_second_id = &[
            Operation::Create,
            Operation::Pin,
            Operation::ReadName,
            Operation::ReadId,
            Operation::Bind,
            Operation::ReadName,
            Operation::ReadId,
        ];
        let through_nonblocking = &[
            Operation::Create,
            Operation::Pin,
            Operation::ReadName,
            Operation::ReadId,
            Operation::Bind,
            Operation::ReadName,
            Operation::ReadId,
            Operation::Nonblocking,
        ];
        let cases: &[(Failure, PinnedRoutedUdpSocketError, &[Operation])] = &[
            (
                Failure::Bind,
                PinnedRoutedUdpSocketError::BindLocal,
                through_bind,
            ),
            (
                Failure::SecondNameRead,
                PinnedRoutedUdpSocketError::ReadInterfaceName,
                through_second_name,
            ),
            (
                Failure::SecondNameMissing,
                PinnedRoutedUdpSocketError::InterfaceNameMismatch,
                through_second_name,
            ),
            (
                Failure::SecondNameMismatch,
                PinnedRoutedUdpSocketError::InterfaceNameMismatch,
                through_second_name,
            ),
            (
                Failure::SecondIdRead,
                PinnedRoutedUdpSocketError::ReadInterfaceId,
                through_second_id,
            ),
            (
                Failure::SecondIdMissing,
                PinnedRoutedUdpSocketError::InterfaceIdMismatch,
                through_second_id,
            ),
            (
                Failure::SecondIdMismatch,
                PinnedRoutedUdpSocketError::InterfaceIdMismatch,
                through_second_id,
            ),
            (
                Failure::Nonblocking,
                PinnedRoutedUdpSocketError::SetNonblocking,
                through_nonblocking,
            ),
        ];

        for &(failure, expected_error, expected_operations) in cases {
            let mut ops = FakeOps::failing(failure);
            assert_eq!(
                open_with(&mut ops),
                Err(expected_error),
                "failure: {failure:?}"
            );
            assert_eq!(ops.operations, expected_operations, "failure: {failure:?}");
            assert_eq!(
                ops.operation_count(Operation::Bind),
                1,
                "failure: {failure:?}"
            );
        }
    }

    #[test]
    fn errors_do_not_retain_or_render_topology_or_operation_details() {
        let secret_name = "wg-secret";
        let secret_id = 123_456_u64;
        let mut ops = FakeOps::failing(Failure::Pin);

        let error =
            open_pinned_routed_udp_socket_with(&mut ops, InterfaceId::new(secret_id), secret_name)
                .unwrap_err();
        let rendered = format!("{error:?} {error}");

        assert!(!rendered.contains(secret_name));
        assert!(!rendered.contains(&secret_id.to_string()));
        assert!(!rendered.contains("sensitive pin detail"));
    }
}
