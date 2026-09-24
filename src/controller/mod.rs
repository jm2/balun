//! GTK-free application state, controller ownership, and UI projections.

mod handoff;
mod network;
mod remembered;
mod resolution;
mod runtime;
mod state;

pub use handoff::{StreamHandoff, StreamHandoffError, StreamHandoffReceiver, StreamSelection};
#[cfg(target_os = "linux")]
pub use network::LinuxNetworkChangeSource;
pub use network::{NetworkChangeSource, UnavailableNetworkChangeSource};
pub use remembered::{ExactSearchOutcome, ExactTargetTracker, RediscoveryQueue, RediscoveryStep};
pub use resolution::HostnameResolutionReceiver;
pub use runtime::{
    CONTROLLER_THREAD_NAME, ControllerCommand, ControllerCommandError, ControllerHandle,
    ControllerJoinError, ControllerRuntime, ControllerRuntimeError, ControllerStartError,
    DEFAULT_COMMAND_CAPACITY, DiscoveryFuture, DiscoveryService, MAX_COMMAND_CAPACITY,
    MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION,
};
pub use state::{
    ApplicationSnapshot, ChannelSummary, DeviceSummary, DiscoveryFailure, DiscoveryKind,
    DiscoveryState, DiscoveryStatus, ExactSearchTicket, LineupFailure, MAX_CHANNEL_NAME_BYTES,
    MAX_DEVICE_LOCATORS, MAX_DEVICE_SUMMARIES, MAX_DEVICE_TEXT_BYTES, MAX_SELECTED_CHANNELS,
    NetworkChangeSummary, OperationGeneration, SelectedLineupState, SelectedLineupStatus,
    SnapshotRevision, StateError,
};
