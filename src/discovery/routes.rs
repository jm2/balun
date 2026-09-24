//! Host of the Linux rtnetlink monitor used by the network-change watcher.
//!
//! Route-table-derived tunnel discovery was retired (ADR-0003). Nothing here
//! reads a route table or derives discovery candidates any more; the monitor
//! stays at this path until it moves beside the watcher.

#[cfg(target_os = "linux")]
mod linux;

/// Part of the monitor's surface for the watcher, though no caller names the
/// type today.
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub(in crate::discovery) use linux::RouteReconciliationRequired;
#[cfg(target_os = "linux")]
pub(in crate::discovery) use linux::{
    LinuxRouteEventMonitor, LinuxRouteMonitorError, NotificationKind, RouteMonitorObserver,
};
