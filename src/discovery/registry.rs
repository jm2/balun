use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

use super::{DiscoveryMethod, DiscoveryObservation};
use crate::domain::DeviceId;

/// A timestamp measured from a caller-owned monotonic epoch.
///
/// Keeping the epoch outside the registry makes expiry deterministic in tests
/// and prevents wall-clock changes from reviving or prematurely removing a
/// locator.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistryInstant(Duration);

impl RegistryInstant {
    #[must_use]
    pub const fn from_duration(since_epoch: Duration) -> Self {
        Self(since_epoch)
    }

    #[must_use]
    pub const fn duration_since_epoch(self) -> Duration {
        self.0
    }
}

impl From<Duration> for RegistryInstant {
    fn from(value: Duration) -> Self {
        Self::from_duration(value)
    }
}

/// One way a locator was observed.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LocatorOrigin {
    pub method: DiscoveryMethod,
    pub interface: Option<String>,
}

/// A network locator claim belonging to one validated DeviceID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocatorClaim {
    source: SocketAddr,
    origins: BTreeMap<LocatorOrigin, OriginFreshness>,
    first_seen: RegistryInstant,
    last_seen: RegistryInstant,
    device_types: Vec<u32>,
    tuner_count: Option<u8>,
    advertised_base_url: Option<String>,
    advertised_lineup_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OriginFreshness {
    first_seen: RegistryInstant,
    last_seen: RegistryInstant,
}

impl LocatorClaim {
    #[must_use]
    pub const fn source(&self) -> SocketAddr {
        self.source
    }

    pub fn origins(&self) -> impl ExactSizeIterator<Item = &LocatorOrigin> {
        self.origins.keys()
    }

    /// Return when this origin first observed the locator during its current
    /// ownership period.
    #[must_use]
    pub fn origin_first_seen(&self, origin: &LocatorOrigin) -> Option<RegistryInstant> {
        self.origins
            .get(origin)
            .map(|freshness| freshness.first_seen)
    }

    /// Return the most recent observation made through this origin.
    #[must_use]
    pub fn origin_last_seen(&self, origin: &LocatorOrigin) -> Option<RegistryInstant> {
        self.origins
            .get(origin)
            .map(|freshness| freshness.last_seen)
    }

    #[must_use]
    pub const fn first_seen(&self) -> RegistryInstant {
        self.first_seen
    }

    #[must_use]
    pub const fn last_seen(&self) -> RegistryInstant {
        self.last_seen
    }

    pub fn device_types(&self) -> &[u32] {
        &self.device_types
    }

    #[must_use]
    pub const fn tuner_count(&self) -> Option<u8> {
        self.tuner_count
    }

    /// Return untrusted advertised metadata. HTTP callers must validate and
    /// normalize it against [`Self::source`].
    pub fn advertised_base_url(&self) -> Option<&str> {
        self.advertised_base_url.as_deref()
    }

    /// Return untrusted advertised metadata. HTTP callers must validate and
    /// normalize it against [`Self::source`].
    pub fn advertised_lineup_url(&self) -> Option<&str> {
        self.advertised_lineup_url.as_deref()
    }

    fn new(observation: DiscoveryObservation, seen_at: RegistryInstant) -> Self {
        let origin = LocatorOrigin {
            method: observation.method,
            interface: observation.interface,
        };
        let mut device_types = observation.device_types;
        device_types.sort_unstable();
        device_types.dedup();

        Self {
            source: observation.source,
            origins: BTreeMap::from([(
                origin,
                OriginFreshness {
                    first_seen: seen_at,
                    last_seen: seen_at,
                },
            )]),
            first_seen: seen_at,
            last_seen: seen_at,
            device_types,
            tuner_count: observation.tuner_count,
            advertised_base_url: observation.advertised_base_url,
            advertised_lineup_url: observation.advertised_lineup_url,
        }
    }

    fn refresh(&mut self, observation: DiscoveryObservation, seen_at: RegistryInstant) {
        let origin = LocatorOrigin {
            method: observation.method,
            interface: observation.interface,
        };
        self.origins
            .entry(origin)
            .and_modify(|freshness| freshness.last_seen = seen_at)
            .or_insert(OriginFreshness {
                first_seen: seen_at,
                last_seen: seen_at,
            });
        self.last_seen = seen_at;

        let mut device_types = observation.device_types;
        device_types.sort_unstable();
        device_types.dedup();
        if !device_types.is_empty() {
            self.device_types = device_types;
        }
        if observation.tuner_count.is_some() {
            self.tuner_count = observation.tuner_count;
        }
        if observation.advertised_base_url.is_some() {
            self.advertised_base_url = observation.advertised_base_url;
        }
        if observation.advertised_lineup_url.is_some() {
            self.advertised_lineup_url = observation.advertised_lineup_url;
        }
    }

    fn origin_preference(&self) -> u8 {
        self.origins
            .iter()
            .map(|(origin, freshness)| {
                let method_preference = match origin.method {
                    DiscoveryMethod::Targeted => 4,
                    DiscoveryMethod::RoutedTargeted => 4,
                    DiscoveryMethod::Ipv4Broadcast => 3,
                    DiscoveryMethod::Ipv6SiteLocalMulticast => 2,
                    DiscoveryMethod::Ipv6LinkLocalMulticast => 1,
                };
                (freshness.last_seen, method_preference)
            })
            .max()
            .map_or(0, |(_, preference)| preference)
    }
}

/// One stable device with one or more independently expiring locators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredDevice {
    device_id: DeviceId,
    locators: BTreeMap<SocketAddr, LocatorClaim>,
}

impl RegisteredDevice {
    #[must_use]
    pub const fn device_id(&self) -> DeviceId {
        self.device_id
    }

    pub fn locators(&self) -> impl ExactSizeIterator<Item = &LocatorClaim> {
        self.locators.values()
    }

    #[must_use]
    pub fn preferred_locator(&self) -> Option<&LocatorClaim> {
        self.locators.values().max_by(|left, right| {
            locator_preference(left, right).then_with(|| right.source.cmp(&left.source))
        })
    }
}

fn locator_preference(left: &LocatorClaim, right: &LocatorClaim) -> Ordering {
    left.last_seen
        .cmp(&right.last_seen)
        .then_with(|| left.origin_preference().cmp(&right.origin_preference()))
        .then_with(|| left.source.is_ipv4().cmp(&right.source.is_ipv4()))
}

/// Result of applying one observation to the registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationOutcome {
    pub device_added: bool,
    pub locator_added: bool,
    pub reassigned_from: Option<DeviceId>,
    pub reassigned_device_removed: bool,
}

/// Result of one deterministic stale-locator sweep.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExpirationOutcome {
    pub removed_origins: usize,
    pub removed_locators: usize,
    pub removed_devices: Vec<DeviceId>,
}

/// Stable DeviceID registry with exclusive ownership of every source locator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceRegistry {
    devices: BTreeMap<DeviceId, RegisteredDevice>,
    locator_owners: BTreeMap<SocketAddr, DeviceId>,
    clock: Option<RegistryInstant>,
    max_devices: usize,
    max_locators_per_device: usize,
    max_origins_per_locator: usize,
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        Self {
            devices: BTreeMap::new(),
            locator_owners: BTreeMap::new(),
            clock: None,
            max_devices: Self::DEFAULT_MAX_DEVICES,
            max_locators_per_device: Self::DEFAULT_MAX_LOCATORS_PER_DEVICE,
            max_origins_per_locator: Self::DEFAULT_MAX_ORIGINS_PER_LOCATOR,
        }
    }
}

impl DeviceRegistry {
    /// Conservative default ceiling for distinct DeviceIDs retained at once.
    pub const DEFAULT_MAX_DEVICES: usize = 256;
    /// Conservative default ceiling for addresses retained for one DeviceID.
    pub const DEFAULT_MAX_LOCATORS_PER_DEVICE: usize = 16;
    /// Conservative default ceiling for discovery paths retained per address.
    pub const DEFAULT_MAX_ORIGINS_PER_LOCATOR: usize = 16;
    /// Largest caller-configured device bound accepted by the registry.
    pub const ABSOLUTE_MAX_DEVICES: usize = 1_024;
    /// Largest caller-configured per-device locator bound.
    pub const ABSOLUTE_MAX_LOCATORS_PER_DEVICE: usize = 64;
    /// Largest caller-configured per-locator origin bound.
    pub const ABSOLUTE_MAX_ORIGINS_PER_LOCATOR: usize = 64;

    /// Build an empty registry with caller-selected hard capacity limits.
    pub fn with_limits(
        max_devices: usize,
        max_locators_per_device: usize,
        max_origins_per_locator: usize,
    ) -> Result<Self, RegistryError> {
        if !(1..=Self::ABSOLUTE_MAX_DEVICES).contains(&max_devices) {
            return Err(RegistryError::InvalidDeviceLimit {
                value: max_devices,
                maximum: Self::ABSOLUTE_MAX_DEVICES,
            });
        }
        if !(1..=Self::ABSOLUTE_MAX_LOCATORS_PER_DEVICE).contains(&max_locators_per_device) {
            return Err(RegistryError::InvalidLocatorLimit {
                value: max_locators_per_device,
                maximum: Self::ABSOLUTE_MAX_LOCATORS_PER_DEVICE,
            });
        }
        if !(1..=Self::ABSOLUTE_MAX_ORIGINS_PER_LOCATOR).contains(&max_origins_per_locator) {
            return Err(RegistryError::InvalidOriginLimit {
                value: max_origins_per_locator,
                maximum: Self::ABSOLUTE_MAX_ORIGINS_PER_LOCATOR,
            });
        }

        Ok(Self {
            devices: BTreeMap::new(),
            locator_owners: BTreeMap::new(),
            clock: None,
            max_devices,
            max_locators_per_device,
            max_origins_per_locator,
        })
    }

    #[must_use]
    pub const fn max_devices(&self) -> usize {
        self.max_devices
    }

    #[must_use]
    pub const fn max_locators_per_device(&self) -> usize {
        self.max_locators_per_device
    }

    #[must_use]
    pub const fn max_origins_per_locator(&self) -> usize {
        self.max_origins_per_locator
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn devices(&self) -> impl ExactSizeIterator<Item = &RegisteredDevice> {
        self.devices.values()
    }

    #[must_use]
    pub fn get(&self, device_id: DeviceId) -> Option<&RegisteredDevice> {
        self.devices.get(&device_id)
    }

    #[must_use]
    pub const fn clock(&self) -> Option<RegistryInstant> {
        self.clock
    }

    /// Apply one validated discovery observation.
    ///
    /// A locator that is still present belongs exclusively to its current
    /// DeviceID. A contradictory observation is rejected without mutation;
    /// callers may expire the old locator and retry, or use
    /// [`Self::confirm_reassignment`] after an independent confirmation.
    pub fn observe(
        &mut self,
        observation: DiscoveryObservation,
        seen_at: RegistryInstant,
    ) -> Result<ObservationOutcome, RegistryError> {
        self.observe_with_reassignment(observation, seen_at, false)
    }

    /// Apply an observation whose address reassignment has been independently
    /// confirmed by a trusted higher-level policy or explicit user action.
    ///
    /// This is the only operation that can move a still-present locator
    /// between DeviceIDs. All capacity and clock checks occur before mutation.
    pub fn confirm_reassignment(
        &mut self,
        observation: DiscoveryObservation,
        seen_at: RegistryInstant,
    ) -> Result<ObservationOutcome, RegistryError> {
        self.observe_with_reassignment(observation, seen_at, true)
    }

    fn observe_with_reassignment(
        &mut self,
        observation: DiscoveryObservation,
        seen_at: RegistryInstant,
        reassignment_confirmed: bool,
    ) -> Result<ObservationOutcome, RegistryError> {
        self.check_clock(seen_at)?;

        let device_id = observation.device_id;
        let source = observation.source;
        let reassigned_from = self
            .locator_owners
            .get(&source)
            .copied()
            .filter(|owner| *owner != device_id);

        if let Some(current_owner) = reassigned_from
            && !reassignment_confirmed
        {
            return Err(RegistryError::LocatorConflict {
                locator: source,
                current_owner,
                claimant: device_id,
            });
        }

        let device_added = !self.devices.contains_key(&device_id);
        let reassigned_device_removed = reassigned_from
            .and_then(|previous_owner| self.devices.get(&previous_owner))
            .is_some_and(|device| device.locators.len() == 1);
        let projected_devices =
            self.devices.len() + usize::from(device_added) - usize::from(reassigned_device_removed);
        if projected_devices > self.max_devices {
            return Err(RegistryError::DeviceLimitReached {
                maximum: self.max_devices,
            });
        }

        let locator_added = self
            .devices
            .get(&device_id)
            .is_none_or(|device| !device.locators.contains_key(&source));
        if locator_added
            && self
                .devices
                .get(&device_id)
                .is_some_and(|device| device.locators.len() >= self.max_locators_per_device)
        {
            return Err(RegistryError::LocatorLimitReached {
                device_id,
                maximum: self.max_locators_per_device,
            });
        }

        let origin = LocatorOrigin {
            method: observation.method,
            interface: observation.interface.clone(),
        };
        if !locator_added
            && self
                .devices
                .get(&device_id)
                .and_then(|device| device.locators.get(&source))
                .is_some_and(|locator| {
                    !locator.origins.contains_key(&origin)
                        && locator.origins.len() >= self.max_origins_per_locator
                })
        {
            return Err(RegistryError::OriginLimitReached {
                device_id,
                locator: source,
                maximum: self.max_origins_per_locator,
            });
        }

        self.clock = Some(seen_at);

        if let Some(previous_owner) = reassigned_from {
            let remove_previous_device =
                if let Some(previous_device) = self.devices.get_mut(&previous_owner) {
                    previous_device.locators.remove(&source);
                    previous_device.locators.is_empty()
                } else {
                    false
                };
            if remove_previous_device {
                self.devices.remove(&previous_owner);
            }
        }

        let device = self
            .devices
            .entry(device_id)
            .or_insert_with(|| RegisteredDevice {
                device_id,
                locators: BTreeMap::new(),
            });
        match device.locators.entry(source) {
            Entry::Vacant(entry) => {
                entry.insert(LocatorClaim::new(observation, seen_at));
            }
            Entry::Occupied(mut entry) => entry.get_mut().refresh(observation, seen_at),
        }
        self.locator_owners.insert(source, device_id);

        Ok(ObservationOutcome {
            device_added,
            locator_added,
            reassigned_from,
            reassigned_device_removed,
        })
    }

    /// Remove locators older than `max_age` at the supplied monotonic `now`.
    pub fn expire_stale(
        &mut self,
        now: RegistryInstant,
        max_age: Duration,
    ) -> Result<ExpirationOutcome, RegistryError> {
        self.check_clock(now)?;
        self.clock = Some(now);
        let cutoff = RegistryInstant(now.0.saturating_sub(max_age));
        let mut removed_origins = 0;
        let mut stale = Vec::new();
        for (device_id, device) in &mut self.devices {
            for (source, locator) in &mut device.locators {
                let before = locator.origins.len();
                locator
                    .origins
                    .retain(|_, freshness| freshness.last_seen >= cutoff);
                removed_origins += before - locator.origins.len();

                if locator.origins.is_empty() {
                    stale.push((*device_id, *source));
                } else if let Some(last_seen) = locator
                    .origins
                    .values()
                    .map(|freshness| freshness.last_seen)
                    .max()
                {
                    locator.last_seen = last_seen;
                }
            }
        }

        let mut removed_devices = BTreeSet::new();
        for (device_id, source) in &stale {
            self.locator_owners.remove(source);
            if let Some(device) = self.devices.get_mut(device_id) {
                device.locators.remove(source);
                if device.locators.is_empty() {
                    removed_devices.insert(*device_id);
                }
            }
        }
        for device_id in &removed_devices {
            self.devices.remove(device_id);
        }

        Ok(ExpirationOutcome {
            removed_origins,
            removed_locators: stale.len(),
            removed_devices: removed_devices.into_iter().collect(),
        })
    }

    fn check_clock(&self, attempted: RegistryInstant) -> Result<(), RegistryError> {
        if let Some(previous) = self.clock
            && attempted < previous
        {
            return Err(RegistryError::TimeWentBackwards {
                previous,
                attempted,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RegistryError {
    #[error("maximum registry device count must be between 1 and {maximum}; got {value}")]
    InvalidDeviceLimit { value: usize, maximum: usize },

    #[error("maximum locators per device must be between 1 and {maximum}; got {value}")]
    InvalidLocatorLimit { value: usize, maximum: usize },

    #[error("maximum origins per locator must be between 1 and {maximum}; got {value}")]
    InvalidOriginLimit { value: usize, maximum: usize },

    #[error("device registry reached its {maximum}-device limit")]
    DeviceLimitReached { maximum: usize },

    #[error("device {device_id} reached its {maximum}-locator limit")]
    LocatorLimitReached { device_id: DeviceId, maximum: usize },

    #[error("locator {locator} for device {device_id} reached its {maximum}-origin limit")]
    OriginLimitReached {
        device_id: DeviceId,
        locator: SocketAddr,
        maximum: usize,
    },

    #[error(
        "locator {locator} is still owned by device {current_owner}; rejected conflicting claim from {claimant}"
    )]
    LocatorConflict {
        locator: SocketAddr,
        current_owner: DeviceId,
        claimant: DeviceId,
    },

    #[error("device registry time moved backwards from {previous:?} to {attempted:?}")]
    TimeWentBackwards {
        previous: RegistryInstant,
        attempted: RegistryInstant,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_ID: u32 = 0x105A_1232;
    const SECOND_ID: u32 = 0x105A_1243;

    fn at(milliseconds: u64) -> RegistryInstant {
        RegistryInstant::from_duration(Duration::from_millis(milliseconds))
    }

    fn observation(device_id: u32, source: &str, method: DiscoveryMethod) -> DiscoveryObservation {
        DiscoveryObservation {
            device_id: DeviceId::new(device_id).unwrap(),
            source: source.parse().unwrap(),
            method,
            interface: Some("test0".to_owned()),
            device_types: vec![5, 1, 1],
            tuner_count: Some(4),
            advertised_base_url: Some(format!("http://{source}")),
            advertised_lineup_url: Some(format!("http://{source}/lineup.json")),
        }
    }

    #[test]
    fn aggregates_locators_without_merging_device_identity() {
        let id = DeviceId::new(FIRST_ID).unwrap();
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.10:65001", DiscoveryMethod::Ipv4Broadcast),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();

        assert_eq!(registry.len(), 1);
        let device = registry.get(id).unwrap();
        assert_eq!(device.locators().len(), 2);
        assert_eq!(
            device.preferred_locator().unwrap().source(),
            "192.0.2.11:65001".parse().unwrap()
        );
    }

    #[test]
    fn records_duplicate_origins_and_preserves_known_optional_metadata() {
        let id = DeviceId::new(FIRST_ID).unwrap();
        let source = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, source, DiscoveryMethod::Ipv4Broadcast),
                at(10),
            )
            .unwrap();

        let mut refresh = observation(FIRST_ID, source, DiscoveryMethod::Targeted);
        refresh.interface = None;
        refresh.device_types.clear();
        refresh.tuner_count = None;
        refresh.advertised_base_url = None;
        refresh.advertised_lineup_url = None;
        let outcome = registry.observe(refresh, at(20)).unwrap();

        assert!(!outcome.device_added);
        assert!(!outcome.locator_added);
        let locator = registry.get(id).unwrap().locators().next().unwrap();
        assert_eq!(locator.origins().len(), 2);
        assert_eq!(locator.device_types(), &[1, 5]);
        assert_eq!(locator.tuner_count(), Some(4));
        assert!(locator.advertised_base_url().is_some());
        assert_eq!(locator.first_seen(), at(10));
        assert_eq!(locator.last_seen(), at(20));
        let broadcast = LocatorOrigin {
            method: DiscoveryMethod::Ipv4Broadcast,
            interface: Some("test0".to_owned()),
        };
        let targeted = LocatorOrigin {
            method: DiscoveryMethod::Targeted,
            interface: None,
        };
        assert_eq!(locator.origin_first_seen(&broadcast), Some(at(10)));
        assert_eq!(locator.origin_last_seen(&broadcast), Some(at(10)));
        assert_eq!(locator.origin_first_seen(&targeted), Some(at(20)));
        assert_eq!(locator.origin_last_seen(&targeted), Some(at(20)));
    }

    #[test]
    fn retains_exact_and_routed_targeted_origins_separately() {
        let id = DeviceId::new(FIRST_ID).unwrap();
        let source = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, source, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, source, DiscoveryMethod::RoutedTargeted),
                at(20),
            )
            .unwrap();

        let locator = registry.get(id).unwrap().locators().next().unwrap();
        assert_eq!(locator.origins().len(), 2);
        assert!(locator.origins().any(|origin| {
            origin.method == DiscoveryMethod::Targeted
                && origin.interface.as_deref() == Some("test0")
        }));
        assert!(locator.origins().any(|origin| {
            origin.method == DiscoveryMethod::RoutedTargeted
                && origin.interface.as_deref() == Some("test0")
        }));
    }

    #[test]
    fn confirmed_reassignment_preserves_other_locator_claims() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();

        let outcome = registry
            .confirm_reassignment(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap();

        assert_eq!(outcome.reassigned_from, Some(first));
        assert!(!outcome.reassigned_device_removed);
        assert_eq!(registry.get(first).unwrap().locators().len(), 1);
        assert_eq!(registry.get(second).unwrap().locators().len(), 1);
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn confirmed_reassignment_removes_an_empty_previous_device() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();

        let outcome = registry
            .confirm_reassignment(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap();

        assert_eq!(outcome.reassigned_from, Some(first));
        assert!(outcome.reassigned_device_removed);
        assert!(registry.get(first).is_none());
        assert!(registry.get(second).is_some());
    }

    #[test]
    fn conflicting_observation_is_rejected_without_mutation() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        let before = registry.clone();

        let error = registry
            .observe(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap_err();

        assert_eq!(
            error,
            RegistryError::LocatorConflict {
                locator: shared.parse().unwrap(),
                current_owner: first,
                claimant: second,
            }
        );
        assert_eq!(registry, before);
    }

    #[test]
    fn expired_locator_can_be_claimed_without_confirmation() {
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .expire_stale(at(100), Duration::from_millis(50))
            .unwrap();

        let outcome = registry
            .observe(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(100),
            )
            .unwrap();

        assert!(outcome.device_added);
        assert!(outcome.locator_added);
        assert_eq!(outcome.reassigned_from, None);
        assert_eq!(registry.len(), 1);
        assert!(registry.get(second).is_some());
    }

    #[test]
    fn stale_origin_expires_without_removing_a_fresh_locator() {
        let id = DeviceId::new(FIRST_ID).unwrap();
        let higher_source = "192.0.2.20:65001";
        let lower_source = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, higher_source, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, higher_source, DiscoveryMethod::Ipv4Broadcast),
                at(90),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, lower_source, DiscoveryMethod::Ipv4Broadcast),
                at(90),
            )
            .unwrap();
        assert_eq!(
            registry
                .get(id)
                .unwrap()
                .preferred_locator()
                .unwrap()
                .source(),
            lower_source.parse().unwrap()
        );

        let expired = registry
            .expire_stale(at(100), Duration::from_millis(50))
            .unwrap();

        assert_eq!(expired.removed_origins, 1);
        assert_eq!(expired.removed_locators, 0);
        let device = registry.get(id).unwrap();
        assert_eq!(device.locators().len(), 2);
        assert_eq!(
            device.preferred_locator().unwrap().source(),
            lower_source.parse().unwrap()
        );
        assert!(
            device
                .locators()
                .all(|locator| locator.origins().len() == 1)
        );
    }

    #[test]
    fn configured_limits_reject_growth_without_partial_mutation() {
        let id = DeviceId::new(FIRST_ID).unwrap();
        let source = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::with_limits(1, 1, 1).unwrap();
        registry
            .observe(
                observation(FIRST_ID, source, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();

        let before = registry.clone();
        assert_eq!(
            registry
                .observe(
                    observation(SECOND_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted,),
                    at(20),
                )
                .unwrap_err(),
            RegistryError::DeviceLimitReached { maximum: 1 }
        );
        assert_eq!(registry, before);

        assert_eq!(
            registry
                .observe(
                    observation(FIRST_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted,),
                    at(20),
                )
                .unwrap_err(),
            RegistryError::LocatorLimitReached {
                device_id: id,
                maximum: 1,
            }
        );
        assert_eq!(registry, before);

        assert_eq!(
            registry
                .observe(
                    observation(FIRST_ID, source, DiscoveryMethod::Ipv4Broadcast),
                    at(20),
                )
                .unwrap_err(),
            RegistryError::OriginLimitReached {
                device_id: id,
                locator: source.parse().unwrap(),
                maximum: 1,
            }
        );
        assert_eq!(registry, before);
    }

    #[test]
    fn confirmed_reassignment_is_capacity_checked_before_mutation() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let other = "192.0.2.11:65001";
        let mut registry = DeviceRegistry::with_limits(2, 1, 1).unwrap();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(SECOND_ID, other, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        let before = registry.clone();

        let error = registry
            .confirm_reassignment(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap_err();

        assert_eq!(
            error,
            RegistryError::LocatorLimitReached {
                device_id: second,
                maximum: 1,
            }
        );
        assert_eq!(registry, before);
        assert!(registry.get(first).is_some());
    }

    #[test]
    fn confirmed_reassignment_can_replace_a_device_at_capacity() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let shared = "192.0.2.10:65001";
        let mut registry = DeviceRegistry::with_limits(1, 1, 1).unwrap();
        registry
            .observe(
                observation(FIRST_ID, shared, DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();

        registry
            .confirm_reassignment(
                observation(SECOND_ID, shared, DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap();

        assert_eq!(registry.len(), 1);
        assert!(registry.get(first).is_none());
        assert!(registry.get(second).is_some());
    }

    #[test]
    fn registry_limits_stay_within_absolute_ceiling() {
        assert_eq!(
            DeviceRegistry::with_limits(0, 1, 1),
            Err(RegistryError::InvalidDeviceLimit {
                value: 0,
                maximum: DeviceRegistry::ABSOLUTE_MAX_DEVICES,
            })
        );
        assert_eq!(
            DeviceRegistry::with_limits(1, 0, 1),
            Err(RegistryError::InvalidLocatorLimit {
                value: 0,
                maximum: DeviceRegistry::ABSOLUTE_MAX_LOCATORS_PER_DEVICE,
            })
        );
        assert_eq!(
            DeviceRegistry::with_limits(1, 1, 0),
            Err(RegistryError::InvalidOriginLimit {
                value: 0,
                maximum: DeviceRegistry::ABSOLUTE_MAX_ORIGINS_PER_LOCATOR,
            })
        );
        assert_eq!(
            DeviceRegistry::with_limits(usize::MAX, 1, 1),
            Err(RegistryError::InvalidDeviceLimit {
                value: usize::MAX,
                maximum: DeviceRegistry::ABSOLUTE_MAX_DEVICES,
            })
        );
        assert_eq!(
            DeviceRegistry::with_limits(1, usize::MAX, 1),
            Err(RegistryError::InvalidLocatorLimit {
                value: usize::MAX,
                maximum: DeviceRegistry::ABSOLUTE_MAX_LOCATORS_PER_DEVICE,
            })
        );
        assert_eq!(
            DeviceRegistry::with_limits(1, 1, usize::MAX),
            Err(RegistryError::InvalidOriginLimit {
                value: usize::MAX,
                maximum: DeviceRegistry::ABSOLUTE_MAX_ORIGINS_PER_LOCATOR,
            })
        );
    }

    #[test]
    fn expires_stale_locators_but_retains_a_device_with_a_fresh_claim() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.10:65001", DiscoveryMethod::Targeted),
                at(10),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted),
                at(90),
            )
            .unwrap();
        registry
            .observe(
                observation(SECOND_ID, "192.0.2.12:65001", DiscoveryMethod::Targeted),
                at(90),
            )
            .unwrap();

        let expired = registry
            .expire_stale(at(100), Duration::from_millis(50))
            .unwrap();

        assert_eq!(expired.removed_locators, 1);
        assert!(expired.removed_devices.is_empty());
        assert_eq!(registry.get(first).unwrap().locators().len(), 1);
        assert!(registry.get(second).is_some());
    }

    #[test]
    fn expiry_reports_devices_in_stable_order() {
        let first = DeviceId::new(FIRST_ID).unwrap();
        let second = DeviceId::new(SECOND_ID).unwrap();
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(SECOND_ID, "192.0.2.12:65001", DiscoveryMethod::Targeted),
                at(1),
            )
            .unwrap();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.10:65001", DiscoveryMethod::Targeted),
                at(1),
            )
            .unwrap();

        let expired = registry
            .expire_stale(at(100), Duration::from_millis(10))
            .unwrap();

        assert_eq!(expired.removed_locators, 2);
        assert_eq!(expired.removed_devices, vec![first, second]);
        assert!(registry.is_empty());
    }

    #[test]
    fn rejects_time_regression_without_mutating_registry() {
        let mut registry = DeviceRegistry::default();
        registry
            .observe(
                observation(FIRST_ID, "192.0.2.10:65001", DiscoveryMethod::Targeted),
                at(20),
            )
            .unwrap();
        let before = registry.clone();

        let error = registry
            .observe(
                observation(SECOND_ID, "192.0.2.11:65001", DiscoveryMethod::Targeted),
                at(19),
            )
            .unwrap_err();

        assert_eq!(
            error,
            RegistryError::TimeWentBackwards {
                previous: at(20),
                attempted: at(19),
            }
        );
        assert_eq!(registry, before);
    }
}
