use std::cmp::Ordering;
use std::fmt;

use thiserror::Error;

use super::DeviceId;

/// Maximum UTF-8 size accepted for a device-provided guide number.
pub const MAX_GUIDE_NUMBER_BYTES: usize = 32;

/// Device-native channel number used as part of stable channel identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GuideNumber(String);

impl GuideNumber {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidGuideNumber> {
        let value = value.into();
        if value.is_empty() {
            return Err(InvalidGuideNumber::Empty);
        }
        if value.len() > MAX_GUIDE_NUMBER_BYTES {
            return Err(InvalidGuideNumber::TooLong {
                actual: value.len(),
                maximum: MAX_GUIDE_NUMBER_BYTES,
            });
        }
        if value.trim() != value {
            return Err(InvalidGuideNumber::SurroundingWhitespace);
        }
        if value.chars().any(char::is_control) {
            return Err(InvalidGuideNumber::ControlCharacter);
        }
        let mut components = value.split('.');
        let major = components
            .next()
            .expect("a nonempty value has one component");
        let minor = components.next();
        if major.is_empty()
            || !major.bytes().all(|byte| byte.is_ascii_digit())
            || minor.is_some_and(|minor| {
                minor.is_empty() || !minor.bytes().all(|byte| byte.is_ascii_digit())
            })
            || components.next().is_some()
        {
            return Err(InvalidGuideNumber::InvalidFormat);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GuideNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Ord for GuideNumber {
    fn cmp(&self, other: &Self) -> Ordering {
        natural_cmp(self.0.as_bytes(), other.0.as_bytes()).then_with(|| self.0.cmp(&other.0))
    }
}

impl PartialOrd for GuideNumber {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidGuideNumber {
    #[error("guide number is empty")]
    Empty,

    #[error("guide number is {actual} bytes; maximum is {maximum}")]
    TooLong { actual: usize, maximum: usize },

    #[error("guide number has surrounding whitespace")]
    SurroundingWhitespace,

    #[error("guide number contains a control character")]
    ControlCharacter,

    #[error("guide number must contain one or two decimal numeric components")]
    InvalidFormat,
}

/// Stable channel identity. Guide numbers are deliberately scoped to a
/// DeviceID so equal numbers on separate tuners can never merge.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ChannelKey {
    device_id: DeviceId,
    guide_number: GuideNumber,
}

impl ChannelKey {
    #[must_use]
    pub const fn new(device_id: DeviceId, guide_number: GuideNumber) -> Self {
        Self {
            device_id,
            guide_number,
        }
    }

    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub const fn guide_number(&self) -> &GuideNumber {
        &self.guide_number
    }
}

fn natural_cmp(left: &[u8], right: &[u8]) -> Ordering {
    let mut left_offset = 0;
    let mut right_offset = 0;

    while left_offset < left.len() && right_offset < right.len() {
        let left_digit = left[left_offset].is_ascii_digit();
        let right_digit = right[right_offset].is_ascii_digit();
        if left_digit && right_digit {
            let left_end = digit_run_end(left, left_offset);
            let right_end = digit_run_end(right, right_offset);
            let left_digits = &left[left_offset..left_end];
            let right_digits = &right[right_offset..right_end];
            let left_significant = trim_zeroes(left_digits);
            let right_significant = trim_zeroes(right_digits);

            let ordering = left_significant
                .len()
                .cmp(&right_significant.len())
                .then_with(|| left_significant.cmp(right_significant));
            if ordering != Ordering::Equal {
                return ordering;
            }
            left_offset = left_end;
            right_offset = right_end;
            continue;
        }

        if left_digit != right_digit {
            return left[left_offset]
                .to_ascii_lowercase()
                .cmp(&right[right_offset].to_ascii_lowercase());
        }

        let left_end = nondigit_run_end(left, left_offset);
        let right_end = nondigit_run_end(right, right_offset);
        let ordering = ascii_case_insensitive_cmp(
            &left[left_offset..left_end],
            &right[right_offset..right_end],
        );
        if ordering != Ordering::Equal {
            return ordering;
        }
        left_offset = left_end;
        right_offset = right_end;
    }

    left.len().cmp(&right.len())
}

fn digit_run_end(value: &[u8], start: usize) -> usize {
    value[start..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(value.len(), |offset| start + offset)
}

fn nondigit_run_end(value: &[u8], start: usize) -> usize {
    value[start..]
        .iter()
        .position(u8::is_ascii_digit)
        .map_or(value.len(), |offset| start + offset)
}

fn trim_zeroes(value: &[u8]) -> &[u8] {
    let first_nonzero = value
        .iter()
        .position(|byte| *byte != b'0')
        .unwrap_or(value.len().saturating_sub(1));
    &value[first_nonzero..]
}

fn ascii_case_insensitive_cmp(left: &[u8], right: &[u8]) -> Ordering {
    left.iter()
        .map(u8::to_ascii_lowercase)
        .cmp(right.iter().map(u8::to_ascii_lowercase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_guide_numbers_naturally() {
        let mut values =
            ["10.1", "2.10", "2.2", "2", "02", "100"].map(|value| GuideNumber::new(value).unwrap());
        values.sort();

        assert_eq!(
            values.map(|value| value.to_string()),
            ["2", "02", "2.2", "2.10", "10.1", "100"]
        );
    }

    #[test]
    fn rejects_ambiguous_or_unsafe_guide_numbers() {
        assert_eq!(GuideNumber::new(""), Err(InvalidGuideNumber::Empty));
        assert_eq!(
            GuideNumber::new(" 7.1"),
            Err(InvalidGuideNumber::SurroundingWhitespace)
        );
        assert_eq!(
            GuideNumber::new("7\n1"),
            Err(InvalidGuideNumber::ControlCharacter)
        );
        for value in ["ABC", "7/1", ".1", "7.", "7.1.2"] {
            assert_eq!(
                GuideNumber::new(value),
                Err(InvalidGuideNumber::InvalidFormat)
            );
        }
        assert!(matches!(
            GuideNumber::new("x".repeat(MAX_GUIDE_NUMBER_BYTES + 1)),
            Err(InvalidGuideNumber::TooLong { .. })
        ));
    }

    #[test]
    fn scopes_equal_guide_numbers_to_their_device() {
        let first = ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        );
        let second = ChannelKey::new(
            DeviceId::new(0x105A_1243).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        );

        assert_ne!(first, second);
    }
}
