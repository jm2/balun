//! Explicit software deinterlacing for playsink's dynamically inserted filter.

use gst::prelude::*;
use gstreamer as gst;

use super::session::PlaybackSessionFailure;

const SETTINGS: [(&str, &str); 5] = [
    ("method", "yadif"),
    ("mode", "auto"),
    ("fields", "all"),
    ("tff", "auto"),
    ("locking", "none"),
];

fn configure(element: &gst::Element) -> Result<(), PlaybackSessionFailure> {
    for (property, nick) in SETTINGS {
        let spec = element
            .find_property(property)
            .ok_or(PlaybackSessionFailure::PipelineConstruction)?;
        let class = gst::glib::EnumClass::with_type(spec.value_type())
            .ok_or(PlaybackSessionFailure::PipelineConstruction)?;
        let value = class
            .to_value_by_nick(nick)
            .ok_or(PlaybackSessionFailure::PipelineConstruction)?;
        element.set_property(property, value);
        if enum_nick(element, property).as_deref() != Some(nick) {
            return Err(PlaybackSessionFailure::PipelineConstruction);
        }
    }
    Ok(())
}

fn enum_nick(element: &gst::Element, property: &str) -> Option<String> {
    element.find_property(property)?;
    let value = element.property_value(property);
    let (_, value) = gst::glib::EnumValue::from_value(&value)?;
    Some(value.nick().to_owned())
}

pub(super) fn install(pipeline: &gst::Pipeline) -> Result<(), PlaybackSessionFailure> {
    // Validate the installed implementation before a stream can be requested.
    // All methods used here are available at the GStreamer 1.20 floor.
    let probe = gst::ElementFactory::make("deinterlace")
        .build()
        .map_err(|_| PlaybackSessionFailure::PipelineConstruction)?;
    configure(&probe)?;
    pipeline.connect("element-setup", false, |args| {
        if let Some(element) = args
            .get(1)
            .and_then(|value| value.get::<gst::Element>().ok())
            && element
                .factory()
                .is_some_and(|factory| factory.name() == "deinterlace")
        {
            if configure(&element).is_err() {
                gst::element_error!(
                    element,
                    gst::CoreError::MissingPlugin,
                    ("The software deinterlacer does not support the required configuration")
                );
            } else {
                tracing::debug!(target: "balun::playback", method = "yadif",
                    fields = "all", mode = "auto", "configured software deinterlacing");
            }
        }
        None
    });
    Ok(())
}

/// Inspect the actual filter and its negotiated output, without arbitrary
/// caps text or element names entering diagnostics.
pub(super) fn describe(pipeline: &gst::Element) -> String {
    let Some(bin) = pipeline.downcast_ref::<gst::Bin>() else {
        return "none".into();
    };
    let filters = bin
        .iterate_recurse()
        .into_iter()
        .flatten()
        .filter(|element| {
            element
                .factory()
                .is_some_and(|factory| factory.name() == "deinterlace")
        })
        .map(|element| {
            let method = enum_nick(&element, "method").unwrap_or_else(|| "unknown".into());
            let rate = element
                .static_pad("src")
                .and_then(|pad| pad.current_caps())
                .and_then(|caps| caps.structure(0)?.get::<gst::Fraction>("framerate").ok())
                .map_or_else(|| "not negotiated".into(), |rate| rate.to_string());
            format!("deinterlace method={method} output-framerate={rate}")
        })
        .collect::<Vec<_>>();
    if filters.is_empty() {
        "none".into()
    } else {
        filters.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Observation {
        frames: Vec<Vec<u8>>,
        rate: Option<gst::Fraction>,
        interlace_mode: Option<String>,
    }

    fn render(width: i32, height: i32, mode: &str, method: &str) -> Observation {
        gst::init().unwrap();
        let pipeline = gst::Pipeline::new();
        let source = gst::ElementFactory::make("appsrc")
            .property("format", gst::Format::Time)
            .build()
            .unwrap();
        let filter = gst::ElementFactory::make("deinterlace").build().unwrap();
        configure(&filter).unwrap();
        filter.set_property_from_str("method", method);
        let sink = gst::ElementFactory::make("fakesink")
            .property("sync", false)
            .property("signal-handoffs", true)
            .build()
            .unwrap();
        let caps = gst::Caps::builder("video/x-raw")
            .field("format", "I420")
            .field("width", width)
            .field("height", height)
            .field("framerate", gst::Fraction::new(30_000, 1_001))
            .field("interlace-mode", mode)
            .build();
        source.set_property("caps", &caps);
        pipeline.add_many([&source, &filter, &sink]).unwrap();
        gst::Element::link_many([&source, &filter, &sink]).unwrap();
        let output = Arc::new(Mutex::new(Observation {
            frames: Vec::new(),
            rate: None,
            interlace_mode: None,
        }));
        let recorded = Arc::clone(&output);
        sink.connect("handoff", false, move |args| {
            let buffer = args[1].get::<gst::Buffer>().unwrap();
            let pad = args[2].get::<gst::Pad>().unwrap();
            let caps = pad.current_caps().unwrap();
            let caps = caps.structure(0).unwrap();
            let mut recorded = recorded.lock().unwrap();
            recorded.rate = caps.get("framerate").ok();
            recorded.interlace_mode = caps.get::<String>("interlace-mode").ok();
            recorded
                .frames
                .push(buffer.map_readable().unwrap().to_vec());
            None
        });
        pipeline.set_state(gst::State::Playing).unwrap();
        let width = usize::try_from(width).unwrap();
        let height = usize::try_from(height).unwrap();
        for index in 0..12_u64 {
            let mut pixels = vec![128_u8; width * height * 3 / 2];
            for row in 0..height {
                pixels[row * width..(row + 1) * width].fill(if row % 4 < 2 { 32 } else { 224 });
            }
            let mut buffer = gst::Buffer::from_mut_slice(pixels);
            let data = buffer.get_mut().unwrap();
            data.set_pts(gst::ClockTime::from_nseconds(index * 1_001_000_000 / 30));
            data.set_duration(gst::ClockTime::from_nseconds(1_001_000_000 / 30));
            if mode != "progressive" {
                // GstVideoBufferFlags extend GstBufferFlags at FLAG_LAST:
                // INTERLACED = 1<<20 and TFF = 1<<21 (video-frame.h).
                data.set_flags(gst::BufferFlags::from_bits_retain((1 << 20) | (1 << 21)));
            }
            assert_eq!(
                source.emit_by_name::<gst::FlowReturn>("push-buffer", &[&buffer]),
                gst::FlowReturn::Ok
            );
        }
        assert_eq!(
            source.emit_by_name::<gst::FlowReturn>("end-of-stream", &[]),
            gst::FlowReturn::Ok
        );
        let terminal = pipeline.bus().unwrap().timed_pop_filtered(
            gst::ClockTime::from_seconds(10),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        );
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(
            matches!(
                terminal.as_ref().map(|m| m.view()),
                Some(gst::MessageView::Eos(_))
            ),
            "{terminal:?}"
        );
        std::mem::take(&mut *output.lock().unwrap())
    }

    #[test]
    fn software_deinterlacing_preserves_static_detail_and_field_rate() {
        for (width, height, mode) in [(528, 480, "mixed"), (1920, 1080, "interleaved")] {
            // Two-line static detail is blurred by the old linear default.
            // Single-line alternating fields are ambiguous with field motion
            // and are deliberately not treated as a reconstruction oracle.
            let linear = render(width, height, mode, "linear");
            let output = render(width, height, mode, "yadif");
            assert_eq!(output.rate, Some(gst::Fraction::new(60_000, 1_001)));
            assert_eq!(output.interlace_mode.as_deref(), Some("progressive"));
            assert!(output.frames.len() >= 20);
            let frame = &output.frames[output.frames.len() / 2];
            let offset = usize::try_from(width * 20 + width / 2).unwrap();
            for row in 0..8 {
                assert_eq!(
                    frame[offset + row * usize::try_from(width).unwrap()],
                    if row % 4 < 2 { 32 } else { 224 }
                );
            }
            let baseline = &linear.frames[linear.frames.len() / 2];
            assert!(baseline[offset..offset + usize::try_from(width * 4).unwrap()].contains(&128));
        }
    }

    #[test]
    fn software_deinterlacing_leaves_progressive_frames_unchanged() {
        let output = render(1280, 720, "progressive", "yadif");
        assert_eq!(output.rate, Some(gst::Fraction::new(30_000, 1_001)));
        assert_eq!(output.frames.len(), 12);
        for frame in output.frames {
            for row in 0..720 {
                assert!(
                    frame[row * 1280..(row + 1) * 1280]
                        .iter()
                        .all(|pixel| *pixel == if row % 4 < 2 { 32 } else { 224 })
                );
            }
        }
    }

    #[test]
    fn playsink_inserted_filters_receive_the_software_policy() {
        gst::init().unwrap();
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        install(&playbin).unwrap();
        let filter = gst::ElementFactory::make("deinterlace").build().unwrap();
        playbin.emit_by_name::<()>("element-setup", &[&filter]);
        assert_eq!(enum_nick(&filter, "method").as_deref(), Some("yadif"));
        assert_eq!(enum_nick(&filter, "mode").as_deref(), Some("auto"));
        assert_eq!(enum_nick(&filter, "fields").as_deref(), Some("all"));
        assert_eq!(enum_nick(&filter, "tff").as_deref(), Some("auto"));
    }
}
