//! Framing and classification of macOS `PF_ROUTE` routing-socket messages.
//!
//! Every message starts with the same prefix: a native-endian `u16` total
//! length, a `u8` version, and a `u8` type. The kernel is trusted to frame
//! its own messages, but this parser still validates every declared length
//! against the bytes actually read before looking at any other field, so a
//! short or inconsistent read ends observation instead of panicking or
//! reading past the buffer. Only the link, address, and route messages that
//! can change discovery evidence are classified; everything else, such as
//! lookups, misses, and multicast memberships, is skipped.

use thiserror::Error;

use super::watch::ChangeKind;

/// `RTM_VERSION` from `<net/route.h>`.
const RTM_VERSION: u8 = 5;

// Message types from `<net/route.h>`.
const RTM_ADD: u8 = 0x1;
const RTM_DELETE: u8 = 0x2;
const RTM_CHANGE: u8 = 0x3;
const RTM_NEWADDR: u8 = 0xc;
const RTM_DELADDR: u8 = 0xd;
const RTM_IFINFO: u8 = 0xe;
const RTM_IFINFO2: u8 = 0x12;

/// Neighbour-cache entries (ARP and NDP) are routes on macOS.
const RTF_LLINFO: i32 = 0x400;
/// Host routes cloned per destination, such as one per TCP peer.
const RTF_WASCLONED: i32 = 0x20000;

/// The common `rtm_msglen`, `rtm_version`, and `rtm_type` prefix.
const MESSAGE_PREFIX_BYTES: usize = 4;
/// `rt_msghdr` field offsets, fixed by its `u16, u8, u8, u16, i32...` layout.
const ROUTE_FLAGS_OFFSET: usize = 8;
const ROUTE_ERRNO_OFFSET: usize = 24;
/// The part of `rt_msghdr` this parser reads, through `rtm_errno`.
const ROUTE_HEADER_BYTES: usize = ROUTE_ERRNO_OFFSET + 4;

/// A read that cannot be framed as routing-socket messages.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("a routing-socket message was malformed")]
pub(super) struct MalformedRouteMessage;

/// Classify every message in one read, in order.
///
/// The whole read is validated before anything is returned, so a malformed
/// message anywhere rejects it. Skipped messages contribute nothing.
pub(super) fn route_message_kinds(bytes: &[u8]) -> Result<Vec<ChangeKind>, MalformedRouteMessage> {
    let mut kinds = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let [low, high, ..] = *rest else {
            return Err(MalformedRouteMessage);
        };
        let declared = usize::from(u16::from_ne_bytes([low, high]));
        if declared < MESSAGE_PREFIX_BYTES || declared > rest.len() {
            return Err(MalformedRouteMessage);
        }
        let (message, remainder) = rest.split_at(declared);
        if let Some(kind) = classify(message)? {
            kinds.push(kind);
        }
        rest = remainder;
    }
    Ok(kinds)
}

/// Classify one message whose declared length is exactly `message.len()`.
fn classify(message: &[u8]) -> Result<Option<ChangeKind>, MalformedRouteMessage> {
    let [_, _, version, message_type, ..] = *message else {
        return Err(MalformedRouteMessage);
    };
    // The layout after the prefix is defined by this version only.
    if version != RTM_VERSION {
        return Err(MalformedRouteMessage);
    }
    Ok(match message_type {
        RTM_IFINFO | RTM_IFINFO2 => Some(ChangeKind::Link),
        RTM_NEWADDR | RTM_DELADDR => Some(ChangeKind::Address),
        RTM_ADD | RTM_DELETE | RTM_CHANGE => route_change(message)?,
        _ => None,
    })
}

/// A route message changes the table unless it reports a failed request or
/// a neighbour-cache or cloned host entry. Those entries come and go with
/// ordinary traffic and say nothing about the network's shape.
fn route_change(message: &[u8]) -> Result<Option<ChangeKind>, MalformedRouteMessage> {
    let header = message
        .get(..ROUTE_HEADER_BYTES)
        .ok_or(MalformedRouteMessage)?;
    let flags = read_i32(header, ROUTE_FLAGS_OFFSET)?;
    let errno = read_i32(header, ROUTE_ERRNO_OFFSET)?;
    if errno != 0 || flags & (RTF_LLINFO | RTF_WASCLONED) != 0 {
        return Ok(None);
    }
    Ok(Some(ChangeKind::Route))
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, MalformedRouteMessage> {
    let end = offset.checked_add(4).ok_or(MalformedRouteMessage)?;
    let field = bytes.get(offset..end).ok_or(MalformedRouteMessage)?;
    let field = <[u8; 4]>::try_from(field).map_err(|_| MalformedRouteMessage)?;
    Ok(i32::from_ne_bytes(field))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RTM_GET: u8 = 0x4;
    const RTM_MISS: u8 = 0x7;
    const RTM_RESOLVE: u8 = 0xb;
    const RTM_NEWMADDR: u8 = 0xf;
    const RTM_GET2: u8 = 0x14;

    /// One message of `length` bytes with its prefix filled in.
    fn message(message_type: u8, length: usize) -> Vec<u8> {
        let mut bytes = vec![0_u8; length];
        let declared = u16::try_from(length).unwrap().to_ne_bytes();
        bytes[..2].copy_from_slice(&declared);
        bytes[2] = RTM_VERSION;
        bytes[3] = message_type;
        bytes
    }

    fn route(message_type: u8, flags: i32, errno: i32) -> Vec<u8> {
        let mut bytes = message(message_type, 92);
        bytes[ROUTE_FLAGS_OFFSET..ROUTE_FLAGS_OFFSET + 4].copy_from_slice(&flags.to_ne_bytes());
        bytes[ROUTE_ERRNO_OFFSET..ROUTE_ERRNO_OFFSET + 4].copy_from_slice(&errno.to_ne_bytes());
        bytes
    }

    #[test]
    fn link_address_and_route_messages_are_classified() {
        const RTF_UP_GATEWAY_STATIC: i32 = 0x1 | 0x2 | 0x800;
        for (bytes, kind) in [
            (message(RTM_IFINFO, 112), ChangeKind::Link),
            (message(RTM_IFINFO2, 160), ChangeKind::Link),
            (message(RTM_NEWADDR, 20), ChangeKind::Address),
            (message(RTM_DELADDR, 20), ChangeKind::Address),
            (route(RTM_ADD, RTF_UP_GATEWAY_STATIC, 0), ChangeKind::Route),
            (
                route(RTM_DELETE, RTF_UP_GATEWAY_STATIC, 0),
                ChangeKind::Route,
            ),
            (route(RTM_CHANGE, 0, 0), ChangeKind::Route),
        ] {
            assert_eq!(route_message_kinds(&bytes), Ok(vec![kind]), "{kind:?}");
        }
    }

    #[test]
    fn unrelated_messages_and_cache_churn_are_skipped() {
        for message_type in [RTM_GET, RTM_MISS, RTM_RESOLVE, RTM_NEWMADDR, RTM_GET2, 0xff] {
            assert_eq!(
                route_message_kinds(&route(message_type, 0, 0)),
                Ok(Vec::new()),
                "{message_type:#x}"
            );
        }
        // Neighbour-cache entries, cloned host routes, and failed requests.
        assert_eq!(
            route_message_kinds(&route(RTM_ADD, RTF_LLINFO | 0x1, 0)),
            Ok(Vec::new())
        );
        assert_eq!(
            route_message_kinds(&route(RTM_DELETE, RTF_WASCLONED | 0x4, 0)),
            Ok(Vec::new())
        );
        assert_eq!(
            route_message_kinds(&route(RTM_ADD, 0x1, 17)),
            Ok(Vec::new())
        );
    }

    #[test]
    fn one_read_may_carry_several_messages() {
        let mut bytes = message(RTM_NEWADDR, 20);
        bytes.extend(route(RTM_GET, 0, 0));
        bytes.extend(message(RTM_IFINFO, 112));
        assert_eq!(
            route_message_kinds(&bytes),
            Ok(vec![ChangeKind::Address, ChangeKind::Link])
        );
        assert_eq!(route_message_kinds(&[]), Ok(Vec::new()));
    }

    #[test]
    fn declared_lengths_are_checked_before_any_field_is_read() {
        let valid = message(RTM_IFINFO, 112);

        // Too short to hold even the length field or the common prefix.
        assert_eq!(route_message_kinds(&valid[..1]), Err(MalformedRouteMessage));
        let mut short = message(RTM_IFINFO, 4);
        short[..2].copy_from_slice(&3_u16.to_ne_bytes());
        assert_eq!(route_message_kinds(&short), Err(MalformedRouteMessage));
        short[..2].copy_from_slice(&0_u16.to_ne_bytes());
        assert_eq!(route_message_kinds(&short), Err(MalformedRouteMessage));

        // A declared length beyond the read, including a truncated read.
        assert_eq!(
            route_message_kinds(&valid[..111]),
            Err(MalformedRouteMessage)
        );
        let mut overlong = valid.clone();
        overlong[..2].copy_from_slice(&u16::MAX.to_ne_bytes());
        assert_eq!(route_message_kinds(&overlong), Err(MalformedRouteMessage));

        // A trailing fragment after a valid message rejects the whole read.
        let mut trailing = valid.clone();
        trailing.extend_from_slice(&[4, 0]);
        assert_eq!(route_message_kinds(&trailing), Err(MalformedRouteMessage));

        // A route message too short for the fields it must be checked by.
        let mut stub = message(RTM_ADD, ROUTE_HEADER_BYTES - 1);
        assert_eq!(route_message_kinds(&stub), Err(MalformedRouteMessage));
        stub = message(RTM_ADD, ROUTE_HEADER_BYTES);
        assert_eq!(route_message_kinds(&stub), Ok(vec![ChangeKind::Route]));

        // Another version's layout is unknown.
        let mut other_version = valid;
        other_version[2] = RTM_VERSION + 1;
        assert_eq!(
            route_message_kinds(&other_version),
            Err(MalformedRouteMessage)
        );
    }

    #[test]
    fn arbitrary_bytes_never_panic() {
        // A deterministic sweep of short prefixes with every length value
        // that could matter, standing in for a fuzzer.
        let mut state = 0x2545_f491_u32;
        for _ in 0..4096 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            let length = (state % 96) as usize;
            let bytes: Vec<u8> = (0..length)
                .map(|index| (state.rotate_left(index as u32) & 0xff) as u8)
                .collect();
            let _ = route_message_kinds(&bytes);
        }
        let _ = route_message_kinds(&[0xff; 3]);
    }

    #[test]
    fn errors_are_topology_free() {
        let rendered = format!("{MalformedRouteMessage} {MalformedRouteMessage:?}");
        assert!(!rendered.contains("en0"));
        assert!(!rendered.contains('.'));
    }
}

/// On macOS the layout this parser hard-codes is checked against the
/// platform's own definitions.
#[cfg(all(test, target_os = "macos"))]
mod layout_tests {
    use std::mem::offset_of;

    use super::*;

    #[test]
    fn constants_and_offsets_match_the_system_headers() {
        assert_eq!(i32::from(RTM_VERSION), libc::RTM_VERSION);
        for (ours, system) in [
            (RTM_ADD, libc::RTM_ADD),
            (RTM_DELETE, libc::RTM_DELETE),
            (RTM_CHANGE, libc::RTM_CHANGE),
            (RTM_NEWADDR, libc::RTM_NEWADDR),
            (RTM_DELADDR, libc::RTM_DELADDR),
            (RTM_IFINFO, libc::RTM_IFINFO),
            (RTM_IFINFO2, libc::RTM_IFINFO2),
        ] {
            assert_eq!(i32::from(ours), system);
        }
        assert_eq!(RTF_LLINFO, libc::RTF_LLINFO);
        assert_eq!(RTF_WASCLONED, libc::RTF_WASCLONED);
        assert_eq!(offset_of!(libc::rt_msghdr, rtm_msglen), 0);
        assert_eq!(offset_of!(libc::rt_msghdr, rtm_version), 2);
        assert_eq!(offset_of!(libc::rt_msghdr, rtm_type), 3);
        assert_eq!(offset_of!(libc::rt_msghdr, rtm_flags), ROUTE_FLAGS_OFFSET);
        assert_eq!(offset_of!(libc::rt_msghdr, rtm_errno), ROUTE_ERRNO_OFFSET);
        assert!(size_of::<libc::rt_msghdr>() >= ROUTE_HEADER_BYTES);
        for prefix in [
            offset_of!(libc::if_msghdr, ifm_type),
            offset_of!(libc::ifa_msghdr, ifam_type),
        ] {
            assert_eq!(prefix, 3);
        }
    }
}
