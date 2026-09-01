use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use thiserror::Error;

use crate::domain::{ChannelKey, DeviceId};

/// Maximum number of devices retained in one UI projection.
pub const MAX_DEVICE_SUMMARIES: usize = 256;
/// Maximum locator count represented for one device.
pub const MAX_DEVICE_LOCATORS: usize = 64;
/// Maximum number of selected-device channels retained in one UI projection.
pub const MAX_SELECTED_CHANNELS: usize = 4_096;
/// Maximum UTF-8 size of device-provided display metadata.
pub const MAX_DEVICE_TEXT_BYTES: usize = 256;
/// Maximum UTF-8 size of a device-provided channel name.
pub const MAX_CHANNEL_NAME_BYTES: usize = 256;

/// Monotonic revision of a complete application snapshot.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SnapshotRevision(u64);

impl SnapshotRevision {
    pub const INITIAL: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the next revision, or `None` rather than wrapping.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Monotonic generation for one independently supersedable operation lane.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OperationGeneration(u64);

impl OperationGeneration {
    pub const INITIAL: Self = Self(0);

    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Return the next generation, or `None` rather than wrapping.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// UI-safe summary of one stable HDHomeRun identity.
///
/// This projection deliberately contains no advertised, lineup, or stream
/// URL. The preferred locator is display-only; later network work must return
/// to the controller-owned registry and revalidate its locator claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceSummary {
    device_id: DeviceId,
    friendly_name: Option<String>,
    model_number: Option<String>,
    tuner_count: Option<u8>,
    preferred_locator: SocketAddr,
    locator_count: usize,
}

impl DeviceSummary {
    pub fn new(
        device_id: DeviceId,
        friendly_name: Option<String>,
        model_number: Option<String>,
        tuner_count: Option<u8>,
        preferred_locator: SocketAddr,
        locator_count: usize,
    ) -> Result<Self, StateError> {
        let friendly_name = validate_optional_text("friendly name", friendly_name)?;
        let model_number = validate_optional_text("model number", model_number)?;
        if tuner_count.is_some_and(|count| !(1..=32).contains(&count)) {
            return Err(StateError::InvalidTunerCount);
        }
        if !(1..=MAX_DEVICE_LOCATORS).contains(&locator_count) {
            return Err(StateError::InvalidLocatorCount {
                value: locator_count,
                maximum: MAX_DEVICE_LOCATORS,
            });
        }
        if invalid_locator(preferred_locator) {
            return Err(StateError::InvalidPreferredLocator);
        }

        Ok(Self {
            device_id,
            friendly_name,
            model_number,
            tuner_count,
            preferred_locator,
            locator_count,
        })
    }

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
    pub const fn tuner_count(&self) -> Option<u8> {
        self.tuner_count
    }

    #[must_use]
    pub const fn preferred_locator(&self) -> SocketAddr {
        self.preferred_locator
    }

    #[must_use]
    pub const fn locator_count(&self) -> usize {
        self.locator_count
    }
}

/// UI-safe channel metadata. Stream locators remain controller-owned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelSummary {
    key: ChannelKey,
    name: String,
    favorite: bool,
    drm: bool,
    hd: bool,
}

impl ChannelSummary {
    pub fn new(
        key: ChannelKey,
        name: String,
        favorite: bool,
        drm: bool,
        hd: bool,
    ) -> Result<Self, StateError> {
        validate_text("channel name", &name, MAX_CHANNEL_NAME_BYTES)?;
        Ok(Self {
            key,
            name,
            favorite,
            drm,
            hd,
        })
    }

    #[must_use]
    pub const fn key(&self) -> &ChannelKey {
        &self.key
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn is_favorite(&self) -> bool {
        self.favorite
    }

    #[must_use]
    pub const fn is_drm(&self) -> bool {
        self.drm
    }

    #[must_use]
    pub const fn is_hd(&self) -> bool {
        self.hd
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryFailure {
    InterfaceEnumeration,
    Network,
    ExactTargetLimitReached,
    Internal,
}

/// Address-free kind of discovery operation represented by a state update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryKind {
    Local,
    Exact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiscoveryStatus {
    Idle,
    Refreshing,
    Ready,
    NoResponse,
    Failed(DiscoveryFailure),
}

/// Bounded, topology-free status for the discovery lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DiscoveryState {
    generation: OperationGeneration,
    kind: DiscoveryKind,
    status: DiscoveryStatus,
    issue_count: u16,
}

impl DiscoveryState {
    #[must_use]
    pub const fn idle(generation: OperationGeneration) -> Self {
        Self::idle_for(generation, DiscoveryKind::Local)
    }

    #[must_use]
    pub const fn idle_for(generation: OperationGeneration, kind: DiscoveryKind) -> Self {
        Self {
            generation,
            kind,
            status: DiscoveryStatus::Idle,
            issue_count: 0,
        }
    }

    #[must_use]
    pub const fn refreshing(generation: OperationGeneration) -> Self {
        Self::refreshing_for(generation, DiscoveryKind::Local)
    }

    #[must_use]
    pub const fn refreshing_for(generation: OperationGeneration, kind: DiscoveryKind) -> Self {
        Self {
            generation,
            kind,
            status: DiscoveryStatus::Refreshing,
            issue_count: 0,
        }
    }

    #[must_use]
    pub const fn ready(generation: OperationGeneration, issue_count: u16) -> Self {
        Self::ready_for(generation, DiscoveryKind::Local, issue_count)
    }

    #[must_use]
    pub const fn ready_for(
        generation: OperationGeneration,
        kind: DiscoveryKind,
        issue_count: u16,
    ) -> Self {
        Self {
            generation,
            kind,
            status: DiscoveryStatus::Ready,
            issue_count,
        }
    }

    /// Complete an exact-address probe that received no valid device reply.
    #[must_use]
    pub const fn exact_no_response(generation: OperationGeneration, issue_count: u16) -> Self {
        Self {
            generation,
            kind: DiscoveryKind::Exact,
            status: DiscoveryStatus::NoResponse,
            issue_count,
        }
    }

    #[must_use]
    pub const fn failed(generation: OperationGeneration, failure: DiscoveryFailure) -> Self {
        Self::failed_for(generation, DiscoveryKind::Local, failure)
    }

    #[must_use]
    pub const fn failed_for(
        generation: OperationGeneration,
        kind: DiscoveryKind,
        failure: DiscoveryFailure,
    ) -> Self {
        Self {
            generation,
            kind,
            status: DiscoveryStatus::Failed(failure),
            issue_count: 0,
        }
    }

    #[must_use]
    pub const fn generation(self) -> OperationGeneration {
        self.generation
    }

    #[must_use]
    pub const fn kind(self) -> DiscoveryKind {
        self.kind
    }

    #[must_use]
    pub const fn status(self) -> DiscoveryStatus {
        self.status
    }

    #[must_use]
    pub const fn issue_count(self) -> u16 {
        self.issue_count
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineupFailure {
    NoSupportedLocator,
    Unreachable,
    IdentityMismatch,
    InvalidMetadata,
    InvalidLineup,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectedLineupStatus {
    Unselected,
    Loading,
    Ready,
    Failed(LineupFailure),
}

/// State for exactly the selected device's lineup.
///
/// Construction validates that every ready channel is scoped to the same
/// DeviceID and that no ChannelKey occurs twice. Non-ready states cannot carry
/// stale channel rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectedLineupState {
    device_id: Option<DeviceId>,
    generation: OperationGeneration,
    status: SelectedLineupStatus,
    channels: Arc<[ChannelSummary]>,
}

impl SelectedLineupState {
    #[must_use]
    pub fn unselected(generation: OperationGeneration) -> Self {
        Self {
            device_id: None,
            generation,
            status: SelectedLineupStatus::Unselected,
            channels: Arc::from([]),
        }
    }

    #[must_use]
    pub fn loading(device_id: DeviceId, generation: OperationGeneration) -> Self {
        Self {
            device_id: Some(device_id),
            generation,
            status: SelectedLineupStatus::Loading,
            channels: Arc::from([]),
        }
    }

    pub fn ready(
        device_id: DeviceId,
        generation: OperationGeneration,
        channels: impl IntoIterator<Item = ChannelSummary>,
    ) -> Result<Self, StateError> {
        let mut bounded = Vec::new();
        for channel in channels {
            if bounded.len() == MAX_SELECTED_CHANNELS {
                return Err(StateError::TooManyChannels {
                    maximum: MAX_SELECTED_CHANNELS,
                });
            }
            if channel.key.device_id() != device_id {
                return Err(StateError::ChannelDeviceMismatch {
                    selected: device_id,
                    actual: channel.key.device_id(),
                });
            }
            bounded.push(channel);
        }
        bounded.sort_by(|left, right| left.key.cmp(&right.key));
        if let Some(pair) = bounded.windows(2).find(|pair| pair[0].key == pair[1].key) {
            return Err(StateError::DuplicateChannelKey(pair[0].key.clone()));
        }

        Ok(Self {
            device_id: Some(device_id),
            generation,
            status: SelectedLineupStatus::Ready,
            channels: Arc::from(bounded.into_boxed_slice()),
        })
    }

    #[must_use]
    pub fn failed(
        device_id: DeviceId,
        generation: OperationGeneration,
        failure: LineupFailure,
    ) -> Self {
        Self {
            device_id: Some(device_id),
            generation,
            status: SelectedLineupStatus::Failed(failure),
            channels: Arc::from([]),
        }
    }

    #[must_use]
    pub const fn device_id(&self) -> Option<DeviceId> {
        self.device_id
    }

    #[must_use]
    pub const fn generation(&self) -> OperationGeneration {
        self.generation
    }

    #[must_use]
    pub const fn status(&self) -> SelectedLineupStatus {
        self.status
    }

    #[must_use]
    pub fn channels(&self) -> &[ChannelSummary] {
        &self.channels
    }
}

/// One complete, immutable state publication for GTK.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationSnapshot {
    revision: SnapshotRevision,
    discovery_generation: OperationGeneration,
    selection_generation: OperationGeneration,
    discovery: DiscoveryState,
    devices: Arc<[DeviceSummary]>,
    selected_device: Option<DeviceId>,
    selected_lineup: SelectedLineupState,
}

impl ApplicationSnapshot {
    pub fn new(
        revision: SnapshotRevision,
        discovery_generation: OperationGeneration,
        selection_generation: OperationGeneration,
        discovery: DiscoveryState,
        devices: impl IntoIterator<Item = DeviceSummary>,
        selected_device: Option<DeviceId>,
        selected_lineup: SelectedLineupState,
    ) -> Result<Self, StateError> {
        if discovery.generation != discovery_generation {
            return Err(StateError::DiscoveryGenerationMismatch {
                snapshot: discovery_generation,
                state: discovery.generation,
            });
        }
        if selected_lineup.generation != selection_generation {
            return Err(StateError::SelectionGenerationMismatch {
                snapshot: selection_generation,
                state: selected_lineup.generation,
            });
        }

        let mut bounded = Vec::new();
        for device in devices {
            if bounded.len() == MAX_DEVICE_SUMMARIES {
                return Err(StateError::TooManyDevices {
                    maximum: MAX_DEVICE_SUMMARIES,
                });
            }
            bounded.push(device);
        }
        bounded.sort_by_key(DeviceSummary::device_id);
        if let Some(pair) = bounded
            .windows(2)
            .find(|pair| pair[0].device_id == pair[1].device_id)
        {
            return Err(StateError::DuplicateDeviceId(pair[0].device_id));
        }

        match (selected_device, selected_lineup.device_id) {
            (None, None) if selected_lineup.status == SelectedLineupStatus::Unselected => {}
            (Some(selected), Some(lineup)) if selected == lineup => {
                if selected_lineup.status == SelectedLineupStatus::Unselected {
                    return Err(StateError::SelectedLineupStateMismatch);
                }
                if bounded
                    .binary_search_by_key(&selected, DeviceSummary::device_id)
                    .is_err()
                {
                    return Err(StateError::SelectedDeviceMissing(selected));
                }
            }
            _ => return Err(StateError::SelectedLineupStateMismatch),
        }

        Ok(Self {
            revision,
            discovery_generation,
            selection_generation,
            discovery,
            devices: Arc::from(bounded.into_boxed_slice()),
            selected_device,
            selected_lineup,
        })
    }

    #[must_use]
    pub fn initial() -> Self {
        Self {
            revision: SnapshotRevision::INITIAL,
            discovery_generation: OperationGeneration::INITIAL,
            selection_generation: OperationGeneration::INITIAL,
            discovery: DiscoveryState::idle(OperationGeneration::INITIAL),
            devices: Arc::from([]),
            selected_device: None,
            selected_lineup: SelectedLineupState::unselected(OperationGeneration::INITIAL),
        }
    }

    #[must_use]
    pub const fn revision(&self) -> SnapshotRevision {
        self.revision
    }

    #[must_use]
    pub const fn discovery_generation(&self) -> OperationGeneration {
        self.discovery_generation
    }

    #[must_use]
    pub const fn selection_generation(&self) -> OperationGeneration {
        self.selection_generation
    }

    #[must_use]
    pub const fn discovery(&self) -> DiscoveryState {
        self.discovery
    }

    #[must_use]
    pub fn devices(&self) -> &[DeviceSummary] {
        &self.devices
    }

    #[must_use]
    pub const fn selected_device(&self) -> Option<DeviceId> {
        self.selected_device
    }

    #[must_use]
    pub const fn selected_lineup(&self) -> &SelectedLineupState {
        &self.selected_lineup
    }

    /// Whether this publication can safely replace `previous` in a reducer.
    ///
    /// Revisions must advance, and neither independent operation generation
    /// may move backwards even if a malformed producer advances the revision.
    /// Within one generation, operation scope cannot change and only an
    /// in-progress state may advance to a terminal result.
    #[must_use]
    pub fn can_replace(&self, previous: &Self) -> bool {
        if self.revision <= previous.revision
            || self.discovery_generation < previous.discovery_generation
            || self.selection_generation < previous.selection_generation
        {
            return false;
        }

        let discovery_is_safe = self.discovery_generation > previous.discovery_generation
            || self.discovery.kind == previous.discovery.kind
                && match (previous.discovery.status, self.discovery.status) {
                    (
                        DiscoveryStatus::Refreshing,
                        DiscoveryStatus::Ready
                        | DiscoveryStatus::NoResponse
                        | DiscoveryStatus::Failed(_),
                    ) => true,
                    _ => {
                        self.discovery == previous.discovery
                            && self.has_same_discovery_scope(previous)
                    }
                };
        let selection_is_safe = self.selection_generation > previous.selection_generation
            || self.selected_device == previous.selected_device
                && match (previous.selected_lineup.status, self.selected_lineup.status) {
                    (
                        SelectedLineupStatus::Loading,
                        SelectedLineupStatus::Ready | SelectedLineupStatus::Failed(_),
                    ) => true,
                    _ => self.selected_lineup == previous.selected_lineup,
                };

        discovery_is_safe && selection_is_safe
    }

    fn has_same_discovery_scope(&self, previous: &Self) -> bool {
        self.devices.len() == previous.devices.len()
            && self
                .devices
                .iter()
                .zip(previous.devices.iter())
                .all(|(current, prior)| {
                    current.device_id == prior.device_id
                        && current.preferred_locator == prior.preferred_locator
                        && current.locator_count == prior.locator_count
                        && (current.has_same_metadata(prior)
                            || self.selection_completion_may_enrich(current.device_id, previous))
                })
    }

    fn selection_completion_may_enrich(&self, device_id: DeviceId, previous: &Self) -> bool {
        self.selection_generation == previous.selection_generation
            && self.selected_device == Some(device_id)
            && previous.selected_lineup.status == SelectedLineupStatus::Loading
            && matches!(
                self.selected_lineup.status,
                SelectedLineupStatus::Ready | SelectedLineupStatus::Failed(_)
            )
    }
}

impl DeviceSummary {
    fn has_same_metadata(&self, previous: &Self) -> bool {
        self.friendly_name == previous.friendly_name
            && self.model_number == previous.model_number
            && self.tuner_count == previous.tuner_count
    }
}

impl Default for ApplicationSnapshot {
    fn default() -> Self {
        Self::initial()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StateError {
    #[error("{field} is empty")]
    EmptyText { field: &'static str },

    #[error("{field} has surrounding whitespace")]
    SurroundingWhitespace { field: &'static str },

    #[error("{field} contains a control character")]
    ControlCharacter { field: &'static str },

    #[error("{field} is {actual} bytes; maximum is {maximum}")]
    TextTooLong {
        field: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("tuner count must be between 1 and 32")]
    InvalidTunerCount,

    #[error("locator count must be between 1 and {maximum}; got {value}")]
    InvalidLocatorCount { value: usize, maximum: usize },

    #[error("preferred device locator is not a usable unicast socket address")]
    InvalidPreferredLocator,

    #[error("application snapshot exceeds the {maximum}-device limit")]
    TooManyDevices { maximum: usize },

    #[error("application snapshot contains DeviceID {0} more than once")]
    DuplicateDeviceId(DeviceId),

    #[error("selected DeviceID {0} is absent from the device projection")]
    SelectedDeviceMissing(DeviceId),

    #[error("selected device and selected-lineup state do not match")]
    SelectedLineupStateMismatch,

    #[error("application discovery generation {snapshot:?} does not match state {state:?}")]
    DiscoveryGenerationMismatch {
        snapshot: OperationGeneration,
        state: OperationGeneration,
    },

    #[error("application selection generation {snapshot:?} does not match state {state:?}")]
    SelectionGenerationMismatch {
        snapshot: OperationGeneration,
        state: OperationGeneration,
    },

    #[error("selected lineup exceeds the {maximum}-channel limit")]
    TooManyChannels { maximum: usize },

    #[error("selected DeviceID {selected} cannot contain a channel from DeviceID {actual}")]
    ChannelDeviceMismatch {
        selected: DeviceId,
        actual: DeviceId,
    },

    #[error("selected lineup contains duplicate ChannelKey {0:?}")]
    DuplicateChannelKey(ChannelKey),
}

fn validate_optional_text(
    field: &'static str,
    value: Option<String>,
) -> Result<Option<String>, StateError> {
    if let Some(value) = &value {
        validate_text(field, value, MAX_DEVICE_TEXT_BYTES)?;
    }
    Ok(value)
}

fn validate_text(field: &'static str, value: &str, maximum: usize) -> Result<(), StateError> {
    if value.is_empty() {
        return Err(StateError::EmptyText { field });
    }
    if value.trim() != value {
        return Err(StateError::SurroundingWhitespace { field });
    }
    if value.len() > maximum {
        return Err(StateError::TextTooLong {
            field,
            actual: value.len(),
            maximum,
        });
    }
    if value.chars().any(char::is_control) {
        return Err(StateError::ControlCharacter { field });
    }
    Ok(())
}

fn invalid_locator(locator: SocketAddr) -> bool {
    locator.port() == 0
        || locator.ip().is_unspecified()
        || locator.ip().is_multicast()
        || matches!(locator.ip(), IpAddr::V4(address) if address == Ipv4Addr::BROADCAST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::GuideNumber;

    fn first_id() -> DeviceId {
        DeviceId::new(0x105A_1232).unwrap()
    }

    fn second_id() -> DeviceId {
        DeviceId::new(0x105A_1243).unwrap()
    }

    fn device(device_id: DeviceId, address: &str) -> DeviceSummary {
        DeviceSummary::new(
            device_id,
            Some("HDHomeRun".to_owned()),
            Some("Synthetic".to_owned()),
            Some(4),
            address.parse().unwrap(),
            1,
        )
        .unwrap()
    }

    fn channel(device_id: DeviceId, number: &str) -> ChannelSummary {
        ChannelSummary::new(
            ChannelKey::new(device_id, GuideNumber::new(number).unwrap()),
            format!("Channel {number}"),
            false,
            false,
            true,
        )
        .unwrap()
    }

    #[test]
    fn initial_snapshot_is_empty_and_generation_aligned() {
        let snapshot = ApplicationSnapshot::initial();

        assert_eq!(snapshot.revision(), SnapshotRevision::INITIAL);
        assert!(snapshot.devices().is_empty());
        assert_eq!(snapshot.selected_device(), None);
        assert_eq!(
            snapshot.selected_lineup().status(),
            SelectedLineupStatus::Unselected
        );
        assert_eq!(
            snapshot.discovery_generation(),
            snapshot.discovery().generation()
        );
        assert_eq!(
            snapshot.selection_generation(),
            snapshot.selected_lineup().generation()
        );
    }

    #[test]
    fn reducer_keeps_discovery_kind_fixed_within_one_generation() {
        let generation = OperationGeneration::new(1);
        let selection_generation = OperationGeneration::INITIAL;
        let local_refreshing = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            generation,
            selection_generation,
            DiscoveryState::refreshing_for(generation, DiscoveryKind::Local),
            [],
            None,
            SelectedLineupState::unselected(selection_generation),
        )
        .unwrap();
        let exact_terminal = ApplicationSnapshot::new(
            SnapshotRevision::new(2),
            generation,
            selection_generation,
            DiscoveryState::ready_for(generation, DiscoveryKind::Exact, 0),
            [],
            None,
            SelectedLineupState::unselected(selection_generation),
        )
        .unwrap();
        let exact_refreshing = ApplicationSnapshot::new(
            SnapshotRevision::new(3),
            generation,
            selection_generation,
            DiscoveryState::refreshing_for(generation, DiscoveryKind::Exact),
            [],
            None,
            SelectedLineupState::unselected(selection_generation),
        )
        .unwrap();
        let exact_no_response = ApplicationSnapshot::new(
            SnapshotRevision::new(4),
            generation,
            selection_generation,
            DiscoveryState::exact_no_response(generation, 0),
            [],
            None,
            SelectedLineupState::unselected(selection_generation),
        )
        .unwrap();

        assert!(!exact_terminal.can_replace(&local_refreshing));
        assert!(!exact_refreshing.can_replace(&local_refreshing));
        assert!(exact_no_response.can_replace(&exact_refreshing));
        assert_eq!(exact_no_response.discovery().kind(), DiscoveryKind::Exact);
        assert_eq!(
            exact_no_response.discovery().status(),
            DiscoveryStatus::NoResponse
        );
    }

    #[test]
    fn ready_lineup_is_naturally_sorted_and_scoped_to_one_device() {
        let id = first_id();
        let lineup = SelectedLineupState::ready(
            id,
            OperationGeneration::new(3),
            [channel(id, "10.1"), channel(id, "2.2")],
        )
        .unwrap();

        assert_eq!(lineup.device_id(), Some(id));
        assert_eq!(lineup.status(), SelectedLineupStatus::Ready);
        assert_eq!(
            lineup
                .channels()
                .iter()
                .map(|row| row.key().guide_number().as_str())
                .collect::<Vec<_>>(),
            ["2.2", "10.1"]
        );
        assert!(
            lineup
                .channels()
                .iter()
                .all(|row| row.key().device_id() == id)
        );
    }

    #[test]
    fn equal_channel_numbers_on_different_devices_cannot_merge() {
        let first = first_id();
        let second = second_id();
        let error = SelectedLineupState::ready(
            first,
            OperationGeneration::new(1),
            [channel(first, "7.1"), channel(second, "7.1")],
        )
        .unwrap_err();

        assert_eq!(
            error,
            StateError::ChannelDeviceMismatch {
                selected: first,
                actual: second,
            }
        );
    }

    #[test]
    fn duplicate_channel_keys_are_rejected() {
        let id = first_id();
        let duplicate = channel(id, "7.1");

        assert!(matches!(
            SelectedLineupState::ready(
                id,
                OperationGeneration::new(1),
                [duplicate.clone(), duplicate]
            ),
            Err(StateError::DuplicateChannelKey(_))
        ));
    }

    #[test]
    fn application_snapshot_requires_exact_selection_scope_and_generation() {
        let first = first_id();
        let second = second_id();
        let discovery_generation = OperationGeneration::new(4);
        let selection_generation = OperationGeneration::new(7);
        let devices = [
            device(second, "192.0.2.20:65001"),
            device(first, "192.0.2.10:65001"),
        ];
        let lineup =
            SelectedLineupState::ready(first, selection_generation, [channel(first, "5.1")])
                .unwrap();

        let snapshot = ApplicationSnapshot::new(
            SnapshotRevision::new(9),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            devices.clone(),
            Some(first),
            lineup.clone(),
        )
        .unwrap();
        assert_eq!(snapshot.devices()[0].device_id(), first);
        assert_eq!(snapshot.devices()[1].device_id(), second);

        assert_eq!(
            ApplicationSnapshot::new(
                SnapshotRevision::new(10),
                discovery_generation,
                selection_generation,
                DiscoveryState::ready(discovery_generation, 0),
                devices,
                Some(second),
                lineup,
            ),
            Err(StateError::SelectedLineupStateMismatch)
        );
    }

    #[test]
    fn selected_device_must_remain_in_the_device_projection() {
        let id = first_id();
        let generation = OperationGeneration::new(1);

        assert_eq!(
            ApplicationSnapshot::new(
                SnapshotRevision::new(1),
                generation,
                generation,
                DiscoveryState::ready(generation, 0),
                [],
                Some(id),
                SelectedLineupState::loading(id, generation),
            ),
            Err(StateError::SelectedDeviceMissing(id))
        );
    }

    #[test]
    fn mismatched_operation_generations_are_rejected() {
        let zero = OperationGeneration::INITIAL;
        let one = OperationGeneration::new(1);

        assert!(matches!(
            ApplicationSnapshot::new(
                SnapshotRevision::new(1),
                one,
                zero,
                DiscoveryState::idle(zero),
                [],
                None,
                SelectedLineupState::unselected(zero),
            ),
            Err(StateError::DiscoveryGenerationMismatch { .. })
        ));
        assert!(matches!(
            ApplicationSnapshot::new(
                SnapshotRevision::new(1),
                zero,
                one,
                DiscoveryState::idle(zero),
                [],
                None,
                SelectedLineupState::unselected(zero),
            ),
            Err(StateError::SelectionGenerationMismatch { .. })
        ));
    }

    #[test]
    fn reducer_rejects_stale_revision_or_generation_regression() {
        let generation = OperationGeneration::new(3);
        let current = ApplicationSnapshot::new(
            SnapshotRevision::new(5),
            generation,
            generation,
            DiscoveryState::idle(generation),
            [],
            None,
            SelectedLineupState::unselected(generation),
        )
        .unwrap();
        let newer = ApplicationSnapshot::new(
            SnapshotRevision::new(6),
            generation,
            OperationGeneration::new(4),
            DiscoveryState::idle(generation),
            [],
            None,
            SelectedLineupState::unselected(OperationGeneration::new(4)),
        )
        .unwrap();
        let regressed = ApplicationSnapshot::new(
            SnapshotRevision::new(7),
            OperationGeneration::new(2),
            generation,
            DiscoveryState::idle(OperationGeneration::new(2)),
            [],
            None,
            SelectedLineupState::unselected(generation),
        )
        .unwrap();

        assert!(newer.can_replace(&current));
        assert!(!current.can_replace(&current));
        assert!(!regressed.can_replace(&current));
    }

    #[test]
    fn reducer_requires_generation_scoped_selection_and_forward_phase_progress() {
        let first = first_id();
        let second = second_id();
        let discovery_generation = OperationGeneration::new(2);
        let selection_generation = OperationGeneration::new(5);
        let devices = [
            device(first, "192.0.2.10:65001"),
            device(second, "192.0.2.20:65001"),
        ];
        let loading_first = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            devices.clone(),
            Some(first),
            SelectedLineupState::loading(first, selection_generation),
        )
        .unwrap();
        let late_other_scope = ApplicationSnapshot::new(
            SnapshotRevision::new(2),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            devices.clone(),
            Some(second),
            SelectedLineupState::loading(second, selection_generation),
        )
        .unwrap();
        let ready_first = ApplicationSnapshot::new(
            SnapshotRevision::new(3),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            devices.clone(),
            Some(first),
            SelectedLineupState::ready(first, selection_generation, [channel(first, "7.1")])
                .unwrap(),
        )
        .unwrap();
        let regressed_to_loading = ApplicationSnapshot::new(
            SnapshotRevision::new(4),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            devices,
            Some(first),
            SelectedLineupState::loading(first, selection_generation),
        )
        .unwrap();

        assert!(!late_other_scope.can_replace(&loading_first));
        assert!(ready_first.can_replace(&loading_first));
        assert!(!regressed_to_loading.can_replace(&ready_first));
    }

    #[test]
    fn reducer_rejects_device_mutation_after_discovery_is_terminal() {
        let first = first_id();
        let generation = OperationGeneration::new(3);
        let current = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            generation,
            OperationGeneration::INITIAL,
            DiscoveryState::ready(generation, 0),
            [device(first, "192.0.2.10:65001")],
            None,
            SelectedLineupState::unselected(OperationGeneration::INITIAL),
        )
        .unwrap();
        let mutated = ApplicationSnapshot::new(
            SnapshotRevision::new(2),
            generation,
            OperationGeneration::INITIAL,
            DiscoveryState::ready(generation, 0),
            [device(first, "192.0.2.11:65001")],
            None,
            SelectedLineupState::unselected(OperationGeneration::INITIAL),
        )
        .unwrap();

        assert!(!mutated.can_replace(&current));
    }

    #[test]
    fn selection_completion_may_enrich_metadata_without_changing_discovery_scope() {
        let id = first_id();
        let discovery_generation = OperationGeneration::new(3);
        let selection_generation = OperationGeneration::new(4);
        let address = "192.0.2.10:65001".parse().unwrap();
        let loading = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            [DeviceSummary::new(id, None, None, None, address, 1).unwrap()],
            Some(id),
            SelectedLineupState::loading(id, selection_generation),
        )
        .unwrap();
        let ready = ApplicationSnapshot::new(
            SnapshotRevision::new(2),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            [DeviceSummary::new(
                id,
                Some("Enriched tuner".to_owned()),
                Some("Synthetic model".to_owned()),
                Some(4),
                address,
                1,
            )
            .unwrap()],
            Some(id),
            SelectedLineupState::ready(id, selection_generation, [channel(id, "7.1")]).unwrap(),
        )
        .unwrap();

        assert!(ready.can_replace(&loading));
    }

    #[test]
    fn terminal_metadata_cannot_mutate_without_an_owning_operation_transition() {
        let id = first_id();
        let discovery_generation = OperationGeneration::new(3);
        let selection_generation = OperationGeneration::new(4);
        let address = "192.0.2.10:65001".parse().unwrap();
        let lineup =
            SelectedLineupState::ready(id, selection_generation, [channel(id, "7.1")]).unwrap();
        let current = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            [DeviceSummary::new(
                id,
                Some("Known tuner".to_owned()),
                Some("Known model".to_owned()),
                Some(4),
                address,
                1,
            )
            .unwrap()],
            Some(id),
            lineup.clone(),
        )
        .unwrap();
        let regressed = ApplicationSnapshot::new(
            SnapshotRevision::new(2),
            discovery_generation,
            selection_generation,
            DiscoveryState::ready(discovery_generation, 0),
            [DeviceSummary::new(id, None, None, None, address, 1).unwrap()],
            Some(id),
            lineup,
        )
        .unwrap();

        assert!(!regressed.can_replace(&current));
    }

    #[test]
    fn projection_bounds_are_enforced_before_storage() {
        let id = first_id();
        let device = device(id, "192.0.2.10:65001");
        let too_many_devices = vec![device; MAX_DEVICE_SUMMARIES + 1];
        assert_eq!(
            ApplicationSnapshot::new(
                SnapshotRevision::new(1),
                OperationGeneration::INITIAL,
                OperationGeneration::INITIAL,
                DiscoveryState::idle(OperationGeneration::INITIAL),
                too_many_devices,
                None,
                SelectedLineupState::unselected(OperationGeneration::INITIAL),
            ),
            Err(StateError::TooManyDevices {
                maximum: MAX_DEVICE_SUMMARIES
            })
        );

        let channel = channel(id, "7.1");
        let too_many_channels = vec![channel; MAX_SELECTED_CHANNELS + 1];
        assert_eq!(
            SelectedLineupState::ready(id, OperationGeneration::new(1), too_many_channels),
            Err(StateError::TooManyChannels {
                maximum: MAX_SELECTED_CHANNELS
            })
        );
    }

    #[test]
    fn display_text_and_locator_inputs_are_revalidated() {
        let id = first_id();
        assert!(matches!(
            DeviceSummary::new(
                id,
                Some(" bad ".to_owned()),
                None,
                Some(4),
                "192.0.2.10:65001".parse().unwrap(),
                1,
            ),
            Err(StateError::SurroundingWhitespace { .. })
        ));
        assert_eq!(
            DeviceSummary::new(id, None, None, Some(4), "0.0.0.0:65001".parse().unwrap(), 1,),
            Err(StateError::InvalidPreferredLocator)
        );
        assert!(matches!(
            ChannelSummary::new(
                ChannelKey::new(id, GuideNumber::new("7.1").unwrap()),
                "bad\nname".to_owned(),
                false,
                false,
                false,
            ),
            Err(StateError::ControlCharacter { .. })
        ));
    }
}
