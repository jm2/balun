//! Windows network-change observation over IP Helper notifications.

use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::observation::ObservationGate;
use super::watch::{EventKinds, NetworkChangeWatchError, deliver_bursts};
use super::{InterfaceInventory, NetworkChange};

/// Debounced Windows network-change observation over IP Helper.
///
/// Interface, unicast-address, and route notifications for both address
/// families are registered first; from then on every change reaches the
/// watcher, so reading the interface baseline afterwards leaves no gap.
/// Every later burst is coalesced, the inventory is diffed, and one
/// [`NetworkChange`] naming the lost interfaces is sent. An interface or
/// route that was added or deleted always counts; a parameter update to one
/// that already exists counts only when it changed the inventory.
pub struct WindowsNetworkChangeWatcher;

impl WindowsNetworkChangeWatcher {
    /// Observe until `changes` closes (`Ok`) or the observation ends.
    ///
    /// `inventory` carries the last known interfaces across attempts: when
    /// it is `Some`, the previous attempt ended and events may have been
    /// missed, so one change is sent as soon as the new baseline exists.
    /// On return it holds the latest baseline this attempt established.
    ///
    /// Dropping the future cancels every registration, waiting for any
    /// notification callback already running. `gate` is ready only while
    /// this attempt observes from a reconciled baseline; an interface or
    /// route notification revokes it inside the callback that reports it.
    pub async fn observe(
        changes: &mpsc::Sender<NetworkChange>,
        inventory: &mut Option<InterfaceInventory>,
        gate: &ObservationGate,
    ) -> Result<(), NetworkChangeWatchError> {
        Handle::try_current().map_err(|_| NetworkChangeWatchError::RuntimeUnavailable)?;
        let kinds = Arc::new(EventKinds::revoking(gate.clone()));
        let (signal, mut events) = mpsc::channel(1);
        let _registrations = ip_helper::Registrations::register(Arc::clone(&kinds), signal)
            .map_err(|_| NetworkChangeWatchError::MonitorUnavailable)?;
        let current = InterfaceInventory::current()
            .map_err(|_| NetworkChangeWatchError::InventoryUnavailable)?;
        // The registrations own the only sender, so `events` stays open and
        // this ends only when `changes` closes.
        deliver_bursts(
            changes,
            inventory,
            current,
            &kinds,
            &mut events,
            InterfaceInventory::current,
            gate,
        )
        .await
    }
}

/// Balun's only unsafe code: three IP Helper registrations and the
/// callbacks they invoke. Everything the callbacks touch lives in one
/// reference-counted context that outlives every registration.
#[allow(unsafe_code)]
mod ip_helper {
    use std::ffi::c_void;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::ptr;
    use std::sync::Arc;

    use tokio::sync::mpsc;
    use windows_sys::Win32::Foundation::{HANDLE, NO_ERROR, WIN32_ERROR};
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        CancelMibChangeNotify2, MIB_IPFORWARD_ROW2, MIB_IPINTERFACE_ROW, MIB_NOTIFICATION_TYPE,
        MIB_UNICASTIPADDRESS_ROW, MibParameterNotification, NotifyIpInterfaceChange,
        NotifyRouteChange2, NotifyUnicastIpAddressChange,
    };
    use windows_sys::Win32::Networking::WinSock::AF_UNSPEC;

    use super::super::watch::{ChangeKind, EventKinds};

    /// What every callback shares.
    struct CallbackContext {
        kinds: Arc<EventKinds>,
        signal: mpsc::Sender<()>,
    }

    impl CallbackContext {
        /// Record the kind, then wake the watcher. Neither step blocks or
        /// panics: a full channel already holds a wake-up and a closed one
        /// means the watcher is gone.
        fn notify(&self, kind: ChangeKind) {
            self.kinds.record(kind);
            let _ = self.signal.try_send(());
        }
    }

    /// The live registrations. Dropping this cancels each one before the
    /// context they share can be freed.
    pub(super) struct Registrations {
        context: Arc<CallbackContext>,
        handles: Vec<HANDLE>,
    }

    // SAFETY: the handles are opaque tokens that `CancelMibChangeNotify2`
    // accepts from any thread, and the context is itself `Send` and `Sync`.
    // This keeps the observation future `Send`, as on the other platforms.
    unsafe impl Send for Registrations {}

    impl Registrations {
        /// Register for interface, unicast-address, and route changes in
        /// both address families, or cancel whatever was registered and
        /// return the first failure.
        pub(super) fn register(
            kinds: Arc<EventKinds>,
            signal: mpsc::Sender<()>,
        ) -> Result<Self, WIN32_ERROR> {
            let mut registrations = Self {
                context: Arc::new(CallbackContext { kinds, signal }),
                handles: Vec::with_capacity(3),
            };
            let context = Arc::as_ptr(&registrations.context).cast::<c_void>();
            let mut handle: HANDLE = ptr::null_mut();
            // SAFETY: each callback matches the signature the function
            // expects, and `context` points into the Arc this value owns,
            // which `Drop` keeps alive until every handle is cancelled. The
            // out-pointer is a valid local, read only after success.
            let status = unsafe {
                NotifyIpInterfaceChange(
                    AF_UNSPEC,
                    Some(interface_changed),
                    context,
                    false,
                    &raw mut handle,
                )
            };
            registrations.admit(status, handle)?;
            // SAFETY: as above.
            let status = unsafe {
                NotifyUnicastIpAddressChange(
                    AF_UNSPEC,
                    Some(address_changed),
                    context,
                    false,
                    &raw mut handle,
                )
            };
            registrations.admit(status, handle)?;
            // SAFETY: as above.
            let status = unsafe {
                NotifyRouteChange2(
                    AF_UNSPEC,
                    Some(route_changed),
                    context,
                    false,
                    &raw mut handle,
                )
            };
            registrations.admit(status, handle)?;
            Ok(registrations)
        }

        fn admit(&mut self, status: WIN32_ERROR, handle: HANDLE) -> Result<(), WIN32_ERROR> {
            if status != NO_ERROR {
                return Err(status);
            }
            self.handles.push(handle);
            Ok(())
        }
    }

    impl Drop for Registrations {
        fn drop(&mut self) {
            let mut cancelled = true;
            for handle in self.handles.drain(..) {
                // SAFETY: each handle came from one successful registration
                // and is cancelled once. This never runs inside a callback,
                // and the call returns only after any callback in progress
                // has finished.
                cancelled &= unsafe { CancelMibChangeNotify2(handle) } == NO_ERROR;
            }
            if !cancelled {
                // A registration that could not be cancelled may still call
                // back, so its context must never be freed.
                std::mem::forget(Arc::clone(&self.context));
            }
        }
    }

    /// Callbacks run on system thread-pool threads.
    unsafe extern "system" fn interface_changed(
        context: *const c_void,
        _row: *const MIB_IPINTERFACE_ROW,
        notification: MIB_NOTIFICATION_TYPE,
    ) {
        notify(context, added_or_deleted(notification, ChangeKind::Link));
    }

    unsafe extern "system" fn address_changed(
        context: *const c_void,
        _row: *const MIB_UNICASTIPADDRESS_ROW,
        _notification: MIB_NOTIFICATION_TYPE,
    ) {
        notify(context, ChangeKind::Address);
    }

    unsafe extern "system" fn route_changed(
        context: *const c_void,
        _row: *const MIB_IPFORWARD_ROW2,
        notification: MIB_NOTIFICATION_TYPE,
    ) {
        notify(context, added_or_deleted(notification, ChangeKind::Route));
    }

    /// Router advertisements refresh route lifetimes and interface
    /// parameters as routinely as address lifetimes. Such an update is a
    /// refresh; an addition or deletion keeps its kind.
    fn added_or_deleted(notification: MIB_NOTIFICATION_TYPE, kind: ChangeKind) -> ChangeKind {
        if notification == MibParameterNotification {
            ChangeKind::Refresh
        } else {
            kind
        }
    }

    fn notify(context: *const c_void, kind: ChangeKind) {
        // A panic must never unwind into the operating system's thread.
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: a non-null `context` is the address `register` passed,
            // of a context that `Registrations::drop` keeps alive until no
            // callback can run.
            if let Some(context) = unsafe { context.cast::<CallbackContext>().as_ref() } {
                context.notify(kind);
            }
        }));
    }

    #[cfg(test)]
    mod tests {
        use windows_sys::Win32::NetworkManagement::IpHelper::{MibAddInstance, MibDeleteInstance};

        use super::*;

        #[test]
        fn every_callback_records_its_kind_then_wakes_the_watcher() {
            let kinds = Arc::new(EventKinds::default());
            let (signal, mut events) = mpsc::channel(1);
            let context = CallbackContext {
                kinds: Arc::clone(&kinds),
                signal,
            };
            let pointer = ptr::from_ref(&context).cast::<c_void>();

            // SAFETY: `pointer` refers to a live context and the rows are
            // never read.
            unsafe { address_changed(pointer, ptr::null(), MibAddInstance) };
            assert!(events.try_recv().is_ok());
            assert!(!kinds.take_beyond_addresses());

            // Parameter updates are refreshes. SAFETY: as above.
            unsafe {
                interface_changed(pointer, ptr::null(), MibParameterNotification);
                route_changed(pointer, ptr::null(), MibParameterNotification);
            }
            assert!(events.try_recv().is_ok());
            assert!(!kinds.take_beyond_addresses());

            for (interface, route) in [
                (MibAddInstance, MibDeleteInstance),
                (MibDeleteInstance, MibAddInstance),
            ] {
                // SAFETY: as above.
                unsafe {
                    interface_changed(pointer, ptr::null(), interface);
                    route_changed(pointer, ptr::null(), route);
                }
                // Two notifications, one pending wake-up.
                assert!(events.try_recv().is_ok());
                assert!(events.try_recv().is_err());
                assert!(kinds.take_beyond_addresses());
            }
            // SAFETY: as above.
            unsafe { route_changed(pointer, ptr::null(), MibAddInstance) };
            assert!(kinds.take_beyond_addresses());

            // A missing context or a departed watcher is ignored.
            drop(events);
            // SAFETY: a null context is never dereferenced.
            unsafe {
                route_changed(ptr::null(), ptr::null(), MibAddInstance);
                route_changed(pointer, ptr::null(), MibAddInstance);
            }
        }

        #[test]
        fn registrations_cancel_and_release_their_context() {
            let kinds = Arc::new(EventKinds::default());
            let (signal, _events) = mpsc::channel(1);
            let Ok(registrations) = Registrations::register(kinds, signal) else {
                // A sandbox without IP Helper fails closed.
                return;
            };
            assert_eq!(registrations.handles.len(), 3);
            let context = Arc::downgrade(&registrations.context);
            drop(registrations);
            assert!(context.upgrade().is_none(), "the context was freed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn observation_establishes_a_baseline_then_stops_with_its_receiver() {
        let (changes, receiver) = mpsc::channel(1);
        drop(receiver);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            WindowsNetworkChangeWatcher::observe(&changes, &mut inventory, &gate),
        )
        .await
        .expect("observation must notice its closed receiver promptly");
        match outcome {
            Ok(()) => assert!(
                inventory.is_some(),
                "a clean observation leaves its baseline behind"
            ),
            // A sandbox without IP Helper fails closed.
            Err(NetworkChangeWatchError::MonitorUnavailable) => {}
            Err(error) => panic!("unexpected observation failure {error:?}"),
        }
    }

    #[test]
    fn observation_fails_closed_outside_a_runtime_and_is_send() {
        fn assert_send<T: Send>(value: T) -> T {
            value
        }
        let (changes, _receiver) = mpsc::channel(1);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let observation = assert_send(WindowsNetworkChangeWatcher::observe(
            &changes,
            &mut inventory,
            &gate,
        ));
        // Outside any runtime, the first poll returns before registering.
        let mut context = Context::from_waker(Waker::noop());
        let outcome = pin!(observation).poll(&mut context);
        assert_eq!(
            outcome,
            Poll::Ready(Err(NetworkChangeWatchError::RuntimeUnavailable))
        );
        assert!(inventory.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_open_observation_is_ready_and_stops_promptly_when_cancelled() {
        let (changes, _receiver) = mpsc::channel(1);
        let mut inventory = None;
        let gate = ObservationGate::new();
        let mut watch = gate.watch();
        let started = std::time::Instant::now();
        {
            let mut observation = pin!(WindowsNetworkChangeWatcher::observe(
                &changes,
                &mut inventory,
                &gate,
            ));
            let ready = tokio::select! {
                outcome = &mut observation => match outcome {
                    // A sandbox without IP Helper fails closed.
                    Err(NetworkChangeWatchError::MonitorUnavailable) => return,
                    other => panic!("unexpected end of observation {other:?}"),
                },
                state = watch.changed() => state,
            };
            assert!(ready.generation().is_some(), "a baseline makes it ready");
            // The observation runs until cancelled; dropping it cancels every
            // registration.
            let outcome = tokio::time::timeout(Duration::from_millis(500), observation).await;
            assert!(
                outcome.is_err(),
                "unexpected end of observation {outcome:?}"
            );
        }
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(inventory.is_some(), "the baseline exists while observing");
        assert_eq!(
            watch.current().generation(),
            None,
            "a cancelled observation is no longer ready"
        );
    }
}
