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
}

impl RediscoveryQueue {
    /// Queue the targets in order, dropping repeats and anything beyond the
    /// remembered-target limit.
    #[must_use]
    pub fn new(targets: impl IntoIterator<Item = ExactDiscoveryTarget>) -> Self {
        let mut queue = VecDeque::new();
        for target in targets {
            if queue.len() >= MAX_REMEMBERED_TARGETS {
                break;
            }
            if !queue.contains(&target) {
                queue.push_back(target);
            }
        }
        Self {
            queue,
            awaiting: None,
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
    }

    /// Feed the newest discovery state; returns the next target to send when
    /// the lane is idle and the previous send has settled.
    pub fn next(&mut self, discovery: DiscoveryState) -> Option<ExactDiscoveryTarget> {
        if let Some(sent_at) = self.awaiting {
            if discovery.generation() <= sent_at
                || discovery.status() == DiscoveryStatus::Refreshing
            {
                return None;
            }
            self.awaiting = None;
        } else if discovery.status() == DiscoveryStatus::Refreshing {
            return None;
        }
        let target = self.queue.pop_front()?;
        self.awaiting = Some(discovery.generation());
        Some(target)
    }

    /// Put a target back at the front after its command could not be queued.
    pub fn send_failed(&mut self, target: ExactDiscoveryTarget) {
        self.awaiting = None;
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
    fn queue_sends_one_target_per_settled_operation() {
        let mut queue = RediscoveryQueue::new([target(1), target(2)]);

        assert_eq!(
            queue.next(DiscoveryState::idle(generation(0))),
            Some(target(1))
        );
        assert_eq!(
            queue.next(DiscoveryState::idle(generation(0))),
            None,
            "the same stale snapshot cannot trigger a second send"
        );
        assert_eq!(
            queue.next(DiscoveryState::refreshing_for(
                generation(1),
                DiscoveryKind::Exact
            )),
            None
        );
        assert_eq!(
            queue.next(DiscoveryState::exact_no_response(generation(1), 0)),
            Some(target(2)),
            "a settled probe releases the next target even without a reply"
        );
        assert_eq!(
            queue.next(DiscoveryState::ready_for(
                generation(2),
                DiscoveryKind::Exact,
                0
            )),
            None
        );
        assert!(queue.is_settled());
    }

    #[test]
    fn queue_waits_for_a_user_started_operation() {
        let mut queue = RediscoveryQueue::new([target(1)]);

        assert_eq!(
            queue.next(DiscoveryState::refreshing(generation(4))),
            None,
            "a local refresh in flight is never superseded"
        );
        assert_eq!(
            queue.next(DiscoveryState::ready(generation(4), 0)),
            Some(target(1))
        );
    }

    #[test]
    fn queue_requeues_a_failed_send_and_cancels_cleanly() {
        let mut queue = RediscoveryQueue::new([target(1), target(2)]);
        let first = queue
            .next(DiscoveryState::idle(generation(0)))
            .expect("first send");
        queue.send_failed(first);
        assert_eq!(queue.remaining(), 2);
        assert_eq!(
            queue.next(DiscoveryState::idle(generation(0))),
            Some(target(1))
        );

        queue.cancel();
        assert!(queue.is_settled());
        assert_eq!(queue.next(DiscoveryState::idle(generation(1))), None);
    }
}
