//! Bounded, endpoint-free startup observations on the session's main context.

use std::time::Instant;

use super::TuneGeneration;

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Requested,
    PredecessorRetired,
    HandoffAccepted,
    GraphPrepared,
    PausedRequestReturned,
    StreamNoticeReceived,
    PlayingNoticeReceived,
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
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Outcome {
    PlayingNotice,
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
            Self::PlayingNotice => "playing_notice",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::Stopped => "stopped",
            Self::ShutDown => "shut_down",
            Self::Dropped => "dropped",
        }
    }
}

/// One fixed-size recorder per pending/connecting tune; no history or timers.
/// Only the session owner writes it, after the existing generation checks.
pub(super) struct TuneTiming {
    generation: TuneGeneration,
    started: Instant,
    seen: u8,
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
        };
        timing.record_at(Phase::Requested, started);
        timing
    }

    pub(super) fn record(&mut self, phase: Phase) {
        self.record_at(phase, Instant::now());
    }

    fn record_at(&mut self, phase: Phase, observed: Instant) {
        let bit = 1 << phase as u8;
        if self.seen & bit != 0 {
            return;
        }
        self.seen |= bit;
        tracing::debug!(
            target: "balun::playback::timing",
            generation = self.generation.get(),
            phase = phase.label(),
            elapsed_us = self.elapsed_us(observed),
            "tune startup phase"
        );
    }

    pub(super) fn finish(self, outcome: Outcome) {
        self.finish_at(outcome, Instant::now());
    }

    fn finish_at(self, outcome: Outcome, observed: Instant) {
        tracing::debug!(
            target: "balun::playback::timing",
            generation = self.generation.get(),
            outcome = outcome.label(),
            elapsed_us = self.elapsed_us(observed),
            "tune startup ended"
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
                Phase::StreamNoticeReceived,
                Phase::PlayingNoticeReceived,
            ]
            .into_iter()
            .enumerate()
            {
                let time = start + Duration::from_micros((index as u64 + 1) * 125);
                timing.record_at(phase, time);
                timing.record_at(phase, time + Duration::from_micros(7));
            }
            timing.finish_at(Outcome::PlayingNotice, start + Duration::from_micros(800));
        });
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(lines.len(), 8);
        for (index, line) in lines[..7].iter().enumerate() {
            assert!(line.contains("generation=27"), "{line}");
            assert!(
                line.ends_with(&format!("elapsed_us={}", index * 125)),
                "{line}"
            );
        }
        assert!(lines[7].ends_with("outcome=\"playing_notice\" elapsed_us=800"));
        assert!(lines[0].contains("phase=\"requested\""));
        assert!(lines[5].contains("phase=\"stream_notice_received\""));
        assert!(lines[6].contains("phase=\"playing_notice_received\""));
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
}
