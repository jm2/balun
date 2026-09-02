//! Default-main-context GStreamer initialization and capability inspection.

use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use gstreamer as gst;
use thiserror::Error;

/// The GStreamer API/runtime floor selected for the first Balun player.
pub const GSTREAMER_API_FLOOR: RuntimeVersion = RuntimeVersion::new(1, 20, 0);

/// One exact GStreamer element factory needed by the first live-TV path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaybackFactory {
    /// Modern URI playback pipeline owner.
    Playbin3,
    /// URI decode owner used internally by `playbin3`.
    UriDecodeBin3,
    /// Stream decode owner used internally by `uridecodebin3`.
    DecodeBin3,
    /// Application-fed source filled by Balun's private HTTP transport.
    AppSource,
    /// MPEG transport-stream demultiplexer.
    MpegTsDemuxer,
    /// Interlaced-video deinterlacer required for ordinary 1080i channels.
    Deinterlace,
    /// GTK 4 paintable video sink.
    Gtk4PaintableSink,
}

impl PlaybackFactory {
    /// Every factory in the deterministic v0 playback-foundation contract.
    pub const ALL: [Self; 7] = [
        Self::Playbin3,
        Self::UriDecodeBin3,
        Self::DecodeBin3,
        Self::AppSource,
        Self::MpegTsDemuxer,
        Self::Deinterlace,
        Self::Gtk4PaintableSink,
    ];

    /// Return the exact GStreamer registry name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Playbin3 => "playbin3",
            Self::UriDecodeBin3 => "uridecodebin3",
            Self::DecodeBin3 => "decodebin3",
            Self::AppSource => "appsrc",
            Self::MpegTsDemuxer => "tsdemux",
            Self::Deinterlace => "deinterlace",
            Self::Gtk4PaintableSink => "gtk4paintablesink",
        }
    }
}

impl fmt::Display for PlaybackFactory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}

/// Availability of one required factory at the instant GStreamer initialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FactoryCapability {
    factory: PlaybackFactory,
    available: bool,
}

impl FactoryCapability {
    /// Return the factory represented by this record.
    pub const fn factory(self) -> PlaybackFactory {
        self.factory
    }

    /// Return whether the factory was present in the active registry.
    pub const fn is_available(self) -> bool {
        self.available
    }
}

/// Sanitized GStreamer runtime and factory capability snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackCapabilities {
    runtime_version: RuntimeVersion,
    factories: [FactoryCapability; PlaybackFactory::ALL.len()],
}

impl PlaybackCapabilities {
    /// Return the GStreamer runtime version observed after initialization.
    pub const fn runtime_version(&self) -> RuntimeVersion {
        self.runtime_version
    }

    /// Return every required factory in stable contract order.
    pub const fn factories(&self) -> &[FactoryCapability] {
        &self.factories
    }

    /// Iterate over required factories absent from the active registry.
    pub fn missing_required(&self) -> impl Iterator<Item = PlaybackFactory> + '_ {
        self.factories
            .iter()
            .filter(|capability| !capability.available)
            .map(|capability| capability.factory)
    }

    /// Return whether the foundation needed to attempt the playback spike is present.
    pub fn is_foundation_ready(&self) -> bool {
        self.missing_required().next().is_none()
    }
}

/// Three-component GStreamer runtime version, excluding the nano marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuntimeVersion {
    major: u32,
    minor: u32,
    micro: u32,
}

impl RuntimeVersion {
    /// Construct a runtime version.
    pub const fn new(major: u32, minor: u32, micro: u32) -> Self {
        Self {
            major,
            minor,
            micro,
        }
    }

    /// Return the major component.
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Return the minor component.
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Return the micro component.
    pub const fn micro(self) -> u32 {
        self.micro
    }
}

impl fmt::Display for RuntimeVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.micro)
    }
}

/// GStreamer initialization failed before Balun could inspect capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PlaybackInitializationError {
    /// The current thread does not own the default GLib main context.
    #[error("Balun playback initialization requires ownership of the default GLib main context")]
    MainContextUnavailable,
    /// The linked GStreamer library rejected initialization.
    #[error("GStreamer could not be initialized")]
    InitializationFailed,
    /// The loaded runtime is older than Balun's compile-time API floor.
    #[error("GStreamer {found} is too old; Balun requires GStreamer {minimum} or newer")]
    RuntimeTooOld {
        /// Sanitized loaded runtime version.
        found: RuntimeVersion,
        /// Balun's selected minimum.
        minimum: RuntimeVersion,
    },
}

/// Default-main-context owner for process-global GStreamer initialization.
///
/// The `Rc` marker deliberately keeps this owner `!Send` and `!Sync`. Balun
/// does not call GStreamer's process-global deinitializer when the owner drops;
/// future pipelines must still reach `NULL` and release their own resources
/// explicitly before application shutdown.
#[derive(Debug)]
pub struct PlaybackRuntime {
    capabilities: PlaybackCapabilities,
    _main_context_only: PhantomData<Rc<()>>,
}

impl PlaybackRuntime {
    /// Verify ownership of the default GLib main context, initialize
    /// GStreamer, and take one exact, path-free capability snapshot.
    ///
    /// Native GStreamer error text and plugin filenames are intentionally not
    /// retained because later pipeline errors can contain complete stream URLs.
    pub fn initialize() -> Result<Self, PlaybackInitializationError> {
        let main_context = gst::glib::MainContext::default();
        require_main_context_owner(main_context.is_owner())?;
        gst::init().map_err(|_| PlaybackInitializationError::InitializationFailed)?;
        let (major, minor, micro, _) = gst::version();
        let runtime_version = RuntimeVersion::new(major, minor, micro);
        if runtime_version < GSTREAMER_API_FLOOR {
            return Err(PlaybackInitializationError::RuntimeTooOld {
                found: runtime_version,
                minimum: GSTREAMER_API_FLOOR,
            });
        }

        Ok(Self {
            capabilities: probe_capabilities(runtime_version, |factory| {
                gst::ElementFactory::find(factory.name()).is_some()
            }),
            _main_context_only: PhantomData,
        })
    }

    /// Return the immutable startup capability snapshot.
    pub const fn capabilities(&self) -> &PlaybackCapabilities {
        &self.capabilities
    }
}

fn require_main_context_owner(is_owner: bool) -> Result<(), PlaybackInitializationError> {
    if is_owner {
        Ok(())
    } else {
        Err(PlaybackInitializationError::MainContextUnavailable)
    }
}

fn probe_capabilities(
    runtime_version: RuntimeVersion,
    mut available: impl FnMut(PlaybackFactory) -> bool,
) -> PlaybackCapabilities {
    PlaybackCapabilities {
        runtime_version,
        factories: PlaybackFactory::ALL.map(|factory| FactoryCapability {
            factory,
            available: available(factory),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn required_factory_contract_is_exact_unique_and_stable() {
        assert_eq!(
            PlaybackFactory::ALL.map(PlaybackFactory::name),
            [
                "playbin3",
                "uridecodebin3",
                "decodebin3",
                "appsrc",
                "tsdemux",
                "deinterlace",
                "gtk4paintablesink",
            ]
        );
        assert_eq!(
            PlaybackFactory::ALL
                .into_iter()
                .collect::<BTreeSet<_>>()
                .len(),
            PlaybackFactory::ALL.len()
        );
    }

    #[test]
    fn capability_probe_calls_each_factory_once_and_reports_ready() {
        let mut calls = Vec::new();
        let capabilities = probe_capabilities(RuntimeVersion::new(1, 28, 6), |factory| {
            calls.push(factory);
            true
        });

        assert_eq!(calls, PlaybackFactory::ALL);
        assert_eq!(
            capabilities.runtime_version(),
            RuntimeVersion::new(1, 28, 6)
        );
        assert!(capabilities.is_foundation_ready());
        assert_eq!(capabilities.missing_required().count(), 0);
    }

    #[test]
    fn capability_probe_reports_only_missing_factories_in_contract_order() {
        let missing = [
            PlaybackFactory::AppSource,
            PlaybackFactory::Gtk4PaintableSink,
        ];
        let capabilities =
            probe_capabilities(GSTREAMER_API_FLOOR, |factory| !missing.contains(&factory));

        assert!(!capabilities.is_foundation_ready());
        assert_eq!(capabilities.missing_required().collect::<Vec<_>>(), missing);
        assert_eq!(capabilities.factories().len(), PlaybackFactory::ALL.len());
    }

    #[test]
    fn versions_compare_and_render_without_the_gstreamer_nano_marker() {
        assert!(RuntimeVersion::new(1, 20, 1) > GSTREAMER_API_FLOOR);
        assert!(RuntimeVersion::new(1, 18, 6) < GSTREAMER_API_FLOOR);
        assert_eq!(RuntimeVersion::new(1, 20, 0).to_string(), "1.20.0");
    }

    #[test]
    fn initialization_errors_are_fixed_and_path_free() {
        assert_eq!(
            PlaybackInitializationError::MainContextUnavailable.to_string(),
            "Balun playback initialization requires ownership of the default GLib main context"
        );
        assert_eq!(
            PlaybackInitializationError::InitializationFailed.to_string(),
            "GStreamer could not be initialized"
        );
        assert_eq!(
            PlaybackInitializationError::RuntimeTooOld {
                found: RuntimeVersion::new(1, 18, 6),
                minimum: GSTREAMER_API_FLOOR,
            }
            .to_string(),
            "GStreamer 1.18.6 is too old; Balun requires GStreamer 1.20.0 or newer"
        );
    }

    #[test]
    fn main_context_ownership_fails_before_runtime_initialization() {
        assert_eq!(
            require_main_context_owner(false),
            Err(PlaybackInitializationError::MainContextUnavailable)
        );
        assert_eq!(require_main_context_owner(true), Ok(()));
    }

    #[test]
    #[ignore = "requires the complete development playback runtime"]
    fn installed_runtime_has_the_exact_playback_foundation() {
        let main_context = gst::glib::MainContext::default();
        let _main_context_guard = main_context
            .acquire()
            .expect("acquire default main context for installed-runtime test");
        let runtime = PlaybackRuntime::initialize().expect("initialize installed GStreamer");
        let missing = runtime
            .capabilities()
            .missing_required()
            .map(PlaybackFactory::name)
            .collect::<Vec<_>>();

        assert!(
            missing.is_empty(),
            "installed GStreamer runtime is missing required factories: {}",
            missing.join(", ")
        );
    }
}
