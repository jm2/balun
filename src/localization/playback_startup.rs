//! Startup status from typed runtime capabilities and fixed failure categories.
//! Native exception text never enters translated presentation.

use std::borrow::Cow;

use crate::playback::{PlaybackCapabilities, PlaybackInitializationError, RuntimeVersion};

pub struct StartupText {
    pub title: Cow<'static, str>,
    pub description: Cow<'static, str>,
}

pub fn capabilities(capabilities: &PlaybackCapabilities) -> StartupText {
    let locale = rust_i18n::locale();
    if capabilities.is_foundation_ready() {
        ready_in(&locale, capabilities.runtime_version())
    } else {
        let missing = capabilities
            .missing_required()
            .map(|factory| factory.name())
            .collect::<Vec<_>>()
            .join(", ");
        missing_in(&locale, &missing)
    }
}

fn ready_in(locale: &str, version: RuntimeVersion) -> StartupText {
    StartupText {
        title: rust_i18n::t!("startup.ready_title", locale = locale),
        description: rust_i18n::t!(
            "startup.ready_description",
            locale = locale,
            version = version
        ),
    }
}

fn missing_in(locale: &str, missing: &str) -> StartupText {
    StartupText {
        title: rust_i18n::t!("startup.missing_title", locale = locale),
        description: rust_i18n::t!(
            "startup.missing_description",
            locale = locale,
            missing = missing
        ),
    }
}

pub fn initialization(failure: PlaybackInitializationError) -> StartupText {
    initialization_in(&rust_i18n::locale(), failure)
}

fn initialization_in(locale: &str, failure: PlaybackInitializationError) -> StartupText {
    let description = match failure {
        PlaybackInitializationError::MainContextUnavailable => {
            rust_i18n::t!("startup.main_context_description", locale = locale)
        }
        PlaybackInitializationError::InitializationFailed => {
            rust_i18n::t!("startup.initialization_failed_description", locale = locale)
        }
        PlaybackInitializationError::RuntimeTooOld { found, minimum } => rust_i18n::t!(
            "startup.old_runtime_description",
            locale = locale,
            found = found,
            minimum = minimum
        ),
    };
    StartupText {
        title: rust_i18n::t!("startup.failed_title", locale = locale),
        description,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::SUPPORTED_LOCALES;

    #[test]
    fn every_startup_failure_is_localized_with_typed_versions() {
        let old = PlaybackInitializationError::RuntimeTooOld {
            found: RuntimeVersion::new(1, 18, 6),
            minimum: RuntimeVersion::new(1, 20, 0),
        };
        for locale in SUPPORTED_LOCALES {
            for failure in [
                PlaybackInitializationError::MainContextUnavailable,
                PlaybackInitializationError::InitializationFailed,
                old,
            ] {
                let text = initialization_in(locale, failure);
                assert!(
                    !text.title.is_empty() && !text.description.is_empty(),
                    "{locale}"
                );
                assert!(!text.description.contains("%{"), "{locale}");
            }
            let text = initialization_in(locale, old);
            assert!(text.description.contains("1.18.6"), "{locale}");
            assert!(text.description.contains("1.20.0"), "{locale}");
        }
        assert_eq!(
            initialization_in("en", PlaybackInitializationError::InitializationFailed).description,
            "GStreamer could not be initialized. Device discovery and lineup inspection remain available."
        );
        assert_eq!(
            initialization_in("de", old).title,
            "Wiedergabeinitialisierung nicht verfügbar"
        );
        assert_eq!(
            initialization_in("en", PlaybackInitializationError::MainContextUnavailable)
                .description,
            "Balun playback initialization requires ownership of the default GLib main context. Device discovery and lineup inspection remain available."
        );
    }

    #[test]
    fn ready_and_missing_components_keep_literal_parameter_values() {
        let missing = "appsrc, gtk4paintablesink & <fixture> %{version}";
        for locale in SUPPORTED_LOCALES {
            let ready = ready_in(locale, RuntimeVersion::new(1, 28, 7));
            assert!(ready.description.contains("1.28.7"), "{locale}");
            let text = missing_in(locale, missing);
            assert!(text.description.contains(missing), "{locale}");
            assert_ne!(ready.title, text.title, "{locale}");
        }
        assert_eq!(
            ready_in("de", RuntimeVersion::new(1, 28, 7)).title,
            "Kanal auswählen"
        );
        assert_eq!(
            missing_in("fr", "appsrc").description,
            "Les composants GStreamer requis sont manquants : appsrc."
        );
    }
}
