#![cfg(feature = "desktop")]

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use balun::playback::PlaybackRuntime;
use gst::prelude::*;
use gstreamer as gst;
use gtk::prelude::*;

const FIXTURE_BYTES: &[u8] = include_bytes!("fixtures/synthetic-mpeg2.ts");
const FIXTURE_LENGTH: usize = 18_424;
const TRANSPORT_PACKET_LENGTH: usize = 188;
const EXPECTED_BLAKE3: [u8; 32] = [
    0x78, 0xa4, 0xa8, 0xa9, 0x4c, 0x2f, 0x92, 0x86, 0x09, 0x42, 0x7f, 0xfb, 0x8f, 0x69, 0x27, 0x4c,
    0x03, 0xbd, 0xb1, 0xf8, 0x33, 0xce, 0x2a, 0xe1, 0x3e, 0x20, 0x17, 0x35, 0xa0, 0x97, 0x19, 0xc2,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalOutcome {
    Pending,
    Eos,
    PipelineError,
    WatchdogExpired,
}

struct PipelineTeardown {
    pipeline: gst::Pipeline,
    armed: bool,
}

impl PipelineTeardown {
    fn new(pipeline: gst::Pipeline) -> Self {
        Self {
            pipeline,
            armed: true,
        }
    }

    fn pipeline(&self) -> &gst::Pipeline {
        &self.pipeline
    }

    fn shutdown(&mut self) -> bool {
        let state_request_succeeded = self.pipeline.set_state(gst::State::Null).is_ok();
        let (transition, current, pending) = self.pipeline.state(gst::ClockTime::from_seconds(5));
        self.armed = current != gst::State::Null;

        state_request_succeeded
            && transition.is_ok()
            && current == gst::State::Null
            && pending == gst::State::VoidPending
    }
}

impl Drop for PipelineTeardown {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.pipeline.set_state(gst::State::Null);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SampleObservation {
    is_raw_video: bool,
    width: i32,
    height: i32,
}

#[test]
#[ignore = "requires an isolated display-backed GTK session and complete playback runtime"]
fn synthetic_mpeg2_reaches_eos_and_renders_multiple_frames() {
    verify_fixture_integrity();

    gtk::init().unwrap_or_else(|_| panic!("GTK could not initialize for synthetic playback"));
    let main_context = gst::glib::MainContext::default();
    let _main_context_guard = main_context
        .acquire()
        .unwrap_or_else(|_| panic!("the default main context is unavailable"));
    let _runtime = PlaybackRuntime::initialize()
        .unwrap_or_else(|_| panic!("the playback runtime could not initialize"));

    let playbin = make_element("playbin3", "playbin3 is unavailable")
        .downcast::<gst::Pipeline>()
        .unwrap_or_else(|_| panic!("playbin3 did not create a pipeline"));
    let mut teardown = PipelineTeardown::new(playbin);
    let video_sink = make_element("gtk4paintablesink", "gtk4paintablesink is unavailable");
    let audio_sink = make_element("fakesink", "the silent audio sink is unavailable");

    let paintable = video_sink.property::<gtk::gdk::Paintable>("paintable");
    assert!(
        paintable.has_property("force-aspect-ratio"),
        "the GTK paintable must expose its aspect-ratio contract"
    );
    paintable.set_property("force-aspect-ratio", true);
    assert!(
        paintable.property::<bool>("force-aspect-ratio"),
        "the GTK paintable must retain forced aspect-ratio preservation"
    );
    let invalidations = Rc::new(Cell::new(0_u32));
    let counted_invalidations = Rc::clone(&invalidations);
    let invalidation_handler = paintable.connect_invalidate_contents(move |_| {
        counted_invalidations.set(counted_invalidations.get().saturating_add(1));
    });

    let picture = gtk::Picture::for_paintable(&paintable);
    picture.set_can_shrink(true);
    picture.set_content_fit(gtk::ContentFit::Contain);
    assert_eq!(picture.content_fit(), gtk::ContentFit::Contain);
    let window = gtk::Window::builder()
        .title("Balun synthetic playback acceptance")
        .default_width(320)
        .default_height(192)
        .child(&picture)
        .build();
    window.present();

    let fixture_uri = gst::glib::filename_to_uri(fixture_path(), None)
        .unwrap_or_else(|_| panic!("the synthetic fixture URI could not be constructed"));
    teardown.pipeline().set_property("video-sink", &video_sink);
    teardown.pipeline().set_property("audio-sink", &audio_sink);
    teardown.pipeline().set_property("uri", fixture_uri);

    let terminal = Rc::new(Cell::new(TerminalOutcome::Pending));
    let observed_playing = Rc::new(Cell::new(false));
    let main_loop = gst::glib::MainLoop::new(Some(&main_context), false);
    let bus = teardown
        .pipeline()
        .bus()
        .unwrap_or_else(|| panic!("playbin3 did not provide a message bus"));
    let watched_terminal = Rc::clone(&terminal);
    let watched_playing = Rc::clone(&observed_playing);
    let watched_pipeline = teardown.pipeline().clone();
    let watched_loop = main_loop.clone();
    let bus_watch = bus
        .add_watch_local(move |_, message| {
            match message.view() {
                gst::MessageView::Eos(_) => {
                    watched_terminal.set(TerminalOutcome::Eos);
                    watched_loop.quit();
                }
                gst::MessageView::Error(_) => {
                    // Native error strings and debug details can contain the full URI.
                    watched_terminal.set(TerminalOutcome::PipelineError);
                    watched_loop.quit();
                }
                gst::MessageView::StateChanged(state_changed)
                    if message.src().is_some_and(|source| {
                        source == watched_pipeline.upcast_ref::<gst::Object>()
                    }) && state_changed.current() == gst::State::Playing =>
                {
                    watched_playing.set(true);
                }
                _ => {}
            }
            gst::glib::ControlFlow::Continue
        })
        .unwrap_or_else(|_| panic!("the playback message watch could not be installed"));

    let watchdog_source = Rc::new(RefCell::new(None));
    let fired_watchdog_source = Rc::clone(&watchdog_source);
    let watchdog_terminal = Rc::clone(&terminal);
    let watchdog_loop = main_loop.clone();
    let source_id = gst::glib::timeout_add_local_once(Duration::from_secs(8), move || {
        fired_watchdog_source.borrow_mut().take();
        if watchdog_terminal.get() == TerminalOutcome::Pending {
            watchdog_terminal.set(TerminalOutcome::WatchdogExpired);
            watchdog_loop.quit();
        }
    });
    *watchdog_source.borrow_mut() = Some(source_id);

    teardown
        .pipeline()
        .set_state(gst::State::Playing)
        .unwrap_or_else(|_| panic!("synthetic playback could not enter the playing state"));
    main_loop.run();

    if let Some(source_id) = watchdog_source.borrow_mut().take() {
        source_id.remove();
    }
    let terminal = terminal.get();
    let observed_playing = observed_playing.get();
    let window_was_mapped = window.is_mapped();
    let invalidations = invalidations.get();
    let rendered = rendered_frames(&video_sink);
    let sample = sample_observation(teardown.pipeline());

    drop(bus_watch);
    paintable.disconnect(invalidation_handler);
    let reached_null = teardown.shutdown();
    window.close();

    assert_eq!(
        terminal,
        TerminalOutcome::Eos,
        "synthetic playback must reach EOS without a pipeline error or timeout"
    );
    assert!(
        observed_playing,
        "playbin3 must publish its top-level transition to PLAYING"
    );
    assert!(
        window_was_mapped,
        "the synthetic playback window must become mapped"
    );
    assert!(
        invalidations >= 2,
        "the GTK paintable must invalidate at least twice during decoded playback"
    );
    assert!(
        rendered.is_some_and(|count| count >= 2),
        "gtk4paintablesink must report at least two rendered frames"
    );
    let sample = sample.unwrap_or_else(|| panic!("playbin3 must retain a decoded video sample"));
    assert!(
        sample.is_raw_video,
        "the retained playback sample must have raw-video caps"
    );
    assert_eq!(sample.width, 160, "the decoded sample width must be stable");
    assert_eq!(
        sample.height, 96,
        "the decoded sample height must be stable"
    );
    assert!(
        reached_null,
        "the synthetic playback pipeline must reach NULL within five seconds"
    );
}

fn verify_fixture_integrity() {
    assert_eq!(
        FIXTURE_BYTES.len(),
        FIXTURE_LENGTH,
        "the synthetic fixture length must remain stable"
    );
    assert_eq!(
        FIXTURE_BYTES.len() / TRANSPORT_PACKET_LENGTH,
        98,
        "the synthetic fixture packet count must remain stable"
    );
    assert!(
        FIXTURE_BYTES
            .as_chunks::<TRANSPORT_PACKET_LENGTH>()
            .0
            .iter()
            .all(|packet| packet[0] == 0x47),
        "every synthetic fixture transport packet must retain its sync byte"
    );
    assert_eq!(
        blake3::hash(FIXTURE_BYTES).as_bytes(),
        &EXPECTED_BLAKE3,
        "the synthetic fixture digest must remain stable"
    );
}

fn fixture_path() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/synthetic-mpeg2.ts"
    ))
}

fn make_element(factory: &str, failure: &'static str) -> gst::Element {
    gst::ElementFactory::make(factory)
        .build()
        .unwrap_or_else(|_| panic!("{failure}"))
}

fn rendered_frames(sink: &gst::Element) -> Option<u64> {
    sink.property::<gst::Structure>("stats")
        .get::<u64>("rendered")
        .ok()
}

fn sample_observation(playbin: &gst::Pipeline) -> Option<SampleObservation> {
    let sample = playbin.property::<Option<gst::Sample>>("sample")?;
    let caps = sample.caps()?;
    let structure = caps.structure(0)?;
    Some(SampleObservation {
        is_raw_video: structure.name().as_str() == "video/x-raw",
        width: structure.get::<i32>("width").ok()?,
        height: structure.get::<i32>("height").ok()?,
    })
}
