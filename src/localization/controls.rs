//! Navigation titles and plain-text player control names.

use std::borrow::Cow;

/// Navigation-page names shared with the corresponding visible headers.
pub struct NavigationLabels {
    pub devices: Cow<'static, str>,
    pub channels: Cow<'static, str>,
    pub live_tv: Cow<'static, str>,
    pub channels_and_live_tv: Cow<'static, str>,
}

impl NavigationLabels {
    /// Read the startup locale's labels without changing global locale state.
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            devices: rust_i18n::t!("navigation.devices", locale = locale),
            channels: rust_i18n::t!("navigation.channels", locale = locale),
            live_tv: rust_i18n::t!("navigation.live_tv", locale = locale),
            channels_and_live_tv: rust_i18n::t!("navigation.channels_and_live_tv", locale = locale),
        }
    }
}

/// Player tooltip and accessible-name copy. Shortcut syntax stays in the UI.
pub struct PlayerLabels {
    pub volume_label: Cow<'static, str>,
    pub mute_label: Cow<'static, str>,
    pub unmute_label: Cow<'static, str>,
    pub stop_label: Cow<'static, str>,
    pub enter_fullscreen_label: Cow<'static, str>,
    pub exit_fullscreen_label: Cow<'static, str>,
    pub video_label: Cow<'static, str>,
    pub status_label: Cow<'static, str>,
    pub watching_reason: Cow<'static, str>,
}

impl PlayerLabels {
    /// Read labels for the startup locale. The mute toggle keeps its stable
    /// accessible name; its checked state conveys muting and its tooltip names
    /// the action that a click will perform.
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    fn for_locale(locale: &str) -> Self {
        Self {
            volume_label: rust_i18n::t!("controls.volume", locale = locale),
            mute_label: rust_i18n::t!("controls.mute", locale = locale),
            unmute_label: rust_i18n::t!("controls.unmute", locale = locale),
            stop_label: rust_i18n::t!("controls.stop", locale = locale),
            enter_fullscreen_label: rust_i18n::t!("controls.enter_fullscreen", locale = locale),
            exit_fullscreen_label: rust_i18n::t!("controls.exit_fullscreen", locale = locale),
            video_label: rust_i18n::t!("controls.video", locale = locale),
            status_label: rust_i18n::t!("controls.status", locale = locale),
            watching_reason: rust_i18n::t!("controls.watching", locale = locale),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::SUPPORTED_LOCALES;
    use std::collections::BTreeSet;

    #[test]
    fn player_actions_and_navigation_names_are_distinct_in_every_locale() {
        for locale in SUPPORTED_LOCALES {
            let player = PlayerLabels::for_locale(locale);
            let actions = [
                player.volume_label,
                player.mute_label,
                player.unmute_label,
                player.stop_label,
                player.enter_fullscreen_label,
                player.exit_fullscreen_label,
            ];
            assert_eq!(
                actions.iter().collect::<BTreeSet<_>>().len(),
                actions.len(),
                "{locale}"
            );
            let navigation = NavigationLabels::for_locale(locale);
            let titles = [
                navigation.devices,
                navigation.channels,
                navigation.live_tv,
                navigation.channels_and_live_tv,
            ];
            assert_eq!(
                titles.iter().collect::<BTreeSet<_>>().len(),
                titles.len(),
                "{locale}"
            );
            for text in actions.iter().chain(titles.iter()) {
                assert!(
                    !text.contains('_'),
                    "{locale}: plain-text accessible copy has no mnemonic markup"
                );
            }
        }
    }

    #[test]
    fn translated_controls_and_navigation_do_not_use_english_fallback() {
        let german = PlayerLabels::for_locale("de");
        assert_eq!(german.mute_label, "Live-TV stummschalten");
        assert_eq!(german.unmute_label, "Live-TV-Ton einschalten");
        assert_eq!(german.exit_fullscreen_label, "Vollbild verlassen");
        assert_eq!(NavigationLabels::for_locale("de").devices, "Geräte");
        assert_eq!(NavigationLabels::for_locale("ja").channels, "チャンネル");
        assert_eq!(PlayerLabels::for_locale("zh-TW").stop_label, "停止電視直播");
    }
}
