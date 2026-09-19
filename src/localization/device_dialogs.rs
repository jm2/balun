//! Device-dialog presentation. Parser failures carry no entered values, and
//! remembered display names are interpolated once into plain-text dialog bodies.

use std::borrow::Cow;

use crate::discovery::{InvalidExactDiscoveryTarget, InvalidHostnameTarget};

pub struct FindLabels {
    pub title: Cow<'static, str>,
    pub description: Cow<'static, str>,
    pub target: Cow<'static, str>,
    pub cancel: Cow<'static, str>,
    pub find: Cow<'static, str>,
}

impl FindLabels {
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            title: rust_i18n::t!("device_dialogs.find_title", locale = locale),
            description: rust_i18n::t!("device_dialogs.find_description", locale = locale),
            target: rust_i18n::t!("device_dialogs.target", locale = locale),
            cancel: rust_i18n::t!("device_dialogs.cancel", locale = locale),
            find: rust_i18n::t!("device_dialogs.find", locale = locale),
        }
    }
}

pub struct ForgetLabels {
    pub menu: Cow<'static, str>,
    pub heading: Cow<'static, str>,
    pub cancel: Cow<'static, str>,
    pub forget: Cow<'static, str>,
    pub forgotten: Cow<'static, str>,
    pub forgotten_session: Cow<'static, str>,
}

impl ForgetLabels {
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            menu: rust_i18n::t!("device_dialogs.forget_menu", locale = locale),
            heading: rust_i18n::t!("device_dialogs.forget_heading", locale = locale),
            cancel: rust_i18n::t!("device_dialogs.cancel", locale = locale),
            forget: rust_i18n::t!("device_dialogs.forget", locale = locale),
            forgotten: rust_i18n::t!("device_dialogs.forgotten", locale = locale),
            forgotten_session: rust_i18n::t!("device_dialogs.forgotten_session", locale = locale),
        }
    }
}

pub fn forget_description(device: &str) -> Cow<'static, str> {
    forget_description_in(&rust_i18n::locale(), device)
}

fn forget_description_in(locale: &str, device: &str) -> Cow<'static, str> {
    rust_i18n::t!(
        "device_dialogs.forget_description",
        locale = locale,
        device = device
    )
}

pub fn address_validation(error: InvalidExactDiscoveryTarget) -> Cow<'static, str> {
    address_validation_in(&rust_i18n::locale(), error)
}

fn address_validation_in(locale: &str, error: InvalidExactDiscoveryTarget) -> Cow<'static, str> {
    match error {
        InvalidExactDiscoveryTarget::Empty => {
            rust_i18n::t!("device_dialogs.enter_target", locale = locale)
        }
        InvalidExactDiscoveryTarget::TooLong { .. }
        | InvalidExactDiscoveryTarget::ControlCharacter => {
            rust_i18n::t!("device_dialogs.numeric_address", locale = locale)
        }
        InvalidExactDiscoveryTarget::InvalidSyntax => {
            rust_i18n::t!("device_dialogs.address_syntax", locale = locale)
        }
        InvalidExactDiscoveryTarget::UnicastRequired => {
            rust_i18n::t!("device_dialogs.unicast", locale = locale)
        }
        InvalidExactDiscoveryTarget::Ipv4MappedIpv6Unsupported => {
            rust_i18n::t!("device_dialogs.ipv4_direct", locale = locale)
        }
        InvalidExactDiscoveryTarget::LinkLocalIpv6ScopeRequired => {
            rust_i18n::t!("device_dialogs.link_local", locale = locale)
        }
        InvalidExactDiscoveryTarget::ScopedIpv6Unsupported => {
            rust_i18n::t!("device_dialogs.scoped", locale = locale)
        }
    }
}

pub fn hostname_validation(error: InvalidHostnameTarget) -> Cow<'static, str> {
    hostname_validation_in(&rust_i18n::locale(), error)
}

fn hostname_validation_in(locale: &str, error: InvalidHostnameTarget) -> Cow<'static, str> {
    match error {
        InvalidHostnameTarget::Empty => {
            rust_i18n::t!("device_dialogs.enter_target", locale = locale)
        }
        InvalidHostnameTarget::TooLong { .. } | InvalidHostnameTarget::ControlCharacter => {
            rust_i18n::t!("device_dialogs.hostname_characters", locale = locale)
        }
        InvalidHostnameTarget::InvalidSyntax => {
            rust_i18n::t!("device_dialogs.hostname_syntax", locale = locale)
        }
        InvalidHostnameTarget::IpAddressLiteral => {
            rust_i18n::t!("device_dialogs.unicast", locale = locale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{DiscoveryEntry, InvalidDiscoveryEntry};
    use crate::localization::SUPPORTED_LOCALES;

    #[test]
    fn rejected_entries_never_reach_translated_validation_copy() {
        for locale in SUPPORTED_LOCALES {
            for value in [
                "private-tuner-name_example",
                "http://198.51.100.247/private-token",
                "198.51.100.247:65001",
                "fe80::247%private-scope",
            ] {
                let message = match DiscoveryEntry::parse(value).unwrap_err() {
                    InvalidDiscoveryEntry::Address(error) => address_validation_in(locale, error),
                    InvalidDiscoveryEntry::Hostname(error) => hostname_validation_in(locale, error),
                };
                for marker in [value, "198.51.100.247", "private-", "fe80::247"] {
                    assert!(!message.contains(marker), "{locale}");
                }
                assert!(!message.starts_with("device_dialogs."), "{locale}");
            }
        }
        assert_eq!(
            address_validation_in("de", InvalidExactDiscoveryTarget::Ipv4MappedIpv6Unsupported),
            "Geben Sie die IPv4-Adresse direkt ein."
        );
        assert_eq!(
            hostname_validation_in("fr", InvalidHostnameTarget::Empty),
            "Saisissez l’adresse IP ou le nom d’hôte d’un appareil."
        );
    }

    #[test]
    fn confirmation_preserves_literal_names_and_distinguishes_session_only_forgetting() {
        let device = "Tuner <b>Room & %{device}</b>";
        for locale in SUPPORTED_LOCALES {
            let body = forget_description_in(locale, device);
            assert_eq!(body.matches(device).count(), 1, "{locale}");
            let find = FindLabels::for_locale(locale);
            let forget = ForgetLabels::for_locale(locale);
            assert_eq!(find.cancel, forget.cancel);
            assert_ne!(forget.forgotten, forget.forgotten_session, "{locale}");
            for example in ["192.168.1.20", "fd00::20", "tuner.example"] {
                assert!(find.description.contains(example), "{locale}");
            }
        }
        assert_eq!(
            FindLabels::for_locale("de").title,
            "Gerät über Adresse suchen"
        );
        assert_eq!(ForgetLabels::for_locale("fr").forget, "Oublier");
    }
}
