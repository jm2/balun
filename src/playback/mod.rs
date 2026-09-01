//! Optional, GTK-free playback foundations.
//!
//! Enabling `playback` links GStreamer and exposes the main-thread runtime
//! owner plus its exact factory-capability snapshot. The module deliberately
//! does not accept stream URLs or create a media pipeline yet.

mod runtime;

pub use runtime::{
    FactoryCapability, GSTREAMER_API_FLOOR, PlaybackCapabilities, PlaybackFactory,
    PlaybackInitializationError, PlaybackRuntime, RuntimeVersion,
};
