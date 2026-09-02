//! GTK-free decisions about remembered exact-address targets.
//!
//! The discovery lane is supersedable and its snapshots never carry the
//! probed address, so the desktop keeps the admitted target itself and uses
//! these helpers to decide, from successive discovery states alone, when a
//! target proved reachable and when the next remembered target may be sent.

use std::collections::VecDeque;

use crate::discovery::ExactDiscoveryTarget;
use crate::settings::MAX_REMEMBERED_TARGETS;

use super::state::{DiscoveryKind, DiscoveryState, DiscoveryStatus, OperationGeneration};

/// Tracks one admitted exact target until its probe settles.
///
/// A target is reported back only when a newer exact operation reaches
/// `Ready`, which the controller publishes solely after a direct reply from
/// that address. Any other settled outcome, including a superseding local
/// refresh, discards the pending target without reporting it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExactTargetTracker {
    pending: Option<(ExactDiscoveryTarget, OperationGeneration)>,
}

impl ExactTargetTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Record that `target` was admitted while the discovery lane showed
    /// `observed`. A later admission replaces an unsettled one.
    pub fn admit(&mut self, target: ExactDiscoveryTarget, observed: OperationGeneration) {
        self.pending = Some((target, observed));
    }

    /// Whether an admitted target is still waiting for its outcome.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed the newest discovery state; returns the target that proved
    /// reachable, if this state settled it successfully.
    pub fn observe(&mut self, discovery: DiscoveryState) -> Option<ExactDiscoveryTarget> {
        let (target, admitted_at) = self.pending?;
        if discovery.generation() <= admitted_at {
            return None;
        }
        match (discovery.kind(), discovery.status()) {
            (_, DiscoveryStatus::Refreshing) => None,
            (DiscoveryKind::Exact, DiscoveryStatus::Ready) => {
                self.pending = None;
                Some(target)
            }
            _ => {
                self.pending = None;
                None
            }
        }
    }
}

/// Sequences remembered targets through the single supersedable discovery
/// lane, one probe at a time, without cancelling a probe the user started.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RediscoveryQueue {
    queue: VecDeque<ExactDiscoveryTarget>,
    awaiting: Option<OperationGeneration>,
    in_flight: Option<ExactDiscoveryTarget>,
}

/// What one discovery state lets the queue do: report the queued target
/// whose probe just received a valid reply, and hand out the next target to
/// send.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RediscoveryStep {
    pub reachable: Option<ExactDiscoveryTarget>,
    pub send: Option<ExactDiscoveryTarget>,
}

impl RediscoveryQueue {
    /// Queue the targets in order, dropping repeats and anything beyond the
    /// remembered-target limit.
    #[must_use]
    pub fn new(targets: impl IntoIterator<Item = ExactDiscoveryTarget>) -> Self {
        let mut queue = Self::default();
        queue.enqueue(targets);
        queue
    }

    /// Append targets, dropping repeats and anything beyond the
    /// remembered-target limit.
    pub fn enqueue(&mut self, targets: impl IntoIterator<Item = ExactDiscoveryTarget>) {
        for target in targets {
            if self.queue.len() >= MAX_REMEMBERED_TARGETS {
                break;
            }
            if !self.queue.contains(&target) && self.in_flight != Some(target) {
                self.queue.push_back(target);
            }
        }
    }

    /// Targets not yet sent.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.queue.len()
    }

    /// Whether nothing is queued or in flight.
    #[must_use]
    pub fn is_settled(&self) -> bool {
        self.queue.is_empty() && self.awaiting.is_none()
    }

    /// Drop every queued target; an in-flight probe finishes on its own.
    pub fn cancel(&mut self) {
        self.queue.clear();
        self.awaiting = None;
        self.in_flight = None;
    }

    /// Feed the newest discovery state. When the previous send has settled,
    /// report it as reachable if a newer exact operation reached `Ready`,
    /// and hand out the next target once the lane is idle.
    pub fn advance(&mut self, discovery: DiscoveryState) -> RediscoveryStep {
        let mut step = RediscoveryStep::default();
        if let Some(sent_at) = self.awaiting {
            if discovery.generation() <= sent_at
                || discovery.status() == DiscoveryStatus::Refreshing
            {
                return step;
            }
            self.awaiting = None;
            let settled = self.in_flight.take();
            if discovery.kind() == DiscoveryKind::Exact
                && discovery.status() == DiscoveryStatus::Ready
            {
                step.reachable = settled;
            }
        } else if discovery.status() == DiscoveryStatus::Refreshing {
            return step;
        }
        if let Some(target) = self.queue.pop_front() {
            self.awaiting = Some(discovery.generation());
            self.in_flight = Some(target);
            step.send = Some(target);
        }
        step
    }

    /// Put a target back at the front after its command could not be queued.
    pub fn send_failed(&mut self, target: ExactDiscoveryTarget) {
        self.awaiting = None;
        self.in_flight = None;
        self.queue.push_front(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::DiscoveryFailure;

    fn target(last_octet: u8) -> ExactDiscoveryTarget {
        ExactDiscoveryTarget::parse(&format!("192.0.2.{last_octet}")).expect("valid target")
    }

    const fn generation(value: u64) -> OperationGeneration {
        OperationGeneration::new(value)
    }

    #[test]
    fn tracker_reports_a_target_only_after_a_newer_exact_ready_state() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), generation(3));
        assert!(tracker.is_pending());

        assert_eq!(tracker.observe(DiscoveryState::idle(generation(3))), None);
        assert_eq!(
            tracker.observe(DiscoveryState::refreshing_for(
                generation(4),
                DiscoveryKind::Exact
            )),
            None
        );
        assert!(
            tracker.is_pending(),
            "an in-flight probe keeps the target pending"
        );
        assert_eq!(
            tracker.observe(DiscoveryState::ready_for(
                generation(4),
                DiscoveryKind::Exact,
                0
            )),
            Some(target(1))
        );
        assert!(!tracker.is_pending());
        assert_eq!(
            tracker.observe(DiscoveryState::ready_for(
                generation(5),
                DiscoveryKind::Exact,
                0
            )),
            None,
            "a reported target is not reported twice"
        );
    }

    #[test]
    fn tracker_discards_the_target_on_every_other_settled_outcome() {
        let outcomes = [
            DiscoveryState::exact_no_response(generation(2), 1),
            DiscoveryState::failed_for(
                generation(2),
                DiscoveryKind::Exact,
                DiscoveryFailure::Internal,
            ),
            DiscoveryState::ready_for(generation(2), DiscoveryKind::Local, 0),
            DiscoveryState::idle_for(generation(2), DiscoveryKind::Exact),
        ];
        for outcome in outcomes {
            let mut tracker = ExactTargetTracker::new();
            tracker.admit(target(1), generation(1));
            assert_eq!(tracker.observe(outcome), None, "{outcome:?}");
            assert!(!tracker.is_pending(), "{outcome:?}");
        }
    }

    #[test]
    fn tracker_keeps_only_the_latest_admission() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), generation(1));
        tracker.admit(target(2), generation(2));

        assert_eq!(
            tracker.observe(DiscoveryState::ready_for(
                generation(3),
                DiscoveryKind::Exact,
                0
            )),
            Some(target(2))
        );
    }

    #[test]
    fn queue_deduplicates_and_caps_its_input() {
        let targets = (1..=40).map(target).chain([target(1), target(2)]);
        let queue = RediscoveryQueue::new(targets);
        assert_eq!(queue.remaining(), MAX_REMEMBERED_TARGETS);

        let queue = RediscoveryQueue::new([target(1), target(1), target(2)]);
        assert_eq!(queue.remaining(), 2);
    }

    #[test]
    fn queue_sends_one_target_per_settled_operation_and_reports_replies() {
        let mut queue = RediscoveryQueue::new([target(1), target(2)]);

        assert_eq!(
            queue.advance(DiscoveryState::idle(generation(0))),
            RediscoveryStep {
                reachable: None,
                send: Some(target(1))
            }
        );
        assert_eq!(
            queue.advance(DiscoveryState::idle(generation(0))),
            RediscoveryStep::default(),
            "the same stale snapshot cannot trigger a second send"
        );
        assert_eq!(
            queue.advance(DiscoveryState::refreshing_for(
                generation(1),
                DiscoveryKind::Exact
            )),
            RediscoveryStep::default()
        );
        assert_eq!(
            queue.advance(DiscoveryState::exact_no_response(generation(1), 0)),
            RediscoveryStep {
                reachable: None,
                send: Some(target(2))
            },
            "a settled probe releases the next target even without a reply"
        );
        assert_eq!(
            queue.advance(DiscoveryState::ready_for(
                generation(2),
                DiscoveryKind::Exact,
                0
            )),
            RediscoveryStep {
                reachable: Some(target(2)),
                send: None
            },
            "a valid reply reports the in-flight target"
        );
        assert!(queue.is_settled());
    }

    #[test]
    fn queue_waits_for_a_user_started_operation() {
        let mut queue = RediscoveryQueue::new([target(1)]);

        assert_eq!(
            queue.advance(DiscoveryState::refreshing(generation(4))),
            RediscoveryStep::default(),
            "a local refresh in flight is never superseded"
        );
        assert_eq!(
            queue.advance(DiscoveryState::ready(generation(4), 0)).send,
            Some(target(1))
        );
        assert_eq!(
            queue
                .advance(DiscoveryState::ready_for(
                    generation(5),
                    DiscoveryKind::Local,
                    0
                ))
                .reachable,
            None,
            "a superseding local result never reports the queued target"
        );
    }

    #[test]
    fn queue_enqueues_later_targets_without_duplicating_the_one_in_flight() {
        let mut queue = RediscoveryQueue::new([target(1)]);
        assert_eq!(
            queue.advance(DiscoveryState::idle(generation(0))).send,
            Some(target(1))
        );
        queue.enqueue([target(1), target(2), target(2)]);
        assert_eq!(queue.remaining(), 1);
        assert_eq!(
            queue
                .advance(DiscoveryState::exact_no_response(generation(1), 0))
                .send,
            Some(target(2))
        );
    }

    #[test]
    fn queue_requeues_a_failed_send_and_cancels_cleanly() {
        let mut queue = RediscoveryQueue::new([target(1), target(2)]);
        let first = queue
            .advance(DiscoveryState::idle(generation(0)))
            .send
            .expect("first send");
        queue.send_failed(first);
        assert_eq!(queue.remaining(), 2);
        assert_eq!(
            queue.advance(DiscoveryState::idle(generation(0))).send,
            Some(target(1))
        );

        queue.cancel();
        assert!(queue.is_settled());
        assert_eq!(
            queue.advance(DiscoveryState::idle(generation(1))),
            RediscoveryStep::default()
        );
    }
}
