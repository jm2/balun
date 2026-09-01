//! GTK-free application state and controller-facing projections.

mod state;

pub use state::{
    ApplicationSnapshot, ChannelSummary, DeviceSummary, DiscoveryFailure, DiscoveryState,
    DiscoveryStatus, LineupFailure, MAX_CHANNEL_NAME_BYTES, MAX_DEVICE_LOCATORS,
    MAX_DEVICE_SUMMARIES, MAX_DEVICE_TEXT_BYTES, MAX_SELECTED_CHANNELS, OperationGeneration,
    SelectedLineupState, SelectedLineupStatus, SnapshotRevision, StateError,
};
