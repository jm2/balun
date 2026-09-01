//! GTK-free application state, controller ownership, and UI projections.

mod runtime;
mod state;

pub use runtime::{
    CONTROLLER_THREAD_NAME, ControllerCommand, ControllerCommandError, ControllerHandle,
    ControllerJoinError, ControllerRuntime, ControllerRuntimeError, ControllerStartError,
    DEFAULT_COMMAND_CAPACITY, LocalDiscoveryFuture, LocalDiscoveryService, MAX_COMMAND_CAPACITY,
};
pub use state::{
    ApplicationSnapshot, ChannelSummary, DeviceSummary, DiscoveryFailure, DiscoveryState,
    DiscoveryStatus, LineupFailure, MAX_CHANNEL_NAME_BYTES, MAX_DEVICE_LOCATORS,
    MAX_DEVICE_SUMMARIES, MAX_DEVICE_TEXT_BYTES, MAX_SELECTED_CHANNELS, OperationGeneration,
    SelectedLineupState, SelectedLineupStatus, SnapshotRevision, StateError,
};
