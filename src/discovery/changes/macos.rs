//! macOS network-change observation over a `PF_ROUTE` routing socket.

use std::io;
use std::os::fd::OwnedFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use rustix::net::{AddressFamily, SocketType, socket, sockopt};
use tokio::io::{Interest, unix::AsyncFd};
use tokio::runtime::Handle;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

use super::observation::ObservationGate;
use super::route_socket::route_message_kinds;
use super::watch::{EventKinds, NetworkChangeWatchError, deliver_bursts};
use super::{InterfaceInventory, NetworkChange};

/// Each read returns one whole routing message: a header of at most a few
/// hundred bytes and at most `RTAX_MAX` (8) socket addresses, each at most
/// 255 bytes long. A truncated read is rejected as malformed.
const READ_BUFFER_BYTES: usize = 8 * 1024;
/// The kernel drops messages silently when this fills; a read error or a
/// malformed message is the only failure the socket can report.
const SOCKET_RECEIVE_BUFFER_BYTES: usize = 256 * 1024;
const MAX_READS_PER_TURN: usize = 64;

/// Debounced macOS network-change observation over a routing socket.
///
/// The routing socket is opened first; from then on the kernel queues every
/// link, address, and route message for it, so reading the interface
/// baseline afterwards leaves no gap. Every later burst is coalesced, the
/// inventory is diffed, and one [`NetworkChange`] naming the lost interfaces
/// is sent.
pub struct MacosNetworkChangeWatcher;

impl MacosNetworkChangeWatcher {
    /// Observe until `changes` closes (`Ok`) or the observation ends.
    ///
    /// `inventory` carries the last known interfaces across attempts: when
    /// it is `Some`, the previous attempt ended and events may have been
    /// missed, so one change is sent as soon as the new baseline exists.
    /// On return it holds the latest baseline this attempt established.
    /// `gate` is ready only while this attempt observes from a reconciled
    /// baseline; a failed, closed, or malformed read revokes it at once.
    pub async fn observe(
        changes: &mpsc::Sender<NetworkChange>,
        inventory: &mut Option<InterfaceInventory>,
        gate: &ObservationGate,
    ) -> Result<(), NetworkChangeWatchError> {
        let runtime =
            Handle::try_current().map_err(|_| NetworkChangeWatchError::RuntimeUnavailable)?;
        let socket = subscribe().map_err(|_| NetworkChangeWatchError::MonitorUnavailable)?;
        let current = InterfaceInventory::current()
            .map_err(|_| NetworkChangeWatchError::InventoryUnavailable)?;
        let kinds = Arc::new(EventKinds::revoking(gate.clone()));
        let (signal, mut events) = mpsc::channel(1);
        let reader = AbortOnDropHandle::new(runtime.spawn(read_messages(
            socket,
            Arc::clone(&kinds),
            signal,
        )));
        let outcome = deliver_bursts(
            changes,
            inventory,
            current,
            &kinds,
            &mut events,
            InterfaceInventory::current,
            gate,
        )
        .await;
        if outcome.is_err() {
            let _ = reader.await;
        }
        outcome
    }
}

/// Open a nonblocking routing socket and register it with the runtime.
fn subscribe() -> io::Result<AsyncFd<OwnedFd>> {
    let socket = socket(AddressFamily::ROUTE, SocketType::RAW, None)?;
    sockopt::set_socket_recv_buffer_size(&socket, SOCKET_RECEIVE_BUFFER_BYTES)?;
    rustix::io::ioctl_fionbio(&socket, true)?;
    // AsyncFd's constructors panic on a runtime without I/O; keep that from
    // escaping, and fail closed instead.
    catch_unwind(AssertUnwindSafe(|| {
        AsyncFd::with_interest(socket, Interest::READABLE)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("the runtime has no I/O driver")))
}

/// Record the kind of every relevant message, then wake the watcher.
///
/// Returns, dropping `signal` and so ending the observation, when the socket
/// fails, closes, or yields a malformed read, or once the watcher is gone.
/// Readiness is revoked the moment reading stops, before the watcher notices.
async fn read_messages(socket: AsyncFd<OwnedFd>, kinds: Arc<EventKinds>, signal: mpsc::Sender<()>) {
    read_until_stopped(socket, &kinds, signal).await;
    kinds.revoke();
}

async fn read_until_stopped(
    socket: AsyncFd<OwnedFd>,
    kinds: &EventKinds,
    signal: mpsc::Sender<()>,
) {
    let mut buffer = vec![0_u8; READ_BUFFER_BYTES].into_boxed_slice();
    loop {
        let mut ready = tokio::select! {
            () = signal.closed() => return,
            ready = socket.readable() => match ready {
                Ok(ready) => ready,
                Err(_) => return,
            },
        };
        let mut reads = 0;
        loop {
            let read = ready.try_io(|socket| {
                rustix::io::read(socket.get_ref(), &mut buffer[..]).map_err(io::Error::from)
            });
            let length = match read {
                // Drained; readiness was cleared.
                Err(_would_block) => break,
                Ok(Ok(0)) => return,
                Ok(Ok(length)) => length,
                Ok(Err(error)) if error.kind() == io::ErrorKind::Interrupted => continue,
                Ok(Err(_)) => return,
            };
            let Some(message_kinds) = buffer
                .get(..length)
                .and_then(|bytes| route_message_kinds(bytes).ok())
            else {
                return;
            };
            if !message_kinds.is_empty() {
                for kind in message_kinds {
                    kinds.record(kind);
                }
                // A full channel already holds a wake-up.
                if let Err(mpsc::error::TrySendError::Closed(())) = signal.try_send(()) {
                    return;
                }
            }
            reads += 1;
            if reads == MAX_READS_PER_TURN {
                drop(ready);
                tokio::task::yield_now().await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::runtime::Builder;

    use super::*;

    #[test]
    fn observation_without_io_fails_closed_before_any_baseline() {
        let runtime = Builder::new_current_thread().build().unwrap();
        let (changes, _receiver) = mpsc::channel(1);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let outcome = runtime.block_on(MacosNetworkChangeWatcher::observe(
            &changes,
            &mut inventory,
            &gate,
        ));
        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorUnavailable));
        assert!(inventory.is_none());
        assert_eq!(gate.state().generation(), None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observation_establishes_a_baseline_then_stops_with_its_receiver() {
        let (changes, receiver) = mpsc::channel(1);
        drop(receiver);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            MacosNetworkChangeWatcher::observe(&changes, &mut inventory, &gate),
        )
        .await
        .expect("observation must notice its closed receiver promptly");
        match outcome {
            Ok(()) => assert!(
                inventory.is_some(),
                "a clean observation leaves its baseline behind"
            ),
            // A sandbox without routing sockets fails closed.
            Err(NetworkChangeWatchError::MonitorUnavailable) => {}
            Err(error) => panic!("unexpected observation failure {error:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_open_observation_is_ready_and_stops_promptly_when_cancelled() {
        let (changes, _receiver) = mpsc::channel(1);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let mut watch = gate.watch();
        {
            let mut observation = std::pin::pin!(MacosNetworkChangeWatcher::observe(
                &changes,
                &mut inventory,
                &gate,
            ));
            let ready = tokio::select! {
                outcome = &mut observation => match outcome {
                    // A sandbox without routing sockets fails closed.
                    Err(NetworkChangeWatchError::MonitorUnavailable) => return,
                    other => panic!("unexpected end of observation {other:?}"),
                },
                state = watch.changed() => state,
            };
            assert!(ready.generation().is_some(), "a baseline makes it ready");
            // The observation runs until cancelled; dropping it closes the
            // socket.
            let outcome = tokio::time::timeout(Duration::from_millis(500), observation).await;
            assert!(
                outcome.is_err(),
                "unexpected end of observation {outcome:?}"
            );
        }
        assert!(inventory.is_some(), "the baseline exists while observing");
        assert_eq!(
            watch.current().generation(),
            None,
            "a cancelled observation is no longer ready"
        );
    }
}
