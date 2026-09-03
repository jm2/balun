//! Generation-owned playback session and deterministic pipeline teardown.

use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gst::prelude::*;
use gstreamer as gst;
use gtk::gdk;
use thiserror::Error;
use tokio::sync::watch;

use super::PlaybackRuntime;
use super::pipeline_failure::{self, PlaybackPipelineFailure};
use super::source_policy::SourcePolicy;
use super::transport::{PIPELINE_URI, STREAM_STARTED_MESSAGE, StreamTransport, TransportConfig};
use crate::controller::{OperationGeneration, StreamHandoff, StreamHandoffError, StreamSelection};
use crate::domain::ChannelKey;

const PIPELINE_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DEINTERLACE_PLAY_FLAG: &str = "deinterlace";
const PAINTABLE_ASPECT_PROPERTY: &str = "force-aspect-ratio";
const PLAYBIN_VOLUME_PROPERTY: &str = "volume";
const PLAYBIN_MUTE_PROPERTY: &str = "mute";

/// Process-local audio settings inherited by every successor tune.
///
/// Volume is a normalized user level in the inclusive range `0.0..=1.0` and
/// is converted to a cubic GStreamer gain at the private pipeline boundary.
/// Muting is independent, so adjusting volume while muted preserves the level
/// which will be restored when audio is unmuted.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackAudioState {
    volume: f64,
    muted: bool,
}

impl PlaybackAudioState {
    /// Return the normalized volume in the inclusive range `0.0..=1.0`.
    #[must_use]
    pub const fn volume(self) -> f64 {
        self.volume
    }

    /// Return whether audio is muted independently of the retained volume.
    #[must_use]
    pub const fn is_muted(self) -> bool {
        self.muted
    }
}

impl Default for PlaybackAudioState {
    fn default() -> Self {
        Self {
            volume: 1.0,
            muted: false,
        }
    }
}

/// Monotonic identity for one playback attempt.
///
/// Tune generations are independent from selected-lineup generations. A new
/// generation is assigned before Balun waits for the controller's private
/// stream response, so a late response or bus message cannot affect its
/// successor.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TuneGeneration(u64);

impl TuneGeneration {
    const INITIAL: Self = Self(0);

    /// Return the numeric generation for diagnostics and tests.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Fixed, URL-free reason one tune attempt failed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum PlaybackSessionFailure {
    /// Required factories were absent from the startup capability snapshot.
    #[error("required playback components are unavailable")]
    ComponentsUnavailable,
    /// No further unique tune generation can be assigned safely.
    #[error("the playback generation is exhausted")]
    GenerationExhausted,
    /// Terminal shutdown was requested, so no future tune can be admitted.
    #[error("the playback session is shut down")]
    ShutDown,
    /// A mutating call was made without owning Balun's default main context.
    #[error("the default main context is not owned by the playback thread")]
    MainContextUnavailable,
    /// A native callback safely reentered an in-progress session mutation.
    #[error("the playback session is already handling another operation")]
    SessionBusy,
    /// A volume value was non-finite or outside the supported linear range.
    #[error("the playback volume must be between zero and one")]
    InvalidVolume,
    /// The controller rejected or lost the URL-private handoff.
    #[error("the controller rejected the stream handoff: {0}")]
    Handoff(StreamHandoffError),
    /// The response did not match the exact pending channel and selection.
    #[error("the stream handoff did not match the pending tune")]
    HandoffMismatch,
    /// The required paintable, sink, or `playbin3` video contract could not be built.
    #[error("the playback pipeline could not be constructed")]
    PipelineConstruction,
    /// The pipeline did not expose a message bus.
    #[error("the playback pipeline did not provide a message bus")]
    BusUnavailable,
    /// The generation-scoped local bus watch could not be installed.
    #[error("the playback message watch could not be installed")]
    BusWatch,
    /// GStreamer rejected the transition into active playback.
    #[error("the playback pipeline could not start")]
    PipelineStart,
    /// GStreamer reported a native pipeline failure. Native text is discarded.
    #[error("the playback pipeline failed: {0}")]
    Pipeline(PlaybackPipelineFailure),
    /// The exact predecessor did not settle to `NULL` inside the fixed bound.
    #[error("the playback pipeline did not stop cleanly")]
    PipelineTeardown,
}

/// URL-free, immutable state of the one owned tune lane.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum PlaybackSessionState {
    /// No stream response or pipeline is retained.
    Stopped,
    /// One exact controller response is pending.
    Resolving {
        generation: TuneGeneration,
        channel_key: ChannelKey,
    },
    /// The pipeline was built and asked to enter `PLAYING`.
    Connecting {
        generation: TuneGeneration,
        channel_key: ChannelKey,
    },
    /// The active top-level pipeline reached `PLAYING`.
    Playing {
        generation: TuneGeneration,
        channel_key: ChannelKey,
    },
    /// The active pipeline published a bounded buffering percentage.
    Buffering {
        generation: TuneGeneration,
        channel_key: ChannelKey,
        percent: u8,
    },
    /// The current attempt failed. A teardown failure can retain one hidden,
    /// quarantined pipeline whose `NULL` settlement remains unproven.
    Failed {
        generation: TuneGeneration,
        channel_key: ChannelKey,
        failure: PlaybackSessionFailure,
    },
    /// Terminal shutdown was requested and future tune work is rejected.
    /// The shutdown call's result reports whether `NULL` was proven.
    ShutDown,
}

/// Single-use token joining an actor request to its tune generation.
///
/// This token is URL-free. Its constructor and fields are private so callers
/// cannot forge a response for another session attempt.
#[must_use = "the tune request must be resolved, cancelled, or deliberately dropped"]
pub struct TuneRequest {
    generation: TuneGeneration,
    selection: StreamSelection,
}

impl TuneRequest {
    /// Return the generation assigned before the controller wait begins.
    #[must_use]
    pub const fn generation(&self) -> TuneGeneration {
        self.generation
    }

    /// Return the URL-free controller selection to submit through the actor.
    #[must_use]
    pub const fn selection(&self) -> &StreamSelection {
        &self.selection
    }
}

impl fmt::Debug for TuneRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TuneRequest")
            .field("generation", &self.generation)
            .field("selection", &self.selection)
            .finish()
    }
}

/// Whether a completed controller response belonged to the current attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TuneCompletion {
    /// The response was applied to the current generation.
    Applied,
    /// A successor or Stop operation had already invalidated the response.
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PipelineEvent {
    /// The transport pushed its first stream bytes; the live clock may start.
    StreamStarted,
    Playing,
    Buffering(u8),
    EndOfStream,
    Error(PlaybackPipelineFailure),
}

type EventSink = Rc<dyn Fn(TuneGeneration, PipelineEvent)>;

enum PipelineStartError<P> {
    Clean(PlaybackSessionFailure),
    Quarantined(P),
}

trait PipelineBackend {
    type Active;

    fn start(
        &mut self,
        generation: TuneGeneration,
        handoff: StreamHandoff,
        audio: PlaybackAudioState,
        events: EventSink,
    ) -> Result<Self::Active, PipelineStartError<Self::Active>>;

    fn set_audio(
        &mut self,
        active: &mut Self::Active,
        audio: PlaybackAudioState,
    ) -> Result<(), PlaybackSessionFailure>;

    /// Move a started pipeline from its paused hold to PLAYING. Called once,
    /// when the stream's first bytes exist, so the running clock starts with
    /// data rather than with the tuner request.
    fn play(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure>;

    fn stop(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure>;
}

struct PendingTune {
    generation: TuneGeneration,
    channel_key: ChannelKey,
    selection_generation: OperationGeneration,
}

struct ActiveTune<P> {
    generation: TuneGeneration,
    channel_key: ChannelKey,
    pipeline: P,
}

struct SessionCore<B: PipelineBackend> {
    backend: B,
    audio: PlaybackAudioState,
    generation: TuneGeneration,
    pending: Option<PendingTune>,
    active: Option<ActiveTune<B::Active>>,
    state: PlaybackSessionState,
    state_sender: watch::Sender<PlaybackSessionState>,
    exhausted: bool,
    teardown_failed: bool,
    shut_down: bool,
}

impl<B: PipelineBackend> SessionCore<B> {
    fn new(backend: B) -> Self {
        let state = PlaybackSessionState::Stopped;
        let (state_sender, _state_receiver) = watch::channel(state.clone());
        Self {
            backend,
            audio: PlaybackAudioState::default(),
            generation: TuneGeneration::INITIAL,
            pending: None,
            active: None,
            state,
            state_sender,
            exhausted: false,
            teardown_failed: false,
            shut_down: false,
        }
    }

    fn state(&self) -> &PlaybackSessionState {
        &self.state
    }

    fn subscribe_state(&self) -> watch::Receiver<PlaybackSessionState> {
        self.state_sender.subscribe()
    }

    fn publish_state(&mut self, state: PlaybackSessionState) {
        if self.state == state {
            return;
        }
        self.state = state.clone();
        self.state_sender.send_replace(state);
    }

    fn audio_state(&self) -> PlaybackAudioState {
        self.audio
    }

    fn set_volume(&mut self, volume: f64) -> Result<(), PlaybackSessionFailure> {
        if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
            return Err(PlaybackSessionFailure::InvalidVolume);
        }
        self.set_audio(PlaybackAudioState {
            volume,
            ..self.audio
        })
    }

    fn set_muted(&mut self, muted: bool) -> Result<(), PlaybackSessionFailure> {
        self.set_audio(PlaybackAudioState {
            muted,
            ..self.audio
        })
    }

    fn set_audio(&mut self, audio: PlaybackAudioState) -> Result<(), PlaybackSessionFailure> {
        if self.shut_down {
            return Err(PlaybackSessionFailure::ShutDown);
        }
        if self.teardown_failed {
            return Err(PlaybackSessionFailure::PipelineTeardown);
        }
        if self.audio == audio {
            return Ok(());
        }
        if let Some(active) = self.active.as_mut() {
            self.backend.set_audio(&mut active.pipeline, audio)?;
        }
        self.audio = audio;
        Ok(())
    }

    fn begin_tune(
        &mut self,
        selection: StreamSelection,
    ) -> Result<TuneRequest, PlaybackSessionFailure> {
        if self.shut_down {
            return Err(PlaybackSessionFailure::ShutDown);
        }
        tracing::info!(target: "balun::playback", channel = ?selection.channel_key(), "tune requested");
        if self.teardown_failed {
            return Err(PlaybackSessionFailure::PipelineTeardown);
        }
        if self.exhausted {
            return Err(PlaybackSessionFailure::GenerationExhausted);
        }

        let channel_key = selection.channel_key().clone();
        let Some(generation) = self.generation.checked_next() else {
            self.exhausted = true;
            self.pending = None;
            if let Err(failure) = self.retire_active() {
                self.publish_state(PlaybackSessionState::Failed {
                    generation: self.generation,
                    channel_key,
                    failure,
                });
                return Err(failure);
            }
            self.publish_state(PlaybackSessionState::Failed {
                generation: self.generation,
                channel_key,
                failure: PlaybackSessionFailure::GenerationExhausted,
            });
            return Err(PlaybackSessionFailure::GenerationExhausted);
        };
        // Publish the successor generation before touching the predecessor so
        // every callback from it is stale even during bounded teardown.
        self.generation = generation;
        self.pending = None;
        if let Err(failure) = self.retire_active() {
            self.publish_state(PlaybackSessionState::Failed {
                generation,
                channel_key,
                failure,
            });
            return Err(failure);
        }

        let selection_generation = selection.selection_generation();
        self.pending = Some(PendingTune {
            generation,
            channel_key: channel_key.clone(),
            selection_generation,
        });
        self.publish_state(PlaybackSessionState::Resolving {
            generation,
            channel_key,
        });
        Ok(TuneRequest {
            generation,
            selection,
        })
    }

    fn complete_tune(
        &mut self,
        request: TuneRequest,
        response: Result<StreamHandoff, StreamHandoffError>,
        events: EventSink,
    ) -> Result<TuneCompletion, PlaybackSessionFailure> {
        if self.shut_down || !self.request_is_current(&request) {
            drop(response);
            return Ok(TuneCompletion::Stale);
        }

        let pending = self
            .pending
            .take()
            .expect("a current request must retain its pending tune");
        let handoff = match response {
            Ok(handoff) => handoff,
            Err(error) => {
                let failure = PlaybackSessionFailure::Handoff(error);
                self.fail_pending(pending, failure);
                return Err(failure);
            }
        };
        if handoff.channel_key() != &pending.channel_key
            || handoff.selection_generation() != pending.selection_generation
        {
            let failure = PlaybackSessionFailure::HandoffMismatch;
            self.fail_pending(pending, failure);
            return Err(failure);
        }

        let generation = pending.generation;
        let channel_key = pending.channel_key;
        let pipeline = match self.backend.start(generation, handoff, self.audio, events) {
            Ok(pipeline) => pipeline,
            Err(PipelineStartError::Clean(failure)) => {
                self.publish_state(PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                });
                return Err(failure);
            }
            Err(PipelineStartError::Quarantined(pipeline)) => {
                let failure = PlaybackSessionFailure::PipelineTeardown;
                self.teardown_failed = true;
                self.active = Some(ActiveTune {
                    generation,
                    channel_key: channel_key.clone(),
                    pipeline,
                });
                self.publish_state(PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                });
                return Err(failure);
            }
        };
        self.active = Some(ActiveTune {
            generation,
            channel_key: channel_key.clone(),
            pipeline,
        });
        self.publish_state(PlaybackSessionState::Connecting {
            generation,
            channel_key,
        });
        Ok(TuneCompletion::Applied)
    }

    fn cancel_tune(&mut self, request: TuneRequest) -> bool {
        if !self.request_is_current(&request) {
            return false;
        }
        self.pending = None;
        self.publish_state(PlaybackSessionState::Stopped);
        true
    }

    fn stop(&mut self) -> Result<(), PlaybackSessionFailure> {
        if self.teardown_failed {
            return Err(PlaybackSessionFailure::PipelineTeardown);
        }
        if self.shut_down {
            return Ok(());
        }
        if let Some(next) = self.generation.checked_next() {
            self.generation = next;
        } else {
            self.exhausted = true;
        }
        self.pending = None;
        let channel_key = self
            .active
            .as_ref()
            .map(|active| active.channel_key.clone());
        if let Err(failure) = self.retire_active() {
            if let Some(channel_key) = channel_key {
                self.publish_state(PlaybackSessionState::Failed {
                    generation: self.generation,
                    channel_key,
                    failure,
                });
            }
            return Err(failure);
        }
        self.publish_state(PlaybackSessionState::Stopped);
        Ok(())
    }

    fn shut_down(&mut self) -> Result<(), PlaybackSessionFailure> {
        if self.shut_down {
            return if self.teardown_failed {
                Err(PlaybackSessionFailure::PipelineTeardown)
            } else {
                Ok(())
            };
        }
        self.shut_down = true;
        self.pending = None;
        let prior_teardown_failure = self.teardown_failed;
        let result = self.retire_active();
        self.publish_state(PlaybackSessionState::ShutDown);
        if prior_teardown_failure && result.is_ok() {
            Err(PlaybackSessionFailure::PipelineTeardown)
        } else {
            result
        }
    }

    fn handle_event(&mut self, generation: TuneGeneration, event: PipelineEvent) {
        if self.shut_down || self.teardown_failed || self.exhausted || self.generation != generation
        {
            return;
        }
        let Some(active) = self.active.as_ref() else {
            return;
        };
        if active.generation != generation {
            return;
        }
        let channel_key = active.channel_key.clone();
        tracing::debug!(target: "balun::playback", ?event, generation = generation.get(), "pipeline event");
        match event {
            PipelineEvent::StreamStarted => {
                let Some(active) = self.active.as_mut() else {
                    return;
                };
                if let Err(failure) = self.backend.play(&mut active.pipeline) {
                    let failure = if self.retire_active().is_ok() {
                        failure
                    } else {
                        PlaybackSessionFailure::PipelineTeardown
                    };
                    self.publish_state(PlaybackSessionState::Failed {
                        generation,
                        channel_key,
                        failure,
                    });
                }
            }
            PipelineEvent::Playing => {
                self.publish_state(PlaybackSessionState::Playing {
                    generation,
                    channel_key,
                });
            }
            PipelineEvent::Buffering(percent) => {
                self.publish_state(PlaybackSessionState::Buffering {
                    generation,
                    channel_key,
                    percent: percent.min(100),
                });
            }
            PipelineEvent::EndOfStream => {
                if self.retire_active().is_ok() {
                    self.publish_state(PlaybackSessionState::Stopped);
                } else {
                    self.publish_state(PlaybackSessionState::Failed {
                        generation,
                        channel_key,
                        failure: PlaybackSessionFailure::PipelineTeardown,
                    });
                }
            }
            PipelineEvent::Error(pipeline_failure) => {
                let failure = if self.retire_active().is_ok() {
                    PlaybackSessionFailure::Pipeline(pipeline_failure)
                } else {
                    PlaybackSessionFailure::PipelineTeardown
                };
                self.publish_state(PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                });
            }
        }
    }

    fn request_is_current(&self, request: &TuneRequest) -> bool {
        self.generation == request.generation
            && self.pending.as_ref().is_some_and(|pending| {
                pending.generation == request.generation
                    && pending.channel_key == *request.selection.channel_key()
                    && pending.selection_generation == request.selection.selection_generation()
            })
    }

    fn fail_pending(&mut self, pending: PendingTune, failure: PlaybackSessionFailure) {
        self.publish_state(PlaybackSessionState::Failed {
            generation: pending.generation,
            channel_key: pending.channel_key,
            failure,
        });
    }

    fn retire_active(&mut self) -> Result<(), PlaybackSessionFailure> {
        let Some(active) = self.active.as_mut() else {
            return Ok(());
        };
        let result = self.backend.stop(&mut active.pipeline);
        if result.is_err() {
            // Once NULL settlement is unproven, this process must never open a
            // successor pipeline. Retain the quarantined owner so terminal
            // shutdown and Drop can issue another fail-safe NULL request.
            self.teardown_failed = true;
        } else {
            self.active.take();
        }
        result
    }
}

impl<B: PipelineBackend> Drop for SessionCore<B> {
    fn drop(&mut self) {
        self.pending = None;
        if let Some(mut active) = self.active.take() {
            let _ = self.backend.stop(&mut active.pipeline);
        }
    }
}

struct GstreamerBackend {
    _runtime: PlaybackRuntime,
    main_context: gst::glib::MainContext,
}

struct GstreamerPipeline {
    source_policy: SourcePolicy,
    unjoined_transport: Option<StreamTransport>,
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
    bus_watch: Option<gst::bus::BusWatchGuard>,
    armed: bool,
}

impl GstreamerPipeline {
    fn stop(&mut self) -> Result<(), PlaybackSessionFailure> {
        let deadline = Instant::now() + PIPELINE_TEARDOWN_TIMEOUT;
        // Detach callbacks before requesting NULL so teardown messages cannot
        // mutate the settled generation.
        self.bus_watch.take();
        // Cancel the private HTTP request first so the device connection
        // starts closing while the pipeline settles. The transport is joined
        // only after NULL, because a flushing appsrc is what unblocks a feeder
        // waiting on the bounded byte limit.
        let mut transport = self
            .source_policy
            .retire()
            .or_else(|| self.unjoined_transport.take());
        // Silence the owned stream before the potentially blocking NULL wait.
        // This is a fail-safe pipeline action and does not alter the retained
        // process-local mute preference inherited by a successor.
        self.pipeline.set_property(PLAYBIN_MUTE_PROPERTY, true);
        let request = self.pipeline.set_state(gst::State::Null);
        let (transition, current, pending) = self.pipeline.state(clock_time_until(deadline));
        let settled = request.is_ok()
            && transition.is_ok()
            && current == gst::State::Null
            && pending == gst::State::VoidPending;
        let joined = match transport.as_mut() {
            Some(transport) => transport.join(deadline).is_ok(),
            None => true,
        };
        if settled && joined {
            self.armed = false;
            Ok(())
        } else {
            // Retain an unjoined transport so a later retry can prove that the
            // device connection was released, not merely requested closed.
            if !joined {
                self.unjoined_transport = transport;
            }
            Err(PlaybackSessionFailure::PipelineTeardown)
        }
    }
}

impl Drop for GstreamerPipeline {
    fn drop(&mut self) {
        self.bus_watch.take();
        if let Some(transport) = self.source_policy.retire() {
            drop(transport);
        }
        self.unjoined_transport.take();
        if self.armed {
            let _ = self.pipeline.set_state(gst::State::Null);
            self.armed = false;
        }
    }
}

fn clock_time_until(deadline: Instant) -> gst::ClockTime {
    let remaining = deadline.saturating_duration_since(Instant::now());
    gst::ClockTime::from_nseconds(remaining.as_nanos().min(u128::from(u64::MAX)) as u64)
}

impl PipelineBackend for GstreamerBackend {
    type Active = GstreamerPipeline;

    fn start(
        &mut self,
        generation: TuneGeneration,
        handoff: StreamHandoff,
        audio: PlaybackAudioState,
        events: EventSink,
    ) -> Result<Self::Active, PipelineStartError<Self::Active>> {
        let video_sink = gst::ElementFactory::make("gtk4paintablesink")
            .build()
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?;
        if !video_sink.has_property("paintable") {
            return Err(PipelineStartError::Clean(
                PlaybackSessionFailure::PipelineConstruction,
            ));
        }
        let paintable = video_sink
            .property_value("paintable")
            .get::<gdk::Paintable>()
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?;
        configure_paintable_aspect(&paintable).map_err(PipelineStartError::Clean)?;
        let pipeline = gst::ElementFactory::make("playbin3")
            .build()
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?;
        configure_playbin_video(&pipeline, &video_sink).map_err(PipelineStartError::Clean)?;
        configure_playbin_audio(&pipeline, audio).map_err(PipelineStartError::Clean)?;

        let bus = pipeline.bus().ok_or(PipelineStartError::Clean(
            PlaybackSessionFailure::BusUnavailable,
        ))?;
        let watched_pipeline = pipeline.clone();
        let watch = move |_: &gst::Bus, message: &gst::Message| {
            pipeline_failure::log_pipeline_message(message);
            let event = match message.view() {
                gst::MessageView::Eos(_) => Some(PipelineEvent::EndOfStream),
                gst::MessageView::Application(application)
                    if message.src().is_some_and(|source| {
                        source == watched_pipeline.upcast_ref::<gst::Object>()
                    }) && application
                        .structure()
                        .is_some_and(|structure| structure.name() == STREAM_STARTED_MESSAGE) =>
                {
                    Some(PipelineEvent::StreamStarted)
                }
                gst::MessageView::Error(_)
                | gst::MessageView::Element(_)
                | gst::MessageView::Application(_) => {
                    pipeline_failure::classify_pipeline_message(message, &watched_pipeline)
                        .map(PipelineEvent::Error)
                }
                gst::MessageView::Buffering(buffering) => {
                    let percent = buffering.percent().clamp(0, 100) as u8;
                    Some(PipelineEvent::Buffering(percent))
                }
                gst::MessageView::StateChanged(state_changed)
                    if message.src().is_some_and(|source| {
                        source == watched_pipeline.upcast_ref::<gst::Object>()
                    }) && state_changed.current() == gst::State::Playing =>
                {
                    Some(PipelineEvent::Playing)
                }
                _ => None,
            };
            let terminal = matches!(
                event,
                Some(PipelineEvent::EndOfStream | PipelineEvent::Error(_))
            );
            if let Some(PipelineEvent::Error(failure)) = event {
                tracing::warn!(target: "balun::playback", category = %failure, "playback failed");
            }
            if let Some(event) = event {
                events(generation, event);
            }
            if terminal {
                gst::glib::ControlFlow::Break
            } else {
                gst::glib::ControlFlow::Continue
            }
        };
        let bus_watch = self
            .main_context
            .with_thread_default(|| bus.add_watch_local(watch))
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::BusWatch))?
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::BusWatch))?;
        // Every fallible structural check precedes this point. GStreamer only
        // ever receives the constant endpoint-free URI; the authorized handoff
        // moves into the source policy's private state and is consumed by the
        // transport when playbin3 delivers its exact appsrc during start.
        let source_policy = SourcePolicy::install(&pipeline, handoff, TransportConfig::PRODUCTION)
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?;
        pipeline.set_property("uri", PIPELINE_URI);
        if pipeline.property::<Option<String>>("uri").as_deref() != Some(PIPELINE_URI) {
            return Err(PipelineStartError::Clean(
                PlaybackSessionFailure::PipelineConstruction,
            ));
        }
        let mut active = GstreamerPipeline {
            source_policy,
            unjoined_transport: None,
            pipeline,
            paintable,
            bus_watch: Some(bus_watch),
            armed: true,
        };
        // Hold at PAUSED: a live source reaches it without preroll, the
        // transport starts fetching, and PLAYING (which fixes the running
        // clock's base time) waits for the first stream bytes. Otherwise the
        // tuner lock and connection time eat the demuxer's live latency budget
        // and every later buffer arrives late, which the audio sink renders as
        // clipped, stuttering sound until the next tune.
        let start_result = active.pipeline.set_state(gst::State::Paused);
        let source_rejected = active.source_policy.is_rejected();
        let start_failure = if source_rejected {
            PlaybackSessionFailure::Pipeline(PlaybackPipelineFailure::Internal)
        } else {
            PlaybackSessionFailure::PipelineStart
        };
        if start_result.is_err() || source_rejected {
            return match active.stop() {
                Ok(()) => Err(PipelineStartError::Clean(start_failure)),
                Err(_) => Err(PipelineStartError::Quarantined(active)),
            };
        }
        Ok(active)
    }

    fn set_audio(
        &mut self,
        active: &mut Self::Active,
        audio: PlaybackAudioState,
    ) -> Result<(), PlaybackSessionFailure> {
        configure_playbin_audio(&active.pipeline, audio)
    }

    fn play(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure> {
        active
            .pipeline
            .set_state(gst::State::Playing)
            .map(|_| ())
            .map_err(|_| PlaybackSessionFailure::PipelineStart)
    }

    fn stop(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure> {
        active.stop()
    }
}

/// Apply the deinterlace flag, forced aspect preservation, and the exact
/// video sink, and read every setting back. Shared with the packaged probe.
pub(super) fn configure_playbin_video(
    pipeline: &gst::Pipeline,
    video_sink: &gst::Element,
) -> Result<(), PlaybackSessionFailure> {
    if ["flags", "force-aspect-ratio", "uri", "video-sink"]
        .into_iter()
        .any(|property| !pipeline.has_property(property))
    {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }

    let flags = pipeline.property_value("flags");
    let flags_class = gst::glib::FlagsClass::with_type(flags.type_())
        .ok_or(PlaybackSessionFailure::PipelineConstruction)?;
    let flags = flags_class
        .builder_with_value(flags)
        .and_then(|builder| builder.set_by_nick(DEINTERLACE_PLAY_FLAG).build())
        .ok_or(PlaybackSessionFailure::PipelineConstruction)?;
    pipeline.set_property("flags", flags);
    pipeline.set_property("force-aspect-ratio", true);
    pipeline.set_property("video-sink", video_sink);

    let configured_flags = pipeline.property_value("flags");
    let configured_sink = pipeline.property::<Option<gst::Element>>("video-sink");
    if !flags_class.is_set_by_nick(&configured_flags, DEINTERLACE_PLAY_FLAG)
        || !pipeline.property::<bool>("force-aspect-ratio")
        || configured_sink.as_ref() != Some(video_sink)
    {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }
    Ok(())
}

fn configure_playbin_audio(
    pipeline: &gst::Pipeline,
    audio: PlaybackAudioState,
) -> Result<(), PlaybackSessionFailure> {
    let volume_property = pipeline
        .find_property(PLAYBIN_VOLUME_PROPERTY)
        .filter(|property| property.value_type() == f64::static_type())
        .filter(|property| {
            property
                .flags()
                .contains(gst::glib::ParamFlags::READABLE | gst::glib::ParamFlags::WRITABLE)
        });
    let mute_property = pipeline
        .find_property(PLAYBIN_MUTE_PROPERTY)
        .filter(|property| property.value_type() == bool::static_type())
        .filter(|property| {
            property
                .flags()
                .contains(gst::glib::ParamFlags::READABLE | gst::glib::ParamFlags::WRITABLE)
        });
    if volume_property.is_none() || mute_property.is_none() {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }

    let gain = playback_gain(audio.volume());
    pipeline.set_property(PLAYBIN_VOLUME_PROPERTY, gain);
    pipeline.set_property(PLAYBIN_MUTE_PROPERTY, audio.is_muted());
    if pipeline.property::<f64>(PLAYBIN_VOLUME_PROPERTY) != gain
        || pipeline.property::<bool>(PLAYBIN_MUTE_PROPERTY) != audio.is_muted()
    {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }
    Ok(())
}

fn playback_gain(volume: f64) -> f64 {
    volume * volume * volume
}

fn configure_paintable_aspect(paintable: &gdk::Paintable) -> Result<(), PlaybackSessionFailure> {
    if !paintable.has_property(PAINTABLE_ASPECT_PROPERTY) {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }
    paintable.set_property(PAINTABLE_ASPECT_PROPERTY, true);
    if !paintable.property::<bool>(PAINTABLE_ASPECT_PROPERTY) {
        return Err(PlaybackSessionFailure::PipelineConstruction);
    }
    Ok(())
}

fn queue_main_context_work(
    main_context: &gst::glib::MainContext,
    mut work: impl FnMut() -> bool + 'static,
) {
    // The local bus watch is attached to this exact context, so its callback
    // owns the context and can safely attach a non-Send future here. Always
    // defer reduction until after the native callback returns. If a nested
    // main loop dispatches the future while a public method still owns the
    // core borrow, retry on the same context instead of panicking or losing a
    // terminal event after the bus watch has returned Break.
    let task = main_context.spawn_local(async move {
        loop {
            if work() {
                return;
            }
            gst::glib::timeout_future(Duration::from_millis(1)).await;
        }
    });
    drop(task);
}

/// Default-main-context owner for one serialized playback lane.
///
/// The session is deliberately neither `Send` nor `Sync`. It owns the runtime,
/// exact active pipeline, and generation-tagged local bus watch. Dropping the
/// session performs a final fail-safe `NULL` request; normal window shutdown
/// should call [`Self::shut_down`] so bounded settlement is observable.
pub struct PlaybackSession {
    inner: Rc<RefCell<SessionCore<GstreamerBackend>>>,
    main_context: gst::glib::MainContext,
    foundation_ready: bool,
}

impl PlaybackSession {
    /// Construct the one playback owner without opening a URI or pipeline.
    #[must_use]
    pub fn new(runtime: PlaybackRuntime) -> Self {
        let foundation_ready = runtime.capabilities().is_foundation_ready();
        let main_context = gst::glib::MainContext::default();
        Self {
            inner: Rc::new(RefCell::new(SessionCore::new(GstreamerBackend {
                _runtime: runtime,
                main_context: main_context.clone(),
            }))),
            main_context,
            foundation_ready,
        }
    }

    /// Return whether the startup capability snapshot permits a tune attempt.
    #[must_use]
    pub const fn is_foundation_ready(&self) -> bool {
        self.foundation_ready
    }

    fn require_main_context(&self) -> Result<(), PlaybackSessionFailure> {
        if self.main_context.is_owner() {
            Ok(())
        } else {
            Err(PlaybackSessionFailure::MainContextUnavailable)
        }
    }

    /// Assign a successor generation, then settle its invalidated predecessor
    /// before the caller submits the URL-free selection to the controller.
    pub fn begin_tune(
        &self,
        selection: StreamSelection,
    ) -> Result<TuneRequest, PlaybackSessionFailure> {
        if !self.foundation_ready {
            return Err(PlaybackSessionFailure::ComponentsUnavailable);
        }
        self.require_main_context()?;
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .begin_tune(selection)
    }

    /// Apply one controller result and start `playbin3` with a library-owned
    /// GTK paintable sink. A superseded response is dropped and zeroized.
    pub fn complete_tune(
        &self,
        request: TuneRequest,
        response: Result<StreamHandoff, StreamHandoffError>,
    ) -> Result<TuneCompletion, PlaybackSessionFailure> {
        self.require_main_context()?;
        let weak: Weak<RefCell<SessionCore<GstreamerBackend>>> = Rc::downgrade(&self.inner);
        let main_context = self.main_context.clone();
        let events: EventSink = Rc::new(move |generation, event| {
            let weak = weak.clone();
            queue_main_context_work(&main_context, move || {
                let Some(inner) = weak.upgrade() else {
                    return true;
                };
                match inner.try_borrow_mut() {
                    Ok(mut inner) => {
                        inner.handle_event(generation, event);
                        true
                    }
                    Err(_) => false,
                }
            });
        });
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .complete_tune(request, response, events)
    }

    /// Return the current URI-opaque GTK paintable, if a pipeline is active.
    ///
    /// The native sink and pipeline never cross the library boundary, so a
    /// desktop caller cannot traverse GStreamer parents to read its URI.
    pub fn paintable(&self) -> Result<Option<gdk::Paintable>, PlaybackSessionFailure> {
        self.require_main_context()?;
        let inner = self
            .inner
            .try_borrow()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?;
        if inner.teardown_failed {
            return Ok(None);
        }
        Ok(inner
            .active
            .as_ref()
            .map(|active| active.pipeline.paintable.clone()))
    }

    /// Cancel a pending request after controller admission failed.
    pub fn cancel_tune(&self, request: TuneRequest) -> Result<bool, PlaybackSessionFailure> {
        self.require_main_context()?;
        Ok(self
            .inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .cancel_tune(request))
    }

    /// Return the process-local audio settings inherited by successor tunes.
    pub fn audio_state(&self) -> Result<PlaybackAudioState, PlaybackSessionFailure> {
        self.require_main_context()?;
        Ok(self
            .inner
            .try_borrow()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .audio_state())
    }

    /// Set linear volume for the current pipeline and every successor tune.
    ///
    /// The accepted range is finite `0.0..=1.0`. Muting remains independent,
    /// so changing this value while muted selects the later unmuted level.
    pub fn set_volume(&self, volume: f64) -> Result<(), PlaybackSessionFailure> {
        self.require_main_context()?;
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .set_volume(volume)
    }

    /// Set mute for the current pipeline and every successor tune.
    pub fn set_muted(&self, muted: bool) -> Result<(), PlaybackSessionFailure> {
        self.require_main_context()?;
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .set_muted(muted)
    }

    /// Invalidate pending work and settle the active pipeline to `NULL`.
    pub fn stop(&self) -> Result<(), PlaybackSessionFailure> {
        self.require_main_context()?;
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .stop()
    }

    /// Perform terminal, idempotent teardown for window or process shutdown.
    pub fn shut_down(&self) -> Result<(), PlaybackSessionFailure> {
        self.require_main_context()?;
        self.inner
            .try_borrow_mut()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .shut_down()
    }

    /// Subscribe to the latest URL-free state of this playback lane.
    ///
    /// State is published only by mutations which own Balun's default main
    /// context. The initial value is the exact current state, and later
    /// generation-scoped bus transitions replace it without retaining native
    /// message text or stream endpoint data.
    pub fn subscribe_state(
        &self,
    ) -> Result<watch::Receiver<PlaybackSessionState>, PlaybackSessionFailure> {
        self.require_main_context()?;
        Ok(self
            .inner
            .try_borrow()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .subscribe_state())
    }

    /// Clone the URL-free current session state.
    pub fn state(&self) -> Result<PlaybackSessionState, PlaybackSessionFailure> {
        self.require_main_context()?;
        Ok(self
            .inner
            .try_borrow()
            .map_err(|_| PlaybackSessionFailure::SessionBusy)?
            .state()
            .clone())
    }
}

impl fmt::Debug for PlaybackSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = formatter.debug_struct("PlaybackSession");
        debug.field("foundation_ready", &self.foundation_ready);
        match self.inner.try_borrow() {
            Ok(inner) => {
                debug.field("state", inner.state());
            }
            Err(_) => {
                debug.field("state", &"<busy>");
            }
        }
        debug.finish()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{DeviceId, GuideNumber};
    use crate::playback::test_support::{
        FixtureStreamServer, StreamBehavior, fixture_response, hold_decoder_selection,
    };
    use gtk::prelude::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Start(TuneGeneration, ChannelKey),
        Play(usize),
        Stop(usize),
    }

    #[derive(Clone, Copy)]
    enum FakeStartFailure {
        Clean,
        Quarantined,
    }

    #[derive(Default)]
    struct FakeShared {
        calls: Vec<Call>,
        events: BTreeMap<TuneGeneration, EventSink>,
        start_audio: BTreeMap<TuneGeneration, PlaybackAudioState>,
        audio_updates: Vec<(Option<usize>, PlaybackAudioState)>,
        next_pipeline: usize,
        fail_start: Option<FakeStartFailure>,
        fail_audio: bool,
        fail_play: bool,
        fail_stop: bool,
        event_during_start: Option<PipelineEvent>,
        event_during_stop: Option<PipelineEvent>,
    }

    #[derive(Clone, Default)]
    struct FakeControl(Rc<RefCell<FakeShared>>);

    impl FakeControl {
        fn backend(&self) -> FakeBackend {
            FakeBackend(self.clone())
        }

        fn calls(&self) -> Vec<Call> {
            self.0.borrow().calls.clone()
        }

        fn emit(&self, generation: TuneGeneration, event: PipelineEvent) {
            let sink = self
                .0
                .borrow()
                .events
                .get(&generation)
                .cloned()
                .expect("the fake pipeline generation must exist");
            sink(generation, event);
        }
    }

    struct FakeBackend(FakeControl);

    impl PipelineBackend for FakeBackend {
        type Active = usize;

        fn start(
            &mut self,
            generation: TuneGeneration,
            handoff: StreamHandoff,
            audio: PlaybackAudioState,
            events: EventSink,
        ) -> Result<Self::Active, PipelineStartError<Self::Active>> {
            let mut shared = self.0.0.borrow_mut();
            if matches!(shared.fail_start, Some(FakeStartFailure::Clean)) {
                return Err(PipelineStartError::Clean(
                    PlaybackSessionFailure::PipelineStart,
                ));
            }
            let pipeline = shared.next_pipeline;
            shared.next_pipeline += 1;
            shared
                .calls
                .push(Call::Start(generation, handoff.channel_key().clone()));
            shared.start_audio.insert(generation, audio);
            shared.events.insert(generation, events);
            drop(handoff);
            let start_failure = shared.fail_start;
            let synchronous_event = shared.event_during_start;
            let event_sink = shared.events.get(&generation).cloned();
            drop(shared);
            if let (Some(event_sink), Some(event)) = (event_sink, synchronous_event) {
                event_sink(generation, event);
            }
            if matches!(start_failure, Some(FakeStartFailure::Quarantined)) {
                Err(PipelineStartError::Quarantined(pipeline))
            } else {
                Ok(pipeline)
            }
        }

        fn set_audio(
            &mut self,
            active: &mut Self::Active,
            audio: PlaybackAudioState,
        ) -> Result<(), PlaybackSessionFailure> {
            let mut shared = self.0.0.borrow_mut();
            if shared.fail_audio {
                return Err(PlaybackSessionFailure::PipelineConstruction);
            }
            shared.audio_updates.push((Some(*active), audio));
            Ok(())
        }

        fn play(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure> {
            let mut shared = self.0.0.borrow_mut();
            if shared.fail_play {
                return Err(PlaybackSessionFailure::PipelineStart);
            }
            shared.calls.push(Call::Play(*active));
            Ok(())
        }

        fn stop(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure> {
            let mut shared = self.0.0.borrow_mut();
            shared.calls.push(Call::Stop(*active));
            let fail_stop = shared.fail_stop;
            let synchronous_event = shared.event_during_stop;
            let event_sink = shared
                .events
                .last_key_value()
                .map(|(generation, sink)| (*generation, Rc::clone(sink)));
            drop(shared);
            if let (Some((generation, event_sink)), Some(event)) = (event_sink, synchronous_event) {
                event_sink(generation, event);
            }
            if fail_stop {
                Err(PlaybackSessionFailure::PipelineTeardown)
            } else {
                Ok(())
            }
        }
    }

    fn first_key() -> ChannelKey {
        ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("5.1").unwrap(),
        )
    }

    fn second_key() -> ChannelKey {
        ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        )
    }

    fn selection(key: &ChannelKey, generation: u64) -> StreamSelection {
        StreamSelection::new(key.clone(), OperationGeneration::new(generation))
    }

    fn handoff(key: &ChannelKey, generation: u64) -> StreamHandoff {
        StreamHandoff::test_fixture(
            key.clone(),
            OperationGeneration::new(generation),
            "http://192.0.2.10:5004/auto/v5.1",
        )
    }

    fn events_for<B: PipelineBackend + 'static>(core: &Rc<RefCell<SessionCore<B>>>) -> EventSink {
        let weak = Rc::downgrade(core);
        Rc::new(move |generation, event| {
            if let Some(core) = weak.upgrade() {
                core.borrow_mut().handle_event(generation, event);
            }
        })
    }

    fn queued_events_for<B: PipelineBackend + 'static>(
        main_context: &gst::glib::MainContext,
        core: &Rc<RefCell<SessionCore<B>>>,
    ) -> EventSink {
        let main_context = main_context.clone();
        let weak = Rc::downgrade(core);
        Rc::new(move |generation, event| {
            let weak = weak.clone();
            queue_main_context_work(&main_context, move || {
                let Some(core) = weak.upgrade() else {
                    return true;
                };
                match core.try_borrow_mut() {
                    Ok(mut core) => {
                        core.handle_event(generation, event);
                        true
                    }
                    Err(_) => false,
                }
            });
        })
    }

    fn drive_context_until(
        main_context: &gst::glib::MainContext,
        mut complete: impl FnMut() -> bool,
    ) {
        for _ in 0..100 {
            while main_context.pending() {
                main_context.iteration(false);
            }
            if complete() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the queued main-context work did not settle");
    }

    #[test]
    fn available_playbin_contract_enables_video_and_applies_audio_before_uri() {
        if gst::init().is_err() {
            return;
        }
        let Some(factory) = gst::ElementFactory::find("playbin3") else {
            return;
        };
        let pipeline = factory
            .create()
            .build()
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let video_sink = gst::Bin::new().upcast::<gst::Element>();
        let initial_flags = pipeline.property_value("flags");
        let initial_flags_class = gst::glib::FlagsClass::with_type(initial_flags.type_()).unwrap();
        let without_deinterlace = initial_flags_class
            .builder_with_value(initial_flags)
            .unwrap()
            .unset_by_nick(DEINTERLACE_PLAY_FLAG)
            .build()
            .unwrap();
        pipeline.set_property("flags", without_deinterlace);
        assert!(
            !initial_flags_class
                .is_set_by_nick(&pipeline.property_value("flags"), DEINTERLACE_PLAY_FLAG)
        );

        configure_playbin_video(&pipeline, &video_sink).unwrap();
        let audio = PlaybackAudioState {
            volume: 0.35,
            muted: true,
        };
        configure_playbin_audio(&pipeline, audio).unwrap();

        let flags = pipeline.property_value("flags");
        let flags_class = gst::glib::FlagsClass::with_type(flags.type_()).unwrap();
        assert!(flags_class.is_set_by_nick(&flags, DEINTERLACE_PLAY_FLAG));
        assert!(flags_class.is_set_by_nick(&flags, "buffering"));
        assert!(pipeline.property::<bool>("force-aspect-ratio"));
        assert_eq!(
            pipeline.property::<Option<gst::Element>>("video-sink"),
            Some(video_sink)
        );
        assert_eq!(
            pipeline.property::<Option<gst::Element>>("video-filter"),
            None
        );
        assert_eq!(pipeline.property::<Option<String>>("uri"), None);
        assert_eq!(
            pipeline.property::<f64>(PLAYBIN_VOLUME_PROPERTY),
            playback_gain(0.35)
        );
        assert!(pipeline.property::<bool>(PLAYBIN_MUTE_PROPERTY));
        assert_eq!(playback_gain(0.0), 0.0);
        assert_eq!(playback_gain(0.5), 0.125);
        assert_eq!(playback_gain(1.0), 1.0);

        let propertyless_pipeline = gst::Pipeline::new();
        assert_eq!(
            configure_playbin_audio(&propertyless_pipeline, PlaybackAudioState::default()),
            Err(PlaybackSessionFailure::PipelineConstruction)
        );
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display- and codec-dependent production proof.
    ///
    /// The checked-in fixture is served by a loopback HTTP listener, so the
    /// real production session exercises the private transport, the exact
    /// `appsrc` feed, decoding into the URI-opaque paintable, natural EOS, and
    /// joined teardown without any external network or tuner.
    #[test]
    #[ignore = "requires the isolated display and complete playback runtime supplied by scripts/test-desktop-lifecycle.sh"]
    fn active_production_session_exposes_opaque_paintable_and_shuts_down() {
        // Decoding autoplugs from the shared registry; hold the selection lock
        // through shutdown so a concurrent rank override cannot be observed.
        let _decoder_selection = hold_decoder_selection();
        adw::init().expect("initialize libadwaita for active-session presentation proof");
        let main_context = gst::glib::MainContext::default();
        let _owner = main_context
            .acquire()
            .expect("acquire the default main context for active-session proof");
        let runtime = PlaybackRuntime::initialize()
            .expect("initialize the complete production playback runtime");
        assert!(
            runtime.capabilities().is_foundation_ready(),
            "the lifecycle harness must install Balun's complete playback foundation"
        );
        let session = PlaybackSession::new(runtime);
        assert_eq!(
            session.audio_state().unwrap(),
            PlaybackAudioState::default()
        );
        session.set_volume(0.35).unwrap();
        session.set_muted(true).unwrap();
        assert_eq!(
            session.audio_state().unwrap(),
            PlaybackAudioState {
                volume: 0.35,
                muted: true,
            }
        );
        let channel_key = first_key();
        let selection_generation = OperationGeneration::new(23);
        let mut states = session
            .subscribe_state()
            .expect("subscribe to the URL-free session state");
        let request = session
            .begin_tune(StreamSelection::new(
                channel_key.clone(),
                selection_generation,
            ))
            .expect("begin one generation-owned loopback fixture tune");
        let server = FixtureStreamServer::start(fixture_response(), StreamBehavior::Close);
        let handoff = StreamHandoff::test_fixture(
            channel_key.clone(),
            selection_generation,
            &server.stream_url(),
        );

        let request_generation = request.generation();
        assert_eq!(
            session.complete_tune(request, Ok(handoff)),
            Ok(TuneCompletion::Applied)
        );
        assert!(matches!(
            session.state().unwrap(),
            PlaybackSessionState::Connecting {
                channel_key: active,
                ..
            } if active == channel_key
        ));
        let paintable = session
            .paintable()
            .expect("read the URI-opaque active paintable")
            .expect("an applied production pipeline must publish a paintable");
        assert!(paintable.property::<bool>(PAINTABLE_ASPECT_PROPERTY));

        let picture = gtk::Picture::for_paintable(&paintable);
        picture.set_content_fit(gtk::ContentFit::Contain);
        let window = gtk::Window::builder()
            .title("Balun active-session presentation proof")
            .default_width(320)
            .default_height(192)
            .child(&picture)
            .build();
        window.present();
        assert_eq!(picture.paintable().as_ref(), Some(&paintable));
        assert_eq!(picture.content_fit(), gtk::ContentFit::Contain);

        session.set_volume(0.65).unwrap();
        session.set_muted(false).unwrap();
        assert_eq!(
            session.audio_state().unwrap(),
            PlaybackAudioState {
                volume: 0.65,
                muted: false,
            }
        );

        // Drive the default main context so the generation-scoped bus watch
        // reduces the real Playing transition and the fixture's natural EOS.
        let mut observed_playing = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            while main_context.pending() {
                main_context.iteration(false);
            }
            if states.has_changed().unwrap_or(false) {
                match states.borrow_and_update().clone() {
                    PlaybackSessionState::Playing { generation, .. } => {
                        assert_eq!(generation, request_generation);
                        observed_playing = true;
                    }
                    PlaybackSessionState::Stopped => break,
                    PlaybackSessionState::Failed { failure, .. } => {
                        panic!("the loopback fixture tune failed: {failure}");
                    }
                    _ => {}
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the loopback fixture must reach EOS through the production session"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            observed_playing,
            "the production session must publish PLAYING from the appsrc feed"
        );
        assert_eq!(session.state().unwrap(), PlaybackSessionState::Stopped);
        assert!(
            session.paintable().unwrap().is_none(),
            "EOS retirement settles the exact pipeline and its paintable"
        );
        assert!(server.request(Duration::from_secs(3)).is_some());

        picture.set_paintable(gtk::gdk::Paintable::NONE);
        assert!(picture.paintable().is_none());
        session
            .shut_down()
            .expect("settle the production session after EOS");
        assert_eq!(session.state().unwrap(), PlaybackSessionState::ShutDown);
        assert!(session.paintable().unwrap().is_none());
        window.close();
    }

    #[test]
    fn audio_defaults_and_invalid_volume_fail_without_mutation() {
        let control = FakeControl::default();
        let mut core = SessionCore::new(control.backend());
        assert_eq!(core.audio_state(), PlaybackAudioState::default());

        for invalid in [-0.01, 1.01, f64::NEG_INFINITY, f64::INFINITY, f64::NAN] {
            assert_eq!(
                core.set_volume(invalid),
                Err(PlaybackSessionFailure::InvalidVolume)
            );
            assert_eq!(core.audio_state(), PlaybackAudioState::default());
        }
        assert!(control.0.borrow().audio_updates.is_empty());
    }

    #[test]
    fn audio_settings_apply_to_the_exact_owner_and_every_successor() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));

        core.borrow_mut().set_volume(0.4).unwrap();
        core.borrow_mut().set_muted(true).unwrap();
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 41))
            .unwrap();
        let first_generation = first.generation();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 41)), events_for(&core))
            .unwrap();
        assert_eq!(
            control.0.borrow().start_audio.get(&first_generation),
            Some(&PlaybackAudioState {
                volume: 0.4,
                muted: true,
            })
        );

        // Volume remains independent while muted, and both updates target only
        // the currently active pipeline.
        core.borrow_mut().set_volume(0.65).unwrap();
        core.borrow_mut().set_muted(false).unwrap();
        assert_eq!(
            core.borrow().audio_state(),
            PlaybackAudioState {
                volume: 0.65,
                muted: false,
            }
        );
        assert_eq!(
            control.0.borrow().audio_updates,
            vec![
                (
                    Some(0),
                    PlaybackAudioState {
                        volume: 0.65,
                        muted: true,
                    }
                ),
                (
                    Some(0),
                    PlaybackAudioState {
                        volume: 0.65,
                        muted: false,
                    }
                ),
            ]
        );

        core.borrow_mut().stop().unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 41))
            .unwrap();
        let second_generation = second.generation();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 41)), events_for(&core))
            .unwrap();
        assert_eq!(
            control.0.borrow().start_audio.get(&second_generation),
            Some(&PlaybackAudioState {
                volume: 0.65,
                muted: false,
            })
        );
    }

    #[test]
    fn rejected_audio_update_preserves_settings_and_shutdown_blocks_controls() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 17))
            .unwrap();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 17)), events_for(&core))
            .unwrap();
        control.0.borrow_mut().fail_audio = true;

        assert_eq!(
            core.borrow_mut().set_volume(0.5),
            Err(PlaybackSessionFailure::PipelineConstruction)
        );
        assert_eq!(core.borrow().audio_state(), PlaybackAudioState::default());
        control.0.borrow_mut().fail_audio = false;
        core.borrow_mut().shut_down().unwrap();
        assert_eq!(
            core.borrow_mut().set_muted(true),
            Err(PlaybackSessionFailure::ShutDown)
        );
        assert_eq!(core.borrow().audio_state(), PlaybackAudioState::default());
    }

    #[test]
    fn late_resolution_is_dropped_after_a_successor_begins() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 11))
            .unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 11))
            .unwrap();

        assert_eq!(
            core.borrow_mut().complete_tune(
                first,
                Ok(handoff(&first_key(), 11)),
                events_for(&core),
            ),
            Ok(TuneCompletion::Stale)
        );
        assert!(control.calls().is_empty());
        assert_eq!(
            core.borrow_mut().complete_tune(
                second,
                Ok(handoff(&second_key(), 11)),
                events_for(&core),
            ),
            Ok(TuneCompletion::Applied)
        );
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(2), second_key())]
        );
    }

    #[test]
    fn current_cancel_settles_while_stale_cancel_cannot_cancel_a_successor() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let current = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 11))
            .unwrap();
        assert!(core.borrow_mut().cancel_tune(current));
        assert_eq!(core.borrow().state(), &PlaybackSessionState::Stopped);

        let stale = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 12))
            .unwrap();
        let successor = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 12))
            .unwrap();
        assert!(!core.borrow_mut().cancel_tune(stale));
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Resolving {
                generation: successor.generation(),
                channel_key: second_key(),
            }
        );
        assert_eq!(
            core.borrow_mut().complete_tune(
                successor,
                Ok(handoff(&second_key(), 12)),
                events_for(&core),
            ),
            Ok(TuneCompletion::Applied)
        );
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(3), second_key())]
        );
    }

    #[test]
    fn replacement_stops_the_exact_predecessor_before_starting_successor() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 3))
            .unwrap();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 3)), events_for(&core))
            .unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 3))
            .unwrap();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 3)), events_for(&core))
            .unwrap();

        assert_eq!(
            control.calls(),
            vec![
                Call::Start(TuneGeneration(1), first_key()),
                Call::Stop(0),
                Call::Start(TuneGeneration(2), second_key()),
            ]
        );
    }

    #[test]
    fn failed_predecessor_teardown_blocks_successor_construction() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 3))
            .unwrap();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 3)), events_for(&core))
            .unwrap();
        control.0.borrow_mut().fail_stop = true;

        assert!(matches!(
            core.borrow_mut().begin_tune(selection(&second_key(), 3)),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(1), first_key()), Call::Stop(0),]
        );
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Failed {
                generation: TuneGeneration(2),
                channel_key: second_key(),
                failure: PlaybackSessionFailure::PipelineTeardown,
            }
        );
        control.0.borrow_mut().fail_stop = false;
        assert!(matches!(
            core.borrow_mut().begin_tune(selection(&second_key(), 3)),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(1), first_key()), Call::Stop(0),]
        );
        assert!(matches!(
            core.borrow_mut().shut_down(),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        assert_eq!(
            control.calls(),
            vec![
                Call::Start(TuneGeneration(1), first_key()),
                Call::Stop(0),
                Call::Stop(0),
            ]
        );
        assert!(matches!(
            core.borrow_mut().shut_down(),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
    }

    #[test]
    fn failed_start_cleanup_permanently_blocks_later_construction() {
        let control = FakeControl::default();
        control.0.borrow_mut().fail_start = Some(FakeStartFailure::Quarantined);
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 3))
            .unwrap();

        assert_eq!(
            core.borrow_mut()
                .complete_tune(first, Ok(handoff(&first_key(), 3)), events_for(&core),),
            Err(PlaybackSessionFailure::PipelineTeardown)
        );
        control.0.borrow_mut().fail_start = None;
        assert!(matches!(
            core.borrow_mut().begin_tune(selection(&second_key(), 3)),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(1), first_key())]
        );
        assert_eq!(
            core.borrow_mut().set_muted(true),
            Err(PlaybackSessionFailure::PipelineTeardown)
        );
        assert!(matches!(
            core.borrow_mut().shut_down(),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(1), first_key()), Call::Stop(0),]
        );
        assert!(matches!(
            core.borrow_mut().stop(),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
    }

    #[test]
    fn clean_start_failure_allows_a_successor() {
        let control = FakeControl::default();
        control.0.borrow_mut().fail_start = Some(FakeStartFailure::Clean);
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 3))
            .unwrap();

        assert_eq!(
            core.borrow_mut()
                .complete_tune(first, Ok(handoff(&first_key(), 3)), events_for(&core),),
            Err(PlaybackSessionFailure::PipelineStart)
        );
        assert!(control.calls().is_empty());

        control.0.borrow_mut().fail_start = None;
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 3))
            .unwrap();
        assert_eq!(
            core.borrow_mut().complete_tune(
                second,
                Ok(handoff(&second_key(), 3)),
                events_for(&core),
            ),
            Ok(TuneCompletion::Applied)
        );
        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(2), second_key())]
        );
    }

    #[test]
    fn explicit_context_work_retries_after_a_reentrant_borrow() {
        let main_context = gst::glib::MainContext::new();
        let _owner = main_context.acquire().unwrap();
        let value = Rc::new(RefCell::new(0_u8));
        let held_borrow = value.borrow_mut();
        let queued_value = Rc::clone(&value);

        queue_main_context_work(&main_context, move || {
            let Ok(mut value) = queued_value.try_borrow_mut() else {
                return false;
            };
            *value = 1;
            true
        });
        assert!(main_context.iteration(false));
        assert_eq!(*held_borrow, 0);
        drop(held_borrow);

        drive_context_until(&main_context, || *value.borrow() == 1);
        assert_eq!(*value.borrow(), 1);
    }

    #[test]
    fn the_first_stream_bytes_release_the_paused_hold_before_playing() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let mut states = core.borrow().subscribe_state();
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 71))
            .unwrap();
        let generation = request.generation();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 71)), events_for(&core))
            .unwrap();
        assert!(
            !control
                .calls()
                .iter()
                .any(|call| matches!(call, Call::Play(_)))
        );

        control.emit(generation, PipelineEvent::StreamStarted);
        assert!(matches!(control.calls().last(), Some(Call::Play(0))));
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Connecting {
                generation,
                channel_key: first_key(),
            }
        );

        control.emit(generation, PipelineEvent::Playing);
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Playing {
                generation,
                channel_key: first_key(),
            }
        );
        // A stale notice for a retired generation is ignored.
        core.borrow_mut().stop().unwrap();
        control.emit(generation, PipelineEvent::StreamStarted);
        assert_eq!(
            control
                .calls()
                .iter()
                .filter(|call| matches!(call, Call::Play(_)))
                .count(),
            1
        );
    }

    #[test]
    fn a_failed_play_retires_the_pipeline_and_reports_the_start_failure() {
        let control = FakeControl::default();
        control.0.borrow_mut().fail_play = true;
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let mut states = core.borrow().subscribe_state();
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 71))
            .unwrap();
        let generation = request.generation();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 71)), events_for(&core))
            .unwrap();

        control.emit(generation, PipelineEvent::StreamStarted);
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Failed {
                generation,
                channel_key: first_key(),
                failure: PlaybackSessionFailure::PipelineStart,
            }
        );
        assert!(matches!(control.calls().last(), Some(Call::Stop(0))));
        assert!(core.borrow().active.is_none());
    }

    #[test]
    fn state_watch_starts_current_and_publishes_owned_transitions() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let mut states = core.borrow().subscribe_state();

        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Stopped
        );
        assert!(!states.has_changed().unwrap());

        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 71))
            .unwrap();
        let generation = request.generation();
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Resolving {
                generation,
                channel_key: first_key(),
            }
        );

        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 71)), events_for(&core))
            .unwrap();
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Connecting {
                generation,
                channel_key: first_key(),
            }
        );

        control.emit(generation, PipelineEvent::Buffering(37));
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Buffering {
                generation,
                channel_key: first_key(),
                percent: 37,
            }
        );

        control.emit(generation, PipelineEvent::Playing);
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Playing {
                generation,
                channel_key: first_key(),
            }
        );

        core.borrow_mut().stop().unwrap();
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Stopped
        );
        core.borrow_mut().stop().unwrap();
        assert!(!states.has_changed().unwrap());

        core.borrow_mut().shut_down().unwrap();
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::ShutDown
        );
        core.borrow_mut().shut_down().unwrap();
        assert!(!states.has_changed().unwrap());
    }

    #[test]
    fn queued_state_watch_ignores_stale_generation_events() {
        let main_context = gst::glib::MainContext::new();
        let _owner = main_context.acquire().unwrap();
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));

        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 72))
            .unwrap();
        let first_generation = first.generation();
        core.borrow_mut()
            .complete_tune(
                first,
                Ok(handoff(&first_key(), 72)),
                queued_events_for(&main_context, &core),
            )
            .unwrap();

        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 72))
            .unwrap();
        let second_generation = second.generation();
        core.borrow_mut()
            .complete_tune(
                second,
                Ok(handoff(&second_key(), 72)),
                queued_events_for(&main_context, &core),
            )
            .unwrap();
        let mut states = core.borrow().subscribe_state();
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Connecting {
                generation: second_generation,
                channel_key: second_key(),
            }
        );

        control.emit(
            first_generation,
            PipelineEvent::Error(PlaybackPipelineFailure::Internal),
        );
        let stale_drained = Rc::new(Cell::new(false));
        let stale_drained_task = Rc::clone(&stale_drained);
        queue_main_context_work(&main_context, move || {
            stale_drained_task.set(true);
            true
        });
        drive_context_until(&main_context, || stale_drained.get());
        assert!(!states.has_changed().unwrap());
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Connecting {
                generation: second_generation,
                channel_key: second_key(),
            }
        );

        control.emit(second_generation, PipelineEvent::Buffering(48));
        drive_context_until(&main_context, || {
            matches!(
                core.borrow().state(),
                PlaybackSessionState::Buffering {
                    generation,
                    percent: 48,
                    ..
                } if *generation == second_generation
            )
        });
        assert_eq!(
            states.borrow_and_update().clone(),
            PlaybackSessionState::Buffering {
                generation: second_generation,
                channel_key: second_key(),
                percent: 48,
            }
        );
    }

    #[test]
    fn synchronous_backend_events_are_deferred_without_overwriting_stop() {
        let main_context = gst::glib::MainContext::new();
        let _owner = main_context.acquire().unwrap();
        let control = FakeControl::default();
        control.0.borrow_mut().event_during_start = Some(PipelineEvent::Playing);
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 9))
            .unwrap();
        let generation = request.generation();

        core.borrow_mut()
            .complete_tune(
                request,
                Ok(handoff(&first_key(), 9)),
                queued_events_for(&main_context, &core),
            )
            .unwrap();
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Connecting {
                generation,
                channel_key: first_key(),
            }
        );
        drive_context_until(&main_context, || {
            matches!(core.borrow().state(), PlaybackSessionState::Playing { .. })
        });

        control.0.borrow_mut().event_during_stop =
            Some(PipelineEvent::Error(PlaybackPipelineFailure::Internal));
        core.borrow_mut().stop().unwrap();
        while main_context.pending() {
            main_context.iteration(false);
        }
        assert_eq!(core.borrow().state(), &PlaybackSessionState::Stopped);
        assert_eq!(
            control.calls(),
            vec![Call::Start(generation, first_key()), Call::Stop(0)]
        );
    }

    #[test]
    fn late_predecessor_bus_events_cannot_settle_the_successor() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 4))
            .unwrap();
        let first_generation = first.generation();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 4)), events_for(&core))
            .unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 4))
            .unwrap();
        let second_generation = second.generation();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 4)), events_for(&core))
            .unwrap();

        control.emit(
            first_generation,
            PipelineEvent::Error(PlaybackPipelineFailure::Internal),
        );
        control.emit(first_generation, PipelineEvent::EndOfStream);
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Connecting {
                generation: second_generation,
                channel_key: second_key(),
            }
        );
        control.emit(second_generation, PipelineEvent::Playing);
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Playing {
                generation: second_generation,
                channel_key: second_key(),
            }
        );
    }

    #[test]
    fn stop_shutdown_and_drop_retire_each_pipeline_once() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 1))
            .unwrap();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 1)), events_for(&core))
            .unwrap();
        core.borrow_mut().stop().unwrap();
        core.borrow_mut().stop().unwrap();

        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 1))
            .unwrap();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 1)), events_for(&core))
            .unwrap();
        core.borrow_mut().shut_down().unwrap();
        core.borrow_mut().shut_down().unwrap();
        drop(core);

        assert_eq!(
            control.calls(),
            vec![
                Call::Start(TuneGeneration(1), first_key()),
                Call::Stop(0),
                Call::Start(TuneGeneration(4), second_key()),
                Call::Stop(1),
            ]
        );
    }

    #[test]
    fn dropping_an_active_owner_retires_its_exact_pipeline() {
        let control = FakeControl::default();
        {
            let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
            let request = core
                .borrow_mut()
                .begin_tune(selection(&first_key(), 1))
                .unwrap();
            core.borrow_mut()
                .complete_tune(request, Ok(handoff(&first_key(), 1)), events_for(&core))
                .unwrap();
        }

        assert_eq!(
            control.calls(),
            vec![Call::Start(TuneGeneration(1), first_key()), Call::Stop(0),]
        );
    }

    #[test]
    fn buffering_is_generation_scoped_and_clamped() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 8))
            .unwrap();
        let generation = request.generation();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 8)), events_for(&core))
            .unwrap();

        core.borrow_mut().handle_event(
            TuneGeneration(generation.get() + 1),
            PipelineEvent::Buffering(7),
        );
        assert!(matches!(
            core.borrow().state(),
            PlaybackSessionState::Connecting { .. }
        ));
        control.emit(generation, PipelineEvent::Buffering(u8::MAX));
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Buffering {
                generation,
                channel_key: first_key(),
                percent: 100,
            }
        );
    }

    #[test]
    fn terminal_events_settle_the_current_owner_once() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 8))
            .unwrap();
        let generation = request.generation();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 8)), events_for(&core))
            .unwrap();

        control.emit(
            generation,
            PipelineEvent::Error(PlaybackPipelineFailure::Internal),
        );
        control.emit(generation, PipelineEvent::EndOfStream);
        assert_eq!(
            control
                .calls()
                .iter()
                .filter(|call| **call == Call::Stop(0))
                .count(),
            1
        );
        assert_eq!(
            core.borrow().state(),
            &PlaybackSessionState::Failed {
                generation,
                channel_key: first_key(),
                failure: PlaybackSessionFailure::Pipeline(PlaybackPipelineFailure::Internal),
            }
        );
    }

    #[test]
    fn audio_settings_survive_error_and_end_of_stream_retirement() {
        for terminal in [
            PipelineEvent::Error(PlaybackPipelineFailure::Internal),
            PipelineEvent::EndOfStream,
        ] {
            let control = FakeControl::default();
            let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
            core.borrow_mut().set_volume(0.45).unwrap();
            core.borrow_mut().set_muted(true).unwrap();
            let first = core
                .borrow_mut()
                .begin_tune(selection(&first_key(), 51))
                .unwrap();
            let first_generation = first.generation();
            core.borrow_mut()
                .complete_tune(first, Ok(handoff(&first_key(), 51)), events_for(&core))
                .unwrap();
            control.emit(first_generation, terminal);

            assert_eq!(
                core.borrow().audio_state(),
                PlaybackAudioState {
                    volume: 0.45,
                    muted: true,
                }
            );
            let successor = core
                .borrow_mut()
                .begin_tune(selection(&second_key(), 51))
                .unwrap();
            let successor_generation = successor.generation();
            core.borrow_mut()
                .complete_tune(successor, Ok(handoff(&second_key(), 51)), events_for(&core))
                .unwrap();
            assert_eq!(
                control.0.borrow().start_audio.get(&successor_generation),
                Some(&PlaybackAudioState {
                    volume: 0.45,
                    muted: true,
                })
            );
        }
    }

    #[test]
    fn mismatched_handoff_fails_without_constructing_a_pipeline() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 2))
            .unwrap();

        assert_eq!(
            core.borrow_mut().complete_tune(
                request,
                Ok(handoff(&second_key(), 2)),
                events_for(&core),
            ),
            Err(PlaybackSessionFailure::HandoffMismatch)
        );
        assert!(control.calls().is_empty());
    }

    #[test]
    fn generation_exhaustion_is_terminal_and_retires_the_owner() {
        let control = FakeControl::default();
        let mut core = SessionCore::new(control.backend());
        core.generation = TuneGeneration(u64::MAX - 1);
        let request = core.begin_tune(selection(&first_key(), 6)).unwrap();
        let isolated = Rc::new(RefCell::new(core));
        isolated
            .borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 6)), events_for(&isolated))
            .unwrap();

        assert!(matches!(
            isolated
                .borrow_mut()
                .begin_tune(selection(&second_key(), 6)),
            Err(PlaybackSessionFailure::GenerationExhausted)
        ));
        assert_eq!(
            isolated.borrow().state(),
            &PlaybackSessionState::Failed {
                generation: TuneGeneration(u64::MAX),
                channel_key: second_key(),
                failure: PlaybackSessionFailure::GenerationExhausted,
            }
        );
        assert_eq!(
            control.calls(),
            vec![
                Call::Start(TuneGeneration(u64::MAX), first_key()),
                Call::Stop(0),
            ]
        );
    }

    #[test]
    fn same_generation_events_cannot_overwrite_failed_exhausted_teardown() {
        let control = FakeControl::default();
        let mut core = SessionCore::new(control.backend());
        core.generation = TuneGeneration(u64::MAX - 1);
        let request = core.begin_tune(selection(&first_key(), 6)).unwrap();
        let generation = request.generation();
        let isolated = Rc::new(RefCell::new(core));
        isolated
            .borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 6)), events_for(&isolated))
            .unwrap();
        control.0.borrow_mut().fail_stop = true;

        assert!(matches!(
            isolated
                .borrow_mut()
                .begin_tune(selection(&second_key(), 6)),
            Err(PlaybackSessionFailure::PipelineTeardown)
        ));
        let poisoned = isolated.borrow().state().clone();
        control.emit(generation, PipelineEvent::Playing);
        control.emit(
            generation,
            PipelineEvent::Error(PlaybackPipelineFailure::Internal),
        );
        assert_eq!(isolated.borrow().state(), &poisoned);
        assert_eq!(
            control.calls(),
            vec![
                Call::Start(TuneGeneration(u64::MAX), first_key()),
                Call::Stop(0),
            ]
        );
    }
}
