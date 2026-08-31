//! Stable application-domain identities.

mod channel;
mod device;

pub use channel::{ChannelKey, GuideNumber, InvalidGuideNumber, MAX_GUIDE_NUMBER_BYTES};
pub use device::{DeviceId, InvalidDeviceId};
