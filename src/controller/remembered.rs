//! GTK-free decisions about remembered exact-address targets.
//!
//! The discovery lane is supersedable and its snapshots never carry the
//! probed address, so the desktop keeps the admitted target itself and uses
//! these helpers to decide, from successive discovery states alone, when a
//! target proved reachable and when the next remembered target may be sent.

use std::cmp::Ordering;
use std::collections::VecDeque;

use crate::discovery::ExactDiscoveryTarget;
use crate::settings::MAX_REMEMBERED_TARGETS;

use super::state::{
    DiscoveryKind, DiscoveryState, DiscoveryStatus, ExactSearchTicket, OperationGeneration,
};

/// Tracks one admitted exact target until its own search settles.
///
/// A target is reported back only when its search reaches `Ready`, which the
/// controller publishes solely after a direct reply from that address. The
/// search is identified by its [`ExactSearchTicket`]: states published before
/// the controller processed it, including a network-change republish of an
/// earlier result, are ignored, and a snapshot whose count has moved past the
/// ticket means a later exact search superseded it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExactTargetTracker {
    pending: Option<(ExactDiscoveryTarget, ExactSearchTicket)>,
}

/// What one discovery state did to the pending exact target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactSearchOutcome {
    /// Nothing is pending, the search has not been processed, or it is still
    /// running.
    Pending,
    /// The pending target answered; remember it.
    Reachable(ExactDiscoveryTarget),
    /// The pending target's own search ended without a valid reply, or a
    /// local or routed search took over its lane; the state says which.
    Settled,
    /// A later exact search was processed before this target's result was
    /// observed.
    Superseded,
}

impl ExactTargetTracker {
    #[must_use]
    pub const fn new() -> Self {
        Self { pending: None }
    }

    /// Record that `target` was admitted with `ticket`. A later admission
    /// replaces an unsettled one.
    pub fn admit(&mut self, target: ExactDiscoveryTarget, ticket: ExactSearchTicket) {
        self.pending = Some((target, ticket));
    }

    /// Whether an admitted target is still waiting for its outcome.
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Feed the newest discovery state and the snapshot's count of processed
    /// exact searches.
    pub fn observe(
        &mut self,
        discovery: DiscoveryState,
        exact_searches: u64,
    ) -> ExactSearchOutcome {
        let Some((target, ticket)) = self.pending else {
            return ExactSearchOutcome::Pending;
        };
        match exact_searches.cmp(&ticket.get()) {
            Ordering::Less => ExactSearchOutcome::Pending,
            Ordering::Greater => {
                self.pending = None;
                ExactSearchOutcome::Superseded
            }
            Ordering::Equal => match (discovery.kind(), discovery.status()) {
                (_, DiscoveryStatus::Refreshing) => ExactSearchOutcome::Pending,
                (DiscoveryKind::Exact, DiscoveryStatus::Ready) => {
                    self.pending = None;
                    ExactSearchOutcome::Reachable(target)
                }
                _ => {
                    self.pending = None;
                    ExactSearchOutcome::Settled
                }
            },
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

    fn ticket(value: u64) -> ExactSearchTicket {
        ExactSearchTicket::new(value)
    }

    #[test]
    fn tracker_reports_a_target_only_when_its_own_search_is_ready() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), ticket(1));
        assert!(tracker.is_pending());

        assert_eq!(
            tracker.observe(DiscoveryState::idle(generation(3)), 0),
            ExactSearchOutcome::Pending,
            "a state published before the search was processed is ignored"
        );
        assert_eq!(
            tracker.observe(
                DiscoveryState::refreshing_for(generation(4), DiscoveryKind::Exact),
                1
            ),
            ExactSearchOutcome::Pending
        );
        assert!(
            tracker.is_pending(),
            "an in-flight probe keeps the target pending"
        );
        assert_eq!(
            tracker.observe(
                DiscoveryState::ready_for(generation(4), DiscoveryKind::Exact, 0),
                1
            ),
            ExactSearchOutcome::Reachable(target(1))
        );
        assert!(!tracker.is_pending());
        assert_eq!(
            tracker.observe(
                DiscoveryState::ready_for(generation(5), DiscoveryKind::Exact, 0),
                2
            ),
            ExactSearchOutcome::Pending,
            "a reported target is not reported twice"
        );
    }

    #[test]
    fn tracker_settles_on_every_other_outcome_of_its_own_search() {
        let outcomes = [
            DiscoveryState::exact_no_response(generation(2), 1),
            DiscoveryState::failed_for(
                generation(2),
                DiscoveryKind::Exact,
                DiscoveryFailure::Internal,
            ),
            DiscoveryState::failed_for(
                generation(2),
                DiscoveryKind::Exact,
                DiscoveryFailure::ExactTargetLimitReached,
            ),
            DiscoveryState::ready_for(generation(2), DiscoveryKind::Local, 0),
            DiscoveryState::idle_for(generation(2), DiscoveryKind::Exact),
        ];
        for outcome in outcomes {
            let mut tracker = ExactTargetTracker::new();
            tracker.admit(target(1), ticket(4));
            assert_eq!(
                tracker.observe(outcome, 4),
                ExactSearchOutcome::Settled,
                "{outcome:?}"
            );
            assert!(!tracker.is_pending(), "{outcome:?}");
        }
    }

    #[test]
    fn tracker_ignores_states_published_before_its_search_was_processed() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), ticket(8));

        // An earlier exact search's result still in the receiver, or a
        // network-change republish of any current state, carries a lower
        // count and cannot be this search's outcome.
        for earlier in [
            DiscoveryState::ready_for(generation(4), DiscoveryKind::Exact, 0),
            DiscoveryState::exact_no_response(generation(4), 1),
            DiscoveryState::ready_for(generation(5), DiscoveryKind::Local, 0),
            DiscoveryState::idle_for(generation(5), DiscoveryKind::Exact),
        ] {
            assert_eq!(
                tracker.observe(earlier, 7),
                ExactSearchOutcome::Pending,
                "{earlier:?}"
            );
            assert!(tracker.is_pending(), "{earlier:?}");
        }

        // The search's own result settles it even when its start, its result,
        // and a later reconciliation were coalesced into one snapshot.
        assert_eq!(
            tracker.observe(
                DiscoveryState::ready_for(generation(7), DiscoveryKind::Exact, 0),
                8
            ),
            ExactSearchOutcome::Reachable(target(1))
        );
        assert!(!tracker.is_pending());
    }

    #[test]
    fn tracker_reports_a_later_exact_search_as_superseding() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), ticket(2));
        assert_eq!(
            tracker.observe(
                DiscoveryState::ready_for(generation(9), DiscoveryKind::Exact, 0),
                3
            ),
            ExactSearchOutcome::Superseded,
            "a later search's reply is never attributed to the pending target"
        );
        assert!(!tracker.is_pending());
    }

    #[test]
    fn tracker_keeps_only_the_latest_admission() {
        let mut tracker = ExactTargetTracker::new();
        tracker.admit(target(1), ticket(1));
        tracker.admit(target(2), ticket(2));

        assert_eq!(
            tracker.observe(
                DiscoveryState::ready_for(generation(3), DiscoveryKind::Exact, 0),
                2
            ),
            ExactSearchOutcome::Reachable(target(2))
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
