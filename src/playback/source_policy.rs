//! Private fail-closed policy for the source element created by `playbin3`.
//!
//! Production playback assigns only the constant
//! [`PIPELINE_URI`](super::transport::PIPELINE_URI) to `playbin3`, so the
//! element it creates must be the exact built-in `appsrc`.
//! This policy validates that element, configures it as a bounded live MPEG-TS
//! byte feed, and hands the one authorized stream handoff to the Balun-owned
//! transport. Any other, repeated, or unconfigurable source is locked to
//! `NULL` and reported through one field-free application marker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use gst::glib;
use gst::prelude::*;
use gstreamer as gst;

use super::PlaybackFactory;
use super::transport::{StreamTransport, TransportConfig};
use crate::controller::StreamHandoff;

const SOURCE_SETUP_SIGNAL: &str = "source-setup";
const REJECTION_MESSAGE: &str = "balun-source-policy-rejected";
const STREAM_CAPS_NAME: &str = "video/mpegts";
const STREAM_CAPS_SYSTEMSTREAM: &str = "systemstream";
const FORMAT_NICK: &str = "bytes";
const STREAM_TYPE_NICK: &str = "stream";
/// Bounded bytes `appsrc` may hold before the feeder blocks.
const MAX_QUEUED_BYTES: u64 = 4 * 1_024 * 1_024;

#[derive(Debug, Clone, Copy)]
pub(super) struct SourcePolicyError;

struct PendingStream {
    handoff: StreamHandoff,
    config: TransportConfig,
}

struct SourcePolicyState {
    expected_factory: gst::ElementFactory,
    pending: Mutex<Option<PendingStream>>,
    transport: Mutex<Option<StreamTransport>>,
    accepted_source: Mutex<Option<gst::Object>>,
    rejected: AtomicBool,
    retired: AtomicBool,
}

pub(super) struct SourcePolicy {
    state: Arc<SourcePolicyState>,
    playbin: glib::WeakRef<gst::Pipeline>,
    signal_handler: Option<glib::SignalHandlerId>,
}

impl SourcePolicy {
    /// Validate the `appsrc` contract without network work, retain the
    /// authorized handoff privately, and connect the `source-setup` handler.
    pub(super) fn install(
        playbin: &gst::Pipeline,
        handoff: StreamHandoff,
        config: TransportConfig,
    ) -> Result<Self, SourcePolicyError> {
        let expected_factory = gst::ElementFactory::find(PlaybackFactory::AppSource.name())
            .ok_or(SourcePolicyError)?;
        let preflight = expected_factory
            .create()
            .build()
            .map_err(|_| SourcePolicyError)?;
        if preflight.factory().as_ref() != Some(&expected_factory)
            || !configure_and_verify(&preflight)
        {
            return Err(SourcePolicyError);
        }

        let signal_id = validated_source_setup_signal(playbin)?;
        let state = Arc::new(SourcePolicyState {
            expected_factory,
            pending: Mutex::new(Some(PendingStream { handoff, config })),
            transport: Mutex::new(None),
            accepted_source: Mutex::new(None),
            rejected: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        });
        let playbin_weak = playbin.downgrade();
        let callback_playbin = playbin_weak.clone();
        let callback_state = Arc::clone(&state);
        let signal_handler = playbin.connect_id(signal_id, None, false, move |args| {
            let playbin = callback_playbin.upgrade();
            let source = args
                .get(1)
                .and_then(|value| value.get::<gst::Element>().ok());
            let valid_emitter = playbin.as_ref().is_some_and(|expected| {
                args.first()
                    .and_then(|value| value.get::<gst::Pipeline>().ok())
                    .is_some_and(|emitter| emitter == *expected)
            });

            if args.len() != 2 || !valid_emitter {
                callback_state.reject(playbin.as_ref(), source.as_ref());
                return None;
            }
            let Some(source) = source else {
                callback_state.reject(playbin.as_ref(), None);
                return None;
            };
            callback_state.inspect_source(
                playbin
                    .as_ref()
                    .expect("a valid emitter retains the weak playbin"),
                &source,
            );
            None
        });

        Ok(Self {
            state,
            playbin: playbin_weak,
            signal_handler: Some(signal_handler),
        })
    }

    pub(super) fn is_rejected(&self) -> bool {
        self.state.rejected.load(Ordering::Acquire)
    }

    /// Stop admitting sources, zeroize any unconsumed handoff, and cancel the
    /// transport. The returned transport must be joined after the pipeline
    /// reaches `NULL`; a later call returns `None`.
    pub(super) fn retire(&self) -> Option<StreamTransport> {
        self.state.retired.store(true, Ordering::Release);
        if let Ok(mut pending) = self.state.pending.lock() {
            pending.take();
        }
        let transport = self
            .state
            .transport
            .lock()
            .ok()
            .and_then(|mut transport| transport.take());
        if let Some(transport) = transport.as_ref() {
            transport.cancel();
        }
        transport
    }

    #[cfg(test)]
    fn accepted_factory_name(&self) -> Option<String> {
        self.state
            .accepted_source
            .lock()
            .ok()?
            .as_ref()?
            .downcast_ref::<gst::Element>()?
            .factory()
            .map(|factory| factory.name().to_string())
    }
}

impl Drop for SourcePolicy {
    fn drop(&mut self) {
        if let Some(transport) = self.retire() {
            drop(transport);
        }
        let Some(signal_handler) = self.signal_handler.take() else {
            return;
        };
        if let Some(playbin) = self.playbin.upgrade() {
            playbin.disconnect(signal_handler);
        }
    }
}

impl SourcePolicyState {
    fn inspect_source(&self, playbin: &gst::Pipeline, source: &gst::Element) {
        if self.rejected.load(Ordering::Acquire) || self.retired.load(Ordering::Acquire) {
            self.reject(Some(playbin), Some(source));
            return;
        }
        if source.factory().as_ref() != Some(&self.expected_factory)
            || !configure_and_verify(source)
        {
            self.reject(Some(playbin), Some(source));
            return;
        }

        let Ok(mut accepted) = self.accepted_source.lock() else {
            self.reject(Some(playbin), Some(source));
            return;
        };
        if accepted.is_some() || self.rejected.load(Ordering::Acquire) {
            drop(accepted);
            self.reject(Some(playbin), Some(source));
            return;
        }
        let pending = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.take());
        let Some(pending) = pending else {
            drop(accepted);
            self.reject(Some(playbin), Some(source));
            return;
        };
        match StreamTransport::start(pending.handoff, source.clone(), playbin, pending.config) {
            Ok(transport) => {
                *accepted = Some(source.clone().upcast::<gst::Object>());
                drop(accepted);
                if let Ok(mut slot) = self.transport.lock() {
                    *slot = Some(transport);
                }
            }
            Err(_) => {
                drop(accepted);
                self.reject(Some(playbin), Some(source));
            }
        }
    }

    fn reject(&self, playbin: Option<&gst::Pipeline>, source: Option<&gst::Element>) {
        let first_rejection = !self.rejected.swap(true, Ordering::AcqRel);
        if let Some(source) = source {
            source.set_locked_state(true);
            let _ = source.set_state(gst::State::Null);
        }

        if !first_rejection {
            return;
        }
        let Some(playbin) = playbin else {
            return;
        };
        let marker = gst::Structure::builder(REJECTION_MESSAGE).build();
        let message = gst::message::Application::builder(marker)
            .src(playbin)
            .build();
        if let Some(bus) = playbin.bus() {
            let _ = bus.post(message);
        }
    }
}

fn validated_source_setup_signal(
    playbin: &gst::Pipeline,
) -> Result<glib::subclass::SignalId, SourcePolicyError> {
    let signal_id = glib::subclass::SignalId::lookup(SOURCE_SETUP_SIGNAL, playbin.type_())
        .ok_or(SourcePolicyError)?;
    let query = signal_id.query();
    let parameters = query.param_types();
    if query.signal_name() != SOURCE_SETUP_SIGNAL
        || query.return_type() != glib::Type::UNIT
        || parameters.len() != 1
        || parameters[0] != gst::Element::static_type()
    {
        return Err(SourcePolicyError);
    }
    Ok(signal_id)
}

fn readable_writable_property<T: glib::types::StaticType>(
    source: &gst::Element,
    name: &str,
) -> bool {
    source.find_property(name).is_some_and(|property| {
        let flags = property.flags();
        property.value_type() == T::static_type()
            && flags.contains(glib::ParamFlags::READABLE | glib::ParamFlags::WRITABLE)
            && !flags.contains(glib::ParamFlags::CONSTRUCT_ONLY)
    })
}

fn readable_writable_enum(source: &gst::Element, name: &str, nick: &str) -> Option<glib::Value> {
    let property = source.find_property(name).filter(|property| {
        let flags = property.flags();
        flags.contains(glib::ParamFlags::READABLE | glib::ParamFlags::WRITABLE)
            && !flags.contains(glib::ParamFlags::CONSTRUCT_ONLY)
    })?;
    glib::EnumClass::with_type(property.value_type())?.to_value_by_nick(nick)
}

fn enum_nick_is(source: &gst::Element, name: &str, nick: &str) -> bool {
    glib::EnumValue::from_value(&source.property_value(name))
        .is_some_and(|(_, value)| value.nick() == nick)
}

/// Configure an `appsrc` as Balun's bounded live MPEG-TS byte feed and read
/// every setting back. This performs no network work and never sees a URL.
pub(super) fn configure_and_verify(source: &gst::Element) -> bool {
    if !readable_writable_property::<gst::Caps>(source, "caps")
        || !readable_writable_property::<bool>(source, "is-live")
        || !readable_writable_property::<bool>(source, "block")
        || !readable_writable_property::<bool>(source, "emit-signals")
        || !readable_writable_property::<bool>(source, "do-timestamp")
        || !readable_writable_property::<u64>(source, "max-bytes")
    {
        return false;
    }
    let (Some(format), Some(stream_type)) = (
        readable_writable_enum(source, "format", FORMAT_NICK),
        readable_writable_enum(source, "stream-type", STREAM_TYPE_NICK),
    ) else {
        return false;
    };

    let caps = gst::Caps::builder(STREAM_CAPS_NAME)
        .field(STREAM_CAPS_SYSTEMSTREAM, true)
        .build();
    source.set_property("caps", &caps);
    source.set_property_from_value("format", &format);
    source.set_property_from_value("stream-type", &stream_type);
    source.set_property("is-live", true);
    source.set_property("block", true);
    source.set_property("emit-signals", false);
    source.set_property("do-timestamp", false);
    source.set_property("max-bytes", MAX_QUEUED_BYTES);

    source
        .property::<Option<gst::Caps>>("caps")
        .is_some_and(|configured| configured.is_strictly_equal(&caps))
        && enum_nick_is(source, "format", FORMAT_NICK)
        && enum_nick_is(source, "stream-type", STREAM_TYPE_NICK)
        && source.property::<bool>("is-live")
        && source.property::<bool>("block")
        && !source.property::<bool>("emit-signals")
        && !source.property::<bool>("do-timestamp")
        && source.property::<u64>("max-bytes") == MAX_QUEUED_BYTES
}

pub(super) fn is_rejection_marker(structure: &gst::StructureRef) -> bool {
    structure.name() == REJECTION_MESSAGE && structure.n_fields() == 0
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::controller::OperationGeneration;
    use crate::domain::{ChannelKey, DeviceId, GuideNumber};
    use crate::playback::test_support::{FixtureStreamServer, StreamBehavior, fixture_response};
    use crate::playback::transport::PIPELINE_URI;

    const QUICK: TransportConfig = TransportConfig::new(
        Duration::from_millis(500),
        Duration::from_millis(1_500),
        Duration::from_millis(500),
    );

    fn pipeline() -> Option<gst::Pipeline> {
        gst::init().ok()?;
        gst::ElementFactory::make("playbin3")
            .build()
            .ok()?
            .downcast::<gst::Pipeline>()
            .ok()
    }

    fn handoff(url: &str) -> StreamHandoff {
        StreamHandoff::test_fixture(
            ChannelKey::new(
                DeviceId::new(0x105A_1232).unwrap(),
                GuideNumber::new("5.1").unwrap(),
            ),
            OperationGeneration::new(9),
            url,
        )
    }

    fn unreachable_handoff() -> StreamHandoff {
        handoff("http://127.0.0.1:9/auto/v5.1")
    }

    /// Whether a software MPEG-2 decoder can decode the checked-in fixture.
    fn mpeg2_decoder_available() -> bool {
        ["avdec_mpeg2video", "mpeg2dec"]
            .into_iter()
            .any(|factory| gst::ElementFactory::find(factory).is_some())
    }

    /// Serializes every registry rank override in this test binary, so two
    /// tests cannot interleave a demotion with another test's autoplugging.
    static DECODER_RANK_OVERRIDE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Holds the rank-override lock and restores the original rank of every
    /// demoted decoder factory when dropped, on every exit path including a
    /// panicking assertion, so a later pipeline in the same process autoplugs
    /// from the registry it started with.
    struct DecoderRankGuard {
        original: Vec<(gst::PluginFeature, gst::Rank)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for DecoderRankGuard {
        /// Restore the recorded ranks before releasing the override lock.
        fn drop(&mut self) {
            for (feature, rank) in self.original.drain(..) {
                feature.set_rank(rank);
            }
        }
    }

    /// Hosted CI virtual machines register hardware MPEG-2 decoders (Apple
    /// VideoToolbox, Direct3D, NVIDIA, Intel, AMD, VA-API) that cannot open a
    /// decoding session without a GPU. This test proves the `appsrc` feed and
    /// demux contract, not hardware decoding, so demote those factories for
    /// the duration of the returned guard and let `decodebin3` choose the
    /// software decoders.
    fn prefer_software_mpeg2_decoders() -> DecoderRankGuard {
        // A test that panicked while holding the lock already restored its
        // ranks in Drop, so the poisoned state carries no stale override.
        let lock = DECODER_RANK_OVERRIDE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = gst::Registry::get();
        let mut original = Vec::new();
        for name in [
            "vtdec_hw",
            "vtdec",
            "d3d11mpeg2dec",
            "d3d12mpeg2dec",
            "nvmpeg2videodec",
            "nvmpeg2dec",
            "qsvmpeg2dec",
            "msdkmpeg2dec",
            "amfmpeg2dec",
            "vampeg2dec",
            "vaapimpeg2dec",
            "v4l2slmpeg2dec",
        ] {
            if let Some(feature) = registry.lookup_feature(name) {
                original.push((feature.clone(), feature.rank()));
                feature.set_rank(gst::Rank::NONE);
            }
        }
        DecoderRankGuard {
            original,
            _lock: lock,
        }
    }

    /// Endpoint-free diagnostics for a failed contract run: the native error
    /// domain and code plus the factory name of the reporting element. No
    /// error or debug text is rendered.
    fn native_error_summary(message: &gst::MessageRef) -> String {
        let gst::MessageView::Error(error) = message.view() else {
            return String::from("non-error message");
        };
        let native = error.error();
        let factory = message
            .src()
            .and_then(|source| source.downcast_ref::<gst::Element>().cloned())
            .and_then(|element| element.factory())
            .map_or_else(
                || String::from("<none>"),
                |factory| factory.name().to_string(),
            );
        format!(
            "domain={} code={} source_factory={factory}",
            native.domain().as_str(),
            native.code()
        )
    }

    #[test]
    fn appsrc_configuration_is_exact_bounded_and_network_free() {
        if gst::init().is_err() {
            return;
        }
        let Ok(source) = gst::ElementFactory::make("appsrc").build() else {
            return;
        };
        assert!(configure_and_verify(&source));
        assert!(source.property::<bool>("is-live"));
        assert!(source.property::<bool>("block"));
        assert!(!source.property::<bool>("emit-signals"));
        assert!(!source.property::<bool>("do-timestamp"));
        assert_eq!(source.property::<u64>("max-bytes"), 4 * 1_024 * 1_024);
        assert!(enum_nick_is(&source, "format", "bytes"));
        assert!(enum_nick_is(&source, "stream-type", "stream"));
        let caps = source.property::<Option<gst::Caps>>("caps").unwrap();
        assert_eq!(caps.to_string(), "video/mpegts, systemstream=(boolean)true");

        let foreign = gst::ElementFactory::make("fakesrc").build().unwrap();
        assert!(!configure_and_verify(&foreign));
    }

    #[test]
    fn accepted_appsrc_consumes_the_handoff_and_rejects_a_repeat() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK) else {
            return;
        };
        assert!(validated_source_setup_signal(&playbin).is_ok());
        let source = gst::ElementFactory::make("appsrc").build().unwrap();

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);

        assert!(!policy.is_rejected());
        assert_eq!(policy.accepted_factory_name().as_deref(), Some("appsrc"));
        assert!(policy.state.pending.lock().unwrap().is_none());
        assert!(policy.state.transport.lock().unwrap().is_some());

        let repeat = gst::ElementFactory::make("appsrc").build().unwrap();
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&repeat]);
        assert!(policy.is_rejected());
        assert!(repeat.is_locked_state());
        assert!(!source.is_locked_state());

        let mut transport = policy
            .retire()
            .expect("the accepted transport is returned once");
        assert!(policy.retire().is_none());
        assert_eq!(
            transport.join(Instant::now() + Duration::from_secs(5)),
            Ok(())
        );
    }

    #[test]
    fn rejection_marker_is_field_free_and_deduplicated() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK) else {
            return;
        };
        let bus = playbin.bus().unwrap();
        bus.set_flushing(false);
        let first = gst::ElementFactory::make("fakesrc").build().unwrap();
        let second = gst::ElementFactory::make("fakesrc").build().unwrap();

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&first]);
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&second]);

        assert!(policy.is_rejected());
        assert!(first.is_locked_state());
        assert!(second.is_locked_state());
        assert!(
            policy.state.pending.lock().unwrap().is_some(),
            "a rejected foreign source never consumes the handoff"
        );
        let message = bus
            .timed_pop_filtered(
                gst::ClockTime::from_mseconds(10),
                &[gst::MessageType::Application],
            )
            .expect("the first rejection posts its fixed marker");
        assert_eq!(message.src(), Some(playbin.upcast_ref::<gst::Object>()));
        let gst::MessageView::Application(application) = message.view() else {
            panic!("application marker");
        };
        assert!(is_rejection_marker(application.structure().unwrap()));
        assert!(
            bus.timed_pop_filtered(gst::ClockTime::ZERO, &[gst::MessageType::Application])
                .is_none()
        );
        assert!(policy.retire().is_none());
        assert!(policy.state.pending.lock().unwrap().is_none());
    }

    #[test]
    fn source_setup_callback_is_safe_from_a_worker_thread() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK) else {
            return;
        };
        let worker_playbin = playbin.clone();
        std::thread::spawn(move || {
            let source = gst::ElementFactory::make("appsrc").build().unwrap();
            worker_playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
        })
        .join()
        .unwrap();

        assert!(!policy.is_rejected());
        assert_eq!(policy.accepted_factory_name().as_deref(), Some("appsrc"));
        let mut transport = policy.retire().unwrap();
        assert_eq!(
            transport.join(Instant::now() + Duration::from_secs(5)),
            Ok(())
        );
    }

    #[test]
    fn retired_policy_rejects_late_sources_and_zeroizes_the_handoff() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK) else {
            return;
        };
        assert!(policy.retire().is_none());
        assert!(policy.state.pending.lock().unwrap().is_none());

        let late = gst::ElementFactory::make("appsrc").build().unwrap();
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&late]);
        assert!(policy.is_rejected());
        assert!(late.is_locked_state());
        assert!(policy.state.transport.lock().unwrap().is_none());
    }

    /// Explicit runtime probe for CI lanes: the installed GStreamer must map
    /// the constant URI to the exact built-in `appsrc`. Unlike the ordinary
    /// unit tests, a missing factory fails instead of skipping, and no decoder
    /// or display is needed because the pipeline only reaches PAUSED.
    #[test]
    #[ignore = "requires the installed GStreamer playback foundation"]
    fn installed_runtime_maps_the_constant_uri_to_exact_appsrc() {
        gst::init().expect("initialize the installed GStreamer runtime");
        assert!(
            gst::ElementFactory::find("appsrc").is_some(),
            "the installed runtime must provide the built-in appsrc"
        );
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .expect("the installed runtime must provide playbin3")
            .downcast::<gst::Pipeline>()
            .expect("playbin3 is a pipeline");
        let video_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        let audio_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-sink", &audio_sink);
        let policy = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK)
            .expect("install the appsrc policy on the installed runtime");
        playbin.set_property("uri", PIPELINE_URI);
        assert_eq!(
            playbin.property::<Option<String>>("uri").as_deref(),
            Some(PIPELINE_URI)
        );

        playbin
            .set_state(gst::State::Paused)
            .expect("playbin3 must accept the constant URI");
        let deadline = Instant::now() + Duration::from_secs(5);
        while policy.accepted_factory_name().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            policy.accepted_factory_name().as_deref(),
            Some("appsrc"),
            "playbin3 must deliver the exact built-in appsrc through source-setup"
        );
        assert!(!policy.is_rejected());

        let mut transport = policy.retire().expect("the accepted transport is returned");
        playbin.set_state(gst::State::Null).unwrap();
        let (transition, current, _) = playbin.state(gst::ClockTime::from_seconds(5));
        assert!(transition.is_ok());
        assert_eq!(current, gst::State::Null);
        assert_eq!(
            transport.join(Instant::now() + Duration::from_secs(5)),
            Ok(())
        );
    }

    /// Network-free end-to-end contract: `playbin3` must resolve the constant
    /// URI to the exact built-in `appsrc`, accept the configured feed from the
    /// loopback transport, decode the checked-in fixture, and reach EOS.
    #[test]
    fn playbin3_resolves_the_constant_uri_to_exact_appsrc_and_plays_a_loopback_fixture() {
        let Some(playbin) = pipeline() else {
            return;
        };
        if !mpeg2_decoder_available() || gst::ElementFactory::find("tsdemux").is_none() {
            return;
        }
        let _software_decoders = prefer_software_mpeg2_decoders();
        let server = FixtureStreamServer::start(fixture_response(), StreamBehavior::Close);
        let video_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        let audio_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-sink", &audio_sink);
        let policy = SourcePolicy::install(&playbin, handoff(&server.stream_url()), QUICK)
            .expect("install the appsrc policy");
        playbin.set_property("uri", PIPELINE_URI);
        assert_eq!(
            playbin.property::<Option<String>>("uri").as_deref(),
            Some(PIPELINE_URI)
        );
        let bus = playbin.bus().unwrap();

        playbin
            .set_state(gst::State::Playing)
            .expect("request playback");
        assert_eq!(policy.accepted_factory_name().as_deref(), Some("appsrc"));

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reached_playing = false;
        let mut terminal = None;
        let mut diagnostic = String::from("no terminal message within the deadline");
        while terminal.is_none() && Instant::now() < deadline {
            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                continue;
            };
            match message.view() {
                gst::MessageView::Eos(_) => terminal = Some("eos"),
                gst::MessageView::Error(_) => {
                    diagnostic = native_error_summary(&message);
                    terminal = Some("error");
                }
                gst::MessageView::Application(application) => {
                    diagnostic = format!(
                        "application marker {:?}",
                        application
                            .structure()
                            .map(|structure| structure.name().to_string())
                    );
                    terminal = Some("application");
                }
                gst::MessageView::StateChanged(changed)
                    if message.src() == Some(playbin.upcast_ref::<gst::Object>())
                        && changed.current() == gst::State::Playing =>
                {
                    reached_playing = true;
                }
                _ => {}
            }
        }
        assert_eq!(terminal, Some("eos"), "{diagnostic}");
        assert!(
            reached_playing,
            "playbin3 must reach PLAYING from the appsrc feed"
        );
        assert!(
            video_sink
                .property::<gst::Structure>("stats")
                .get::<u64>("rendered")
                .is_ok_and(|rendered| rendered >= 2)
        );
        assert!(!policy.is_rejected());

        let mut transport = policy
            .retire()
            .expect("the played transport is returned once");
        playbin.set_state(gst::State::Null).unwrap();
        let (transition, current, _) = playbin.state(gst::ClockTime::from_seconds(5));
        assert!(transition.is_ok());
        assert_eq!(current, gst::State::Null);
        assert_eq!(
            transport.join(Instant::now() + Duration::from_secs(5)),
            Ok(())
        );
    }
}
