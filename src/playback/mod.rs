//! Optional, GTK-free playback foundations.
//!
//! Enabling `playback` links GStreamer and exposes the main-thread runtime
//! owner plus its exact factory-capability snapshot. The `desktop` feature
//! additionally exposes the generation-owned GTK paintable tune session, the
//! fixed endpoint-free pipeline URI, the closed failure categories, and the
//! private `appsrc` source policy plus Balun-owned HTTP transport that the
//! session consumes.

#[cfg(all(test, feature = "desktop"))]
mod fake_device_e2e;
#[cfg(all(test, feature = "desktop"))]
mod live_hardware;
#[cfg(all(feature = "desktop", any(target_os = "windows", target_os = "macos")))]
mod packaged_probe;
#[cfg(feature = "desktop")]
mod pipeline_failure;
#[cfg(feature = "desktop")]
mod platform_runtime;
mod runtime;
#[cfg(feature = "desktop")]
mod session;
#[cfg(feature = "desktop")]
mod source_policy;
#[cfg(all(test, feature = "desktop"))]
mod test_support;
#[cfg(feature = "desktop")]
mod transport;

#[cfg(feature = "desktop")]
pub use pipeline_failure::{MissingMedia, PlaybackPipelineFailure};
#[cfg(feature = "desktop")]
pub use platform_runtime::{
    MACOS_PROBE_SENTINEL, MACOS_PROBE_SENTINEL_NAME, PLATFORM_RUNTIME_PROBE_FLAG,
    PlatformRuntimeError, REGISTRY_ENVIRONMENT_KEY, ToolkitPreparation, WINDOWS_PROBE_SENTINEL,
    WINDOWS_PROBE_SENTINEL_NAME, configure_before_toolkit, emit_macos_install_key_if_requested,
};
pub use runtime::{
    FactoryCapability, GSTREAMER_API_FLOOR, PlaybackCapabilities, PlaybackFactory,
    PlaybackInitializationError, PlaybackRuntime, RuntimeVersion,
};
#[cfg(feature = "desktop")]
pub use session::{
    PlaybackAudioState, PlaybackSession, PlaybackSessionFailure, PlaybackSessionState,
    TuneCompletion, TuneGeneration, TuneRequest,
};
#[cfg(feature = "desktop")]
pub use transport::PIPELINE_URI;
