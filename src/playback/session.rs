//! Generation-owned playback session and deterministic pipeline teardown.

use std::cell::RefCell;
use std::fmt;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gst::prelude::*;
use gstreamer as gst;
use gtk::gdk;
use thiserror::Error;

use super::PlaybackRuntime;
use crate::controller::{OperationGeneration, StreamHandoff, StreamHandoffError, StreamSelection};
use crate::domain::ChannelKey;

const PIPELINE_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

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
    /// The session has already completed its terminal shutdown.
    #[error("the playback session is shut down")]
    ShutDown,
    /// A mutating call was made without owning Balun's default main context.
    #[error("the default main context is not owned by the playback thread")]
    MainContextUnavailable,
    /// A native callback safely reentered an in-progress session mutation.
    #[error("the playback session is already handling another operation")]
    SessionBusy,
    /// The controller rejected or lost the URL-private handoff.
    #[error("the controller rejected the stream handoff: {0}")]
    Handoff(StreamHandoffError),
    /// The response did not match the exact pending channel and selection.
    #[error("the stream handoff did not match the pending tune")]
    HandoffMismatch,
    /// The required paintable sink or `playbin3` pipeline could not be built.
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
    #[error("the playback pipeline failed")]
    Pipeline,
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
    Playing,
    Buffering(u8),
    EndOfStream,
    Error,
}

type EventSink = Rc<dyn Fn(TuneGeneration, PipelineEvent)>;

enum PipelineStartError<P> {
    Clean(PlaybackSessionFailure),
    Quarantined(P),
}

trait PipelineBackend {
    type Active;
    type StartOptions;

    fn start(
        &mut self,
        generation: TuneGeneration,
        handoff: StreamHandoff,
        options: Self::StartOptions,
        events: EventSink,
    ) -> Result<Self::Active, PipelineStartError<Self::Active>>;

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
    generation: TuneGeneration,
    pending: Option<PendingTune>,
    active: Option<ActiveTune<B::Active>>,
    state: PlaybackSessionState,
    exhausted: bool,
    teardown_failed: bool,
    shut_down: bool,
}

impl<B: PipelineBackend> SessionCore<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            generation: TuneGeneration::INITIAL,
            pending: None,
            active: None,
            state: PlaybackSessionState::Stopped,
            exhausted: false,
            teardown_failed: false,
            shut_down: false,
        }
    }

    fn state(&self) -> &PlaybackSessionState {
        &self.state
    }

    fn begin_tune(
        &mut self,
        selection: StreamSelection,
    ) -> Result<TuneRequest, PlaybackSessionFailure> {
        if self.shut_down {
            return Err(PlaybackSessionFailure::ShutDown);
        }
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
                self.state = PlaybackSessionState::Failed {
                    generation: self.generation,
                    channel_key,
                    failure,
                };
                return Err(failure);
            }
            self.state = PlaybackSessionState::Failed {
                generation: self.generation,
                channel_key,
                failure: PlaybackSessionFailure::GenerationExhausted,
            };
            return Err(PlaybackSessionFailure::GenerationExhausted);
        };
        // Publish the successor generation before touching the predecessor so
        // every callback from it is stale even during bounded teardown.
        self.generation = generation;
        self.pending = None;
        if let Err(failure) = self.retire_active() {
            self.state = PlaybackSessionState::Failed {
                generation,
                channel_key,
                failure,
            };
            return Err(failure);
        }

        let selection_generation = selection.selection_generation();
        self.pending = Some(PendingTune {
            generation,
            channel_key: channel_key.clone(),
            selection_generation,
        });
        self.state = PlaybackSessionState::Resolving {
            generation,
            channel_key,
        };
        Ok(TuneRequest {
            generation,
            selection,
        })
    }

    fn complete_tune(
        &mut self,
        request: TuneRequest,
        response: Result<StreamHandoff, StreamHandoffError>,
        options: B::StartOptions,
        events: EventSink,
    ) -> Result<TuneCompletion, PlaybackSessionFailure> {
        if self.shut_down || !self.request_is_current(&request) {
            drop(response);
            drop(options);
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
        let pipeline = match self.backend.start(generation, handoff, options, events) {
            Ok(pipeline) => pipeline,
            Err(PipelineStartError::Clean(failure)) => {
                self.state = PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                };
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
                self.state = PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                };
                return Err(failure);
            }
        };
        self.active = Some(ActiveTune {
            generation,
            channel_key: channel_key.clone(),
            pipeline,
        });
        self.state = PlaybackSessionState::Connecting {
            generation,
            channel_key,
        };
        Ok(TuneCompletion::Applied)
    }

    fn cancel_tune(&mut self, request: TuneRequest) -> bool {
        if !self.request_is_current(&request) {
            return false;
        }
        self.pending = None;
        self.state = PlaybackSessionState::Stopped;
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
                self.state = PlaybackSessionState::Failed {
                    generation: self.generation,
                    channel_key,
                    failure,
                };
            }
            return Err(failure);
        }
        self.state = PlaybackSessionState::Stopped;
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
        self.state = PlaybackSessionState::ShutDown;
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
        match event {
            PipelineEvent::Playing => {
                self.state = PlaybackSessionState::Playing {
                    generation,
                    channel_key,
                };
            }
            PipelineEvent::Buffering(percent) => {
                self.state = PlaybackSessionState::Buffering {
                    generation,
                    channel_key,
                    percent: percent.min(100),
                };
            }
            PipelineEvent::EndOfStream => {
                if self.retire_active().is_ok() {
                    self.state = PlaybackSessionState::Stopped;
                } else {
                    self.state = PlaybackSessionState::Failed {
                        generation,
                        channel_key,
                        failure: PlaybackSessionFailure::PipelineTeardown,
                    };
                }
            }
            PipelineEvent::Error => {
                let failure = if self.retire_active().is_ok() {
                    PlaybackSessionFailure::Pipeline
                } else {
                    PlaybackSessionFailure::PipelineTeardown
                };
                self.state = PlaybackSessionState::Failed {
                    generation,
                    channel_key,
                    failure,
                };
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
        self.state = PlaybackSessionState::Failed {
            generation: pending.generation,
            channel_key: pending.channel_key,
            failure,
        };
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
    pipeline: gst::Pipeline,
    paintable: gdk::Paintable,
    bus_watch: Option<gst::bus::BusWatchGuard>,
    armed: bool,
}

impl GstreamerPipeline {
    fn stop(&mut self) -> Result<(), PlaybackSessionFailure> {
        // Detach callbacks before requesting NULL so teardown messages cannot
        // mutate the settled generation.
        self.bus_watch.take();
        let request = self.pipeline.set_state(gst::State::Null);
        let (transition, current, pending) = self.pipeline.state(gst::ClockTime::from_nseconds(
            PIPELINE_TEARDOWN_TIMEOUT.as_nanos().min(u64::MAX as u128) as u64,
        ));
        if request.is_ok()
            && transition.is_ok()
            && current == gst::State::Null
            && pending == gst::State::VoidPending
        {
            self.armed = false;
            Ok(())
        } else {
            Err(PlaybackSessionFailure::PipelineTeardown)
        }
    }
}

impl Drop for GstreamerPipeline {
    fn drop(&mut self) {
        self.bus_watch.take();
        if self.armed {
            let _ = self.pipeline.set_state(gst::State::Null);
            self.armed = false;
        }
    }
}

impl PipelineBackend for GstreamerBackend {
    type Active = GstreamerPipeline;
    type StartOptions = ();

    fn start(
        &mut self,
        generation: TuneGeneration,
        handoff: StreamHandoff,
        (): Self::StartOptions,
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
        let element = handoff.with_uri(|uri| {
            gst::ElementFactory::make("playbin3")
                .property("uri", uri)
                .build()
        });
        let pipeline = element
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| PipelineStartError::Clean(PlaybackSessionFailure::PipelineConstruction))?;
        pipeline.set_property("video-sink", &video_sink);

        let bus = pipeline.bus().ok_or(PipelineStartError::Clean(
            PlaybackSessionFailure::BusUnavailable,
        ))?;
        let watched_pipeline = pipeline.clone();
        let watch = move |_: &gst::Bus, message: &gst::Message| {
            let event = match message.view() {
                gst::MessageView::Eos(_) => Some(PipelineEvent::EndOfStream),
                gst::MessageView::Error(_) => Some(PipelineEvent::Error),
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
                Some(PipelineEvent::EndOfStream | PipelineEvent::Error)
            );
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
        let mut active = GstreamerPipeline {
            pipeline,
            paintable,
            bus_watch: Some(bus_watch),
            armed: true,
        };
        if active.pipeline.set_state(gst::State::Playing).is_err() {
            return match active.stop() {
                Ok(()) => Err(PipelineStartError::Clean(
                    PlaybackSessionFailure::PipelineStart,
                )),
                Err(_) => Err(PipelineStartError::Quarantined(active)),
            };
        }
        Ok(active)
    }

    fn stop(&mut self, active: &mut Self::Active) -> Result<(), PlaybackSessionFailure> {
        active.stop()
    }
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
            .complete_tune(request, response, (), events)
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
    use std::collections::BTreeMap;

    use super::*;
    use crate::domain::{DeviceId, GuideNumber};

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Call {
        Start(TuneGeneration, ChannelKey),
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
        next_pipeline: usize,
        fail_start: Option<FakeStartFailure>,
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
        type StartOptions = ();

        fn start(
            &mut self,
            generation: TuneGeneration,
            handoff: StreamHandoff,
            (): Self::StartOptions,
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
                (),
                events_for(&core),
            ),
            Ok(TuneCompletion::Stale)
        );
        assert!(control.calls().is_empty());
        assert_eq!(
            core.borrow_mut().complete_tune(
                second,
                Ok(handoff(&second_key(), 11)),
                (),
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
    fn replacement_stops_the_exact_predecessor_before_starting_successor() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let first = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 3))
            .unwrap();
        core.borrow_mut()
            .complete_tune(first, Ok(handoff(&first_key(), 3)), (), events_for(&core))
            .unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 3))
            .unwrap();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 3)), (), events_for(&core))
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
            .complete_tune(first, Ok(handoff(&first_key(), 3)), (), events_for(&core))
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
            core.borrow_mut().complete_tune(
                first,
                Ok(handoff(&first_key(), 3)),
                (),
                events_for(&core),
            ),
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
            core.borrow_mut().complete_tune(
                first,
                Ok(handoff(&first_key(), 3)),
                (),
                events_for(&core),
            ),
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
                (),
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
                (),
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

        control.0.borrow_mut().event_during_stop = Some(PipelineEvent::Error);
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
            .complete_tune(first, Ok(handoff(&first_key(), 4)), (), events_for(&core))
            .unwrap();
        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 4))
            .unwrap();
        let second_generation = second.generation();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 4)), (), events_for(&core))
            .unwrap();

        control.emit(first_generation, PipelineEvent::Error);
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
            .complete_tune(first, Ok(handoff(&first_key(), 1)), (), events_for(&core))
            .unwrap();
        core.borrow_mut().stop().unwrap();
        core.borrow_mut().stop().unwrap();

        let second = core
            .borrow_mut()
            .begin_tune(selection(&second_key(), 1))
            .unwrap();
        core.borrow_mut()
            .complete_tune(second, Ok(handoff(&second_key(), 1)), (), events_for(&core))
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
    fn terminal_events_settle_the_current_owner_once() {
        let control = FakeControl::default();
        let core = Rc::new(RefCell::new(SessionCore::new(control.backend())));
        let request = core
            .borrow_mut()
            .begin_tune(selection(&first_key(), 8))
            .unwrap();
        let generation = request.generation();
        core.borrow_mut()
            .complete_tune(request, Ok(handoff(&first_key(), 8)), (), events_for(&core))
            .unwrap();

        control.emit(generation, PipelineEvent::Error);
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
                failure: PlaybackSessionFailure::Pipeline,
            }
        );
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
                (),
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
            .complete_tune(
                request,
                Ok(handoff(&first_key(), 6)),
                (),
                events_for(&isolated),
            )
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
            .complete_tune(
                request,
                Ok(handoff(&first_key(), 6)),
                (),
                events_for(&isolated),
            )
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
        control.emit(generation, PipelineEvent::Error);
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
