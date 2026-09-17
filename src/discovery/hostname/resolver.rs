//! Bounded ownership of system resolver work, independent of any Tokio runtime.
//!
//! The OS resolver cannot be cancelled portably. Its worker retains a global
//! permit until it actually exits, even when the async waiter times out or is
//! dropped. Saturation rejects immediately: there is no queue. Detached workers
//! may live until the OS call returns or the process exits, but there can never
//! be more than four in the process, across controller restarts too.

use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::Duration;

use tokio::sync::{Semaphore, oneshot};

use super::{
    ExactDiscoveryTarget, HOSTNAME_RESOLUTION_TIMEOUT, HostnameResolutionError, HostnameTarget,
    MAX_CONCURRENT_HOSTNAME_LOOKUPS, system_lookup,
};

type Lookup =
    dyn Fn(&str) -> Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError> + Send + Sync;

static SYSTEM_RESOLVER: LazyLock<HostnameResolver> = LazyLock::new(|| HostnameResolver {
    slots: Arc::new(Semaphore::new(MAX_CONCURRENT_HOSTNAME_LOOKUPS)),
    lookup: Arc::new(system_lookup),
    timeout: HOSTNAME_RESOLUTION_TIMEOUT,
});

#[derive(Clone)]
pub(crate) struct HostnameResolver {
    slots: Arc<Semaphore>,
    lookup: Arc<Lookup>,
    timeout: Duration,
}

impl Default for HostnameResolver {
    fn default() -> Self {
        SYSTEM_RESOLVER.clone()
    }
}

impl HostnameResolver {
    pub(crate) async fn resolve(
        &self,
        target: &HostnameTarget,
    ) -> Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError> {
        let slot = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| HostnameResolutionError::Busy)?;
        let name = target.name().to_owned();
        let lookup = Arc::clone(&self.lookup);
        let (sender, receiver) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("balun-hostname-resolver".to_owned())
            .spawn(move || {
                let _slot = slot;
                // A cancelled waiter cannot cause later discovery. If it
                // disappeared before dispatch, avoid the OS lookup as well.
                if !sender.is_closed() {
                    let _ = sender.send(lookup(&name));
                }
            })
            .map_err(|error| HostnameResolutionError::Lookup(error.kind()))?;
        // The permit, not a Tokio task or this handle, owns the actual work.
        drop(worker);

        tokio::time::timeout(self.timeout, receiver)
            .await
            .map_err(|_| HostnameResolutionError::Timeout)?
            .unwrap_or(Err(HostnameResolutionError::Lookup(
                std::io::ErrorKind::Other,
            )))
    }

    #[cfg(test)]
    pub(crate) fn with_lookup(
        capacity: usize,
        timeout: Duration,
        lookup: impl Fn(&str) -> Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            slots: Arc::new(Semaphore::new(capacity)),
            lookup: Arc::new(lookup),
            timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, mpsc};

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    fn blocked_resolver() -> (HostnameResolver, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (started, entered) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        let resolver = HostnameResolver::with_lookup(1, HOSTNAME_RESOLUTION_TIMEOUT, move |_| {
            started.send(()).unwrap();
            let _ = released.lock().unwrap().recv_timeout(WAIT);
            Ok(vec![
                ExactDiscoveryTarget::from_ip("192.0.2.7".parse().unwrap()).unwrap(),
            ])
        });
        (resolver, entered, release)
    }

    fn submit(
        resolver: &HostnameResolver,
    ) -> tokio::task::JoinHandle<Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError>> {
        let resolver = resolver.clone();
        tokio::spawn(async move {
            resolver
                .resolve(&HostnameTarget::parse("tuner.example").unwrap())
                .await
        })
    }

    async fn assert_saturated(resolver: &HostnameResolver) {
        for _ in 0..100 {
            assert_eq!(
                resolver
                    .resolve(&HostnameTarget::parse("other.example").unwrap())
                    .await,
                Err(HostnameResolutionError::Busy)
            );
        }
        assert_eq!(resolver.slots.available_permits(), 0);
    }

    async fn await_worker_exit(resolver: &HostnameResolver) {
        let slot = tokio::time::timeout(WAIT, Arc::clone(&resolver.slots).acquire_owned())
            .await
            .unwrap()
            .unwrap();
        drop(slot);
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_worker_keeps_its_slot_until_actual_exit() {
        let (resolver, entered, release) = blocked_resolver();
        let result = submit(&resolver);
        tokio::task::yield_now().await;
        entered.recv_timeout(WAIT).unwrap();
        tokio::time::advance(HOSTNAME_RESOLUTION_TIMEOUT).await;
        assert_eq!(result.await.unwrap(), Err(HostnameResolutionError::Timeout));
        tokio::time::resume();
        assert_saturated(&resolver).await;
        assert!(
            entered.try_recv().is_err(),
            "busy requests must not create more resolver work"
        );
        release.send(()).unwrap();
        await_worker_exit(&resolver).await;
    }

    #[tokio::test]
    async fn cancelled_waiter_keeps_its_slot_and_discards_the_late_result() {
        let (resolver, entered, release) = blocked_resolver();
        let result = submit(&resolver);
        tokio::task::yield_now().await;
        entered.recv_timeout(WAIT).unwrap();
        result.abort();
        assert!(result.await.unwrap_err().is_cancelled());
        assert_saturated(&resolver).await;
        release.send(()).unwrap();
        await_worker_exit(&resolver).await;
        assert_eq!(resolver.slots.available_permits(), 1);
    }

    #[test]
    fn every_default_resolver_shares_the_process_wide_admission_limit() {
        let first = HostnameResolver::default();
        let second = HostnameResolver::default();
        assert!(Arc::ptr_eq(&first.slots, &second.slots));
        assert_eq!(MAX_CONCURRENT_HOSTNAME_LOOKUPS, 4);
    }
}
