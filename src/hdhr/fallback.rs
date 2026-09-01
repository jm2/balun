use crate::discovery::{LocatorClaim, RegisteredDevice};

/// Return registry locators in the only order used for HTTP fallback:
/// preferred first, then every remaining address in deterministic order.
pub(super) fn preferred_first_locators(device: &RegisteredDevice) -> Vec<&LocatorClaim> {
    let preferred_source = device.preferred_locator().map(LocatorClaim::source);
    let mut locators = device.locators().collect::<Vec<_>>();
    locators.sort_by_key(|locator| (Some(locator.source()) != preferred_source, locator.source()));
    locators
}
