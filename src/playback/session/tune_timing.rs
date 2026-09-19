//! Bounded, endpoint-free startup observations on the session's main context.

use std::sync::Arc;
use std::time::Instant;

use super::super::transport_timing::{TransportPhase, TransportTiming};
use super::TuneGeneration;
use super::media_observation::{MediaPhase, MediaTiming};

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Requested,
    PredecessorRetired,
    HandoffAccepted,
    GraphPrepared,
    PausedRequestReturned,
    StreamNoticeReceived,
    PlayingNoticeReceived,
    HttpRequestPolled,
    HttpResponseReceived,
    HttpBodyReceived,
    AppsrcBufferAccepted,
    VideoSinkBuffer,
    AudioSinkBuffer,
    PaintableInvalidated,
    MediaObserverIncomplete,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::PredecessorRetired => "predecessor_retired",
            Self::HandoffAccepted => "handoff_accepted",
            Self::GraphPrepared => "graph_prepared",
            Self::PausedRequestReturned => "paused_request_returned",
            Self::StreamNoticeReceived => "stream_notice_received",
            Self::PlayingNoticeReceived => "playing_notice_received",
            Self::HttpRequestPolled => "http_request_polled",
            Self::HttpResponseReceived => "http_response_received",
            Self::HttpBodyReceived => "http_body_received",
            Self::AppsrcBufferAccepted => "appsrc_buffer_accepted",
            Self::VideoSinkBuffer => "video_sink_buffer",
            Self::AudioSinkBuffer => "audio_sink_buffer",
            Self::PaintableInvalidated => "video_paintable_invalidated",
            Self::MediaObserverIncomplete => "media_observer_incomplete",
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Outcome {
    Failed,
    Cancelled,
    Superseded,
    Stopped,
    ShutDown,
    Dropped,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::Stopped => "stopped",
            Self::ShutDown => "shut_down",
            Self::Dropped => "dropped",
        }
    }
}

/// One fixed-size recorder per tune, retained through playback; no history or timers.
/// Only the session owner writes it, after the existing generation checks.
pub(super) struct TuneTiming {
    generation: TuneGeneration,
    started: Instant,
    seen: u16,
    transport: Arc<TransportTiming>,
    media: Arc<MediaTiming>,
}

impl TuneTiming {
    pub(super) fn new(generation: TuneGeneration) -> Self {
        Self::new_at(generation, Instant::now())
    }

    fn new_at(generation: TuneGeneration, started: Instant) -> Self {
        let mut timing = Self {
            generation,
            started,
            seen: 0,
            transport: Arc::new(TransportTiming::new(started)),
            media: Arc::new(MediaTiming::new(started)),
        };
        timing.record_at(Phase::Requested, started);
        timing
    }

    pub(super) fn record(&mut self, phase: Phase) {
        self.flush();
        self.record_at(phase, Instant::now());
    }

    fn record_at(&mut self, phase: Phase, observed: Instant) {
        self.record_offset(phase, self.elapsed_us(observed));
    }

    fn record_offset(&mut self, phase: Phase, elapsed_us: u64) {
        let bit = 1 << phase as u8;
        if self.seen & bit != 0 {
            return;
        }
        self.seen |= bit;
        tracing::debug!(
            target: "balun::playback::timing",
            generation = self.generation.get(),
            phase = phase.label(),
            elapsed_us,
            "tune startup phase"
        );
    }

    pub(super) fn transport(&self) -> Arc<TransportTiming> {
        Arc::clone(&self.transport)
    }

    pub(super) fn media(&self) -> Arc<MediaTiming> {
        Arc::clone(&self.media)
    }

    pub(super) fn flush(&mut self) {
        self.flush_transport();
        for (phase, value) in self.media.snapshot() {
            let phase = match phase {
                MediaPhase::VideoSinkBuffer => Phase::VideoSinkBuffer,
                MediaPhase::AudioSinkBuffer => Phase::AudioSinkBuffer,
                MediaPhase::PaintableInvalidated => Phase::PaintableInvalidated,
                MediaPhase::ObserverIncomplete => Phase::MediaObserverIncomplete,
            };
            if let Some(offset) = value {
                self.record_offset(phase, offset);
            }
        }
    }

    fn flush_transport(&mut self) {
        for (phase, value) in self.transport.snapshot() {
            let phase = match phase {
                TransportPhase::RequestPolled => Phase::HttpRequestPolled,
                TransportPhase::ResponseReceived => Phase::HttpResponseReceived,
                TransportPhase::BodyReceived => Phase::HttpBodyReceived,
                TransportPhase::BufferAccepted => Phase::AppsrcBufferAccepted,
            };
            if let Some(offset) = value {
                self.record_offset(phase, offset);
            }
        }
    }

    pub(super) fn finish(mut self, outcome: Outcome) {
        self.media.close();
        self.flush();
        self.finish_at(outcome, Instant::now());
    }

    fn finish_at(self, outcome: Outcome, observed: Instant) {
        tracing::debug!(
            target: "balun::playback::timing",
            generation = self.generation.get(),
            outcome = outcome.label(),
            elapsed_us = self.elapsed_us(observed),
            "tune observations ended"
        );
    }

    fn elapsed_us(&self, observed: Instant) -> u64 {
        observed
            .saturating_duration_since(self.started)
            .as_micros()
            .min(u128::from(u64::MAX)) as u64
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use super::*;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    pub(in super::super) fn capture(work: impl FnOnce()) -> String {
        // tracing-core's single-dispatch optimization consults the thread's
        // default when a callsite first registers. Another session test with
        // no scoped subscriber can otherwise cache "never" for our callsite.
        // Keep two live dispatches so registration considers both subscribers.
        let _interest_guard = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_max_level(tracing::Level::TRACE)
            .without_time()
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, work);
        String::from_utf8(capture.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter(|line| line.contains("balun::playback::timing:"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn phases_are_once_only_with_exact_monotonic_offsets_and_fixed_fields() {
        let start = Instant::now();
        let output = capture(|| {
            let mut timing = TuneTiming::new_at(TuneGeneration(27), start);
            for (index, phase) in [
                Phase::PredecessorRetired,
                Phase::HandoffAccepted,
                Phase::GraphPrepared,
                Phase::PausedRequestReturned,
                Phase::HttpRequestPolled,
                Phase::HttpResponseReceived,
                Phase::HttpBodyReceived,
                Phase::AppsrcBufferAccepted,
                Phase::StreamNoticeReceived,
                Phase::PlayingNoticeReceived,
                Phase::VideoSinkBuffer,
                Phase::AudioSinkBuffer,
                Phase::PaintableInvalidated,
                Phase::MediaObserverIncomplete,
            ]
            .into_iter()
            .enumerate()
            {
                let time = start + Duration::from_micros((index as u64 + 1) * 125);
                timing.record_at(phase, time);
                timing.record_at(phase, time + Duration::from_micros(7));
            }
            timing.finish_at(Outcome::Stopped, start + Duration::from_micros(2000));
        });
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 16);
        for (index, line) in lines[..15].iter().enumerate() {
            assert!(line.contains("generation=27"), "{line}");
            assert!(
                line.ends_with(&format!("elapsed_us={}", index * 125)),
                "{line}"
            );
        }
        assert!(lines[15].ends_with("outcome=\"stopped\" elapsed_us=2000"));
        assert!(lines[0].contains("phase=\"requested\""));
        assert!(lines[9].contains("phase=\"stream_notice_received\""));
        assert!(lines[10].contains("phase=\"playing_notice_received\""));
    }

    #[test]
    fn worker_offsets_survive_delayed_reduction_and_are_emitted_once() {
        let mut expected = [None; 4];
        let output = capture(|| {
            let mut timing = TuneTiming::new(TuneGeneration(28));
            let worker = timing.transport();
            std::thread::spawn(move || {
                worker.record(TransportPhase::RequestPolled);
                worker.record(TransportPhase::ResponseReceived);
                worker.record(TransportPhase::BodyReceived);
                worker.record(TransportPhase::BufferAccepted);
            })
            .join()
            .unwrap();
            let original = timing.transport.snapshot();
            expected = original.map(|(_, offset)| offset);
            timing.record(Phase::StreamNoticeReceived);
            timing.record(Phase::StreamNoticeReceived);
            for ((_, original), (_, still_first)) in
                original.into_iter().zip(timing.transport.snapshot())
            {
                assert_eq!(original, still_first);
            }
            timing.finish(Outcome::Failed);
        });
        assert_eq!(output.lines().count(), 7, "{output}");
        for (index, label) in [
            "http_request_polled",
            "http_response_received",
            "http_body_received",
            "appsrc_buffer_accepted",
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(output.matches(label).count(), 1, "{output}");
            assert!(
                output.contains(&format!(
                    "phase=\"{label}\" elapsed_us={}",
                    expected[index].unwrap()
                )),
                "{output}"
            );
        }
    }

    #[test]
    fn durations_saturate_and_abort_outcomes_use_closed_labels() {
        let start = Instant::now();
        let timing = TuneTiming::new_at(TuneGeneration(1), start);
        assert_eq!(timing.elapsed_us(start - Duration::from_secs(1)), 0);
        let output = capture(|| {
            for outcome in [
                Outcome::Failed,
                Outcome::Cancelled,
                Outcome::Superseded,
                Outcome::Stopped,
                Outcome::ShutDown,
                Outcome::Dropped,
            ] {
                TuneTiming::new_at(TuneGeneration(1), start).finish_at(outcome, start);
            }
        });
        for label in [
            "failed",
            "cancelled",
            "superseded",
            "stopped",
            "shut_down",
            "dropped",
        ] {
            assert_eq!(output.matches(&format!("outcome=\"{label}\"")).count(), 1);
        }
        assert_eq!(output.lines().count(), 12);
    }

    #[test]
    fn late_worker_observations_do_not_reopen_or_relabel_a_finished_generation() {
        let output = capture(|| {
            let old = TuneTiming::new(TuneGeneration(1));
            let late_worker = old.transport();
            old.finish(Outcome::Cancelled);
            let mut successor = TuneTiming::new(TuneGeneration(2));
            late_worker.record(TransportPhase::RequestPolled);
            successor.record(Phase::PredecessorRetired);
            successor.finish(Outcome::Stopped);
        });
        assert_eq!(output.lines().count(), 5, "{output}");
        assert!(!output.contains("http_request_polled"));
        assert_eq!(output.matches("generation=1").count(), 2);
        assert_eq!(output.matches("generation=2").count(), 3);
    }
}
