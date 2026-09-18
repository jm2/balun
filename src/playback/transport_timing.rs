//! Four fixed, first-observation timestamps shared with the private workers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

#[derive(Clone, Copy)]
pub(super) enum TransportPhase {
    RequestPolled,
    ResponseReceived,
    BodyReceived,
    BufferAccepted,
}

const PHASES: [TransportPhase; 4] = [
    TransportPhase::RequestPolled,
    TransportPhase::ResponseReceived,
    TransportPhase::BodyReceived,
    TransportPhase::BufferAccepted,
];

pub(super) struct TransportTiming {
    started: Instant,
    // Zero means absent; stored microseconds have a one-unit bias.
    offsets: [AtomicU64; 4],
}

impl TransportTiming {
    pub(super) fn new(started: Instant) -> Self {
        Self {
            started,
            offsets: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    pub(super) fn record(&self, phase: TransportPhase) {
        self.record_at(phase, Instant::now());
    }

    fn record_at(&self, phase: TransportPhase, observed: Instant) {
        let micros = observed
            .saturating_duration_since(self.started)
            .as_micros()
            .min(u128::from(u64::MAX - 1)) as u64;
        let _ = self.offsets[phase as usize].compare_exchange(
            0,
            micros + 1,
            Ordering::Release,
            Ordering::Relaxed,
        );
    }

    pub(super) fn snapshot(&self) -> [(TransportPhase, Option<u64>); 4] {
        PHASES.map(|phase| {
            let value = self.offsets[phase as usize].load(Ordering::Acquire);
            (phase, value.checked_sub(1))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn fixed_worker_slots_preserve_zero_and_first_offsets_without_cross_tune_state() {
        let start = Instant::now();
        let first = TransportTiming::new(start);
        let second = TransportTiming::new(start);
        for (index, phase) in PHASES.into_iter().enumerate() {
            first.record_at(phase, start + Duration::from_micros(index as u64));
            first.record_at(phase, start + Duration::from_secs(5));
        }
        for (index, (_, value)) in first.snapshot().into_iter().enumerate() {
            assert_eq!(value, Some(index as u64));
        }
        assert!(second.snapshot().iter().all(|(_, value)| value.is_none()));
    }
}
