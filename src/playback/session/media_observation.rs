//! First raw sink buffers and paintable invalidation, never presentation proof.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gst::prelude::*;
use gstreamer as gst;
use gtk::gdk;
use gtk::prelude::PaintableExt;

pub(super) const MEDIA_PROGRESS_MESSAGE: &str = "balun-media-progress";
const MAX_SINK_PROBES: usize = 9; // Shared by the explicit video and discovered audio sinks.

#[derive(Clone, Copy)]
pub(super) enum MediaPhase {
    VideoSinkBuffer,
    AudioSinkBuffer,
    PaintableInvalidated,
    ObserverIncomplete,
}

const PHASES: [MediaPhase; 4] = [
    MediaPhase::VideoSinkBuffer,
    MediaPhase::AudioSinkBuffer,
    MediaPhase::PaintableInvalidated,
    MediaPhase::ObserverIncomplete,
];

pub(super) struct MediaTiming {
    started: Instant,
    offsets: [AtomicU64; 4],
    closed: AtomicBool,
}

impl MediaTiming {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            offsets: std::array::from_fn(|_| AtomicU64::new(0)),
            closed: AtomicBool::new(false),
        }
    }

    pub(super) fn record(&self, phase: MediaPhase) -> bool {
        self.record_at(phase, Instant::now())
    }

    fn record_at(&self, phase: MediaPhase, observed: Instant) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let micros = observed
            .saturating_duration_since(self.started)
            .as_micros()
            .min(u128::from(u64::MAX - 1)) as u64;
        self.offsets[phase as usize]
            .compare_exchange(0, micros + 1, Ordering::Release, Ordering::Relaxed)
            .is_ok()
    }

    pub(super) fn observed(&self, phase: MediaPhase) -> bool {
        self.offsets[phase as usize].load(Ordering::Acquire) != 0
    }

    pub(super) fn snapshot(&self) -> [(MediaPhase, Option<u64>); 4] {
        PHASES.map(|phase| {
            let value = self.offsets[phase as usize].load(Ordering::Acquire);
            (phase, value.checked_sub(1))
        })
    }

    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }
}

struct PadWatch {
    pad: gst::glib::WeakRef<gst::Pad>,
    id: gst::PadProbeId,
}

struct SinkWatches {
    pipeline: gst::glib::WeakRef<gst::Pipeline>,
    timing: Arc<MediaTiming>,
    pads: Mutex<Vec<PadWatch>>,
}

impl SinkWatches {
    fn retire(&self) {
        self.timing.close();
        for watch in self.pads.lock().unwrap().drain(..) {
            if let Some(pad) = watch.pad.upgrade() {
                pad.remove_probe(watch.id);
            }
        }
    }

    fn attach(&self, element: &gst::Element, phase: MediaPhase) {
        // Serialize installation with retirement. Probe callbacks never acquire
        // this lock, and hold neither a strong pipeline nor this owner.
        let mut pads = self.pads.lock().unwrap();
        if self.timing.closed.load(Ordering::Acquire) || self.timing.observed(phase) {
            return;
        }
        let Some(pad) = element.static_pad("sink") else {
            record_and_notify(&self.timing, &self.pipeline, MediaPhase::ObserverIncomplete);
            return;
        };
        if pads
            .iter()
            .any(|watch| watch.pad.upgrade().as_ref() == Some(&pad))
        {
            return;
        }
        if pads.len() == MAX_SINK_PROBES {
            record_and_notify(&self.timing, &self.pipeline, MediaPhase::ObserverIncomplete);
            return;
        }
        let timing = Arc::clone(&self.timing);
        let pipeline = self.pipeline.clone();
        let id = pad.add_probe(
            gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
            move |pad, info| {
                // After the first observation, only atomic reads remain. No
                // per-buffer logs, queue entries, or retained media buffers.
                if !timing.closed.load(Ordering::Acquire)
                    && !timing.observed(phase)
                    && raw_media_buffer(pad, info, phase)
                {
                    record_and_notify(&timing, &pipeline, phase);
                }
                gst::PadProbeReturn::Ok
            },
        );
        if let Some(id) = id {
            pads.push(PadWatch {
                pad: pad.downgrade(),
                id,
            });
        } else {
            record_and_notify(&self.timing, &self.pipeline, MediaPhase::ObserverIncomplete);
        }
    }
}

fn raw_media_buffer(pad: &gst::Pad, info: &gst::PadProbeInfo<'_>, phase: MediaPhase) -> bool {
    let expected = match phase {
        MediaPhase::VideoSinkBuffer => "video/x-raw",
        MediaPhase::AudioSinkBuffer => "audio/x-raw",
        _ => return false,
    };
    let Some(caps) = pad.current_caps() else {
        return false;
    };
    if !caps.is_fixed() || !caps.structure(0).is_some_and(|s| s.name() == expected) {
        return false;
    }
    info.buffer().is_some_and(|buffer| usable_buffer(buffer))
        || info
            .buffer_list()
            .is_some_and(|list| list.iter().any(usable_buffer))
}

fn usable_buffer(buffer: &gst::BufferRef) -> bool {
    buffer.size() != 0
        && !buffer.flags().intersects(
            gst::BufferFlags::GAP | gst::BufferFlags::CORRUPTED | gst::BufferFlags::DECODE_ONLY,
        )
}

fn record_and_notify(
    timing: &MediaTiming,
    pipeline: &gst::glib::WeakRef<gst::Pipeline>,
    phase: MediaPhase,
) {
    if timing.record(phase)
        && let Some(pipeline) = pipeline.upgrade()
        && let Some(bus) = pipeline.bus()
    {
        // At most four endpoint-free wakeups for this generation. Timestamps
        // stay in Rust; the existing main-context owner emits the fixed fields.
        let message =
            gst::message::Application::builder(gst::Structure::new_empty(MEDIA_PROGRESS_MESSAGE))
                .src(&pipeline)
                .build();
        let _ = bus.post(message);
    }
}

fn concrete_audio_sink(element: &gst::Element) -> bool {
    // Exclude autoaudiosink's bin/ghost pad: it can receive raw samples before
    // choosing an output. Generic fallback fakesinks are not audio sinks.
    if element.is::<gst::Bin>() {
        return false;
    }
    element.factory().is_some_and(|factory| {
        factory.metadata("klass").is_some_and(|class| {
            class.split('/').any(|part| part == "Sink")
                && class.split('/').any(|part| part == "Audio")
        })
    })
}

/// Installed before PAUSED; disconnected before native teardown. No graph or
/// audio-output selection changes are made by these observational hooks.
pub(super) struct MediaObserver {
    sinks: Arc<SinkWatches>,
    element_added: Option<gst::glib::SignalHandlerId>,
    paintable: gst::glib::WeakRef<gdk::Paintable>,
    invalidated: Option<gst::glib::SignalHandlerId>,
}

impl MediaObserver {
    pub(super) fn install(
        pipeline: &gst::Pipeline,
        video_sink: &gst::Element,
        paintable: &gdk::Paintable,
        timing: Arc<MediaTiming>,
    ) -> Self {
        let sinks = Arc::new(SinkWatches {
            pipeline: pipeline.downgrade(),
            timing: Arc::clone(&timing),
            pads: Mutex::new(Vec::with_capacity(MAX_SINK_PROBES)),
        });
        sinks.attach(video_sink, MediaPhase::VideoSinkBuffer);
        let added = Arc::clone(&sinks);
        let element_added = pipeline.connect_deep_element_added(move |_, _, element| {
            if concrete_audio_sink(element) {
                added.attach(element, MediaPhase::AudioSinkBuffer);
            }
        });
        let pipeline = pipeline.downgrade();
        let invalidated = paintable.connect_invalidate_contents(move |paintable| {
            if timing.observed(MediaPhase::VideoSinkBuffer)
                && paintable.intrinsic_width() > 0
                && paintable.intrinsic_height() > 0
            {
                record_and_notify(&timing, &pipeline, MediaPhase::PaintableInvalidated);
            }
        });
        Self {
            sinks,
            element_added: Some(element_added),
            paintable: paintable.downgrade(),
            invalidated: Some(invalidated),
        }
    }
}

impl Drop for MediaObserver {
    fn drop(&mut self) {
        self.sinks.timing.close();
        if let Some(pipeline) = self.sinks.pipeline.upgrade()
            && let Some(id) = self.element_added.take()
        {
            pipeline.disconnect(id);
        }
        if let Some(paintable) = self.paintable.upgrade()
            && let Some(id) = self.invalidated.take()
        {
            paintable.disconnect(id);
        }
        self.sinks.retire();
    }
}

#[cfg(test)]
mod tests;
