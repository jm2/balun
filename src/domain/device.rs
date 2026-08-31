use std::fmt;

use thiserror::Error;

/// Stable identity reported by an HDHomeRun device.
///
/// An address or advertised URL is only a locator. It must never replace this
/// value as the application's device identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceId(u32);

impl DeviceId {
    const CHECKSUM_LOOKUP: [u8; 16] = [
        0xA, 0x5, 0xF, 0x6, 0x7, 0xC, 0x1, 0xB, 0x9, 0x2, 0x8, 0xD, 0x4, 0x3, 0xE, 0x0,
    ];

    /// Build a concrete device identity after validating SiliconDust's
    /// checksum.
    pub fn new(value: u32) -> Result<Self, InvalidDeviceId> {
        if value == 0 || value == u32::MAX || !Self::checksum_is_valid(value) {
            return Err(InvalidDeviceId(value));
        }

        Ok(Self(value))
    }

    /// Return the numeric device identity used on the wire.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Validate the nibble checksum used by HDHomeRun device identifiers.
    #[must_use]
    pub fn checksum_is_valid(value: u32) -> bool {
        let table = &Self::CHECKSUM_LOOKUP;
        let checksum = table[((value >> 28) & 0x0F) as usize]
            ^ ((value >> 24) & 0x0F) as u8
            ^ table[((value >> 20) & 0x0F) as usize]
            ^ ((value >> 16) & 0x0F) as u8
            ^ table[((value >> 12) & 0x0F) as usize]
            ^ ((value >> 8) & 0x0F) as u8
            ^ table[((value >> 4) & 0x0F) as usize]
            ^ (value & 0x0F) as u8;

        checksum == 0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:08X}", self.0)
    }
}

impl TryFrom<u32> for DeviceId {
    type Error = InvalidDeviceId;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// The value is reserved or fails the HDHomeRun DeviceID checksum.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid HDHomeRun DeviceID {0:08X}")]
pub struct InvalidDeviceId(pub u32);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_checksum_valid_device_id() {
        let id = DeviceId::new(0x105A_1232).expect("known-valid synthetic DeviceID");

        assert_eq!(id.get(), 0x105A_1232);
        assert_eq!(id.to_string(), "105A1232");
    }

    #[test]
    fn rejects_bad_checksum() {
        assert_eq!(
            DeviceId::new(0x105A_1233),
            Err(InvalidDeviceId(0x105A_1233))
        );
    }

    #[test]
    fn rejects_reserved_values_even_when_checksum_passes() {
        assert_eq!(DeviceId::new(0), Err(InvalidDeviceId(0)));
        assert_eq!(DeviceId::new(u32::MAX), Err(InvalidDeviceId(u32::MAX)));
    }
}
