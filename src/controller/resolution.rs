//! One-shot hostname resolution delivered through the controller's runtime.

use std::fmt;

use tokio::sync::oneshot;

use crate::discovery::{ExactDiscoveryTarget, HostnameResolutionError};

/// Receives the bounded exact targets one hostname resolved to.
///
/// The resolver never publishes addresses through the shared snapshot; only
/// the caller that submitted the name receives them, and dropping either side
/// drops the result.
#[must_use = "the resolution result must be received or deliberately dropped"]
pub struct HostnameResolutionReceiver {
    receiver: oneshot::Receiver<Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError>>,
}

impl HostnameResolutionReceiver {
    pub(super) const fn new(
        receiver: oneshot::Receiver<Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError>>,
    ) -> Self {
        Self { receiver }
    }

    /// Await the resolution without requiring a Tokio runtime on the
    /// consuming thread.
    pub async fn receive(self) -> Result<Vec<ExactDiscoveryTarget>, HostnameResolutionError> {
        self.receiver
            .await
            .unwrap_or(Err(HostnameResolutionError::ControllerStopped))
    }
}

impl fmt::Debug for HostnameResolutionReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HostnameResolutionReceiver(<private>)")
    }
}
