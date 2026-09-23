use std::time::Duration;

use super::*;

#[test]
fn first_offsets_are_once_only_generation_local_and_close_rejects_late_work() {
    let start = Instant::now();
    let first = MediaTiming::new(start);
    let successor = MediaTiming::new(start);
    for (index, phase) in PHASES.into_iter().enumerate() {
        assert!(first.record_at(phase, start + Duration::from_micros(index as u64)));
        assert!(!first.record_at(phase, start + Duration::from_secs(5)));
    }
    for (index, (_, value)) in first.snapshot().into_iter().enumerate() {
        assert_eq!(value, Some(index as u64));
    }
    successor.close();
    assert!(!successor.record(MediaPhase::VideoSinkBuffer));
    assert!(
        successor
            .snapshot()
            .iter()
            .all(|(_, value)| value.is_none())
    );
}

#[test]
fn concurrent_callbacks_publish_only_one_wakeup_per_phase_without_native_text() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    let timing = Arc::new(MediaTiming::new(Instant::now()));
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let timing = &timing;
            let pipeline = pipeline.downgrade();
            scope.spawn(move || {
                for phase in PHASES {
                    record_and_notify(timing, &pipeline, phase);
                }
            });
        }
    });
    let bus = pipeline.bus().unwrap();
    for _ in 0..4 {
        let message = bus.pop().expect("one wakeup for each fixed slot");
        assert_eq!(message.src(), Some(pipeline.upcast_ref()));
        let gst::MessageView::Application(application) = message.view() else {
            panic!("only a fixed application wakeup is expected");
        };
        let structure = application.structure().unwrap();
        assert_eq!(structure.name(), MEDIA_PROGRESS_MESSAGE);
        assert_eq!(structure.n_fields(), 0);
    }
    assert!(bus.pop().is_none());
}

// Exercise actual GStreamer pad probes and sticky negotiated caps, while the
// harmless sink accepts buffers without decoding or accessing an audio device.
struct PadFixture {
    source: gst::Pad,
    sink: gst::Pad,
    element: gst::Bin,
}

impl PadFixture {
    fn new() -> Self {
        gst::init().unwrap();
        let element = gst::Bin::new();
        let source = gst::Pad::builder(gst::PadDirection::Src).build();
        let sink_template = gst::PadTemplate::new(
            "sink",
            gst::PadDirection::Sink,
            gst::PadPresence::Always,
            &gst::Caps::new_any(),
        )
        .unwrap();
        let sink = gst::Pad::builder_from_template(&sink_template)
            .name("sink")
            .event_function(|_, _, _| true)
            .chain_function(|_, _, _| Ok(gst::FlowSuccess::Ok))
            .chain_list_function(|_, _, _| Ok(gst::FlowSuccess::Ok))
            .build();
        element.add_pad(&sink).unwrap();
        source.link(&sink).unwrap();
        sink.set_active(true).unwrap();
        source.set_active(true).unwrap();
        assert!(source.push_event(gst::event::StreamStart::new("local-observer-fixture")));
        Self {
            source,
            sink,
            element,
        }
    }

    fn caps(&self, media_type: &str) {
        assert!(self.source.push_event(gst::event::Caps::new(
            &gst::Caps::builder(media_type).build(),
        )));
        let segment = gst::FormattedSegment::<gst::ClockTime>::new();
        assert!(
            self.source
                .push_event(gst::event::Segment::new(segment.as_ref()))
        );
    }

    fn push(&self, buffer: gst::Buffer) {
        assert_eq!(self.source.push(buffer), Ok(gst::FlowSuccess::Ok));
    }
}

impl Drop for PadFixture {
    fn drop(&mut self) {
        self.source.set_active(false).unwrap();
        self.sink.set_active(false).unwrap();
    }
}

fn watches(pipeline: &gst::Pipeline) -> SinkWatches {
    SinkWatches {
        pipeline: pipeline.downgrade(),
        timing: Arc::new(MediaTiming::new(Instant::now())),
        pads: Mutex::new(Vec::new()),
    }
}

#[test]
fn sink_probes_require_nonempty_raw_media_and_ignore_gap_corrupt_decode_only_buffers() {
    for (phase, expected) in [
        (MediaPhase::VideoSinkBuffer, "video/x-raw"),
        (MediaPhase::AudioSinkBuffer, "audio/x-raw"),
    ] {
        let fixture = PadFixture::new();
        let pipeline = gst::Pipeline::new();
        let watches = watches(&pipeline);
        watches.attach(fixture.element.upcast_ref(), phase);
        fixture.caps("application/octet-stream");
        fixture.push(gst::Buffer::from_slice([1_u8; 4]));
        assert!(!watches.timing.observed(phase));
        fixture.caps(expected);
        fixture.push(gst::Buffer::new());
        for flag in [
            gst::BufferFlags::GAP,
            gst::BufferFlags::CORRUPTED,
            gst::BufferFlags::DECODE_ONLY,
        ] {
            let mut buffer = gst::Buffer::from_slice([1_u8; 4]);
            buffer.get_mut().unwrap().set_flags(flag);
            fixture.push(buffer);
        }
        assert!(!watches.timing.observed(phase));
        fixture.push(gst::Buffer::from_slice([1_u8; 4]));
        let observed = watches.timing.snapshot().map(|(_, value)| value);
        assert!(watches.timing.observed(phase));
        fixture.push(gst::Buffer::from_slice([1_u8; 4]));
        assert_eq!(watches.timing.snapshot().map(|(_, value)| value), observed);
        assert!(pipeline.bus().unwrap().pop().is_some());
        assert!(pipeline.bus().unwrap().pop().is_none());
        watches.retire();
    }
}

#[test]
fn buffer_lists_are_observed_and_retired_probes_leave_no_late_work() {
    let fixture = PadFixture::new();
    let pipeline = gst::Pipeline::new();
    let watches = watches(&pipeline);
    watches.attach(fixture.element.upcast_ref(), MediaPhase::AudioSinkBuffer);
    fixture.caps("audio/x-raw");
    let mut list = gst::BufferList::new();
    list.get_mut().unwrap().add(gst::Buffer::new());
    list.get_mut()
        .unwrap()
        .add(gst::Buffer::from_slice([1_u8; 4]));
    assert_eq!(fixture.source.push_list(list), Ok(gst::FlowSuccess::Ok));
    assert!(watches.timing.observed(MediaPhase::AudioSinkBuffer));
    watches.retire();
    assert!(watches.pads.lock().unwrap().is_empty());
    let successor_watches = self::watches(&pipeline);
    successor_watches.attach(fixture.element.upcast_ref(), MediaPhase::VideoSinkBuffer);
    successor_watches.retire();
    fixture.caps("video/x-raw");
    fixture.push(gst::Buffer::from_slice([1_u8; 4]));
    assert!(
        !successor_watches
            .timing
            .observed(MediaPhase::VideoSinkBuffer)
    );
}

#[test]
fn probe_count_is_bounded_duplicates_are_ignored_and_generic_sinks_are_excluded() {
    gst::init().unwrap();
    let pipeline = gst::Pipeline::new();
    let watches = watches(&pipeline);
    let fixtures: Vec<_> = (0..MAX_SINK_PROBES + 1)
        .map(|_| PadFixture::new())
        .collect();
    for fixture in &fixtures {
        for _ in 0..2 {
            watches.attach(fixture.element.upcast_ref(), MediaPhase::AudioSinkBuffer);
        }
    }
    assert_eq!(watches.pads.lock().unwrap().len(), MAX_SINK_PROBES);
    assert!(watches.timing.observed(MediaPhase::ObserverIncomplete));
    assert!(pipeline.bus().unwrap().pop().is_some());
    assert!(pipeline.bus().unwrap().pop().is_none());
    assert!(!concrete_audio_sink(fixtures[0].element.upcast_ref()));
    if let Ok(fake) = gst::ElementFactory::make("fakesink").build() {
        assert!(!concrete_audio_sink(&fake));
    }
    watches.retire();
    watches.attach(
        fixtures[0].element.upcast_ref(),
        MediaPhase::AudioSinkBuffer,
    );
    assert!(watches.pads.lock().unwrap().is_empty());
}

#[test]
fn paintable_invalidation_requires_prior_video_and_positive_size_and_hooks_drop_cleanly() {
    for (width, height, expected) in [(16, 16, true), (0, 16, false), (16, 0, false)] {
        let fixture = PadFixture::new();
        let pipeline = gst::Pipeline::new();
        let paintable = gdk::Paintable::new_empty(width, height);
        let timing = Arc::new(MediaTiming::new(Instant::now()));
        let weak_timing = Arc::downgrade(&timing);
        let observer = MediaObserver::install(
            &pipeline,
            fixture.element.upcast_ref(),
            &paintable,
            Arc::clone(&timing),
        );
        // Emit the signal directly on an empty paintable; no display/frame is
        // fabricated, and this does not claim native sink rendering coverage.
        paintable.emit_by_name::<()>("invalidate-contents", &[]);
        assert!(!timing.observed(MediaPhase::PaintableInvalidated));
        fixture.caps("video/x-raw");
        fixture.push(gst::Buffer::from_slice([1_u8; 4]));
        paintable.emit_by_name::<()>("invalidate-contents", &[]);
        assert_eq!(timing.observed(MediaPhase::PaintableInvalidated), expected);
        let first = timing.snapshot().map(|(_, value)| value);
        paintable.emit_by_name::<()>("invalidate-contents", &[]);
        assert_eq!(timing.snapshot().map(|(_, value)| value), first);
        drop(observer);
        drop(timing);
        // Keep every native object alive. Any forgotten pad or signal closure
        // would retain its Arc, exposing an incomplete detachment here.
        assert!(weak_timing.upgrade().is_none());
        paintable.emit_by_name::<()>("invalidate-contents", &[]);
        fixture.push(gst::Buffer::from_slice([1_u8; 4]));
    }
}
