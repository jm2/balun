//! Exercise the demuxer's output timeline without hardware, decoding, or
//! wall-clock sleeps. Sink buffer counts cannot detect audio ringbuffer loss.

use std::sync::{Mutex, mpsc};
use std::time::Duration;

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gstreamer as gst;

use super::configure_and_verify;
use crate::playback::test_support::FIXTURE_BYTES;

mod clock_imp {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Default)]
    pub struct ArrivalClock(pub AtomicU64);

    #[glib::object_subclass]
    impl ObjectSubclass for ArrivalClock {
        const NAME: &'static str = "BalunArrivalTestClock";
        type Type = super::ArrivalClock;
        type ParentType = gst::Clock;
    }

    impl ObjectImpl for ArrivalClock {}
    impl GstObjectImpl for ArrivalClock {}

    impl ClockImpl for ArrivalClock {
        fn internal_time(&self) -> gst::ClockTime {
            gst::ClockTime::from_nseconds(self.0.load(Ordering::SeqCst))
        }
    }
}

glib::wrapper! {
    pub struct ArrivalClock(ObjectSubclass<clock_imp::ArrivalClock>)
        @extends gst::Clock, gst::Object;
}

impl ArrivalClock {
    fn set_time(&self, time: gst::ClockTime) {
        self.imp()
            .0
            .store(time.nseconds(), std::sync::atomic::Ordering::SeqCst);
    }
}

struct PipelineGuard(gst::Pipeline);

impl Drop for PipelineGuard {
    fn drop(&mut self) {
        let _ = self.0.set_state(gst::State::Null);
    }
}

#[test]
fn delayed_mpegts_media_keeps_its_arrival_running_time_after_demuxing() {
    if gst::init().is_err() {
        return;
    }
    let (Ok(source), Ok(demux), Ok(sink)) = (
        gst::ElementFactory::make("appsrc").build(),
        gst::ElementFactory::make("tsdemux").build(),
        gst::ElementFactory::make("fakesink").build(),
    ) else {
        return;
    };
    assert!(configure_and_verify(&source));
    // Observe timestamps, never wait against the manually advanced clock.
    sink.set_property("sync", false);
    sink.set_property("async", false);
    let pipeline = PipelineGuard(gst::Pipeline::new());
    pipeline.0.add_many([&source, &demux, &sink]).unwrap();
    source.link(&demux).unwrap();
    let sink_pad = sink.static_pad("sink").unwrap();
    let linked_pad = sink_pad.clone();
    demux.connect_pad_added(move |_, pad| {
        pad.link(&linked_pad).expect("link the fixture's video PES");
    });

    let (observed, observations) = mpsc::channel();
    let segment = Mutex::new(None);
    sink_pad.add_probe(
        gst::PadProbeType::EVENT_DOWNSTREAM | gst::PadProbeType::BUFFER,
        move |_, info| {
            let mut segment = segment.lock().unwrap();
            if let Some(event) = info.event()
                && let gst::EventView::Segment(event) = event.view()
            {
                *segment = event.segment().downcast_ref::<gst::ClockTime>().cloned();
            }
            if let Some(buffer) = info.buffer()
                && let Some(segment) = segment.as_ref()
                && let Some(time) = segment.to_running_time(buffer.pts())
            {
                let _ = observed.send(time);
            }
            gst::PadProbeReturn::Ok
        },
    );

    let clock: ArrivalClock = glib::Object::builder().build();
    clock.set_time(gst::ClockTime::from_seconds(100));
    pipeline.0.use_clock(Some(&clock));
    pipeline.0.set_state(gst::State::Paused).unwrap();

    // Match production: first bytes are queued in PAUSED, and only then is
    // PLAYING requested. Initial TS null packets contain no PCR or media.
    let (consumed, first_consumed) = mpsc::sync_channel(1);
    source
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, _| {
            let _ = consumed.try_send(());
            gst::PadProbeReturn::Ok
        });
    let mut null_packet = [0xff_u8; 188];
    null_packet[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
    let nulls = gst::Buffer::from_slice(null_packet.repeat(8));
    assert_eq!(
        source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&nulls]),
        gst::FlowReturn::Ok
    );
    pipeline.0.set_state(gst::State::Playing).unwrap();
    first_consumed.recv_timeout(Duration::from_secs(5)).unwrap();

    // Advance only this pipeline's clock. Both the startup delay and the
    // expected running time are deterministic even on a slow CI runner.
    let delay = gst::ClockTime::from_seconds(5);
    clock.set_time(gst::ClockTime::from_seconds(100) + delay);
    let fixture = gst::Buffer::from_slice(FIXTURE_BYTES);
    assert_eq!(
        source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&fixture]),
        gst::FlowReturn::Ok
    );
    assert_eq!(
        source.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]),
        gst::FlowReturn::Ok
    );
    let first_media = observations
        .recv_timeout(Duration::from_secs(5))
        .expect("the fixture must produce timestamped MPEG-2 PES data");
    assert!(
        first_media >= delay && first_media < delay + gst::ClockTime::from_seconds(2),
        "media must be scheduled near its arrival, not near tune start: {first_media}"
    );
}
