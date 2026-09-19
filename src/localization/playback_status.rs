//! Playback progress copy. Device/channel values remain uninterpreted text;
//! the widget boundary escapes the fully composed description before markup.

use std::borrow::Cow;

/// Fixed progress labels shared by the header and status page.
pub struct ProgressLabels {
    pub stopped: Cow<'static, str>,
    pub connecting: Cow<'static, str>,
    pub playing: Cow<'static, str>,
    pub buffering: Cow<'static, str>,
    pub unavailable: Cow<'static, str>,
    pub playback_unavailable: Cow<'static, str>,
}

impl ProgressLabels {
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            stopped: rust_i18n::t!("progress.stopped", locale = locale),
            connecting: rust_i18n::t!("progress.connecting", locale = locale),
            playing: rust_i18n::t!("progress.playing", locale = locale),
            buffering: rust_i18n::t!("progress.buffering", locale = locale),
            unavailable: rust_i18n::t!("progress.unavailable", locale = locale),
            playback_unavailable: rust_i18n::t!("progress.playback_unavailable", locale = locale),
        }
    }
}

/// Header and description use the same bounded integer percentage.
pub struct BufferingText {
    pub status: Cow<'static, str>,
    pub description: Cow<'static, str>,
}

pub fn buffering(percent: u8) -> BufferingText {
    buffering_in(&rust_i18n::locale(), percent)
}

fn buffering_in(locale: &str, percent: u8) -> BufferingText {
    let percent = percent.min(100);
    BufferingText {
        status: rust_i18n::t!(
            "progress.buffering_status",
            locale = locale,
            percent = percent
        ),
        description: rust_i18n::t!(
            "progress.buffering_description",
            locale = locale,
            percent = percent
        ),
    }
}

/// Interpolate display names as values, never translation keys or templates.
pub fn opening(channel: &str, device: &str) -> Cow<'static, str> {
    opening_in(&rust_i18n::locale(), channel, device)
}

fn opening_in(locale: &str, channel: &str, device: &str) -> Cow<'static, str> {
    rust_i18n::t!(
        "progress.opening",
        locale = locale,
        channel = channel,
        device = device
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::SUPPORTED_LOCALES;

    #[test]
    fn formatted_progress_uses_translated_templates_and_bounds_percentages() {
        assert_eq!(
            ProgressLabels::for_locale("de").connecting,
            "Verbindung wird hergestellt"
        );
        assert_eq!(buffering_in("en", 42).status, "Buffering 42%");
        assert_eq!(buffering_in("de", 42).status, "Puffern 42 %");
        assert_eq!(
            buffering_in("fr", 42).description,
            "Mise en mémoire tampon de la télévision en direct : 42 %."
        );
        for locale in SUPPORTED_LOCALES {
            for percent in [0, 42, 100, 255] {
                let text = buffering_in(locale, percent);
                let expected = percent.min(100).to_string();
                assert!(text.status.contains(&expected), "{locale}");
                assert!(text.description.contains(&expected), "{locale}");
                assert!(!text.status.contains("%{"), "{locale}");
                assert!(!text.description.contains("%{"), "{locale}");
            }
        }
    }

    #[test]
    fn display_names_are_preserved_without_recursive_placeholder_expansion() {
        let channel = "7.1 News & <i>%{device}</i>";
        let device = "Tuner %{channel} <b>Room</b>";
        for locale in SUPPORTED_LOCALES {
            let description = opening_in(locale, channel, device);
            assert!(description.contains(channel), "{locale}");
            assert!(description.contains(device), "{locale}");
        }
        assert_eq!(
            opening_in("de", "7.1 News", "Tuner"),
            "7.1 News auf Tuner wird geöffnet."
        );
    }
}
