//! Linux network-change observation over rtnetlink.

use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::{mpsc, oneshot};
use tokio_util::task::AbortOnDropHandle;

use super::watch::{ChangeKind, EventKinds, NetworkChangeWatchError, deliver_bursts};
use super::{InterfaceInventory, NetworkChange};
use crate::discovery::routes::{
    LinuxRouteEventMonitor, LinuxRouteMonitorError, NotificationKind, RouteMonitorObserver,
};

/// The monitor records a kind before queuing its reconciliation, so every
/// notification of a delivered burst is already counted.
impl RouteMonitorObserver for EventKinds {
    fn invalidate(&self) {}

    fn poison(&self) {}

    fn observed(&self, kind: NotificationKind) {
        self.record(match kind {
            NotificationKind::Link => ChangeKind::Link,
            kind if kind.is_address() => ChangeKind::Address,
            _ => ChangeKind::Route,
        });
    }
}

/// Debounced Linux network-change observation over rtnetlink.
///
/// The rtnetlink subscription is taken first and the interface baseline is
/// read inside the monitor's synchronous activation callback, after its
/// final drain to `EAGAIN`, so no change can fall between the two. Every
/// later burst is coalesced, the inventory is diffed, and one
/// [`NetworkChange`] naming the lost interfaces is sent.
pub struct LinuxNetworkChangeWatcher;

impl LinuxNetworkChangeWatcher {
    /// Observe until `changes` closes (`Ok`) or the observation ends.
    ///
    /// `inventory` carries the last known interfaces across attempts: when
    /// it is `Some`, the previous attempt ended and events may have been
    /// missed, so one change is sent as soon as the new baseline exists.
    /// On return it holds the latest baseline this attempt established.
    pub async fn observe(
        changes: &mpsc::Sender<NetworkChange>,
        inventory: &mut Option<InterfaceInventory>,
    ) -> Result<(), NetworkChangeWatchError> {
        let runtime =
            Handle::try_current().map_err(|_| NetworkChangeWatchError::RuntimeUnavailable)?;
        let kinds = Arc::new(EventKinds::default());
        let observer: Arc<dyn RouteMonitorObserver> = kinds.clone();
        let (monitor, mut reconciliation) = LinuxRouteEventMonitor::subscribe(observer)
            .map_err(|_| NetworkChangeWatchError::MonitorUnavailable)?;
        let (baseline, baseline_receiver) = oneshot::channel();
        let monitor_task =
            AbortOnDropHandle::new(runtime.spawn(monitor.run_continuously(move || {
                let inventory = InterfaceInventory::current().map_err(|_| ())?;
                baseline.send(inventory).map_err(|_| ())
            })));

        let current = match baseline_receiver.await {
            Ok(inventory) => inventory,
            Err(_) => {
                return Err(match monitor_task.await {
                    Ok(Err(error)) if is_inventory_failure(error) => {
                        NetworkChangeWatchError::InventoryUnavailable
                    }
                    _ => NetworkChangeWatchError::BaselineChanged,
                });
            }
        };
        let outcome = deliver_bursts(
            changes,
            inventory,
            current,
            &kinds,
            &mut reconciliation,
            InterfaceInventory::current,
        )
        .await;
        if outcome.is_err() {
            let _ = monitor_task.await;
        }
        outcome
    }
}

/// The activation callback runs only after a clean barrier and fails only
/// when the inventory cannot be read (its receiver is awaited above), so a
/// rejected activation is an inventory failure; every other early end is
/// a change during the baseline.
const fn is_inventory_failure(error: LinuxRouteMonitorError) -> bool {
    matches!(error, LinuxRouteMonitorError::ActivationRejected)
}

#[cfg(test)]
mod tests {
    use tokio::runtime::Builder;

    use super::*;

    #[test]
    fn observation_without_io_fails_closed_before_any_baseline() {
        let runtime = Builder::new_current_thread().build().unwrap();
        let (changes, _receiver) = mpsc::channel(1);
        let mut inventory = None;
        let outcome =
            runtime.block_on(LinuxNetworkChangeWatcher::observe(&changes, &mut inventory));
        assert_eq!(outcome, Err(NetworkChangeWatchError::MonitorUnavailable));
        assert!(inventory.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn observation_establishes_a_baseline_then_stops_with_its_receiver() {
        let (changes, receiver) = mpsc::channel(1);
        drop(receiver);
        let mut inventory = None;
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            LinuxNetworkChangeWatcher::observe(&changes, &mut inventory),
        )
        .await
        .expect("observation must notice its closed receiver promptly");
        match outcome {
            Ok(()) => assert!(
                inventory.is_some(),
                "a clean observation leaves its baseline behind"
            ),
            // A busy host or a sandbox without rtnetlink fails closed.
            Err(
                NetworkChangeWatchError::MonitorUnavailable
                | NetworkChangeWatchError::BaselineChanged,
            ) => {}
            Err(error) => panic!("unexpected observation failure {error:?}"),
        }
    }

    #[test]
    fn only_link_route_and_rule_notifications_go_beyond_addresses() {
        let kinds = EventKinds::default();
        kinds.observed(NotificationKind::Ipv6Address);
        kinds.observed(NotificationKind::Ipv4Address);
        assert!(!kinds.take_beyond_addresses());

        for kind in [
            NotificationKind::Link,
            NotificationKind::Ipv4Route,
            NotificationKind::Ipv4Rule,
        ] {
            kinds.observed(NotificationKind::Ipv6Address);
            kinds.observed(kind);
            assert!(kinds.take_beyond_addresses(), "{kind:?}");
            // Taking clears the record for the next burst.
            assert!(!kinds.take_beyond_addresses());
        }
    }
}
