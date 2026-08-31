//! Wire-level HDHomeRun discovery protocol support.
//!
//! Discovery uses one frame per UDP datagram. Multi-byte frame fields and
//! integer TLV values are big-endian; the CRC at the end of the frame is
//! little-endian.

use std::iter::FusedIterator;

use thiserror::Error;

/// UDP port used by HDHomeRun discovery.
pub const DISCOVERY_UDP_PORT: u16 = 65_001;

/// Largest HDHomeRun frame accepted by the protocol.
pub const MAX_PACKET_SIZE: usize = 1_460;

/// Largest payload accepted by the protocol.
pub const MAX_PAYLOAD_SIZE: usize = 1_452;

/// Number of non-payload bytes in a frame: type, length, and CRC.
pub const FRAME_OVERHEAD: usize = 8;

/// Discovery request frame type.
pub const TYPE_DISCOVER_REQUEST: u16 = 0x0002;

/// Discovery reply frame type.
pub const TYPE_DISCOVER_REPLY: u16 = 0x0003;

/// A single device type encoded as a big-endian `u32`.
pub const TAG_DEVICE_TYPE: u8 = 0x01;

/// A device ID encoded as a big-endian `u32`.
pub const TAG_DEVICE_ID: u8 = 0x02;

/// Tuner count encoded as a `u8`.
pub const TAG_TUNER_COUNT: u8 = 0x10;

/// Device lineup URL encoded as a byte string.
pub const TAG_LINEUP_URL: u8 = 0x27;

/// Storage URL encoded as a byte string.
pub const TAG_STORAGE_URL: u8 = 0x28;

/// Deprecated 18-byte binary device authorization value.
pub const TAG_DEVICE_AUTH_BIN_DEPRECATED: u8 = 0x29;

/// Device base URL encoded as a byte string.
pub const TAG_BASE_URL: u8 = 0x2A;

/// Device authorization value encoded as a byte string.
pub const TAG_DEVICE_AUTH_STRING: u8 = 0x2B;

/// Storage ID encoded as a byte string.
pub const TAG_STORAGE_ID: u8 = 0x2C;

/// One or more device types encoded as consecutive big-endian `u32` values.
pub const TAG_MULTI_TYPE: u8 = 0x2D;

/// Match any device type when looking up one known device ID.
pub const DEVICE_TYPE_WILDCARD: u32 = 0xFFFF_FFFF;

/// HDHomeRun tuner device type.
pub const DEVICE_TYPE_TUNER: u32 = 0x0000_0001;

/// HDHomeRun storage device type.
pub const DEVICE_TYPE_STORAGE: u32 = 0x0000_0005;

/// Match any device ID.
pub const DEVICE_ID_WILDCARD: u32 = 0xFFFF_FFFF;

const DEVICE_ID_CHECKSUM_TABLE: [u8; 16] = [
    0xA, 0x5, 0xF, 0x6, 0x7, 0xC, 0x1, 0xB, 0x9, 0x2, 0x8, 0xD, 0x4, 0x3, 0xE, 0x0,
];

/// Errors produced while encoding or parsing discovery frames.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtocolError {
    /// The datagram cannot contain a complete frame.
    #[error("HDHomeRun frame is too short: got {actual} bytes, need at least {FRAME_OVERHEAD}")]
    FrameTooShort { actual: usize },

    /// The datagram exceeds the protocol's packet limit.
    #[error("HDHomeRun frame is too large: got {actual} bytes, maximum is {MAX_PACKET_SIZE}")]
    FrameTooLarge { actual: usize },

    /// The payload exceeds the protocol's payload limit.
    #[error("HDHomeRun payload is too large: got {actual} bytes, maximum is {MAX_PAYLOAD_SIZE}")]
    PayloadTooLarge { actual: usize },

    /// The payload length in the header does not exactly match the datagram.
    #[error(
        "HDHomeRun frame length mismatch: header declares {declared_payload} payload bytes ({expected_frame} total), datagram has {actual_frame} bytes"
    )]
    FrameLengthMismatch {
        declared_payload: usize,
        expected_frame: usize,
        actual_frame: usize,
    },

    /// The frame's CRC does not match its contents.
    #[error("HDHomeRun frame CRC mismatch: wire {wire:#010X}, calculated {calculated:#010X}")]
    CrcMismatch { wire: u32, calculated: u32 },

    /// A discovery reply parser received another kind of frame.
    #[error("unexpected HDHomeRun frame type {actual:#06X}; expected {expected:#06X}")]
    UnexpectedFrameType { expected: u16, actual: u16 },

    /// A TLV ends before its one-byte length field.
    #[error(
        "truncated HDHomeRun TLV header at payload offset {offset}: {remaining} byte(s) remain"
    )]
    TruncatedTlvHeader { offset: usize, remaining: usize },

    /// A two-byte TLV length is missing its second byte.
    #[error("truncated two-byte length for HDHomeRun tag {tag:#04X} at payload offset {offset}")]
    TruncatedTlvLength { tag: u8, offset: usize },

    /// A TLV value extends past the payload.
    #[error(
        "truncated value for HDHomeRun tag {tag:#04X} at payload offset {offset}: declares {declared} byte(s), {remaining} remain"
    )]
    TruncatedTlvValue {
        tag: u8,
        offset: usize,
        declared: usize,
        remaining: usize,
    },

    /// A known tag has a value with an invalid length.
    #[error(
        "invalid length for HDHomeRun tag {tag:#04X}: got {actual} byte(s), expected {expected}"
    )]
    InvalidTagLength {
        tag: u8,
        actual: usize,
        expected: &'static str,
    },

    /// A required tag was not found in a discovery response.
    #[error("missing required HDHomeRun discovery tag {tag:#04X}")]
    MissingRequiredTag { tag: u8 },

    /// Repeated scalar tags disagree with one another.
    #[error("conflicting repeated HDHomeRun discovery tag {tag:#04X}")]
    ConflictingTag { tag: u8 },

    /// The response did not identify itself as a tuner.
    #[error("HDHomeRun discovery response does not include the tuner device type")]
    MissingTunerDeviceType,

    /// A response supplied an invalid device type value.
    #[error("invalid HDHomeRun device type {0:#010X}")]
    InvalidDeviceType(u32),

    /// A concrete device ID is zero, the wildcard, or fails its checksum.
    #[error("invalid concrete HDHomeRun device ID {0:08X}")]
    InvalidDeviceId(u32),

    /// A string tag is not UTF-8.
    #[error("HDHomeRun string tag {tag:#04X} is not valid UTF-8")]
    InvalidUtf8 { tag: u8 },

    /// A string tag contains a NUL before its end.
    #[error("HDHomeRun string tag {tag:#04X} contains an embedded NUL")]
    EmbeddedNul { tag: u8 },
}

/// A validated frame borrowing its payload from the source datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    /// Raw frame type.
    pub frame_type: u16,
    /// Length-delimited frame payload.
    pub payload: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Iterate over TLVs in this frame's payload.
    #[must_use]
    pub fn tlvs(self) -> TlvIter<'a> {
        TlvIter::new(self.payload)
    }
}

/// One validated tag-length-value item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv<'a> {
    /// Raw tag value.
    pub tag: u8,
    /// Length-delimited tag value.
    pub value: &'a [u8],
}

/// A checked, forward-only iterator over an HDHomeRun TLV payload.
#[derive(Debug, Clone)]
pub struct TlvIter<'a> {
    payload: &'a [u8],
    offset: usize,
    failed: bool,
}

impl<'a> TlvIter<'a> {
    /// Construct an iterator over a raw TLV payload.
    #[must_use]
    pub const fn new(payload: &'a [u8]) -> Self {
        Self {
            payload,
            offset: 0,
            failed: false,
        }
    }
}

impl<'a> Iterator for TlvIter<'a> {
    type Item = Result<Tlv<'a>, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed || self.offset == self.payload.len() {
            return None;
        }

        let item_offset = self.offset;
        let remaining = self.payload.len() - item_offset;
        if remaining < 2 {
            self.failed = true;
            return Some(Err(ProtocolError::TruncatedTlvHeader {
                offset: item_offset,
                remaining,
            }));
        }

        let tag = self.payload[item_offset];
        let first_length = self.payload[item_offset + 1];
        let mut value_offset = item_offset + 2;
        let length = if first_length & 0x80 == 0 {
            usize::from(first_length)
        } else {
            if value_offset == self.payload.len() {
                self.failed = true;
                return Some(Err(ProtocolError::TruncatedTlvLength {
                    tag,
                    offset: item_offset + 1,
                }));
            }

            let second_length = self.payload[value_offset];
            value_offset += 1;
            usize::from(first_length & 0x7F) | (usize::from(second_length) << 7)
        };

        let value_end = match value_offset.checked_add(length) {
            Some(value_end) if value_end <= self.payload.len() => value_end,
            _ => {
                self.failed = true;
                return Some(Err(ProtocolError::TruncatedTlvValue {
                    tag,
                    offset: value_offset,
                    declared: length,
                    remaining: self.payload.len() - value_offset,
                }));
            }
        };

        self.offset = value_end;
        Some(Ok(Tlv {
            tag,
            value: &self.payload[value_offset..value_end],
        }))
    }
}

impl FusedIterator for TlvIter<'_> {}

/// A strict, tuner-specific discovery response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoverResponse {
    /// Device types announced by this responder, in wire order without duplicates.
    pub device_types: Vec<u32>,
    /// Concrete checksum-valid HDHomeRun device ID.
    pub device_id: u32,
    /// Announced tuner count. Old firmware may omit it.
    pub tuner_count: Option<u8>,
    /// Advertised device base URL.
    pub base_url: Option<String>,
    /// Advertised lineup URL.
    pub lineup_url: Option<String>,
}

/// Check the self-check sequence embedded in an HDHomeRun device ID.
///
/// This implements the vendor checksum exactly. Both zero and the all-ones
/// wildcard satisfy the checksum mathematically; use [`is_concrete_device_id`]
/// when accepting a device identity.
#[must_use]
pub fn is_valid_device_id(device_id: u32) -> bool {
    let checksum = DEVICE_ID_CHECKSUM_TABLE[((device_id >> 28) & 0x0F) as usize]
        ^ ((device_id >> 24) & 0x0F) as u8
        ^ DEVICE_ID_CHECKSUM_TABLE[((device_id >> 20) & 0x0F) as usize]
        ^ ((device_id >> 16) & 0x0F) as u8
        ^ DEVICE_ID_CHECKSUM_TABLE[((device_id >> 12) & 0x0F) as usize]
        ^ ((device_id >> 8) & 0x0F) as u8
        ^ DEVICE_ID_CHECKSUM_TABLE[((device_id >> 4) & 0x0F) as usize]
        ^ (device_id & 0x0F) as u8;

    checksum == 0
}

/// Check that a device ID is usable as a concrete tuner identity.
#[must_use]
pub fn is_concrete_device_id(device_id: u32) -> bool {
    device_id != 0 && device_id != DEVICE_ID_WILDCARD && is_valid_device_id(device_id)
}

/// Encode a current HDHomeRun device-discovery request.
///
/// `None` discovers all tuner devices and omits the wildcard device-ID TLV,
/// matching the current `discover2` implementation. `Some(id)` locates a known
/// concrete ID and pairs it with the wildcard device type, also matching the
/// current implementation.
pub fn encode_tuner_discover_request(device_id: Option<u32>) -> Result<Vec<u8>, ProtocolError> {
    let mut payload = Vec::with_capacity(if device_id.is_some() { 12 } else { 6 });

    match device_id {
        None => write_u32_tlv(&mut payload, TAG_DEVICE_TYPE, DEVICE_TYPE_TUNER),
        Some(device_id) => {
            if !is_concrete_device_id(device_id) {
                return Err(ProtocolError::InvalidDeviceId(device_id));
            }

            write_u32_tlv(&mut payload, TAG_DEVICE_TYPE, DEVICE_TYPE_WILDCARD);
            write_u32_tlv(&mut payload, TAG_DEVICE_ID, device_id);
        }
    }

    encode_frame(TYPE_DISCOVER_REQUEST, &payload)
}

/// Parse and validate one complete HDHomeRun frame.
///
/// UDP callers should receive into `MAX_PACKET_SIZE + 1` bytes so an oversized
/// datagram can be distinguished from a packet truncated to the receive buffer.
pub fn parse_frame(datagram: &[u8]) -> Result<Frame<'_>, ProtocolError> {
    if datagram.len() < FRAME_OVERHEAD {
        return Err(ProtocolError::FrameTooShort {
            actual: datagram.len(),
        });
    }
    if datagram.len() > MAX_PACKET_SIZE {
        return Err(ProtocolError::FrameTooLarge {
            actual: datagram.len(),
        });
    }

    let frame_type = u16::from_be_bytes([datagram[0], datagram[1]]);
    let payload_length = usize::from(u16::from_be_bytes([datagram[2], datagram[3]]));
    if payload_length > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload_length,
        });
    }

    let expected_length = payload_length
        .checked_add(FRAME_OVERHEAD)
        .expect("payload limit makes frame-length overflow impossible");
    if datagram.len() != expected_length {
        return Err(ProtocolError::FrameLengthMismatch {
            declared_payload: payload_length,
            expected_frame: expected_length,
            actual_frame: datagram.len(),
        });
    }

    let crc_offset = 4 + payload_length;
    let wire_crc = u32::from_le_bytes([
        datagram[crc_offset],
        datagram[crc_offset + 1],
        datagram[crc_offset + 2],
        datagram[crc_offset + 3],
    ]);
    let calculated_crc = crc32fast::hash(&datagram[..crc_offset]);
    if wire_crc != calculated_crc {
        return Err(ProtocolError::CrcMismatch {
            wire: wire_crc,
            calculated: calculated_crc,
        });
    }

    Ok(Frame {
        frame_type,
        payload: &datagram[4..crc_offset],
    })
}

/// Parse a discovery reply and require a concrete tuner identity.
pub fn parse_tuner_discover_response(datagram: &[u8]) -> Result<DiscoverResponse, ProtocolError> {
    let frame = parse_frame(datagram)?;
    if frame.frame_type != TYPE_DISCOVER_REPLY {
        return Err(ProtocolError::UnexpectedFrameType {
            expected: TYPE_DISCOVER_REPLY,
            actual: frame.frame_type,
        });
    }

    let mut device_types = Vec::new();
    let mut device_id = None;
    let mut tuner_count = None;
    let mut base_url = None;
    let mut lineup_url = None;

    for item in frame.tlvs() {
        let item = item?;
        match item.tag {
            TAG_DEVICE_TYPE => {
                require_tag_length(item, 4, "4")?;
                add_device_type(&mut device_types, read_u32(item.value)?)?;
            }
            TAG_MULTI_TYPE => {
                if item.value.is_empty() || item.value.len() % 4 != 0 {
                    return Err(ProtocolError::InvalidTagLength {
                        tag: item.tag,
                        actual: item.value.len(),
                        expected: "a nonzero multiple of 4",
                    });
                }

                let (encoded_types, remainder) = item.value.as_chunks::<4>();
                debug_assert!(remainder.is_empty(), "length was validated above");
                for encoded_type in encoded_types {
                    add_device_type(&mut device_types, read_u32(encoded_type)?)?;
                }
            }
            TAG_DEVICE_ID => {
                require_tag_length(item, 4, "4")?;
                let value = read_u32(item.value)?;
                if !is_concrete_device_id(value) {
                    return Err(ProtocolError::InvalidDeviceId(value));
                }
                set_once(&mut device_id, value, item.tag)?;
            }
            TAG_TUNER_COUNT => {
                require_tag_length(item, 1, "1")?;
                set_once(&mut tuner_count, item.value[0], item.tag)?;
            }
            TAG_BASE_URL => {
                let value = parse_wire_string(item.tag, item.value)?;
                set_once(&mut base_url, value, item.tag)?;
            }
            TAG_LINEUP_URL => {
                let value = parse_wire_string(item.tag, item.value)?;
                set_once(&mut lineup_url, value, item.tag)?;
            }
            TAG_DEVICE_AUTH_BIN_DEPRECATED => {
                require_tag_length(item, 18, "18")?;
            }
            _ => {
                // Discovery replies are explicitly extensible. Unknown tags are
                // length-checked by TlvIter and otherwise ignored.
            }
        }
    }

    let device_id = device_id.ok_or(ProtocolError::MissingRequiredTag { tag: TAG_DEVICE_ID })?;
    if !device_types.contains(&DEVICE_TYPE_TUNER) {
        return Err(ProtocolError::MissingTunerDeviceType);
    }

    Ok(DiscoverResponse {
        device_types,
        device_id,
        tuner_count,
        base_url,
        lineup_url,
    })
}

fn encode_frame(frame_type: u16, payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload.len(),
        });
    }

    let payload_length =
        u16::try_from(payload.len()).expect("the payload limit is smaller than the u16 maximum");
    let mut frame = Vec::with_capacity(payload.len() + FRAME_OVERHEAD);
    frame.extend_from_slice(&frame_type.to_be_bytes());
    frame.extend_from_slice(&payload_length.to_be_bytes());
    frame.extend_from_slice(payload);
    let crc = crc32fast::hash(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());
    Ok(frame)
}

fn write_u32_tlv(payload: &mut Vec<u8>, tag: u8, value: u32) {
    payload.push(tag);
    payload.push(4);
    payload.extend_from_slice(&value.to_be_bytes());
}

fn require_tag_length(
    item: Tlv<'_>,
    required: usize,
    expected: &'static str,
) -> Result<(), ProtocolError> {
    if item.value.len() != required {
        return Err(ProtocolError::InvalidTagLength {
            tag: item.tag,
            actual: item.value.len(),
            expected,
        });
    }

    Ok(())
}

fn read_u32(bytes: &[u8]) -> Result<u32, ProtocolError> {
    let bytes: [u8; 4] = bytes
        .try_into()
        .map_err(|_| ProtocolError::InvalidTagLength {
            tag: 0,
            actual: bytes.len(),
            expected: "4",
        })?;
    Ok(u32::from_be_bytes(bytes))
}

fn add_device_type(device_types: &mut Vec<u32>, device_type: u32) -> Result<(), ProtocolError> {
    if device_type == 0 || device_type == DEVICE_TYPE_WILDCARD {
        return Err(ProtocolError::InvalidDeviceType(device_type));
    }
    if !device_types.contains(&device_type) {
        device_types.push(device_type);
    }
    Ok(())
}

fn set_once<T: Eq>(destination: &mut Option<T>, value: T, tag: u8) -> Result<(), ProtocolError> {
    match destination {
        Some(existing) if existing != &value => Err(ProtocolError::ConflictingTag { tag }),
        Some(_) => Ok(()),
        None => {
            *destination = Some(value);
            Ok(())
        }
    }
}

fn parse_wire_string(tag: u8, value: &[u8]) -> Result<String, ProtocolError> {
    let value = value.strip_suffix(&[0]).unwrap_or(value);
    if value.contains(&0) {
        return Err(ProtocolError::EmbeddedNul { tag });
    }

    std::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| ProtocolError::InvalidUtf8 { tag })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN_ALL_TUNERS_REQUEST: [u8; 14] = [
        0x00, 0x02, 0x00, 0x06, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x39, 0x30, 0x77, 0xE7,
    ];

    const GOLDEN_CLASSIC_WILDCARD_REQUEST: [u8; 20] = [
        0x00, 0x02, 0x00, 0x0C, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x04, 0xFF, 0xFF, 0xFF,
        0xFF, 0x4E, 0x50, 0x7F, 0x35,
    ];

    const GOLDEN_EXACT_DEVICE_REQUEST: [u8; 20] = [
        0x00, 0x02, 0x00, 0x0C, 0x01, 0x04, 0xFF, 0xFF, 0xFF, 0xFF, 0x02, 0x04, 0x10, 0x5A, 0x12,
        0x32, 0x3A, 0x31, 0xD7, 0xD0,
    ];

    const GOLDEN_DISCOVER_REPLY: [u8; 79] = [
        0x00, 0x03, 0x00, 0x47, 0x01, 0x04, 0x00, 0x00, 0x00, 0x01, 0x02, 0x04, 0x10, 0x5A, 0x12,
        0x32, 0x10, 0x01, 0x04, 0x2A, 0x14, 0x68, 0x74, 0x74, 0x70, 0x3A, 0x2F, 0x2F, 0x31, 0x39,
        0x32, 0x2E, 0x30, 0x2E, 0x32, 0x2E, 0x31, 0x30, 0x3A, 0x38, 0x30, 0x27, 0x20, 0x68, 0x74,
        0x74, 0x70, 0x3A, 0x2F, 0x2F, 0x31, 0x39, 0x32, 0x2E, 0x30, 0x2E, 0x32, 0x2E, 0x31, 0x30,
        0x3A, 0x38, 0x30, 0x2F, 0x6C, 0x69, 0x6E, 0x65, 0x75, 0x70, 0x2E, 0x6A, 0x73, 0x6F, 0x6E,
        0x1E, 0x72, 0x20, 0x00,
    ];

    #[test]
    fn encodes_current_all_tuners_request_golden_vector() {
        let encoded = encode_tuner_discover_request(None).expect("request should encode");
        assert_eq!(encoded, GOLDEN_ALL_TUNERS_REQUEST);

        let frame = parse_frame(&encoded).expect("golden request should parse");
        assert_eq!(frame.frame_type, TYPE_DISCOVER_REQUEST);
        let items = frame
            .tlvs()
            .collect::<Result<Vec<_>, _>>()
            .expect("golden TLVs should parse");
        assert_eq!(
            items,
            vec![Tlv {
                tag: TAG_DEVICE_TYPE,
                value: &DEVICE_TYPE_TUNER.to_be_bytes(),
            }]
        );
    }

    #[test]
    fn encodes_current_exact_device_request_golden_vector() {
        let encoded =
            encode_tuner_discover_request(Some(0x105A_1232)).expect("request should encode");
        assert_eq!(encoded, GOLDEN_EXACT_DEVICE_REQUEST);

        let frame = parse_frame(&encoded).expect("golden request should parse");
        let items = frame
            .tlvs()
            .collect::<Result<Vec<_>, _>>()
            .expect("golden TLVs should parse");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].tag, TAG_DEVICE_TYPE);
        assert_eq!(items[0].value, DEVICE_TYPE_WILDCARD.to_be_bytes());
        assert_eq!(items[1].tag, TAG_DEVICE_ID);
        assert_eq!(items[1].value, 0x105A_1232_u32.to_be_bytes());
    }

    #[test]
    fn parses_classic_explicit_wildcard_request() {
        let frame = parse_frame(&GOLDEN_CLASSIC_WILDCARD_REQUEST)
            .expect("classic golden request should parse");
        let items = frame
            .tlvs()
            .collect::<Result<Vec<_>, _>>()
            .expect("classic golden TLVs should parse");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].value, DEVICE_TYPE_TUNER.to_be_bytes());
        assert_eq!(items[1].value, DEVICE_ID_WILDCARD.to_be_bytes());
    }

    #[test]
    fn parses_synthetic_discovery_reply_golden_vector() {
        let reply = parse_tuner_discover_response(&GOLDEN_DISCOVER_REPLY)
            .expect("synthetic golden reply should parse");

        assert_eq!(reply.device_types, vec![DEVICE_TYPE_TUNER]);
        assert_eq!(reply.device_id, 0x105A_1232);
        assert_eq!(reply.tuner_count, Some(4));
        assert_eq!(reply.base_url.as_deref(), Some("http://192.0.2.10:80"));
        assert_eq!(
            reply.lineup_url.as_deref(),
            Some("http://192.0.2.10:80/lineup.json")
        );
    }

    #[test]
    fn implements_vendor_device_id_checksum_exactly() {
        assert!(is_valid_device_id(0x105A_1232));
        assert!(is_concrete_device_id(0x105A_1232));
        assert!(!is_valid_device_id(0x105A_1233));
        assert!(!is_concrete_device_id(0x105A_1233));

        // These are checksum-valid sentinels, but not concrete identities.
        assert!(is_valid_device_id(0));
        assert!(is_valid_device_id(DEVICE_ID_WILDCARD));
        assert!(!is_concrete_device_id(0));
        assert!(!is_concrete_device_id(DEVICE_ID_WILDCARD));
    }

    #[test]
    fn rejects_invalid_exact_request_ids() {
        for invalid in [0, DEVICE_ID_WILDCARD, 0x105A_1233] {
            assert_eq!(
                encode_tuner_discover_request(Some(invalid)),
                Err(ProtocolError::InvalidDeviceId(invalid))
            );
        }
    }

    #[test]
    fn frame_parser_enforces_all_size_and_length_boundaries() {
        assert_eq!(
            parse_frame(&[0; FRAME_OVERHEAD - 1]),
            Err(ProtocolError::FrameTooShort {
                actual: FRAME_OVERHEAD - 1
            })
        );
        assert_eq!(
            parse_frame(&vec![0; MAX_PACKET_SIZE + 1]),
            Err(ProtocolError::FrameTooLarge {
                actual: MAX_PACKET_SIZE + 1
            })
        );

        let payload_too_large = [0x00, 0x03, 0x05, 0xAD, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(
            parse_frame(&payload_too_large),
            Err(ProtocolError::PayloadTooLarge { actual: 1_453 })
        );

        let mut truncated = GOLDEN_ALL_TUNERS_REQUEST.to_vec();
        truncated.pop();
        assert_eq!(
            parse_frame(&truncated),
            Err(ProtocolError::FrameLengthMismatch {
                declared_payload: 6,
                expected_frame: 14,
                actual_frame: 13,
            })
        );

        let mut trailing = GOLDEN_ALL_TUNERS_REQUEST.to_vec();
        trailing.push(0);
        assert_eq!(
            parse_frame(&trailing),
            Err(ProtocolError::FrameLengthMismatch {
                declared_payload: 6,
                expected_frame: 14,
                actual_frame: 15,
            })
        );

        let maximum = encode_frame(TYPE_DISCOVER_REPLY, &[0; MAX_PAYLOAD_SIZE])
            .expect("maximum payload should encode");
        assert_eq!(maximum.len(), MAX_PACKET_SIZE);
        assert_eq!(
            parse_frame(&maximum).unwrap().payload.len(),
            MAX_PAYLOAD_SIZE
        );
        assert_eq!(
            encode_frame(TYPE_DISCOVER_REPLY, &[0; MAX_PAYLOAD_SIZE + 1]),
            Err(ProtocolError::PayloadTooLarge {
                actual: MAX_PAYLOAD_SIZE + 1
            })
        );
    }

    #[test]
    fn frame_parser_rejects_crc_corruption() {
        let mut corrupted = GOLDEN_ALL_TUNERS_REQUEST;
        corrupted[7] ^= 1;
        assert!(matches!(
            parse_frame(&corrupted),
            Err(ProtocolError::CrcMismatch { .. })
        ));
    }

    #[test]
    fn tlv_parser_handles_one_and_two_byte_lengths() {
        let mut payload = vec![0x80, 0x7F];
        payload.extend([0xA5; 127]);
        payload.extend([0x81, 0x80, 0x01]);
        payload.extend([0x5A; 128]);
        // Noncanonical two-byte encoding of zero is accepted for compatibility.
        payload.extend([0x82, 0x80, 0x00]);

        let items = TlvIter::new(&payload)
            .collect::<Result<Vec<_>, _>>()
            .expect("all boundary TLVs should parse");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].value, &[0xA5; 127]);
        assert_eq!(items[1].value, &[0x5A; 128]);
        assert!(items[2].value.is_empty());
    }

    #[test]
    fn tlv_parser_stops_after_each_truncation_error() {
        let cases = [
            (
                vec![0x01],
                ProtocolError::TruncatedTlvHeader {
                    offset: 0,
                    remaining: 1,
                },
            ),
            (
                vec![0x01, 0x80],
                ProtocolError::TruncatedTlvLength {
                    tag: 0x01,
                    offset: 1,
                },
            ),
            (
                vec![0x01, 0x04, 0x00],
                ProtocolError::TruncatedTlvValue {
                    tag: 0x01,
                    offset: 2,
                    declared: 4,
                    remaining: 1,
                },
            ),
        ];

        for (payload, expected) in cases {
            let mut iterator = TlvIter::new(&payload);
            assert_eq!(iterator.next(), Some(Err(expected)));
            assert_eq!(iterator.next(), None);
            assert_eq!(iterator.next(), None);
        }
    }

    #[test]
    fn response_accepts_multi_type_unknown_tags_and_identical_duplicates() {
        let mut payload = Vec::new();
        push_tlv(
            &mut payload,
            TAG_MULTI_TYPE,
            &[
                0,
                0,
                0,
                DEVICE_TYPE_TUNER as u8,
                0,
                0,
                0,
                DEVICE_TYPE_STORAGE as u8,
            ],
        );
        push_tlv(&mut payload, 0xFE, b"future");
        write_u32_tlv(&mut payload, TAG_DEVICE_ID, 0x105A_1232);
        write_u32_tlv(&mut payload, TAG_DEVICE_ID, 0x105A_1232);

        let frame = encode_frame(TYPE_DISCOVER_REPLY, &payload).unwrap();
        let response = parse_tuner_discover_response(&frame).unwrap();
        assert_eq!(
            response.device_types,
            vec![DEVICE_TYPE_TUNER, DEVICE_TYPE_STORAGE]
        );
    }

    #[test]
    fn response_rejects_wrong_frame_type_and_missing_required_fields() {
        assert_eq!(
            parse_tuner_discover_response(&GOLDEN_ALL_TUNERS_REQUEST),
            Err(ProtocolError::UnexpectedFrameType {
                expected: TYPE_DISCOVER_REPLY,
                actual: TYPE_DISCOVER_REQUEST,
            })
        );

        let only_type =
            encode_frame(TYPE_DISCOVER_REPLY, &[TAG_DEVICE_TYPE, 4, 0, 0, 0, 1]).unwrap();
        assert_eq!(
            parse_tuner_discover_response(&only_type),
            Err(ProtocolError::MissingRequiredTag { tag: TAG_DEVICE_ID })
        );

        let mut only_id_payload = Vec::new();
        write_u32_tlv(&mut only_id_payload, TAG_DEVICE_ID, 0x105A_1232);
        let only_id = encode_frame(TYPE_DISCOVER_REPLY, &only_id_payload).unwrap();
        assert_eq!(
            parse_tuner_discover_response(&only_id),
            Err(ProtocolError::MissingTunerDeviceType)
        );
    }

    #[test]
    fn response_rejects_bad_known_tag_lengths_and_values() {
        for (tag, value, expected) in [
            (TAG_DEVICE_TYPE, vec![0, 0, 1], "4"),
            (TAG_DEVICE_ID, vec![0, 0, 1], "4"),
            (TAG_TUNER_COUNT, vec![1, 2], "1"),
            (
                TAG_MULTI_TYPE,
                vec![0, 0, 0, 1, 0],
                "a nonzero multiple of 4",
            ),
            (TAG_DEVICE_AUTH_BIN_DEPRECATED, vec![0; 17], "18"),
        ] {
            let frame = response_with_extra_tlv(tag, &value);
            assert_eq!(
                parse_tuner_discover_response(&frame),
                Err(ProtocolError::InvalidTagLength {
                    tag,
                    actual: value.len(),
                    expected,
                })
            );
        }

        let mut payload = base_response_payload();
        write_u32_tlv(&mut payload, TAG_DEVICE_TYPE, DEVICE_TYPE_WILDCARD);
        let frame = encode_frame(TYPE_DISCOVER_REPLY, &payload).unwrap();
        assert_eq!(
            parse_tuner_discover_response(&frame),
            Err(ProtocolError::InvalidDeviceType(DEVICE_TYPE_WILDCARD))
        );

        let invalid_id = response_payload_with_id(0x105A_1233);
        let frame = encode_frame(TYPE_DISCOVER_REPLY, &invalid_id).unwrap();
        assert_eq!(
            parse_tuner_discover_response(&frame),
            Err(ProtocolError::InvalidDeviceId(0x105A_1233))
        );
    }

    #[test]
    fn response_rejects_conflicting_scalar_duplicates() {
        let mut payload = base_response_payload();
        payload.extend([TAG_TUNER_COUNT, 1, 2, TAG_TUNER_COUNT, 1, 3]);
        let frame = encode_frame(TYPE_DISCOVER_REPLY, &payload).unwrap();
        assert_eq!(
            parse_tuner_discover_response(&frame),
            Err(ProtocolError::ConflictingTag {
                tag: TAG_TUNER_COUNT
            })
        );
    }

    #[test]
    fn response_string_parser_handles_terminal_nul_and_rejects_bad_text() {
        let frame = response_with_extra_tlv(TAG_BASE_URL, b"http://192.0.2.1\0");
        assert_eq!(
            parse_tuner_discover_response(&frame).unwrap().base_url,
            Some("http://192.0.2.1".to_owned())
        );

        for (value, expected) in [
            (
                b"bad\0url".as_slice(),
                ProtocolError::EmbeddedNul { tag: TAG_BASE_URL },
            ),
            (
                b"bad\0\0".as_slice(),
                ProtocolError::EmbeddedNul { tag: TAG_BASE_URL },
            ),
            (
                &[0xFF][..],
                ProtocolError::InvalidUtf8 { tag: TAG_BASE_URL },
            ),
        ] {
            let frame = response_with_extra_tlv(TAG_BASE_URL, value);
            assert_eq!(parse_tuner_discover_response(&frame), Err(expected));
        }
    }

    fn base_response_payload() -> Vec<u8> {
        response_payload_with_id(0x105A_1232)
    }

    fn response_payload_with_id(device_id: u32) -> Vec<u8> {
        let mut payload = Vec::new();
        write_u32_tlv(&mut payload, TAG_DEVICE_TYPE, DEVICE_TYPE_TUNER);
        write_u32_tlv(&mut payload, TAG_DEVICE_ID, device_id);
        payload
    }

    fn response_with_extra_tlv(tag: u8, value: &[u8]) -> Vec<u8> {
        let mut payload = base_response_payload();
        push_tlv(&mut payload, tag, value);
        encode_frame(TYPE_DISCOVER_REPLY, &payload).unwrap()
    }

    fn push_tlv(payload: &mut Vec<u8>, tag: u8, value: &[u8]) {
        assert!(value.len() <= 127, "test helper only uses one-byte lengths");
        payload.push(tag);
        payload.push(value.len() as u8);
        payload.extend_from_slice(value);
    }
}
