//! One-shot, URL-redacted stream handoff types.

use std::fmt;

use thiserror::Error;
use tokio::sync::oneshot;
use zeroize::{Zeroize, Zeroizing};

use super::OperationGeneration;
use crate::domain::ChannelKey;

/// URL-free channel intent captured from one complete application snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamSelection {
    channel_key: ChannelKey,
    selection_generation: OperationGeneration,
}

impl StreamSelection {
    #[must_use]
    pub const fn new(channel_key: ChannelKey, selection_generation: OperationGeneration) -> Self {
        Self {
            channel_key,
            selection_generation,
        }
    }

    #[must_use]
    pub const fn channel_key(&self) -> &ChannelKey {
        &self.channel_key
    }

    #[must_use]
    pub const fn selection_generation(&self) -> OperationGeneration {
        self.selection_generation
    }

    pub(super) fn into_parts(self) -> (ChannelKey, OperationGeneration) {
        (self.channel_key, self.selection_generation)
    }
}

/// One actor-authorized stream locator for exactly one channel selection.
///
/// This value never enters [`super::ApplicationSnapshot`]. Its debug
/// representation is URL-free, and its owned URI bytes are zeroized when the
/// handoff drops. The playback-session owner exposes the URI only while
/// constructing its generation-scoped pipeline.
pub struct StreamHandoff {
    channel_key: ChannelKey,
    selection_generation: OperationGeneration,
    uri: Zeroizing<String>,
}

impl StreamHandoff {
    pub(super) fn new(
        channel_key: ChannelKey,
        selection_generation: OperationGeneration,
        uri: &str,
    ) -> Self {
        Self {
            channel_key,
            selection_generation,
            uri: Zeroizing::new(uri.to_owned()),
        }
    }

    /// Return the stable channel identity authorized by the controller.
    #[must_use]
    pub const fn channel_key(&self) -> &ChannelKey {
        &self.channel_key
    }

    /// Return the selected-device generation that authorized this handoff.
    #[must_use]
    pub const fn selection_generation(&self) -> OperationGeneration {
        self.selection_generation
    }

    /// Expose the authorized URI only to one library-owned consuming closure.
    ///
    /// The higher-ranked input lifetime prevents the closure from returning a
    /// borrow of the URI. The desktop binary cannot call this crate-private
    /// method; it can only move the opaque handoff into Balun's player owner.
    #[cfg(feature = "desktop")]
    pub(crate) fn with_uri<R>(self, consume: impl for<'a> FnOnce(&'a str) -> R) -> R {
        consume(self.uri.as_str())
    }

    #[cfg(all(test, feature = "desktop"))]
    pub(crate) fn test_fixture(
        channel_key: ChannelKey,
        selection_generation: OperationGeneration,
        uri: &str,
    ) -> Self {
        Self::new(channel_key, selection_generation, uri)
    }

    /// Build the one handoff the packaged Windows runtime probe feeds to the
    /// production source policy: a loopback fixture URL that never leaves
    /// the probe process. The desktop binary cannot reach this constructor.
    #[cfg(all(feature = "desktop", target_os = "windows"))]
    pub(crate) fn packaged_probe_fixture(
        channel_key: ChannelKey,
        selection_generation: OperationGeneration,
        uri: &str,
    ) -> Self {
        Self::new(channel_key, selection_generation, uri)
    }
}

impl fmt::Debug for StreamHandoff {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamHandoff")
            .field("channel_key", &self.channel_key)
            .field("selection_generation", &self.selection_generation)
            .field("uri", &"<redacted>")
            .finish()
    }
}

impl Drop for StreamHandoff {
    fn drop(&mut self) {
        self.uri.zeroize();
    }
}

/// Fixed, endpoint-free reason a stream handoff was rejected or lost.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[non_exhaustive]
pub enum StreamHandoffError {
    #[error("the selected-device generation changed before stream resolution")]
    SelectionChanged,
    #[error("no complete selected-device snapshot is ready")]
    SelectionNotReady,
    #[error("the requested channel does not belong to the selected device")]
    DeviceMismatch,
    #[error("the requested channel is not present in the current selected lineup")]
    ChannelUnavailable,
    #[error("protected channels are not supported")]
    Protected,
    #[error("the selected stream origin is no longer authorized")]
    OriginRejected,
    #[error("the selected-device stream invariant failed")]
    Internal,
    #[error("the controller stopped before completing the stream handoff")]
    ControllerStopped,
}

/// Private one-shot response to an admitted stream request.
///
/// Unlike application snapshots, this receiver is single-consumer and carries
/// at most one URL-bearing value. Dropping either side drops and zeroizes any
/// undelivered successful handoff.
#[must_use = "the private stream response must be received or deliberately dropped"]
pub struct StreamHandoffReceiver {
    receiver: oneshot::Receiver<Result<StreamHandoff, StreamHandoffError>>,
}

impl StreamHandoffReceiver {
    pub(super) const fn new(
        receiver: oneshot::Receiver<Result<StreamHandoff, StreamHandoffError>>,
    ) -> Self {
        Self { receiver }
    }

    /// Await the actor's fixed-result response without requiring a Tokio
    /// runtime on the consuming thread.
    pub async fn receive(self) -> Result<StreamHandoff, StreamHandoffError> {
        self.receiver
            .await
            .unwrap_or(Err(StreamHandoffError::ControllerStopped))
    }
}

impl fmt::Debug for StreamHandoffReceiver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StreamHandoffReceiver(<private>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DeviceId, GuideNumber};

    fn key() -> ChannelKey {
        ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("5.1").unwrap(),
        )
    }

    #[test]
    fn handoff_debug_always_redacts_the_uri() {
        let private = "http://192.0.2.10:5004/auto/v5.1";
        let handoff = StreamHandoff::new(key(), OperationGeneration::new(7), private);
        let rendered = format!("{handoff:?}");

        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(private));
        assert!(!rendered.contains("192.0.2.10"));
        assert_eq!(handoff.uri.as_str(), private);
    }

    #[tokio::test]
    async fn closed_private_response_has_one_fixed_error() {
        let (sender, receiver) = oneshot::channel();
        drop(sender);

        assert!(matches!(
            StreamHandoffReceiver::new(receiver).receive().await,
            Err(StreamHandoffError::ControllerStopped)
        ));
    }
}
