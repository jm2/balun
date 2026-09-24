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
use super::transport_timing::TransportTiming;
use crate::controller::StreamHandoff;

#[cfg(test)]
mod lifecycle_tests;
#[cfg(test)]
mod timing_tests;

const SOURCE_SETUP_SIGNAL: &str = "source-setup";
const REJECTION_MESSAGE: &str = "balun-source-policy-rejected";
const STREAM_CAPS_NAME: &str = "video/mpegts";
const STREAM_CAPS_SYSTEMSTREAM: &str = "systemstream";
const FORMAT_NICK: &str = "time";
const STREAM_TYPE_NICK: &str = "stream";
/// Bounded bytes `appsrc` may hold before the feeder blocks.
const MAX_QUEUED_BYTES: u64 = 4 * 1_024 * 1_024;

#[derive(Debug, Clone, Copy)]
pub(super) struct SourcePolicyError;

struct PendingStream {
    handoff: StreamHandoff,
    config: TransportConfig,
    timing: Option<Arc<TransportTiming>>,
}

struct SourcePolicyState {
    expected_factory: gst::ElementFactory,
    lifecycle: Mutex<SourceLifecycle>,
    rejected: AtomicBool,
    #[cfg(test)]
    startup_hook: Mutex<Option<StartupHook>>,
}

/// Admission, handoff consumption, worker startup/publication, and retirement
/// share one lock, including rejection. Once retirement returns, no callback
/// can publish new workers; rejection cancels workers while retaining their join.
struct SourceLifecycle {
    pending: Option<PendingStream>,
    transport: Option<StreamTransport>,
    accepted_source: Option<gst::Object>,
    retired: bool,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StartupStage {
    BeforeAdmission,
    HandoffTaken,
    TransportStarted,
}

#[cfg(test)]
type StartupHook = Arc<dyn Fn(StartupStage) + Send + Sync>;

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
        timing: Option<Arc<TransportTiming>>,
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
            lifecycle: Mutex::new(SourceLifecycle {
                pending: Some(PendingStream {
                    handoff,
                    config,
                    timing,
                }),
                transport: None,
                accepted_source: None,
                retired: false,
            }),
            rejected: AtomicBool::new(false),
            #[cfg(test)]
            startup_hook: Mutex::new(None),
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
                callback_state.reject(
                    playbin.as_ref(),
                    source.as_ref(),
                    "source-setup arrived with unexpected arguments or from another pipeline",
                );
                return None;
            }
            let Some(source) = source else {
                callback_state.reject(playbin.as_ref(), None, "source-setup carried no element");
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
        // Even a poisoned admission must retain its workers for the same
        // teardown join; poison is never evidence that no transport exists.
        let mut lifecycle = self
            .state
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        lifecycle.retired = true;
        lifecycle.pending.take();
        let transport = lifecycle.transport.take();
        if let Some(transport) = transport.as_ref() {
            transport.cancel();
        }
        transport
    }

    #[cfg(test)]
    fn accepted_factory_name(&self) -> Option<String> {
        self.state
            .lifecycle
            .lock()
            .ok()?
            .accepted_source
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
    #[cfg(test)]
    fn at_startup_stage(&self, stage: StartupStage) {
        let hook = self.startup_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(stage);
        }
    }

    fn inspect_source(&self, playbin: &gst::Pipeline, source: &gst::Element) {
        if self.rejected.load(Ordering::Acquire) {
            self.reject(
                Some(playbin),
                Some(source),
                "policy already rejected or retired",
            );
            return;
        }
        if source.factory().as_ref() != Some(&self.expected_factory) {
            self.reject(
                Some(playbin),
                Some(source),
                "source is not the exact appsrc factory",
            );
            return;
        }
        if !configure_and_verify(source) {
            self.reject(
                Some(playbin),
                Some(source),
                "appsrc refused the required configuration",
            );
            return;
        }

        #[cfg(test)]
        self.at_startup_stage(StartupStage::BeforeAdmission);
        let mut lifecycle = match self.lifecycle.lock() {
            Ok(lifecycle) => lifecycle,
            Err(poisoned) => {
                // Drop the recovered guard before rejection reacquires it.
                drop(poisoned.into_inner());
                self.reject(
                    Some(playbin),
                    Some(source),
                    "source lifecycle lock poisoned",
                );
                return;
            }
        };
        if lifecycle.retired
            || lifecycle.accepted_source.is_some()
            || self.rejected.load(Ordering::Acquire)
        {
            drop(lifecycle);
            self.reject(Some(playbin), Some(source), "source admission is closed");
            return;
        }
        let pending = lifecycle.pending.take();
        let Some(pending) = pending else {
            drop(lifecycle);
            self.reject(
                Some(playbin),
                Some(source),
                "no pending handoff for this source",
            );
            return;
        };
        #[cfg(test)]
        self.at_startup_stage(StartupStage::HandoffTaken);
        match StreamTransport::start(
            pending.handoff,
            source.clone(),
            playbin,
            pending.config,
            pending.timing,
        ) {
            Ok(transport) => {
                lifecycle.accepted_source = Some(source.clone().upcast::<gst::Object>());
                lifecycle.transport = Some(transport);
                #[cfg(test)]
                self.at_startup_stage(StartupStage::TransportStarted);
            }
            Err(failure) => {
                // A partial start still belongs to this generation. Teardown
                // must join it before the session can admit a successor.
                lifecycle.transport = failure.transport;
                let error = failure.error;
                lifecycle.retired = true;
                drop(lifecycle);
                tracing::warn!(target: "balun::playback", ?error, "stream transport failed to start");
                self.reject(
                    Some(playbin),
                    Some(source),
                    "stream transport failed to start",
                );
            }
        }
    }

    fn reject(
        &self,
        playbin: Option<&gst::Pipeline>,
        source: Option<&gst::Element>,
        reason: &'static str,
    ) {
        let first_rejection = {
            let mut lifecycle = self
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let first = !self.rejected.swap(true, Ordering::AcqRel);
            lifecycle.retired = true;
            lifecycle.pending.take();
            // Cancellation closes the reader now. Keep ownership so ordinary
            // retirement can take and join both workers after pipeline NULL.
            if let Some(transport) = &lifecycle.transport {
                transport.cancel();
            }
            first
        };
        tracing::warn!(
            target: "balun::playback",
            reason,
            first_rejection,
            "source policy rejected playbin3's source"
        );
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

/// Configure Balun's bounded live MPEG-TS feed with arrival timestamps.
/// A TIME segment lets tsdemux map broadcast PCR/PTS onto the running clock;
/// BYTES instead anchors media to stream offsets and disables clock skew
/// correction, leaving late-starting media permanently behind the sink.
/// This performs no network work and never sees a URL.
pub(super) fn configure_and_verify(source: &gst::Element) -> bool {
    if !readable_writable_property::<gst::Caps>(source, "caps")
        || !readable_writable_property::<bool>(source, "is-live")
        || !readable_writable_property::<bool>(source, "block")
        || !readable_writable_property::<bool>(source, "emit-signals")
        || !readable_writable_property::<bool>(source, "do-timestamp")
        || !readable_writable_property::<i64>(source, "min-latency")
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
    source.set_property("do-timestamp", true);
    // The timestamp describes arrival at appsrc, not an earlier capture.
    source.set_property("min-latency", 0_i64);
    source.set_property("max-bytes", MAX_QUEUED_BYTES);

    source
        .property::<Option<gst::Caps>>("caps")
        .is_some_and(|configured| configured.is_strictly_equal(&caps))
        && enum_nick_is(source, "format", FORMAT_NICK)
        && enum_nick_is(source, "stream-type", STREAM_TYPE_NICK)
        && source.property::<bool>("is-live")
        && source.property::<bool>("block")
        && !source.property::<bool>("emit-signals")
        && source.property::<bool>("do-timestamp")
        && source.property::<i64>("min-latency") == 0
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
    use crate::playback::pipeline_failure::{PlaybackPipelineFailure, classify_pipeline_message};
    use crate::playback::test_support::{
        FIXTURE_BYTES, FixtureStreamServer, StreamBehavior, fixture_response, http_response,
        mpeg2_decoder_available, open_ended_response_head, prefer_software_mpeg2_decoders,
    };
    use crate::playback::transport::{PIPELINE_URI, STREAM_STARTED_MESSAGE};

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
        assert!(source.property::<bool>("do-timestamp"));
        assert_eq!(source.property::<i64>("min-latency"), 0);
        assert_eq!(source.property::<u64>("max-bytes"), 4 * 1_024 * 1_024);
        assert!(enum_nick_is(&source, "format", "time"));
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
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK, None) else {
            return;
        };
        assert!(validated_source_setup_signal(&playbin).is_ok());
        let source = gst::ElementFactory::make("appsrc").build().unwrap();

        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);

        assert!(!policy.is_rejected());
        assert_eq!(policy.accepted_factory_name().as_deref(), Some("appsrc"));
        assert!(policy.state.lifecycle.lock().unwrap().pending.is_none());
        assert!(policy.state.lifecycle.lock().unwrap().transport.is_some());

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
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK, None) else {
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
            policy.state.lifecycle.lock().unwrap().pending.is_none(),
            "rejection zeroizes the unconsumed handoff"
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
        assert!(policy.state.lifecycle.lock().unwrap().pending.is_none());
    }

    #[test]
    fn source_setup_callback_is_safe_from_a_worker_thread() {
        let Some(playbin) = pipeline() else {
            return;
        };
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK, None) else {
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
        let Ok(policy) = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK, None) else {
            return;
        };
        assert!(policy.retire().is_none());
        assert!(policy.state.lifecycle.lock().unwrap().pending.is_none());

        let late = gst::ElementFactory::make("appsrc").build().unwrap();
        playbin.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&late]);
        assert!(policy.is_rejected());
        assert!(late.is_locked_state());
        assert!(policy.state.lifecycle.lock().unwrap().transport.is_none());
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
        let policy = SourcePolicy::install(&playbin, unreachable_handoff(), QUICK, None)
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
        crate::playback::session::configure_playbin_video(&playbin, &video_sink).unwrap();
        playbin.set_property("audio-sink", &audio_sink);
        let timing = Arc::new(TransportTiming::new(Instant::now()));
        let policy = SourcePolicy::install(
            &playbin,
            handoff(&server.stream_url()),
            QUICK,
            Some(Arc::clone(&timing)),
        )
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
                gst::MessageView::Application(application)
                    if application.structure().is_some_and(|structure| {
                        structure.name() == crate::playback::transport::STREAM_STARTED_MESSAGE
                    }) => {}
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

        assert!(
            timing.snapshot().iter().all(|(_, value)| value.is_some()),
            "the admitted source must publish the exact tune's transport observations"
        );

        let deinterlacing = crate::playback::deinterlace::describe(playbin.upcast_ref());

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
        assert!(
            deinterlacing.contains("deinterlace method=yadif output-framerate=")
                && !deinterlacing.contains("output-framerate=not negotiated"),
            "the actual playsink filter must retain the selected software method: {deinterlacing}"
        );
    }

    /// A tuner can answer 200 and end the body before any byte. The session
    /// holds at PAUSED until the stream-started notice, and a live `appsrc`
    /// never delivers EOS while paused, so only a transport failure can end
    /// the tune. Production deadlines keep a timeout from standing in for it.
    #[test]
    fn playbin3_paused_hold_fails_offline_when_the_stream_ends_before_any_data() {
        for (response, shape) in [
            (
                http_response("200 OK", &[("Content-Length", "0".to_owned())], b""),
                "zero content length",
            ),
            (open_ended_response_head(), "open-ended head then close"),
        ] {
            let playbin = pipeline().expect("the empty-stream regression requires playbin3");
            let server = FixtureStreamServer::start(response, StreamBehavior::Close);
            let video_sink = gst::ElementFactory::make("fakesink").build().unwrap();
            let audio_sink = gst::ElementFactory::make("fakesink").build().unwrap();
            crate::playback::session::configure_playbin_video(&playbin, &video_sink).unwrap();
            playbin.set_property("audio-sink", &audio_sink);
            let policy = SourcePolicy::install(
                &playbin,
                handoff(&server.stream_url()),
                TransportConfig::PRODUCTION,
                None,
            )
            .expect("install the appsrc policy");
            playbin.set_property("uri", PIPELINE_URI);
            let bus = playbin.bus().unwrap();
            playbin
                .set_state(gst::State::Paused)
                .expect("hold the pipeline at PAUSED");

            let deadline = Instant::now() + Duration::from_secs(5);
            let mut outcome = None;
            let mut started = false;
            while outcome.is_none() && Instant::now() < deadline {
                let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                    continue;
                };
                if let Some(failure) = classify_pipeline_message(&message, &playbin) {
                    outcome = Some(failure);
                    continue;
                }
                match message.view() {
                    gst::MessageView::Eos(_) => panic!("{shape}: EOS during the PAUSED hold"),
                    gst::MessageView::Application(application)
                        if application.structure().is_some_and(|structure| {
                            structure.name() == STREAM_STARTED_MESSAGE
                        }) =>
                    {
                        started = true;
                    }
                    _ => {}
                }
            }
            assert!(server.request(Duration::from_secs(3)).is_some(), "{shape}");
            let (_, held, _) = playbin.state(gst::ClockTime::ZERO);
            let mut transport = policy
                .retire()
                .expect("the accepted transport is returned once");
            playbin.set_state(gst::State::Null).unwrap();
            let (transition, current, _) = playbin.state(gst::ClockTime::from_seconds(5));
            assert!(transition.is_ok());
            assert_eq!(current, gst::State::Null);
            assert_eq!(
                transport.join(Instant::now() + Duration::from_secs(5)),
                Ok(())
            );

            assert_eq!(outcome, Some(PlaybackPipelineFailure::Offline), "{shape}");
            assert!(!started, "{shape}: no bytes means no stream-started notice");
            assert_eq!(held, gst::State::Paused, "{shape}");
        }
    }

    /// A stream Balun never presents, muxed beside the checked-in video.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AncillaryStream {
        Teletext,
        DvbSubtitles,
    }

    /// Remux the checked-in MPEG-2 fixture with one teletext or DVB subtitle
    /// stream, as DVB broadcasts carry them. Every factory is required, so the
    /// regression below can never pass by skipping.
    fn fixture_with(ancillary: AncillaryStream) -> Vec<u8> {
        let branch = match ancillary {
            AncillaryStream::Teletext => {
                "appsrc name=teletext format=time caps=application/x-teletext ! queue ! mux."
            }
            AncillaryStream::DvbSubtitles => {
                "videotestsrc num-buffers=25 pattern=ball \
                 ! video/x-raw,format=AYUV,width=160,height=96,framerate=25/1 \
                 ! dvbsubenc ! queue ! mux."
            }
        };
        let muxer = gst::parse::launch(&format!(
            "appsrc name=video format=bytes caps=video/mpegts,systemstream=true \
             ! tsdemux ! mpegvideoparse ! queue ! mpegtsmux name=mux \
             ! appsink name=out sync=false {branch}"
        ))
        .expect("the ancillary-stream fixture requires its muxing factories")
        .downcast::<gst::Pipeline>()
        .unwrap();
        let video = muxer.by_name("video").unwrap();
        let buffer = gst::Buffer::from_slice(FIXTURE_BYTES);
        assert_eq!(
            video.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
            gst::FlowReturn::Ok
        );
        assert_eq!(
            video.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]),
            gst::FlowReturn::Ok
        );
        if let Some(teletext) = muxer.by_name("teletext") {
            for index in 0..25_u64 {
                // EBU teletext data identifier, then one 44-byte stuffing unit
                // per video frame of the fixture.
                let mut payload = vec![0x10, 0xFF, 0x2C];
                payload.resize(47, 0);
                let mut buffer = gst::Buffer::from_mut_slice(payload);
                let timed = buffer.get_mut().unwrap();
                timed.set_pts(gst::ClockTime::from_mseconds(165 + 40 * index));
                timed.set_duration(gst::ClockTime::from_mseconds(40));
                assert_eq!(
                    teletext.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
                    gst::FlowReturn::Ok
                );
            }
            assert_eq!(
                teletext.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]),
                gst::FlowReturn::Ok
            );
        }
        muxer
            .set_state(gst::State::Playing)
            .expect("start the fixture muxer");
        let out = muxer.by_name("out").unwrap();
        let mut stream = Vec::new();
        while let Some(sample) = out.emit_by_name::<Option<gst::Sample>>(
            "try-pull-sample",
            &[&gst::ClockTime::from_seconds(5).nseconds()],
        ) {
            stream.extend_from_slice(&sample.buffer().unwrap().map_readable().unwrap());
        }
        assert!(out.property::<bool>("eos"), "the fixture muxer must finish");
        assert!(
            muxer
                .bus()
                .unwrap()
                .pop_filtered(&[gst::MessageType::Error])
                .is_none(),
            "the fixture muxer failed"
        );
        muxer.set_state(gst::State::Null).unwrap();
        assert!(stream.len() > FIXTURE_BYTES.len());
        stream
    }

    /// DVB broadcasts can carry teletext and subtitle streams Balun never
    /// presents. Their missing decoder or renderer must not end the tune, and
    /// playbin must not burn subtitles into the video. The renderers are
    /// demoted so the missing condition holds on every runtime, and the stream
    /// runs through the production source policy, video configuration, PAUSED
    /// hold, and bus classifier.
    #[test]
    fn playbin3_plays_past_unpresented_teletext_and_subtitle_streams() {
        gst::init().expect("initialize GStreamer");
        assert!(
            mpeg2_decoder_available(),
            "the ancillary-stream regression requires an MPEG-2 decoder"
        );
        let mut decoders = prefer_software_mpeg2_decoders();
        let registry = gst::Registry::get();
        for name in ["teletextdec", "dvbsuboverlay"] {
            if let Some(feature) = registry.lookup_feature(name) {
                decoders.original.push((feature.clone(), feature.rank()));
                feature.set_rank(gst::Rank::NONE);
            }
        }

        for ancillary in [AncillaryStream::Teletext, AncillaryStream::DvbSubtitles] {
            let stream = fixture_with(ancillary);
            let server = FixtureStreamServer::start(
                http_response(
                    "200 OK",
                    &[("Content-Length", stream.len().to_string())],
                    &stream,
                ),
                StreamBehavior::Close,
            );
            let playbin = pipeline().expect("the ancillary-stream regression requires playbin3");
            let video_sink = gst::ElementFactory::make("fakesink").build().unwrap();
            let audio_sink = gst::ElementFactory::make("fakesink").build().unwrap();
            crate::playback::session::configure_playbin_video(&playbin, &video_sink).unwrap();
            playbin.set_property("audio-sink", &audio_sink);
            let policy =
                SourcePolicy::install(&playbin, handoff(&server.stream_url()), QUICK, None)
                    .expect("install the appsrc policy");
            playbin.set_property("uri", PIPELINE_URI);
            let bus = playbin.bus().unwrap();
            playbin
                .set_state(gst::State::Paused)
                .expect("hold the pipeline at PAUSED");

            let deadline = Instant::now() + Duration::from_secs(10);
            let mut outcome = None;
            let mut missing = Vec::new();
            let mut text_streams = 0;
            while outcome.is_none() && Instant::now() < deadline {
                let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                    continue;
                };
                if let Some(failure) = classify_pipeline_message(&message, &playbin) {
                    outcome = Some(Err(failure));
                    continue;
                }
                match message.view() {
                    gst::MessageView::Eos(_) => outcome = Some(Ok(())),
                    gst::MessageView::Application(application)
                        if message.src() == Some(playbin.upcast_ref::<gst::Object>())
                            && application.structure().is_some_and(|structure| {
                                structure.name() == STREAM_STARTED_MESSAGE
                            }) =>
                    {
                        playbin
                            .set_state(gst::State::Playing)
                            .expect("leave the PAUSED hold");
                    }
                    gst::MessageView::Element(element) if element.has_name("missing-plugin") => {
                        missing.push(
                            element
                                .structure()
                                .and_then(|structure| structure.get::<gst::Caps>("detail").ok())
                                .and_then(|caps| {
                                    caps.structure(0)
                                        .map(|structure| structure.name().to_string())
                                })
                                .unwrap_or_default(),
                        );
                    }
                    gst::MessageView::StreamCollection(collection) => {
                        text_streams = text_streams.max(
                            collection
                                .stream_collection()
                                .iter()
                                .filter(|stream| {
                                    stream.stream_type().contains(gst::StreamType::TEXT)
                                })
                                .count(),
                        );
                    }
                    _ => {}
                }
            }
            let rendered = video_sink
                .property::<gst::Structure>("stats")
                .get::<u64>("rendered")
                .unwrap_or(0);
            let overlays = playbin
                .iterate_recurse()
                .into_iter()
                .flatten()
                .filter(|element| {
                    element.factory().is_some_and(|factory| {
                        ["subtitleoverlay", "dvbsuboverlay", "textoverlay"]
                            .contains(&factory.name().as_str())
                    })
                })
                .count();
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

            assert_eq!(
                outcome,
                Some(Ok::<(), PlaybackPipelineFailure>(())),
                "{ancillary:?}"
            );
            assert!(rendered >= 2, "{ancillary:?}: rendered {rendered}");
            assert_eq!(overlays, 0, "{ancillary:?}: subtitles must not be rendered");
            match ancillary {
                AncillaryStream::Teletext => assert!(
                    missing.iter().any(|name| name == "application/x-teletext"),
                    "the missing teletext decoder must be reported: {missing:?}"
                ),
                AncillaryStream::DvbSubtitles => {
                    assert!(text_streams >= 1, "the subtitle stream must be advertised");
                    assert!(missing.is_empty(), "{missing:?}");
                }
            }
        }
    }
}
