//! Linux rtnetlink event monitoring for the network-change watcher.

mod monitor;

pub(in crate::discovery) use monitor::{
    LinuxRouteEventMonitor, LinuxRouteMonitorError, NotificationKind, RouteMonitorObserver,
    RouteReconciliationRequired,
};
