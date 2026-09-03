use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(all(test, feature = "desktop"))]
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::Duration;

use reqwest::header::ACCEPT;
use reqwest::{StatusCode, Url, redirect};
use serde::Deserialize;
use thiserror::Error;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

use crate::discovery::LocatorClaim;
use crate::domain::DeviceId;

pub const MAX_ADVERTISED_URL_BYTES: usize = 4_096;
pub const MAX_DEVICE_JSON_BYTES: usize = 64 * 1_024;
pub const MAX_LINEUP_JSON_BYTES: usize = 4 * 1_024 * 1_024;
pub const MAX_LINEUP_CHANNELS: usize = 4_096;

const MAX_DEVICE_TEXT_BYTES: usize = 256;
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A responder-pinned set of local HDHomeRun HTTP endpoints.
///
/// Advertised hostnames are rewritten to the UDP responder address, so using
/// this type never performs DNS resolution or follows an advertised cross-host
/// authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceEndpoint {
    source: SocketAddr,
    base_url: Url,
    discover_url: Url,
    lineup_url: Url,
}

impl DeviceEndpoint {
    pub fn from_locator(locator: &LocatorClaim) -> Result<Self, EndpointError> {
        let endpoint = Self::from_discovery(
            locator.source(),
            locator.advertised_base_url(),
            locator.advertised_lineup_url(),
        )?;
        ensure_metadata_port(&endpoint.base_url, UrlRole::Base)?;
        ensure_metadata_port(&endpoint.lineup_url, UrlRole::Lineup)?;
        Ok(endpoint)
    }

    /// Construct an endpoint without the production port policy. This exists
    /// for sibling protocol modules and loopback HTTP fixtures; application
    /// callers must start from a validated registry locator.
    pub(super) fn from_discovery(
        source: SocketAddr,
        advertised_base_url: Option<&str>,
        advertised_lineup_url: Option<&str>,
    ) -> Result<Self, EndpointError> {
        validate_source(source)?;

        let base_url = match advertised_base_url {
            Some(value) => normalize_url(value, source.ip(), UrlRole::Base)?,
            None => default_base_url(source.ip())?,
        };
        let discover_url =
            base_url
                .join("discover.json")
                .map_err(|_| EndpointError::InvalidUrl {
                    role: UrlRole::Discover,
                    reason: "could not append the metadata path",
                })?;
        let lineup_url = match advertised_lineup_url {
            Some(value) => {
                let lineup = normalize_url(value, source.ip(), UrlRole::Lineup)?;
                if !same_origin(&base_url, &lineup) {
                    return Err(EndpointError::OriginMismatch);
                }
                lineup
            }
            None => base_url
                .join("lineup.json")
                .map_err(|_| EndpointError::InvalidUrl {
                    role: UrlRole::Lineup,
                    reason: "could not append the lineup path",
                })?,
        };

        Ok(Self {
            source,
            base_url,
            discover_url,
            lineup_url,
        })
    }

    #[must_use]
    pub const fn source(&self) -> SocketAddr {
        self.source
    }

    #[must_use]
    pub const fn base_url(&self) -> &Url {
        &self.base_url
    }

    #[must_use]
    pub const fn discover_url(&self) -> &Url {
        &self.discover_url
    }

    #[must_use]
    pub const fn lineup_url(&self) -> &Url {
        &self.lineup_url
    }

    pub(crate) fn normalize_stream_url(&self, value: &str) -> Result<Url, EndpointError> {
        let url = normalize_url(value, self.source.ip(), UrlRole::Stream)?;
        require_port(&url, UrlRole::Stream, 5_004)?;
        Ok(url)
    }
}

fn require_port(url: &Url, role: UrlRole, expected: u16) -> Result<(), EndpointError> {
    let actual = url
        .port_or_known_default()
        .expect("validated HTTP URLs always have a known effective port");
    if actual != expected {
        return Err(EndpointError::UnexpectedPort {
            role,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Enforce the production metadata-port policy.
///
/// The effective port must be 80. Test builds additionally exempt exactly one
/// installed loopback fake-device port, because an unprivileged test process
/// cannot bind port 80; only that exact port on loopback is accepted, and
/// production URLs and neighboring fixtures keep their fixed behavior.
fn ensure_metadata_port(url: &Url, role: UrlRole) -> Result<(), EndpointError> {
    let actual = url
        .port_or_known_default()
        .expect("validated HTTP URLs always have a known effective port");
    if actual == 80 {
        return Ok(());
    }
    #[cfg(all(test, feature = "desktop"))]
    if actual == TEST_METADATA_PORT.load(Ordering::SeqCst) && is_loopback_host(url) {
        return Ok(());
    }
    Err(EndpointError::UnexpectedPort {
        role,
        expected: 80,
        actual,
    })
}

#[cfg(all(test, feature = "desktop"))]
fn is_loopback_host(url: &Url) -> bool {
    url.host_str() == Some("127.0.0.1")
}

#[cfg(all(test, feature = "desktop"))]
static TEST_METADATA_PORT: AtomicU16 = AtomicU16::new(0);

/// A process-wide test-only loopback metadata-port exemption.
///
/// The guard serializes nothing by itself; every holder must also serialize
/// the fake device and controller it applies to. Restoring the prior value on
/// drop keeps leaked references from silently weakening later tests.
#[cfg(all(test, feature = "desktop"))]
pub(crate) struct MetadataPortOverride {
    prior: u16,
}

#[cfg(all(test, feature = "desktop"))]
impl MetadataPortOverride {
    pub(crate) fn install(port: u16) -> Self {
        let prior = TEST_METADATA_PORT.swap(port, Ordering::SeqCst);
        Self { prior }
    }
}

#[cfg(all(test, feature = "desktop"))]
impl Drop for MetadataPortOverride {
    fn drop(&mut self) {
        TEST_METADATA_PORT.store(self.prior, Ordering::SeqCst);
    }
}

fn validate_source(source: SocketAddr) -> Result<(), EndpointError> {
    if source.ip().is_unspecified() || source.ip().is_multicast() {
        return Err(EndpointError::InvalidSource);
    }
    if matches!(source.ip(), IpAddr::V4(address) if address == Ipv4Addr::BROADCAST) {
        return Err(EndpointError::InvalidSource);
    }
    if matches!(source, SocketAddr::V6(address) if address.scope_id() != 0) {
        return Err(EndpointError::ScopedIpv6Unsupported);
    }
    Ok(())
}

fn default_base_url(source: IpAddr) -> Result<Url, EndpointError> {
    let authority = SocketAddr::new(source, 80);
    Url::parse(&format!("http://{authority}/")).map_err(|_| EndpointError::InvalidUrl {
        role: UrlRole::Base,
        reason: "could not construct a responder URL",
    })
}

fn normalize_url(value: &str, source: IpAddr, role: UrlRole) -> Result<Url, EndpointError> {
    if value.len() > MAX_ADVERTISED_URL_BYTES {
        return Err(EndpointError::UrlTooLong {
            role,
            actual: value.len(),
            maximum: MAX_ADVERTISED_URL_BYTES,
        });
    }

    let mut url = Url::parse(value).map_err(|_| EndpointError::InvalidUrl {
        role,
        reason: "URL syntax is invalid",
    })?;
    if url.scheme() != "http" {
        return Err(EndpointError::InvalidUrl {
            role,
            reason: "only direct HTTP device URLs are supported",
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(EndpointError::InvalidUrl {
            role,
            reason: "credentials are forbidden",
        });
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(EndpointError::InvalidUrl {
            role,
            reason: "query strings and fragments are forbidden",
        });
    }
    if url.port() == Some(0) {
        return Err(EndpointError::InvalidUrl {
            role,
            reason: "port zero is forbidden",
        });
    }

    let host = url.host_str().ok_or(EndpointError::InvalidUrl {
        role,
        reason: "host is missing",
    })?;
    let numeric_host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(advertised_ip) = numeric_host.parse::<IpAddr>()
        && advertised_ip != source
    {
        return Err(EndpointError::HostMismatch {
            expected: source,
            actual: advertised_ip,
        });
    }
    url.set_ip_host(source)
        .map_err(|()| EndpointError::InvalidUrl {
            role,
            reason: "could not pin the URL to the responder",
        })?;

    match role {
        UrlRole::Base => {
            if url.path() != "/" {
                return Err(EndpointError::InvalidUrl {
                    role,
                    reason: "base path must be root",
                });
            }
        }
        UrlRole::Discover => {
            if url.path() != "/discover.json" {
                return Err(EndpointError::InvalidUrl {
                    role,
                    reason: "metadata path must be /discover.json",
                });
            }
        }
        UrlRole::Lineup => {
            if url.path() != "/lineup.json" {
                return Err(EndpointError::InvalidUrl {
                    role,
                    reason: "lineup path must be /lineup.json",
                });
            }
        }
        UrlRole::Stream => {
            let channel = url.path().strip_prefix("/auto/v").unwrap_or_default();
            if channel.is_empty() {
                return Err(EndpointError::InvalidUrl {
                    role,
                    reason: "stream path must identify /auto/v<channel>",
                });
            }
        }
    }

    Ok(url)
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host() == right.host()
        && left.port_or_known_default() == right.port_or_known_default()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UrlRole {
    Base,
    Discover,
    Lineup,
    Stream,
}

impl fmt::Display for UrlRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Base => "base URL",
            Self::Discover => "metadata URL",
            Self::Lineup => "lineup URL",
            Self::Stream => "stream URL",
        })
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum EndpointError {
    #[error("device responder is not a usable unicast address")]
    InvalidSource,

    #[error("scoped IPv6 device HTTP is not supported yet; prefer another discovered locator")]
    ScopedIpv6Unsupported,

    #[error("{role} is {actual} bytes; maximum is {maximum}")]
    UrlTooLong {
        role: UrlRole,
        actual: usize,
        maximum: usize,
    },

    #[error("invalid {role}: {reason}")]
    InvalidUrl { role: UrlRole, reason: &'static str },

    #[error("advertised numeric host {actual} does not match responder {expected}")]
    HostMismatch { expected: IpAddr, actual: IpAddr },

    #[error("{role} uses port {actual}; expected port {expected}")]
    UnexpectedPort {
        role: UrlRole,
        expected: u16,
        actual: u16,
    },

    #[error("advertised lineup URL does not share the accepted device origin")]
    OriginMismatch,
}

/// Hard-bounded HTTP policy for direct device metadata and lineup requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceHttpConfig {
    connect_timeout: Duration,
    device_timeout: Duration,
    lineup_timeout: Duration,
    max_device_bytes: usize,
    max_lineup_bytes: usize,
    max_lineup_channels: usize,
}

impl DeviceHttpConfig {
    pub fn new(
        connect_timeout: Duration,
        device_timeout: Duration,
        lineup_timeout: Duration,
        max_device_bytes: usize,
        max_lineup_bytes: usize,
        max_lineup_channels: usize,
    ) -> Result<Self, InvalidHttpConfig> {
        validate_timeout("connect", connect_timeout, MAX_CONNECT_TIMEOUT)?;
        validate_timeout("device", device_timeout, MAX_REQUEST_TIMEOUT)?;
        validate_timeout("lineup", lineup_timeout, MAX_REQUEST_TIMEOUT)?;
        validate_limit("device bytes", max_device_bytes, MAX_DEVICE_JSON_BYTES)?;
        validate_limit("lineup bytes", max_lineup_bytes, MAX_LINEUP_JSON_BYTES)?;
        validate_limit("lineup channels", max_lineup_channels, MAX_LINEUP_CHANNELS)?;

        Ok(Self {
            connect_timeout,
            device_timeout,
            lineup_timeout,
            max_device_bytes,
            max_lineup_bytes,
            max_lineup_channels,
        })
    }

    #[must_use]
    pub const fn connect_timeout(self) -> Duration {
        self.connect_timeout
    }

    #[must_use]
    pub const fn device_timeout(self) -> Duration {
        self.device_timeout
    }

    #[must_use]
    pub const fn lineup_timeout(self) -> Duration {
        self.lineup_timeout
    }

    #[must_use]
    pub const fn max_device_bytes(self) -> usize {
        self.max_device_bytes
    }

    #[must_use]
    pub const fn max_lineup_bytes(self) -> usize {
        self.max_lineup_bytes
    }

    #[must_use]
    pub const fn max_lineup_channels(self) -> usize {
        self.max_lineup_channels
    }
}

impl Default for DeviceHttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(3),
            device_timeout: Duration::from_secs(5),
            lineup_timeout: Duration::from_secs(10),
            max_device_bytes: MAX_DEVICE_JSON_BYTES,
            max_lineup_bytes: MAX_LINEUP_JSON_BYTES,
            max_lineup_channels: MAX_LINEUP_CHANNELS,
        }
    }
}

fn validate_timeout(
    name: &'static str,
    value: Duration,
    maximum: Duration,
) -> Result<(), InvalidHttpConfig> {
    if value.is_zero() || value > maximum {
        return Err(InvalidHttpConfig::Timeout {
            name,
            value,
            maximum,
        });
    }
    Ok(())
}

fn validate_limit(
    name: &'static str,
    value: usize,
    maximum: usize,
) -> Result<(), InvalidHttpConfig> {
    if value == 0 || value > maximum {
        return Err(InvalidHttpConfig::Limit {
            name,
            value,
            maximum,
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InvalidHttpConfig {
    #[error("{name} timeout must be nonzero and at most {maximum:?}; got {value:?}")]
    Timeout {
        name: &'static str,
        value: Duration,
        maximum: Duration,
    },

    #[error("{name} limit must be between 1 and {maximum}; got {value}")]
    Limit {
        name: &'static str,
        value: usize,
        maximum: usize,
    },
}

/// Metadata returned by `/discover.json`, deliberately excluding DeviceAuth
/// and all advertised URLs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceInfo {
    device_id: DeviceId,
    friendly_name: Option<String>,
    model_number: Option<String>,
    firmware_name: Option<String>,
    firmware_version: Option<String>,
    tuner_count: Option<u8>,
}

impl DeviceInfo {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub fn friendly_name(&self) -> Option<&str> {
        self.friendly_name.as_deref()
    }

    #[must_use]
    pub fn model_number(&self) -> Option<&str> {
        self.model_number.as_deref()
    }

    #[must_use]
    pub fn firmware_name(&self) -> Option<&str> {
        self.firmware_name.as_deref()
    }

    #[must_use]
    pub fn firmware_version(&self) -> Option<&str> {
        self.firmware_version.as_deref()
    }

    #[must_use]
    pub const fn tuner_count(&self) -> Option<u8> {
        self.tuner_count
    }

    #[cfg(test)]
    pub(super) fn debug_redaction_fixture(device_id: DeviceId) -> Self {
        Self {
            device_id,
            friendly_name: Some("private fixture name".to_owned()),
            model_number: None,
            firmware_name: None,
            firmware_version: None,
            tuner_count: Some(1),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawDeviceId {
    Text(String),
    Number(u32),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawDeviceInfo {
    #[serde(rename = "DeviceID")]
    device_id: RawDeviceId,
    #[serde(default)]
    friendly_name: Option<String>,
    #[serde(default)]
    model_number: Option<String>,
    #[serde(default)]
    firmware_name: Option<String>,
    #[serde(default)]
    firmware_version: Option<String>,
    #[serde(default)]
    tuner_count: Option<u16>,
}

fn parse_device_info(body: &[u8], expected: DeviceId) -> Result<DeviceInfo, DeviceHttpError> {
    let raw: RawDeviceInfo = serde_json::from_slice(body).map_err(DeviceHttpError::Json)?;
    let device_id = match raw.device_id {
        RawDeviceId::Text(value) => {
            if value.len() != 8 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(DeviceHttpError::InvalidDeviceId);
            }
            let value =
                u32::from_str_radix(&value, 16).map_err(|_| DeviceHttpError::InvalidDeviceId)?;
            DeviceId::new(value).map_err(|_| DeviceHttpError::InvalidDeviceId)?
        }
        RawDeviceId::Number(value) => {
            DeviceId::new(value).map_err(|_| DeviceHttpError::InvalidDeviceId)?
        }
    };
    if device_id != expected {
        return Err(DeviceHttpError::DeviceIdMismatch {
            expected,
            actual: device_id,
        });
    }

    let tuner_count = match raw.tuner_count {
        Some(value @ 1..=32) => Some(u8::try_from(value).expect("range is bounded to u8")),
        Some(_) => {
            return Err(DeviceHttpError::InvalidField {
                field: "TunerCount",
                reason: "must be between 1 and 32",
            });
        }
        None => None,
    };

    Ok(DeviceInfo {
        device_id,
        friendly_name: normalize_optional_text("FriendlyName", raw.friendly_name)?,
        model_number: normalize_optional_text("ModelNumber", raw.model_number)?,
        firmware_name: normalize_optional_text("FirmwareName", raw.firmware_name)?,
        firmware_version: normalize_optional_text("FirmwareVersion", raw.firmware_version)?,
        tuner_count,
    })
}

fn normalize_optional_text(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<String>, DeviceHttpError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > MAX_DEVICE_TEXT_BYTES {
        return Err(DeviceHttpError::InvalidField {
            field,
            reason: "value is too long",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(DeviceHttpError::InvalidField {
            field,
            reason: "control characters are forbidden",
        });
    }
    Ok(Some(value.to_owned()))
}

#[derive(Clone)]
pub struct DeviceHttpClient {
    client: reqwest::Client,
    config: DeviceHttpConfig,
}

impl fmt::Debug for DeviceHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceHttpClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl DeviceHttpClient {
    pub fn new(config: DeviceHttpConfig) -> Result<Self, DeviceHttpError> {
        let request_timeout = config.device_timeout.max(config.lineup_timeout);
        let client = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .timeout(request_timeout)
            .redirect(redirect::Policy::none())
            .referer(false)
            .no_proxy()
            .pool_max_idle_per_host(1)
            .user_agent(concat!("Balun/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| DeviceHttpError::Transport(error.without_url()))?;
        Ok(Self { client, config })
    }

    #[must_use]
    pub const fn config(&self) -> DeviceHttpConfig {
        self.config
    }

    pub async fn fetch_device_info(
        &self,
        endpoint: &DeviceEndpoint,
        expected: DeviceId,
        cancellation: &CancellationToken,
    ) -> Result<DeviceInfo, DeviceHttpError> {
        let body = Zeroizing::new(
            self.get_json(
                endpoint.discover_url(),
                "fetch device metadata",
                self.config.max_device_bytes,
                self.config.device_timeout,
                cancellation,
            )
            .await?,
        );
        parse_device_info(&body, expected)
    }

    pub(super) async fn get_lineup_json(
        &self,
        endpoint: &DeviceEndpoint,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, DeviceHttpError> {
        self.get_json(
            endpoint.lineup_url(),
            "fetch channel lineup",
            self.config.max_lineup_bytes,
            self.config.lineup_timeout,
            cancellation,
        )
        .await
    }

    async fn get_json(
        &self,
        url: &Url,
        operation: &'static str,
        maximum: usize,
        deadline: Duration,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, DeviceHttpError> {
        if cancellation.is_cancelled() {
            return Err(DeviceHttpError::Cancelled);
        }

        let request = async {
            let mut response = self
                .client
                .get(url.clone())
                .header(ACCEPT, "application/json")
                .send()
                .await
                .map_err(|error| DeviceHttpError::Transport(error.without_url()))?;
            if response.status() != StatusCode::OK {
                return Err(DeviceHttpError::UnexpectedStatus {
                    operation,
                    status: response.status().as_u16(),
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > maximum as u64)
            {
                return Err(DeviceHttpError::BodyTooLarge { operation, maximum });
            }

            let capacity = response
                .content_length()
                .and_then(|length| usize::try_from(length).ok())
                .unwrap_or(0)
                .min(maximum);
            let mut body = Vec::with_capacity(capacity);
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| DeviceHttpError::Transport(error.without_url()))?
            {
                if chunk.len() > maximum.saturating_sub(body.len()) {
                    return Err(DeviceHttpError::BodyTooLarge { operation, maximum });
                }
                body.extend_from_slice(&chunk);
            }
            Ok(body)
        };

        tokio::select! {
            () = cancellation.cancelled() => Err(DeviceHttpError::Cancelled),
            result = timeout(deadline, request) => {
                result.map_err(|_| DeviceHttpError::Deadline { operation, deadline })?
            }
        }
    }
}

impl Default for DeviceHttpClient {
    fn default() -> Self {
        Self::new(DeviceHttpConfig::default()).expect("the built-in HTTP policy is valid")
    }
}

#[derive(Debug, Error)]
pub enum DeviceHttpError {
    #[error("device HTTP request failed: {0}")]
    Transport(#[source] reqwest::Error),

    #[error("{operation} returned HTTP {status}; redirects are not followed")]
    UnexpectedStatus {
        operation: &'static str,
        status: u16,
    },

    #[error("{operation} exceeded its {deadline:?} deadline")]
    Deadline {
        operation: &'static str,
        deadline: Duration,
    },

    #[error("{operation} exceeded the {maximum}-byte response limit")]
    BodyTooLarge {
        operation: &'static str,
        maximum: usize,
    },

    #[error("device HTTP operation was cancelled")]
    Cancelled,

    #[error("invalid device JSON: {0}")]
    Json(#[source] serde_json::Error),

    #[error("device JSON contains an invalid DeviceID")]
    InvalidDeviceId,

    #[error("device JSON identifies {actual}, expected {expected}")]
    DeviceIdMismatch {
        expected: DeviceId,
        actual: DeviceId,
    },

    #[error("invalid device field {field}: {reason}")]
    InvalidField {
        field: &'static str,
        reason: &'static str,
    },

    #[error(transparent)]
    Endpoint(#[from] EndpointError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{
        DeviceRegistry, DiscoveryMethod, DiscoveryObservation, RegistryInstant,
    };
    use crate::hdhr::test_support::{ScriptedHttpServer, ScriptedResponse, response};

    fn expected_id() -> DeviceId {
        DeviceId::new(0x105A_1232).unwrap()
    }

    fn endpoint_for(server: &ScriptedHttpServer) -> DeviceEndpoint {
        DeviceEndpoint::from_discovery(
            SocketAddr::new(server.address().ip(), 65_001),
            Some(&server.base_url()),
            None,
        )
        .unwrap()
    }

    fn endpoint_from_registry_urls(
        base_url: Option<&str>,
        lineup_url: Option<&str>,
    ) -> Result<DeviceEndpoint, EndpointError> {
        endpoint_from_registry_source_urls("192.0.2.10:65001", base_url, lineup_url)
    }

    fn endpoint_from_registry_source_urls(
        source: &str,
        base_url: Option<&str>,
        lineup_url: Option<&str>,
    ) -> Result<DeviceEndpoint, EndpointError> {
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                DiscoveryObservation {
                    device_id: expected_id(),
                    source: source.parse().unwrap(),
                    method: DiscoveryMethod::Targeted,
                    interface: None,
                    device_types: vec![1],
                    tuner_count: Some(4),
                    advertised_base_url: base_url.map(str::to_owned),
                    advertised_lineup_url: lineup_url.map(str::to_owned),
                },
                RegistryInstant::default(),
            )
            .unwrap();
        let locator = registry
            .get(expected_id())
            .unwrap()
            .preferred_locator()
            .unwrap();
        DeviceEndpoint::from_locator(locator)
    }

    fn config_with_device_limit(maximum: usize, deadline: Duration) -> DeviceHttpConfig {
        DeviceHttpConfig::new(
            Duration::from_secs(1),
            deadline,
            Duration::from_secs(1),
            maximum,
            1_024,
            16,
        )
        .unwrap()
    }

    #[test]
    fn constructs_and_pins_numeric_endpoints() {
        let endpoint = DeviceEndpoint::from_discovery(
            "192.0.2.10:65001".parse().unwrap(),
            Some("http://192.0.2.10:80"),
            Some("http://192.0.2.10:80/lineup.json"),
        )
        .unwrap();

        assert_eq!(endpoint.base_url().as_str(), "http://192.0.2.10/");
        assert_eq!(
            endpoint.discover_url().as_str(),
            "http://192.0.2.10/discover.json"
        );
        assert_eq!(
            endpoint.lineup_url().as_str(),
            "http://192.0.2.10/lineup.json"
        );
    }

    #[test]
    fn rewrites_advertised_hostnames_without_dns() {
        let endpoint = DeviceEndpoint::from_discovery(
            "192.0.2.10:65001".parse().unwrap(),
            Some("http://fixture.invalid:8080"),
            Some("http://another.invalid:8080/lineup.json"),
        )
        .unwrap();

        assert_eq!(endpoint.base_url().as_str(), "http://192.0.2.10:8080/");
        assert_eq!(
            endpoint.lineup_url().as_str(),
            "http://192.0.2.10:8080/lineup.json"
        );
    }

    #[test]
    fn production_locators_require_the_device_http_port() {
        assert!(endpoint_from_registry_urls(None, None).is_ok());
        assert!(endpoint_from_registry_urls(Some("http://fixture.invalid:80"), None).is_ok());
        assert!(matches!(
            endpoint_from_registry_urls(Some("http://fixture.invalid:8080"), None),
            Err(EndpointError::UnexpectedPort {
                role: UrlRole::Base,
                expected: 80,
                actual: 8080,
            })
        ));
    }

    #[cfg(all(test, feature = "desktop"))]
    #[test]
    fn loopback_fake_device_metadata_port_is_exempt_exactly_while_installed() {
        let loopback_urls =
            |base: &str| endpoint_from_registry_source_urls("127.0.0.1:65001", Some(base), None);
        assert!(matches!(
            loopback_urls("http://127.0.0.1:49151"),
            Err(EndpointError::UnexpectedPort {
                role: UrlRole::Base,
                expected: 80,
                actual: 49151,
            })
        ));
        let _ports = crate::hdhr::fake_device::hold_fake_device_ports();
        {
            let _exemption = super::MetadataPortOverride::install(49151);
            assert!(loopback_urls("http://127.0.0.1:49151").is_ok());
            assert!(matches!(
                loopback_urls("http://127.0.0.1:49152"),
                Err(EndpointError::UnexpectedPort {
                    role: UrlRole::Base,
                    expected: 80,
                    actual: 49152,
                })
            ));
            assert!(matches!(
                endpoint_from_registry_urls(Some("http://fixture.invalid:49151"), None),
                Err(EndpointError::UnexpectedPort {
                    role: UrlRole::Base,
                    expected: 80,
                    actual: 49151,
                })
            ));
        }
        assert!(matches!(
            loopback_urls("http://127.0.0.1:49151"),
            Err(EndpointError::UnexpectedPort {
                role: UrlRole::Base,
                expected: 80,
                actual: 49151,
            })
        ));
    }

    #[test]
    fn rejects_cross_host_numeric_and_unsafe_urls() {
        let source = "192.0.2.10:65001".parse().unwrap();
        assert!(matches!(
            DeviceEndpoint::from_discovery(source, Some("http://192.0.2.11"), None),
            Err(EndpointError::HostMismatch { .. })
        ));
        for value in [
            "https://192.0.2.10",
            "http://user@192.0.2.10",
            "http://192.0.2.10/path",
            "http://192.0.2.10?query",
            "http://192.0.2.10#fragment",
        ] {
            assert!(DeviceEndpoint::from_discovery(source, Some(value), None).is_err());
        }
    }

    #[test]
    fn rejects_scoped_ipv6_until_the_http_transport_can_preserve_it() {
        assert_eq!(
            DeviceEndpoint::from_discovery("[fe80::1%7]:65001".parse().unwrap(), None, None),
            Err(EndpointError::ScopedIpv6Unsupported)
        );
    }

    #[test]
    fn accepts_matching_global_ipv6_and_rejects_a_numeric_mismatch() {
        let endpoint = DeviceEndpoint::from_discovery(
            "[2001:db8::10]:65001".parse().unwrap(),
            Some("http://[2001:db8::10]:8080"),
            None,
        )
        .unwrap();
        assert_eq!(
            endpoint.discover_url().as_str(),
            "http://[2001:db8::10]:8080/discover.json"
        );

        assert!(matches!(
            DeviceEndpoint::from_discovery(
                "[2001:db8::10]:65001".parse().unwrap(),
                Some("http://[2001:db8::11]"),
                None,
            ),
            Err(EndpointError::HostMismatch { .. })
        ));
    }

    #[test]
    fn parses_sparse_and_modern_device_documents_without_retaining_auth() {
        let sparse = parse_device_info(br#"{"DeviceID":"105A1232"}"#, expected_id()).unwrap();
        assert_eq!(sparse.device_id(), expected_id());
        assert_eq!(sparse.friendly_name(), None);

        let body = br#"{
            "FriendlyName":" HDHomeRun CONNECT 4K ",
            "ModelNumber":"HDHR5-4K",
            "FirmwareName":"fixture-firmware",
            "FirmwareVersion":"20990101",
            "DeviceID":"105A1232",
            "DeviceAuth":"fixture-secret-never-log",
            "TunerCount":4,
            "FutureField":{"ignored":true}
        }"#;
        let info = parse_device_info(body, expected_id()).unwrap();

        assert_eq!(info.friendly_name(), Some("HDHomeRun CONNECT 4K"));
        assert_eq!(info.model_number(), Some("HDHR5-4K"));
        assert_eq!(info.tuner_count(), Some(4));
        assert!(!format!("{info:?}").contains("fixture-secret-never-log"));
    }

    #[test]
    fn rejects_identity_mismatch_duplicate_fields_and_controls() {
        assert!(matches!(
            parse_device_info(br#"{"DeviceID":"105A1243"}"#, expected_id()),
            Err(DeviceHttpError::DeviceIdMismatch { .. })
        ));
        assert!(matches!(
            parse_device_info(
                br#"{"DeviceID":"105A1232","DeviceID":"105A1232"}"#,
                expected_id()
            ),
            Err(DeviceHttpError::Json(_))
        ));
        assert!(matches!(
            parse_device_info(
                b"{\"DeviceID\":\"105A1232\",\"FriendlyName\":\"bad\\nname\"}",
                expected_id()
            ),
            Err(DeviceHttpError::InvalidField {
                field: "FriendlyName",
                ..
            })
        ));
    }

    #[test]
    fn rejects_unbounded_http_configuration() {
        assert!(matches!(
            DeviceHttpConfig::new(
                Duration::ZERO,
                Duration::from_secs(5),
                Duration::from_secs(10),
                MAX_DEVICE_JSON_BYTES,
                MAX_LINEUP_JSON_BYTES,
                MAX_LINEUP_CHANNELS,
            ),
            Err(InvalidHttpConfig::Timeout {
                name: "connect",
                ..
            })
        ));
        assert!(matches!(
            DeviceHttpConfig::new(
                Duration::from_secs(3),
                Duration::from_secs(5),
                Duration::from_secs(10),
                MAX_DEVICE_JSON_BYTES,
                MAX_LINEUP_JSON_BYTES + 1,
                MAX_LINEUP_CHANNELS,
            ),
            Err(InvalidHttpConfig::Limit {
                name: "lineup bytes",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn fetches_metadata_without_referer_and_discards_device_auth() {
        let body = br#"{
          "DeviceID":"105A1232",
          "FriendlyName":"Fixture tuner",
          "DeviceAuth":"transport-secret-never-retain",
          "TunerCount":4
        }"#;
        let server = ScriptedHttpServer::start(vec![ScriptedResponse::immediate(response(
            "200 OK",
            &[
                ("Content-Type", "application/json".to_owned()),
                ("Content-Length", body.len().to_string()),
            ],
            body,
        ))]);
        let endpoint = endpoint_for(&server);

        let info = DeviceHttpClient::default()
            .fetch_device_info(&endpoint, expected_id(), &CancellationToken::new())
            .await
            .unwrap();
        let requests = server.finish();

        assert_eq!(info.friendly_name(), Some("Fixture tuner"));
        assert!(!format!("{info:?}").contains("transport-secret-never-retain"));
        assert_eq!(requests.len(), 1);
        let request = String::from_utf8_lossy(&requests[0]).to_ascii_lowercase();
        assert!(request.starts_with("get /discover.json http/1.1\r\n"));
        assert!(request.contains("accept: application/json\r\n"));
        assert!(request.contains("user-agent: balun/"));
        assert!(!request.contains("referer:"));
    }

    #[tokio::test]
    async fn rejects_redirects_without_contacting_the_location() {
        let server = ScriptedHttpServer::start(vec![ScriptedResponse::immediate(response(
            "302 Found",
            &[("Location", "http://127.0.0.1:9/secret".to_owned())],
            b"",
        ))]);
        let endpoint = endpoint_for(&server);

        let error = DeviceHttpClient::default()
            .fetch_device_info(&endpoint, expected_id(), &CancellationToken::new())
            .await
            .unwrap_err();
        server.finish();

        assert!(matches!(
            error,
            DeviceHttpError::UnexpectedStatus { status: 302, .. }
        ));
    }

    #[tokio::test]
    async fn rejects_declared_and_streamed_oversized_bodies() {
        let declared = ScriptedHttpServer::start(vec![ScriptedResponse::immediate(response(
            "200 OK",
            &[("Content-Length", "33".to_owned())],
            b"",
        ))]);
        let error = DeviceHttpClient::new(config_with_device_limit(32, Duration::from_secs(1)))
            .unwrap()
            .fetch_device_info(
                &endpoint_for(&declared),
                expected_id(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        declared.finish();
        assert!(matches!(
            error,
            DeviceHttpError::BodyTooLarge { maximum: 32, .. }
        ));

        let chunk = "A".repeat(33);
        let chunked_response = format!(
            "HTTP/1.1 200 OK\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n21\r\n{chunk}\r\n0\r\n\r\n"
        );
        let streamed =
            ScriptedHttpServer::start(vec![ScriptedResponse::immediate(chunked_response)]);
        let error = DeviceHttpClient::new(config_with_device_limit(32, Duration::from_secs(1)))
            .unwrap()
            .fetch_device_info(
                &endpoint_for(&streamed),
                expected_id(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        streamed.finish();
        assert!(matches!(
            error,
            DeviceHttpError::BodyTooLarge { maximum: 32, .. }
        ));
    }

    #[tokio::test]
    async fn enforces_wall_clock_deadline_and_preflight_cancellation() {
        let body = br#"{"DeviceID":"105A1232"}"#;
        let server = ScriptedHttpServer::start(vec![ScriptedResponse::delayed(
            response(
                "200 OK",
                &[("Content-Length", body.len().to_string())],
                body,
            ),
            Duration::from_millis(100),
        )]);
        let endpoint = endpoint_for(&server);
        let client =
            DeviceHttpClient::new(config_with_device_limit(64, Duration::from_millis(20))).unwrap();

        let error = client
            .fetch_device_info(&endpoint, expected_id(), &CancellationToken::new())
            .await
            .unwrap_err();
        server.finish();
        assert!(matches!(error, DeviceHttpError::Deadline { .. }));

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let unreachable = DeviceEndpoint::from_discovery(
            "127.0.0.1:65001".parse().unwrap(),
            Some("http://127.0.0.1:9"),
            None,
        )
        .unwrap();
        assert!(matches!(
            client
                .fetch_device_info(&unreachable, expected_id(), &cancellation)
                .await,
            Err(DeviceHttpError::Cancelled)
        ));
    }

    #[test]
    fn parses_sanitized_real_device_info() {
        let connect = parse_device_info(
            include_bytes!("../../tests/fixtures/hdhr/discover-hdhr4-2us.json"),
            expected_id(),
        )
        .unwrap();
        assert_eq!(connect.model_number(), Some("HDHR4-2US"));
        assert_eq!(connect.tuner_count(), Some(2));
        assert_eq!(connect.firmware_version(), Some("20260313"));

        let flex = parse_device_info(
            include_bytes!("../../tests/fixtures/hdhr/discover-hdhr5-4k.json"),
            DeviceId::new(0x105B_1233).unwrap(),
        )
        .unwrap();
        assert_eq!(flex.model_number(), Some("HDHR5-4K"));
        assert_eq!(flex.tuner_count(), Some(4));
        assert_eq!(flex.friendly_name(), Some("HDHomeRun CONNECT 4K"));
    }
}
