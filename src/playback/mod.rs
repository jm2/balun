//! Optional, GTK-free playback foundations.
//!
//! Enabling `playback` links GStreamer and exposes the main-thread runtime
//! owner plus its exact factory-capability snapshot. The `desktop` feature
//! additionally exposes the generation-owned GTK paintable tune session.

mod runtime;
#[cfg(feature = "desktop")]
mod session;
#[cfg(feature = "desktop")]
mod source_policy;

pub use runtime::{
    FactoryCapability, GSTREAMER_API_FLOOR, PlaybackCapabilities, PlaybackFactory,
    PlaybackInitializationError, PlaybackRuntime, RuntimeVersion,
};
#[cfg(feature = "desktop")]
pub use session::{
    PlaybackAudioState, PlaybackSession, PlaybackSessionFailure, PlaybackSessionState,
    TuneCompletion, TuneGeneration, TuneRequest,
};
