//! Subnet-search presentation.
//!
//! Validation copy never echoes rejected input. The only interpolated
//! subnet is a validated, canonical [`TypedSubnetScope`], and every text that
//! contains it must be rendered as plain text, never as markup.

use std::borrow::Cow;

use crate::controller::{DiscoveryFailure, DiscoveryIncomplete, DiscoveryStatus};
use crate::discovery::{InvalidTypedSubnetScope, TypedSubnetScope};

/// Labels of the entry dialog and the sidebar action.
pub struct SubnetEntryLabels {
    pub button: Cow<'static, str>,
    pub unavailable: Cow<'static, str>,
    pub title: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub entry: Cow<'static, str>,
    pub cancel: Cow<'static, str>,
    pub proceed: Cow<'static, str>,
    pub forget: Cow<'static, str>,
}

impl SubnetEntryLabels {
    /// Resolve the complete label set using the currently selected locale.
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            button: rust_i18n::t!("subnet_search.button", locale = locale),
            unavailable: rust_i18n::t!("subnet_search.unavailable", locale = locale),
            title: rust_i18n::t!("subnet_search.title", locale = locale),
            description: rust_i18n::t!("subnet_search.description", locale = locale),
            entry: rust_i18n::t!("subnet_search.entry", locale = locale),
            cancel: rust_i18n::t!("subnet_search.cancel", locale = locale),
            proceed: rust_i18n::t!("subnet_search.continue", locale = locale),
            forget: rust_i18n::t!("subnet_search.forget", locale = locale),
        }
    }
}

/// Copy of the confirmation that authorizes exactly one search of `scope`,
/// with the candidate count and request budget it displays.
pub struct SubnetConfirmation {
    pub heading: Cow<'static, str>,
    pub body: Cow<'static, str>,
    pub cancel: Cow<'static, str>,
    pub search: Cow<'static, str>,
    pub candidates: usize,
    pub requests: usize,
}

impl SubnetConfirmation {
    /// The confirmation for `scope`, with its exact candidate count and
    /// outbound request budget. Render it as plain text.
    pub fn current(scope: TypedSubnetScope) -> Self {
        Self::for_locale(&rust_i18n::locale(), scope)
    }

    fn for_locale(locale: &str, scope: TypedSubnetScope) -> Self {
        let subnet = scope.to_string();
        let candidate_count = scope.candidate_count();
        let request_budget = scope.maximum_request_attempts();
        let candidates = candidate_count.to_string();
        let requests = request_budget.to_string();
        Self {
            heading: rust_i18n::t!(
                "subnet_search.confirm_heading",
                locale = locale,
                subnet = subnet
            ),
            body: rust_i18n::t!(
                "subnet_search.confirm_body",
                locale = locale,
                subnet = subnet,
                candidates = candidates,
                requests = requests
            ),
            cancel: rust_i18n::t!("subnet_search.cancel", locale = locale),
            search: rust_i18n::t!("subnet_search.search", locale = locale),
            candidates: candidate_count,
            requests: request_budget,
        }
    }
}

/// The live preview under the entry: the exact candidate count and request
/// budget the confirmation will show.
pub fn preview(scope: TypedSubnetScope) -> Cow<'static, str> {
    preview_in(&rust_i18n::locale(), scope)
}

fn preview_in(locale: &str, scope: TypedSubnetScope) -> Cow<'static, str> {
    rust_i18n::t!(
        "subnet_search.preview",
        locale = locale,
        candidates = scope.candidate_count().to_string(),
        requests = scope.maximum_request_attempts().to_string()
    )
}

/// Describe a rejected subnet by its value-free category.
pub fn validation(error: InvalidTypedSubnetScope) -> Cow<'static, str> {
    validation_in(&rust_i18n::locale(), error)
}

fn validation_in(locale: &str, error: InvalidTypedSubnetScope) -> Cow<'static, str> {
    match error {
        InvalidTypedSubnetScope::InvalidSyntax => {
            rust_i18n::t!("subnet_search.invalid_syntax", locale = locale)
        }
        InvalidTypedSubnetScope::Noncanonical => {
            rust_i18n::t!("subnet_search.noncanonical", locale = locale)
        }
        InvalidTypedSubnetScope::TooWide => {
            rust_i18n::t!("subnet_search.too_wide", locale = locale)
        }
        InvalidTypedSubnetScope::NotPrivate => {
            rust_i18n::t!("subnet_search.not_private", locale = locale)
        }
    }
}

/// Toasts about starting, refusing, and forgetting subnet searches.
pub struct SubnetNotices {
    pub searching: Cow<'static, str>,
    pub busy: Cow<'static, str>,
    pub confirmation_expired: Cow<'static, str>,
    pub forgotten: Cow<'static, str>,
    pub forgotten_session: Cow<'static, str>,
}

impl SubnetNotices {
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            searching: rust_i18n::t!("subnet_search.searching", locale = locale),
            busy: rust_i18n::t!("subnet_search.busy", locale = locale),
            confirmation_expired: rust_i18n::t!(
                "subnet_search.confirmation_expired",
                locale = locale
            ),
            forgotten: rust_i18n::t!("subnet_search.forgotten", locale = locale),
            forgotten_session: rust_i18n::t!("subnet_search.forgotten_session", locale = locale),
        }
    }
}

/// The empty-list title and description for a subnet search in `status`.
pub fn status(status: DiscoveryStatus) -> (Cow<'static, str>, Cow<'static, str>) {
    status_in(&rust_i18n::locale(), status)
}

fn status_in(locale: &str, status: DiscoveryStatus) -> (Cow<'static, str>, Cow<'static, str>) {
    let text = |key: &'static str| -> Cow<'static, str> { rust_i18n::t!(key, locale = locale) };
    let (title, description) = match status {
        DiscoveryStatus::Idle => ("subnet_search.stopped_title", "subnet_search.stopped"),
        DiscoveryStatus::Refreshing => (
            "subnet_search.searching_title",
            "subnet_search.searching_status",
        ),
        DiscoveryStatus::Ready => ("subnet_search.complete_title", "subnet_search.complete"),
        DiscoveryStatus::NoResponse => ("subnet_search.none_title", "subnet_search.none"),
        DiscoveryStatus::Incomplete(DiscoveryIncomplete::Deadline) => {
            ("subnet_search.incomplete_title", "subnet_search.deadline")
        }
        DiscoveryStatus::Incomplete(DiscoveryIncomplete::DeviceLimit) => (
            "subnet_search.incomplete_title",
            "subnet_search.device_limit",
        ),
        DiscoveryStatus::Failed(DiscoveryFailure::SubnetUnavailable) => (
            "subnet_search.unavailable_title",
            "subnet_search.unavailable_status",
        ),
        DiscoveryStatus::Failed(DiscoveryFailure::SubnetConfirmationStale) => {
            ("subnet_search.stale_title", "subnet_search.stale")
        }
        DiscoveryStatus::Failed(DiscoveryFailure::NetworkChanged) => {
            ("subnet_search.stopped_title", "subnet_search.changed")
        }
        DiscoveryStatus::Failed(_) => ("subnet_search.failed_title", "subnet_search.failed"),
    };
    (text(title), text(description))
}

/// The banner kept above listed devices for a subnet search in `status`, or
/// `None` while it runs or before it starts.
pub fn banner(status: DiscoveryStatus) -> Option<Cow<'static, str>> {
    banner_in(&rust_i18n::locale(), status)
}

fn banner_in(locale: &str, status: DiscoveryStatus) -> Option<Cow<'static, str>> {
    let key = match status {
        DiscoveryStatus::Idle | DiscoveryStatus::Refreshing => return None,
        DiscoveryStatus::Ready => "subnet_search.banner_complete",
        DiscoveryStatus::NoResponse => "subnet_search.banner_none",
        DiscoveryStatus::Incomplete(DiscoveryIncomplete::Deadline) => {
            "subnet_search.banner_deadline"
        }
        DiscoveryStatus::Incomplete(DiscoveryIncomplete::DeviceLimit) => {
            "subnet_search.banner_device_limit"
        }
        DiscoveryStatus::Failed(DiscoveryFailure::NetworkChanged) => "subnet_search.banner_changed",
        DiscoveryStatus::Failed(DiscoveryFailure::SubnetConfirmationStale) => {
            "subnet_search.banner_stale"
        }
        DiscoveryStatus::Failed(DiscoveryFailure::SubnetUnavailable) => {
            "subnet_search.banner_unavailable"
        }
        DiscoveryStatus::Failed(_) => "subnet_search.banner_failed",
    };
    Some(rust_i18n::t!(key, locale = locale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::SUPPORTED_LOCALES;

    fn every_status() -> Vec<DiscoveryStatus> {
        let mut statuses = vec![
            DiscoveryStatus::Idle,
            DiscoveryStatus::Refreshing,
            DiscoveryStatus::Ready,
            DiscoveryStatus::NoResponse,
            DiscoveryStatus::Incomplete(DiscoveryIncomplete::Deadline),
            DiscoveryStatus::Incomplete(DiscoveryIncomplete::DeviceLimit),
        ];
        statuses.extend(
            [
                DiscoveryFailure::InterfaceEnumeration,
                DiscoveryFailure::Network,
                DiscoveryFailure::ExactTargetLimitReached,
                DiscoveryFailure::Internal,
                DiscoveryFailure::SubnetUnavailable,
                DiscoveryFailure::SubnetConfirmationStale,
                DiscoveryFailure::NetworkChanged,
            ]
            .map(DiscoveryStatus::Failed),
        );
        statuses
    }

    /// The confirmation carries the exact scope and budget in every locale,
    /// and it states that routing picks the path.
    #[test]
    fn confirmation_names_the_entered_scope_and_its_exact_budget_everywhere() {
        for (text, candidates, requests) in [
            ("192.168.2.0/23", "510", "1020"),
            ("10.0.0.0/24", "254", "508"),
            ("172.16.0.0/31", "2", "4"),
            ("192.168.9.9/32", "1", "2"),
        ] {
            let scope: TypedSubnetScope = text.parse().unwrap();
            for locale in SUPPORTED_LOCALES {
                let confirmation = SubnetConfirmation::for_locale(locale, scope);
                assert_eq!(confirmation.candidates.to_string(), candidates);
                assert_eq!(confirmation.requests.to_string(), requests);
                assert!(confirmation.heading.contains(text), "{locale}");
                assert_eq!(confirmation.body.matches(text).count(), 1, "{locale}");
                assert!(confirmation.body.contains(candidates), "{locale}");
                assert!(confirmation.body.contains(requests), "{locale}");
                assert!(confirmation.body.contains("64"), "{locale}");
                assert!(confirmation.body.contains("30"), "{locale}");
                let preview = preview_in(locale, scope);
                assert!(preview.contains(candidates) && preview.contains(requests));
                assert!(!confirmation.body.contains("%{"), "{locale}");
                assert_eq!(
                    confirmation.cancel,
                    SubnetEntryLabels::for_locale(locale).cancel
                );
                assert_eq!(
                    confirmation.cancel,
                    rust_i18n::t!("device_dialogs.cancel", locale = locale)
                );
            }
        }
        let english = SubnetConfirmation::for_locale("en", "192.168.2.0/23".parse().unwrap());
        assert_eq!(english.heading, "Search 192.168.2.0/23?");
        assert!(
            english
                .body
                .contains("current network routing selects the path")
        );
        assert!(english.body.contains("not what routers deliver"));
    }

    /// Rejections are explained by category and never repeat the input.
    #[test]
    fn validation_never_echoes_rejected_text() {
        for locale in SUPPORTED_LOCALES {
            for value in [
                "10.9.8.7/23",
                "203.0.113.0/24",
                "10.0.0.0/22",
                "tuner-secret",
            ] {
                let error = value.parse::<TypedSubnetScope>().unwrap_err();
                let message = validation_in(locale, error);
                assert!(!message.contains(value), "{locale}");
                assert!(!message.contains("10.9.8.7"), "{locale}");
                assert!(!message.starts_with("subnet_search."), "{locale}");
            }
        }
        assert_eq!(
            validation_in("en", InvalidTypedSubnetScope::TooWide),
            "Enter a subnet from /23 through /32."
        );
    }

    #[test]
    fn every_subnet_status_and_label_has_translated_copy() {
        for locale in SUPPORTED_LOCALES {
            for status in every_status() {
                let (title, description) = status_in(locale, status);
                for text in [&title, &description] {
                    assert!(!text.is_empty() && !text.starts_with("subnet_search."));
                }
                if let Some(banner) = banner_in(locale, status) {
                    assert!(!banner.starts_with("subnet_search."), "{locale}");
                }
            }
            let labels = SubnetEntryLabels::for_locale(locale);
            let notices = SubnetNotices::for_locale(locale);
            for text in [
                labels.button,
                labels.unavailable,
                labels.title,
                labels.description,
                labels.entry,
                labels.proceed,
                labels.forget,
                notices.searching,
                notices.busy,
                notices.confirmation_expired,
                notices.forgotten,
                notices.forgotten_session,
            ] {
                assert!(
                    !text.is_empty() && !text.starts_with("subnet_search."),
                    "{locale}"
                );
            }
        }
        assert!(banner_in("en", DiscoveryStatus::Refreshing).is_none());
        assert!(banner_in("en", DiscoveryStatus::Idle).is_none());
        assert_eq!(
            status_in(
                "en",
                DiscoveryStatus::Failed(DiscoveryFailure::NetworkChanged)
            )
            .0,
            "Subnet search stopped"
        );
        // The current-locale entry points agree with the explicit ones.
        assert!(!SubnetEntryLabels::current().title.is_empty());
        assert!(!SubnetNotices::current().busy.is_empty());
        let scope: TypedSubnetScope = "10.0.0.0/24".parse().unwrap();
        assert!(SubnetConfirmation::current(scope).body.contains("254"));
        assert!(preview(scope).contains("508"));
        assert!(!validation(InvalidTypedSubnetScope::NotPrivate).is_empty());
        assert!(!status(DiscoveryStatus::Ready).0.is_empty());
        assert!(banner(DiscoveryStatus::Ready).is_some());
    }
}
