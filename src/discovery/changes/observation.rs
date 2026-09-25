//! Readiness and health of network-change observation.
//!
//! A change stream alone cannot show that observation has started or is
//! still healthy. The watcher therefore publishes one more signal beside
//! its debounced changes: [`ObservationState::Ready`] once a baseline is
//! established with no change pending, and [`ObservationState::Unavailable`]
//! the moment a change is detected, observation ends, or it has not started.
//! Every return to readiness carries a new [`ObservationGeneration`], so
//! authority bound to an earlier generation never becomes valid again, even
//! if the topology later looks unchanged.
//!
//! Publishing is synchronous and never blocks, so a platform notification
//! callback can revoke readiness at the moment of detection, before any
//! debounce. Nothing here sends a packet or names an interface.

use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

/// One continuous period of healthy observation from an established baseline.
///
/// Generations only grow: a detected change, a loss of observation, or a
/// resubscription ends the current one, and the next baseline starts another.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObservationGeneration(NonZeroU64);

impl ObservationGeneration {
    /// The first generation a fresh gate publishes.
    pub const FIRST: Self = Self(NonZeroU64::MIN);

    /// A generation with this value, or `None` for zero.
    #[must_use]
    pub const fn new(value: u64) -> Option<Self> {
        match NonZeroU64::new(value) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Debug for ObservationGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ObservationGeneration({})", self.0)
    }
}

/// Whether network-change observation is currently healthy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ObservationState {
    /// Not observing, not yet established, lost, or a change is pending.
    #[default]
    Unavailable,
    /// Observing from an established baseline with no change pending.
    Ready(ObservationGeneration),
}

impl ObservationState {
    /// The healthy generation, or `None` while unavailable.
    #[must_use]
    pub const fn generation(self) -> Option<ObservationGeneration> {
        match self {
            Self::Ready(generation) => Some(generation),
            Self::Unavailable => None,
        }
    }

    /// Whether this state is exactly `generation` and still healthy.
    #[must_use]
    pub fn is_ready_for(self, generation: ObservationGeneration) -> bool {
        self == Self::Ready(generation)
    }
}

/// The publishing half, owned by one change source and shared with the
/// platform callbacks of each observation attempt.
#[derive(Clone)]
pub struct ObservationGate {
    inner: Arc<GateInner>,
}

struct GateInner {
    state: watch::Sender<ObservationState>,
    /// The generation the next establishment will publish.
    next: AtomicU64,
}

impl Drop for GateInner {
    /// A source that stops publishing is no longer observing.
    fn drop(&mut self) {
        self.state.send_replace(ObservationState::Unavailable);
    }
}

impl ObservationGate {
    /// A gate that starts unavailable.
    #[must_use]
    pub fn new() -> Self {
        let (state, _) = watch::channel(ObservationState::Unavailable);
        Self {
            inner: Arc::new(GateInner {
                state,
                next: AtomicU64::new(ObservationGeneration::FIRST.get()),
            }),
        }
    }

    /// Declare a baseline established with no change pending. From
    /// unavailability this publishes a new generation; while already ready it
    /// changes nothing.
    pub fn establish(&self) {
        self.inner.state.send_if_modified(|state| {
            if matches!(state, ObservationState::Ready(_)) {
                return false;
            }
            let value = self.inner.next.fetch_add(1, Ordering::AcqRel);
            // Generations are not recycled; a gate that somehow exhausted
            // them stays unavailable rather than repeating one.
            let Some(generation) = ObservationGeneration::new(value) else {
                return false;
            };
            *state = ObservationState::Ready(generation);
            true
        });
    }

    /// Revoke readiness at once: a change was detected or observation ended.
    /// Safe to call from any thread, including a platform callback.
    pub fn invalidate(&self) {
        self.inner.state.send_if_modified(|state| {
            let ready = matches!(state, ObservationState::Ready(_));
            *state = ObservationState::Unavailable;
            ready
        });
    }

    /// The state published right now.
    #[must_use]
    pub fn state(&self) -> ObservationState {
        *self.inner.state.borrow()
    }

    /// A receiver for the controller or a scan.
    #[must_use]
    pub fn watch(&self) -> ObservationWatch {
        ObservationWatch {
            receiver: self.inner.state.subscribe(),
        }
    }
}

impl Default for ObservationGate {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for ObservationGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationGate")
            .field("state", &self.state())
            .finish()
    }
}

/// The receiving half: the live state, and a wake-up when it changes.
#[derive(Clone)]
pub struct ObservationWatch {
    receiver: watch::Receiver<ObservationState>,
}

impl ObservationWatch {
    /// A watch that is, and stays, unavailable: nothing observes this system.
    #[must_use]
    pub fn unavailable() -> Self {
        let (_, receiver) = watch::channel(ObservationState::Unavailable);
        Self { receiver }
    }

    /// The live state. A gate that no longer exists is unavailable.
    #[must_use]
    pub fn current(&self) -> ObservationState {
        if self.receiver.has_changed().is_err() {
            return ObservationState::Unavailable;
        }
        *self.receiver.borrow()
    }

    /// Whether the gate is gone, so the state can never change again.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.receiver.has_changed().is_err()
    }

    /// Whether observation is still healthy in exactly `generation`.
    #[must_use]
    pub fn is_ready_for(&self, generation: ObservationGeneration) -> bool {
        self.current().is_ready_for(generation)
    }

    /// Wait until the published state may have changed and return it. Once
    /// the gate is gone this returns [`ObservationState::Unavailable`]
    /// immediately, every time.
    pub async fn changed(&mut self) -> ObservationState {
        if self.receiver.changed().await.is_err() {
            return ObservationState::Unavailable;
        }
        *self.receiver.borrow_and_update()
    }

    /// Mark the current state as seen, so [`Self::changed`] waits for the
    /// next publication.
    pub fn mark_seen(&mut self) -> ObservationState {
        if self.receiver.has_changed().is_err() {
            return ObservationState::Unavailable;
        }
        *self.receiver.borrow_and_update()
    }
}

impl fmt::Debug for ObservationWatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ObservationWatch")
            .field("state", &self.current())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_gate_is_unavailable_and_establishes_increasing_generations() {
        let gate = ObservationGate::new();
        let watch = gate.watch();
        assert_eq!(gate.state(), ObservationState::Unavailable);
        assert_eq!(watch.current(), ObservationState::Unavailable);

        gate.establish();
        let first = gate.state().generation().unwrap();
        assert_eq!(first, ObservationGeneration::FIRST);
        // Establishing again while ready keeps the generation.
        gate.establish();
        assert!(watch.is_ready_for(first));

        gate.invalidate();
        assert_eq!(watch.current(), ObservationState::Unavailable);
        assert!(!watch.is_ready_for(first));
        gate.invalidate();

        gate.establish();
        let second = watch.current().generation().unwrap();
        assert!(second > first, "a return to readiness is a new generation");
        assert!(!watch.is_ready_for(first));
        assert_eq!(second.get(), 2);
        assert!(format!("{gate:?} {watch:?}").contains("ObservationGeneration(2)"));
    }

    #[test]
    fn a_dropped_gate_is_unavailable_to_every_watch() {
        let gate = ObservationGate::new();
        let watch = gate.watch();
        gate.establish();
        assert!(watch.current().generation().is_some());
        drop(gate);
        assert_eq!(watch.current(), ObservationState::Unavailable);
        assert_eq!(
            ObservationWatch::unavailable().current(),
            ObservationState::Unavailable
        );
        assert_eq!(ObservationGeneration::new(0), None);
        assert_eq!(ObservationState::default(), ObservationState::Unavailable);
    }

    #[tokio::test]
    async fn watchers_wake_on_every_transition_and_after_the_gate_is_gone() {
        let gate = ObservationGate::new();
        let mut watch = gate.watch();
        assert_eq!(watch.mark_seen(), ObservationState::Unavailable);

        gate.establish();
        let ready = watch.changed().await;
        assert!(matches!(ready, ObservationState::Ready(_)));

        let invalidating = gate.clone();
        let waiter = tokio::spawn(async move { watch.changed().await });
        tokio::task::yield_now().await;
        std::thread::spawn(move || invalidating.invalidate())
            .join()
            .unwrap();
        assert_eq!(waiter.await.unwrap(), ObservationState::Unavailable);

        let mut watch = gate.watch();
        assert!(!watch.is_closed());
        drop(gate);
        assert!(watch.is_closed());
        assert_eq!(watch.changed().await, ObservationState::Unavailable);
        assert_eq!(watch.changed().await, ObservationState::Unavailable);
        assert_eq!(watch.mark_seen(), ObservationState::Unavailable);
    }
}
