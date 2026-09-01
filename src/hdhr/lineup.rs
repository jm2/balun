use std::collections::BTreeSet;
use std::fmt;

use reqwest::Url;
use serde::Deserialize;
use serde::de::{DeserializeSeed, IgnoredAny, SeqAccess, Visitor};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::{DeviceEndpoint, DeviceHttpClient, DeviceHttpError, DeviceInfo, EndpointError};
use crate::domain::{ChannelKey, DeviceId, GuideNumber, InvalidGuideNumber};

pub const MAX_GUIDE_NAME_BYTES: usize = 256;
pub const MAX_TAGS_BYTES: usize = 256;
pub const MAX_TAG_COUNT: usize = 16;
pub const MAX_TAG_BYTES: usize = 32;

/// One validated channel row from a device-scoped lineup.
#[derive(Clone, Eq, PartialEq)]
pub struct LineupChannel {
    key: ChannelKey,
    name: String,
    tags: BTreeSet<String>,
    stream_url: Url,
}

impl LineupChannel {
    #[must_use]
    pub const fn key(&self) -> &ChannelKey {
        &self.key
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tags(&self) -> &BTreeSet<String> {
        &self.tags
    }

    #[must_use]
    pub fn is_favorite(&self) -> bool {
        self.tags.contains("favorite")
    }

    #[must_use]
    pub fn is_drm(&self) -> bool {
        self.tags.contains("drm")
    }

    #[must_use]
    pub fn is_hd(&self) -> bool {
        self.tags.contains("hd")
    }

    /// A responder-pinned URL. Callers must still refuse playback when
    /// [`Self::is_drm`] is true.
    #[must_use]
    pub const fn stream_url(&self) -> &Url {
        &self.stream_url
    }
}

impl fmt::Debug for LineupChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LineupChannel")
            .field("key", &self.key)
            .field("name", &self.name)
            .field("tags", &self.tags)
            .field("stream_url", &"<redacted>")
            .finish()
    }
}

/// Complete last-known-good lineup for exactly one DeviceID.
#[derive(Clone, Eq, PartialEq)]
pub struct DeviceLineup {
    device_id: DeviceId,
    channels: Vec<LineupChannel>,
}

/// Identity-verified metadata and lineup fetched from one pinned device
/// endpoint. Keeping this operation combined prevents callers from stamping a
/// lineup with an identity that was never validated against `/discover.json`.
#[derive(Clone, Eq, PartialEq)]
pub struct DeviceSnapshot {
    info: DeviceInfo,
    lineup: DeviceLineup,
}

impl DeviceSnapshot {
    #[must_use]
    pub const fn info(&self) -> &DeviceInfo {
        &self.info
    }

    #[must_use]
    pub const fn lineup(&self) -> &DeviceLineup {
        &self.lineup
    }

    #[cfg(test)]
    pub(super) fn debug_redaction_fixture(device_id: DeviceId) -> Self {
        let endpoint = DeviceEndpoint::from_discovery(
            "127.0.0.1:65001".parse().unwrap(),
            Some("http://127.0.0.1/"),
            None,
        )
        .unwrap();
        let lineup = parse_lineup(
            br#"[{"GuideNumber":"5.1","GuideName":"private channel","Tags":"favorite,drm,hd","URL":"http://127.0.0.1:5004/auto/v5.1"}]"#,
            device_id,
            &endpoint,
            1,
        )
        .unwrap();
        Self {
            info: DeviceInfo::debug_redaction_fixture(device_id),
            lineup,
        }
    }
}

impl fmt::Debug for DeviceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceSnapshot")
            .field("device_id", &self.info.device_id())
            .field("channel_count", &self.lineup.channels().len())
            .finish()
    }
}

impl DeviceLineup {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub fn channels(&self) -> &[LineupChannel] {
        &self.channels
    }
}

impl fmt::Debug for DeviceLineup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceLineup")
            .field("device_id", &self.device_id)
            .field("channel_count", &self.channels.len())
            .finish()
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawLineupChannel {
    guide_number: String,
    guide_name: String,
    #[serde(default)]
    tags: Option<String>,
    #[serde(default)]
    favorite: Option<RawFlag>,
    #[serde(default, rename = "DRM")]
    drm: Option<RawFlag>,
    #[serde(default, rename = "HD")]
    hd: Option<RawFlag>,
    #[serde(rename = "URL")]
    url: String,
}

/// Firmware generations disagree on whether lineup attributes live in the
/// documented comma-separated `Tags` field or in dedicated 0/1 fields. Some
/// third-party fixtures also use JSON booleans, so accept both representations
/// while rejecting every other value.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawFlag {
    Number(u8),
    Boolean(bool),
}

enum BoundedRawLineup {
    WithinLimit(Vec<RawLineupChannel>),
    TooMany { actual: usize },
}

struct RawLineupSeed {
    maximum_channels: usize,
}

impl<'de> DeserializeSeed<'de> for RawLineupSeed {
    type Value = BoundedRawLineup;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(RawLineupVisitor {
            maximum_channels: self.maximum_channels,
        })
    }
}

struct RawLineupVisitor {
    maximum_channels: usize,
}

impl<'de> Visitor<'de> for RawLineupVisitor {
    type Value = BoundedRawLineup;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an array of HDHomeRun lineup rows")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence.size_hint().unwrap_or(0).min(self.maximum_channels);
        let mut rows = Vec::with_capacity(capacity);
        while rows.len() < self.maximum_channels {
            let Some(row) = sequence.next_element::<RawLineupChannel>()? else {
                return Ok(BoundedRawLineup::WithinLimit(rows));
            };
            rows.push(row);
        }

        let mut actual = rows.len();
        while sequence.next_element::<IgnoredAny>()?.is_some() {
            actual = actual.saturating_add(1);
        }
        if actual > self.maximum_channels {
            Ok(BoundedRawLineup::TooMany { actual })
        } else {
            Ok(BoundedRawLineup::WithinLimit(rows))
        }
    }
}

fn parse_lineup(
    body: &[u8],
    device_id: DeviceId,
    endpoint: &DeviceEndpoint,
    maximum_channels: usize,
) -> Result<DeviceLineup, LineupError> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let bounded = RawLineupSeed { maximum_channels }
        .deserialize(&mut deserializer)
        .map_err(LineupError::Json)?;
    deserializer.end().map_err(LineupError::Json)?;
    let raw_channels = match bounded {
        BoundedRawLineup::WithinLimit(rows) => rows,
        BoundedRawLineup::TooMany { actual } => {
            return Err(LineupError::TooManyChannels {
                actual,
                maximum: maximum_channels,
            });
        }
    };

    let mut seen_numbers = BTreeSet::new();
    let mut channels = Vec::with_capacity(raw_channels.len());
    for (index, raw) in raw_channels.into_iter().enumerate() {
        let guide_number = GuideNumber::new(raw.guide_number)
            .map_err(|source| LineupError::InvalidGuideNumber { index, source })?;
        if !seen_numbers.insert(guide_number.clone()) {
            return Err(LineupError::DuplicateGuideNumber { guide_number });
        }

        let name = normalize_name(index, raw.guide_name)?;
        let mut tags = parse_tags(index, raw.tags)?;
        merge_flag(index, "Favorite", raw.favorite, "favorite", &mut tags)?;
        merge_flag(index, "DRM", raw.drm, "drm", &mut tags)?;
        merge_flag(index, "HD", raw.hd, "hd", &mut tags)?;
        let stream_url = endpoint
            .normalize_stream_url(&raw.url)
            .map_err(|source| LineupError::InvalidStreamUrl { index, source })?;
        let expected_path = format!("/auto/v{guide_number}");
        if stream_url.path() != expected_path {
            return Err(LineupError::InvalidField {
                index,
                field: "URL",
                reason: "stream path does not match GuideNumber",
            });
        }

        channels.push(LineupChannel {
            key: ChannelKey::new(device_id, guide_number),
            name,
            tags,
            stream_url,
        });
    }
    channels.sort_by(|left, right| left.key.cmp(&right.key));

    Ok(DeviceLineup {
        device_id,
        channels,
    })
}

fn normalize_name(index: usize, value: String) -> Result<String, LineupError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(LineupError::InvalidField {
            index,
            field: "GuideName",
            reason: "value is empty",
        });
    }
    if value.len() > MAX_GUIDE_NAME_BYTES {
        return Err(LineupError::InvalidField {
            index,
            field: "GuideName",
            reason: "value is too long",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(LineupError::InvalidField {
            index,
            field: "GuideName",
            reason: "control characters are forbidden",
        });
    }
    Ok(value.to_owned())
}

fn parse_tags(index: usize, value: Option<String>) -> Result<BTreeSet<String>, LineupError> {
    let Some(value) = value else {
        return Ok(BTreeSet::new());
    };
    if value.len() > MAX_TAGS_BYTES {
        return Err(LineupError::InvalidField {
            index,
            field: "Tags",
            reason: "value is too long",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(LineupError::InvalidField {
            index,
            field: "Tags",
            reason: "control characters are forbidden",
        });
    }

    let mut tags = BTreeSet::new();
    for token in value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        if token.len() > MAX_TAG_BYTES {
            return Err(LineupError::InvalidField {
                index,
                field: "Tags",
                reason: "one tag is too long",
            });
        }
        tags.insert(token.to_ascii_lowercase());
        if tags.len() > MAX_TAG_COUNT {
            return Err(LineupError::InvalidField {
                index,
                field: "Tags",
                reason: "too many tags",
            });
        }
    }
    Ok(tags)
}

fn merge_flag(
    index: usize,
    field: &'static str,
    value: Option<RawFlag>,
    tag: &'static str,
    tags: &mut BTreeSet<String>,
) -> Result<(), LineupError> {
    let enabled = match value {
        None | Some(RawFlag::Number(0)) | Some(RawFlag::Boolean(false)) => false,
        Some(RawFlag::Number(1)) | Some(RawFlag::Boolean(true)) => true,
        Some(RawFlag::Number(_)) => {
            return Err(LineupError::InvalidField {
                index,
                field,
                reason: "flag must be 0, 1, false, or true",
            });
        }
    };
    if enabled {
        tags.insert(tag.to_owned());
    }
    Ok(())
}

impl DeviceHttpClient {
    /// Fetch and identity-check metadata before accepting a lineup from the
    /// same responder-pinned endpoint.
    pub async fn fetch_device_snapshot(
        &self,
        endpoint: &DeviceEndpoint,
        expected_device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<DeviceSnapshot, DeviceSnapshotError> {
        let info = self
            .fetch_device_info(endpoint, expected_device_id, cancellation)
            .await
            .map_err(DeviceSnapshotError::Metadata)?;
        let lineup = self
            .fetch_lineup(endpoint, expected_device_id, cancellation)
            .await
            .map_err(DeviceSnapshotError::Lineup)?;
        Ok(DeviceSnapshot { info, lineup })
    }

    async fn fetch_lineup(
        &self,
        endpoint: &DeviceEndpoint,
        device_id: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<DeviceLineup, LineupFetchError> {
        let body = self
            .get_lineup_json(endpoint, cancellation)
            .await
            .map_err(LineupFetchError::Http)?;
        parse_lineup(
            &body,
            device_id,
            endpoint,
            self.config().max_lineup_channels(),
        )
        .map_err(LineupFetchError::Lineup)
    }
}

#[derive(Debug, Error)]
pub enum DeviceSnapshotError {
    #[error("device metadata failed: {0}")]
    Metadata(#[source] DeviceHttpError),

    #[error("device lineup failed: {0}")]
    Lineup(#[source] LineupFetchError),
}

#[derive(Debug, Error)]
pub enum LineupFetchError {
    #[error(transparent)]
    Http(#[from] DeviceHttpError),

    #[error(transparent)]
    Lineup(#[from] LineupError),
}

#[derive(Debug, Error)]
pub enum LineupError {
    #[error("invalid lineup JSON: {0}")]
    Json(#[source] serde_json::Error),

    #[error("lineup has {actual} channels; maximum is {maximum}")]
    TooManyChannels { actual: usize, maximum: usize },

    #[error("invalid GuideNumber in lineup row {index}: {source}")]
    InvalidGuideNumber {
        index: usize,
        #[source]
        source: InvalidGuideNumber,
    },

    #[error("lineup contains duplicate GuideNumber {guide_number}")]
    DuplicateGuideNumber { guide_number: GuideNumber },

    #[error("invalid stream URL in lineup row {index}: {source}")]
    InvalidStreamUrl {
        index: usize,
        #[source]
        source: EndpointError,
    },

    #[error("invalid {field} in lineup row {index}: {reason}")]
    InvalidField {
        index: usize,
        field: &'static str,
        reason: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hdhr::test_support::{ScriptedHttpServer, ScriptedResponse, response};

    fn id() -> DeviceId {
        DeviceId::new(0x105A_1232).unwrap()
    }

    fn endpoint() -> DeviceEndpoint {
        DeviceEndpoint::from_discovery(
            "192.0.2.10:65001".parse().unwrap(),
            Some("http://fixture.invalid"),
            None,
        )
        .unwrap()
    }

    #[test]
    fn parses_sorts_and_scopes_documented_lineup_rows() {
        let body = br#"[
          {
            "GuideNumber":"10.1",
            "GuideName":"WXYZ-HD",
            "Tags":" favorite,Future ",
            "URL":"http://fixture.invalid:5004/auto/v10.1",
            "VideoCodec":"MPEG2",
            "HD":1
          },
          {
            "GuideNumber":"2.2",
            "GuideName":"WXQZ",
            "Tags":"drm",
            "URL":"http://192.0.2.10:5004/auto/v2.2"
          }
        ]"#;

        let lineup = parse_lineup(body, id(), &endpoint(), 10).unwrap();

        assert_eq!(lineup.device_id(), id());
        assert_eq!(lineup.channels().len(), 2);
        assert_eq!(lineup.channels()[0].key().guide_number().as_str(), "2.2");
        assert!(lineup.channels()[0].is_drm());
        assert_eq!(lineup.channels()[1].key().device_id(), id());
        assert!(lineup.channels()[1].is_favorite());
        assert!(lineup.channels()[1].is_hd());
        assert!(lineup.channels()[1].tags().contains("future"));
        assert_eq!(
            lineup.channels()[1].stream_url().as_str(),
            "http://192.0.2.10:5004/auto/v10.1"
        );
    }

    #[test]
    fn accepts_current_firmware_flags_and_boolean_compatibility() {
        let body = br#"[
          {
            "GuideNumber":"7.1",
            "GuideName":"CURRENT",
            "Favorite":1,
            "DRM":1,
            "HD":1,
            "URL":"http://192.0.2.10:5004/auto/v7.1"
          },
          {
            "GuideNumber":"8.1",
            "GuideName":"BOOLEAN",
            "Favorite":true,
            "DRM":false,
            "HD":true,
            "URL":"http://192.0.2.10:5004/auto/v8.1"
          }
        ]"#;

        let lineup = parse_lineup(body, id(), &endpoint(), 10).unwrap();

        assert!(lineup.channels()[0].is_favorite());
        assert!(lineup.channels()[0].is_drm());
        assert!(lineup.channels()[0].is_hd());
        assert!(lineup.channels()[1].is_favorite());
        assert!(!lineup.channels()[1].is_drm());
        assert!(lineup.channels()[1].is_hd());
    }

    #[test]
    fn rejects_invalid_dedicated_flag_values_and_duplicates() {
        for body in [
            br#"[{"GuideNumber":"7.1","GuideName":"A","Favorite":2,"URL":"http://192.0.2.10:5004/auto/v7.1"}]"#.as_slice(),
            br#"[{"GuideNumber":"7.1","GuideName":"A","DRM":"yes","URL":"http://192.0.2.10:5004/auto/v7.1"}]"#.as_slice(),
            br#"[{"GuideNumber":"7.1","GuideName":"A","HD":1,"HD":1,"URL":"http://192.0.2.10:5004/auto/v7.1"}]"#.as_slice(),
        ] {
            assert!(parse_lineup(body, id(), &endpoint(), 10).is_err());
        }
    }

    #[test]
    fn accepts_an_empty_lineup_and_missing_tags() {
        assert!(
            parse_lineup(b"[]", id(), &endpoint(), 10)
                .unwrap()
                .channels()
                .is_empty()
        );
        let lineup = parse_lineup(
            br#"[{"GuideNumber":"7.1","GuideName":"TEST","URL":"http://192.0.2.10:5004/auto/v7.1"}]"#,
            id(),
            &endpoint(),
            10,
        )
        .unwrap();
        assert!(lineup.channels()[0].tags().is_empty());
    }

    #[test]
    fn rejects_duplicate_numbers_and_recognized_json_fields() {
        let duplicate_number = br#"[
          {"GuideNumber":"7.1","GuideName":"A","URL":"http://192.0.2.10:5004/auto/v7.1"},
          {"GuideNumber":"7.1","GuideName":"B","URL":"http://192.0.2.10:5004/auto/v7.1"}
        ]"#;
        assert!(matches!(
            parse_lineup(duplicate_number, id(), &endpoint(), 10),
            Err(LineupError::DuplicateGuideNumber { .. })
        ));

        let duplicate_field = br#"[{
          "GuideNumber":"7.1",
          "GuideNumber":"7.2",
          "GuideName":"A",
          "URL":"http://192.0.2.10:5004/auto/v7.1"
        }]"#;
        assert!(matches!(
            parse_lineup(duplicate_field, id(), &endpoint(), 10),
            Err(LineupError::Json(_))
        ));
    }

    #[test]
    fn rejects_oversized_lineups_before_allocating_every_raw_row() {
        let body = br#"[
          {"GuideNumber":"1","GuideName":"A","URL":"http://192.0.2.10:5004/auto/v1"},
          {"GuideNumber":"2","GuideName":"B","URL":"http://192.0.2.10:5004/auto/v2"}
        ]"#;
        assert!(matches!(
            parse_lineup(body, id(), &endpoint(), 1),
            Err(LineupError::TooManyChannels {
                actual: 2,
                maximum: 1
            })
        ));
    }

    #[test]
    fn oversized_lineup_skips_semantic_conversion_after_the_limit() {
        let body = br#"[
          {"GuideNumber":"1","GuideName":"A","URL":"http://192.0.2.10:5004/auto/v1"},
          {"GuideNumber":{"unexpected":"shape"},"GuideName":["not","a","name"],"URL":false},
          {"arbitrary":[1,2,3]}
        ]"#;

        assert!(matches!(
            parse_lineup(body, id(), &endpoint(), 1),
            Err(LineupError::TooManyChannels {
                actual: 3,
                maximum: 1
            })
        ));
    }

    #[test]
    fn rejects_stream_urls_that_escape_the_device_or_channel() {
        for url in [
            "http://192.0.2.11:5004/auto/v7.1",
            "http://192.0.2.10:80/auto/v7.1",
            "http://192.0.2.10:6000/auto/v7.1",
            "http://user@192.0.2.10:5004/auto/v7.1",
            "http://192.0.2.10:5004/auto/v7.1?secret=1",
            "http://192.0.2.10:5004/not-auto/v7.1",
            "http://192.0.2.10:5004/auto/v8.1",
        ] {
            let body = format!(r#"[{{"GuideNumber":"7.1","GuideName":"TEST","URL":"{url}"}}]"#);
            assert!(parse_lineup(body.as_bytes(), id(), &endpoint(), 10).is_err());
        }
    }

    #[test]
    fn rejects_unsafe_names_tags_and_invalid_guide_numbers() {
        for body in [
            br#"[{"GuideNumber":"","GuideName":"A","URL":"http://192.0.2.10:5004/auto/v1"}]"#.as_slice(),
            b"[{\"GuideNumber\":\"1\",\"GuideName\":\"bad\\nname\",\"URL\":\"http://192.0.2.10:5004/auto/v1\"}]".as_slice(),
            b"[{\"GuideNumber\":\"1\",\"GuideName\":\"A\",\"Tags\":\"bad\\ntag\",\"URL\":\"http://192.0.2.10:5004/auto/v1\"}]".as_slice(),
        ] {
            assert!(parse_lineup(body, id(), &endpoint(), 10).is_err());
        }
    }

    #[tokio::test]
    async fn fetches_a_bounded_lineup_from_the_pinned_responder() {
        let body = br#"[
          {
            "GuideNumber":"5.1",
            "GuideName":"WXYZ-HD",
            "Tags":"favorite",
            "URL":"http://127.0.0.1:5004/auto/v5.1"
          }
        ]"#;
        let server = ScriptedHttpServer::start(vec![ScriptedResponse::immediate(response(
            "200 OK",
            &[("Content-Length", body.len().to_string())],
            body,
        ))]);
        let endpoint = DeviceEndpoint::from_discovery(
            std::net::SocketAddr::new(server.address().ip(), 65_001),
            Some(&server.base_url()),
            None,
        )
        .unwrap();

        let lineup = DeviceHttpClient::default()
            .fetch_lineup(&endpoint, id(), &CancellationToken::new())
            .await
            .unwrap();
        let requests = server.finish();

        assert_eq!(lineup.channels().len(), 1);
        assert!(lineup.channels()[0].is_favorite());
        assert_eq!(requests.len(), 1);
        assert!(String::from_utf8_lossy(&requests[0]).starts_with("GET /lineup.json HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn snapshot_verifies_identity_before_requesting_the_lineup() {
        let metadata = br#"{"DeviceID":"105A1232","FriendlyName":"Fixture"}"#;
        let lineup = br#"[{"GuideNumber":"5.1","GuideName":"WXYZ","URL":"http://127.0.0.1:5004/auto/v5.1"}]"#;
        let server = ScriptedHttpServer::start(vec![
            ScriptedResponse::immediate(response(
                "200 OK",
                &[("Content-Length", metadata.len().to_string())],
                metadata,
            )),
            ScriptedResponse::immediate(response(
                "200 OK",
                &[("Content-Length", lineup.len().to_string())],
                lineup,
            )),
        ]);
        let endpoint = DeviceEndpoint::from_discovery(
            std::net::SocketAddr::new(server.address().ip(), 65_001),
            Some(&server.base_url()),
            None,
        )
        .unwrap();

        let snapshot = DeviceHttpClient::default()
            .fetch_device_snapshot(&endpoint, id(), &CancellationToken::new())
            .await
            .unwrap();
        let requests = server.finish();

        assert_eq!(snapshot.info().device_id(), id());
        assert_eq!(snapshot.lineup().device_id(), id());
        assert_eq!(requests.len(), 2);
        assert!(String::from_utf8_lossy(&requests[0]).starts_with("GET /discover.json "));
        assert!(String::from_utf8_lossy(&requests[1]).starts_with("GET /lineup.json "));
    }

    #[tokio::test]
    async fn snapshot_does_not_fetch_lineup_after_identity_mismatch() {
        let metadata = br#"{"DeviceID":"105A1243"}"#;
        let server = ScriptedHttpServer::start(vec![ScriptedResponse::immediate(response(
            "200 OK",
            &[("Content-Length", metadata.len().to_string())],
            metadata,
        ))]);
        let endpoint = DeviceEndpoint::from_discovery(
            std::net::SocketAddr::new(server.address().ip(), 65_001),
            Some(&server.base_url()),
            None,
        )
        .unwrap();

        let error = DeviceHttpClient::default()
            .fetch_device_snapshot(&endpoint, id(), &CancellationToken::new())
            .await
            .unwrap_err();
        let requests = server.finish();

        assert!(matches!(
            error,
            DeviceSnapshotError::Metadata(DeviceHttpError::DeviceIdMismatch { .. })
        ));
        assert_eq!(requests.len(), 1);
    }
}
