//! Shared, GTK-free presentation text and startup locale selection.
//!
//! Catalogs are compiled into the library. They never come from the network or
//! a profile. Only presentation copy belongs here; tracing and domain errors
//! retain their stable diagnostic vocabulary.

use std::borrow::Cow;
use std::sync::OnceLock;

pub mod controls;
pub mod device_dialogs;
#[cfg(feature = "playback")]
pub mod playback_failure;
#[cfg(feature = "playback")]
pub mod playback_startup;
pub mod playback_status;
pub mod subnet_search;

// Keep the generated initializer isolated as the catalogs grow. Like Tributary,
// desktop startup forces it on one joined thread with an explicit stack size.
#[allow(clippy::large_stack_frames)]
pub(crate) mod catalog {
    rust_i18n::i18n!("locales", fallback = "en");

    pub(super) fn initialize() {
        let _ = std::sync::LazyLock::force(&_RUST_I18N_BACKEND);
    }
}

/// Catalog identifiers, matching Tributary's initial thirteen locales.
pub const SUPPORTED_LOCALES: [&str; 13] = [
    "de", "en", "es", "fr", "it", "ja", "ko", "nl", "pl", "pt-BR", "ru", "zh-CN", "zh-TW",
];

/// Select a shipped catalog from an OS locale, quietly falling back to English.
///
/// Accept POSIX encoding/modifier suffixes, underscores, and ASCII case variants.
/// Prefer an exact catalog, then a Chinese script mapping, then a base language.
/// An unsupported regional-only language (for example `pt-PT`) uses English.
pub fn select_locale(raw: Option<&str>) -> &'static str {
    let Some(raw) = raw.filter(|raw| raw.len() <= 128) else {
        return "en";
    };
    let normalized = raw
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-");
    if normalized
        .split('-')
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    {
        return "en";
    }
    if let Some(locale) = SUPPORTED_LOCALES
        .iter()
        .find(|locale| locale.eq_ignore_ascii_case(&normalized))
    {
        return locale;
    }
    let mut parts = normalized.split('-');
    let language = parts.next().unwrap_or_default();
    if language.eq_ignore_ascii_case("zh") {
        match parts.next() {
            Some(script) if script.eq_ignore_ascii_case("Hans") => return "zh-CN",
            Some(script) if script.eq_ignore_ascii_case("Hant") => return "zh-TW",
            _ => {}
        }
    }
    SUPPORTED_LOCALES
        .into_iter()
        .find(|locale| locale.eq_ignore_ascii_case(language))
        .unwrap_or("en")
}

/// Fixed startup failure with no locale, environment, or thread-panic details.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("Balun could not initialize interface translations")]
pub struct InitializationError;

/// Initialize the shared catalog and locale once, before building any widgets.
///
/// One owned, joined thread gives the generated catalog an 8 MiB stack on every
/// platform. Subsequent calls retain the startup locale; there is no live reload.
pub fn initialize() -> Result<&'static str, InitializationError> {
    static RESULT: OnceLock<Result<&'static str, InitializationError>> = OnceLock::new();
    *RESULT.get_or_init(|| {
        std::thread::Builder::new()
            .name("balun-i18n-initializer".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                catalog::initialize();
                let locale = select_locale(sys_locale::get_locale().as_deref());
                rust_i18n::set_locale(locale);
                locale
            })
            .map_err(|_| InitializationError)?
            .join()
            .map_err(|_| InitializationError)
    })
}

/// About action label, including the GTK mnemonic marker.
pub fn about_label() -> Cow<'static, str> {
    rust_i18n::t!("app.about")
}

/// Quit action label, including the GTK mnemonic marker.
pub fn quit_label() -> Cow<'static, str> {
    rust_i18n::t!("app.quit")
}

/// Main-menu tooltip and accessible name (without mnemonic markup).
pub fn main_menu_label() -> Cow<'static, str> {
    rust_i18n::t!("app.main_menu")
}

/// About dialog's description; the application and hardware brands stay intact.
pub fn application_description() -> Cow<'static, str> {
    rust_i18n::t!("app.description")
}

/// Notice shown once when this session's preferences cannot be loaded or saved.
pub fn settings_unavailable_notice() -> Cow<'static, str> {
    rust_i18n::t!("settings.unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn placeholders(text: &str) -> BTreeSet<&str> {
        text.split("%{")
            .skip(1)
            .map(|part| {
                let (name, _) = part.split_once('}').expect("close catalog placeholder");
                assert!(
                    !name.is_empty()
                        && name
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                );
                name
            })
            .collect()
    }

    #[test]
    fn locale_selection_normalizes_supported_os_forms() {
        for locale in SUPPORTED_LOCALES {
            assert_eq!(select_locale(Some(locale)), locale);
        }
        for (raw, expected) in [
            ("de_DE.UTF-8", "de"),
            ("fr_CA@euro", "fr"),
            ("EN_us.UTF-8", "en"),
            ("pt_br", "pt-BR"),
            ("zh_cn", "zh-CN"),
            ("zh_TW.UTF-8", "zh-TW"),
            ("zh-Hans", "zh-CN"),
            ("zh-Hant-TW", "zh-TW"),
            ("ja-JP", "ja"),
        ] {
            assert_eq!(select_locale(Some(raw)), expected, "{raw}");
        }
    }

    #[test]
    fn absent_unsupported_and_malformed_locales_fall_back_to_english() {
        assert_eq!(select_locale(None), "en");
        for raw in [
            "", "C", "C.UTF-8", "POSIX", "pt-PT", "zh", "ar-EG", "de-", "de-!", "de:fr",
        ] {
            assert_eq!(select_locale(Some(raw)), "en", "{raw}");
        }
        assert_eq!(select_locale(Some(&"de-".repeat(100))), "en");
    }

    #[test]
    fn every_catalog_has_exact_english_keys_and_compiled_nonempty_values() {
        // Read raw catalog keys: the runtime's English fallback must not hide a
        // missing translation. Use the same parser as the compile-time macro.
        let locales = rust_i18n_support::try_load_locales(
            concat!(env!("CARGO_MANIFEST_DIR"), "/locales"),
            |_| false,
            true,
        )
        .expect("read all translation catalogs");
        assert_eq!(
            locales.keys().map(String::as_str).collect::<BTreeSet<_>>(),
            SUPPORTED_LOCALES.into_iter().collect()
        );
        let english = &locales["en"];
        assert!(!english.is_empty());
        for (locale, messages) in &locales {
            assert_eq!(
                messages.keys().collect::<Vec<_>>(),
                english.keys().collect::<Vec<_>>(),
                "{locale}"
            );
            for (key, text) in messages {
                assert!(!text.trim().is_empty(), "{locale}: {key}");
                assert_eq!(
                    placeholders(text),
                    placeholders(&english[key]),
                    "{locale}: {key}"
                );
                assert_eq!(
                    catalog::_rust_i18n_backend()
                        .translate(locale, key)
                        .as_deref(),
                    Some(text.as_str()),
                    "{locale}: {key}"
                );
            }
        }
    }

    #[test]
    fn explicit_locale_lookup_and_missing_locale_fallback_do_not_mutate_global_state() {
        assert_eq!(rust_i18n::t!("app.main_menu", locale = "de"), "Hauptmenü");
        assert_eq!(
            rust_i18n::t!("app.main_menu", locale = "unsupported"),
            "Main menu"
        );
        assert_eq!(rust_i18n::t!("app.about", locale = "en"), "_About Balun");
    }

    // Environment and the global selected locale belong to a process. Exercise
    // actual startup in child test processes instead of racing parallel tests.
    #[cfg(target_os = "linux")]
    #[test]
    fn startup_locale_child() {
        let Ok(expected) = std::env::var("BALUN_TEST_EXPECTED_LOCALE") else {
            return;
        };
        assert_eq!(initialize().unwrap(), expected);
        assert_eq!(initialize().unwrap(), expected);
        assert_eq!(&*rust_i18n::locale(), expected);
        assert_eq!(
            main_menu_label(),
            if expected == "de" {
                "Hauptmenü"
            } else {
                "Main menu"
            }
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn startup_selects_lang_or_quiet_english_in_isolated_processes() {
        for (lang, expected) in [
            (Some("de_DE.UTF-8"), "de"),
            (Some("xx_YY.UTF-8"), "en"),
            (None, "en"),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "localization::tests::startup_locale_child",
                    "--nocapture",
                ])
                .env_remove("LANGUAGE")
                .env_remove("LC_ALL")
                .env_remove("LC_MESSAGES")
                .env_remove("LANG")
                .env("BALUN_TEST_EXPECTED_LOCALE", expected);
            if let Some(lang) = lang {
                child.env("LANG", lang);
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "unsupported locales must be quiet"
            );
        }
    }
}
