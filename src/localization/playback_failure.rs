//! Playback failure presentation from closed categories, never native error text.
//! Display names remain literal values; the widget escapes the completed message.

use std::borrow::Cow;

use crate::playback::{MissingMedia, PlaybackPipelineFailure};

/// Localized status-page title and complete recovery description.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureText {
    pub title: Cow<'static, str>,
    pub description: Cow<'static, str>,
}

/// Describe a typed failure using only the admitted device/channel display names.
pub fn pipeline(failure: PlaybackPipelineFailure, channel: &str, device: &str) -> FailureText {
    pipeline_in(&rust_i18n::locale(), failure, channel, device)
}

/// Resolve a complete message without changing the process-wide locale.
fn pipeline_in(
    locale: &str,
    failure: PlaybackPipelineFailure,
    channel: &str,
    device: &str,
) -> FailureText {
    let (title, description) = match failure {
        PlaybackPipelineFailure::TunerBusy => (
            rust_i18n::t!("failure.busy_title", locale = locale),
            rust_i18n::t!("failure.busy_description", locale = locale, device = device),
        ),
        PlaybackPipelineFailure::ChannelMissing => (
            rust_i18n::t!("failure.channel_title", locale = locale),
            rust_i18n::t!(
                "failure.channel_description",
                locale = locale,
                channel = channel,
                device = device
            ),
        ),
        PlaybackPipelineFailure::HttpRejected => (
            rust_i18n::t!("failure.rejected_title", locale = locale),
            rust_i18n::t!(
                "failure.rejected_description",
                locale = locale,
                channel = channel,
                device = device
            ),
        ),
        PlaybackPipelineFailure::Offline => (
            rust_i18n::t!("failure.offline_title", locale = locale),
            rust_i18n::t!(
                "failure.offline_description",
                locale = locale,
                channel = channel,
                device = device
            ),
        ),
        PlaybackPipelineFailure::MissingCodecOrPlugin(media) => (
            rust_i18n::t!("failure.codec_title", locale = locale),
            missing_description(locale, media, channel, device),
        ),
        PlaybackPipelineFailure::Protected => (
            rust_i18n::t!("failure.protected_title", locale = locale),
            rust_i18n::t!(
                "failure.protected_description",
                locale = locale,
                channel = channel,
                device = device
            ),
        ),
        PlaybackPipelineFailure::Internal => (
            rust_i18n::t!("failure.internal_title", locale = locale),
            rust_i18n::t!(
                "failure.internal_description",
                locale = locale,
                channel = channel,
                device = device
            ),
        ),
    };
    FailureText { title, description }
}

/// Keep standardized codec names literal and translate the audio/video wording.
fn missing_description(
    locale: &str,
    media: MissingMedia,
    channel: &str,
    device: &str,
) -> Cow<'static, str> {
    let (codec, audio) = match media {
        MissingMedia::Mpeg2Video => ("MPEG-2", false),
        MissingMedia::H264Video => ("H.264", false),
        MissingMedia::HevcVideo => ("HEVC", false),
        MissingMedia::MpegAudio => ("MPEG", true),
        MissingMedia::AacAudio => ("AAC", true),
        MissingMedia::Ac3Audio => ("AC-3", true),
        MissingMedia::Eac3Audio => ("E-AC-3", true),
        MissingMedia::Ac4Audio => ("AC-4", true),
        MissingMedia::Unknown => {
            return rust_i18n::t!(
                "failure.missing_component_description",
                locale = locale,
                channel = channel,
                device = device
            );
        }
    };
    if audio {
        rust_i18n::t!(
            "failure.missing_audio_description",
            locale = locale,
            codec = codec,
            channel = channel,
            device = device
        )
    } else {
        rust_i18n::t!(
            "failure.missing_video_description",
            locale = locale,
            codec = codec,
            channel = channel,
            device = device
        )
    }
}

/// Resolve the fallback message for a channel that could not be started.
pub fn generic() -> FailureText {
    generic_in(&rust_i18n::locale())
}

/// Keep generic failure copy independent of native exception strings.
fn generic_in(locale: &str) -> FailureText {
    FailureText {
        title: rust_i18n::t!("failure.generic_title", locale = locale),
        description: rust_i18n::t!("failure.generic_description", locale = locale),
    }
}

/// Preserve the distinct close-before-retuning recovery instruction on teardown failure.
pub fn stop() -> FailureText {
    stop_in(&rust_i18n::locale())
}

/// Resolve teardown failure copy without changing the process-wide locale.
fn stop_in(locale: &str) -> FailureText {
    FailureText {
        title: rust_i18n::t!("failure.stop_title", locale = locale),
        description: rust_i18n::t!("failure.stop_description", locale = locale),
    }
}

/// Localized fallbacks used only when the accepted snapshot has no display name.
pub struct ContextLabels {
    pub selected_device: Cow<'static, str>,
    pub selected_channel: Cow<'static, str>,
}

impl ContextLabels {
    /// Resolve fallback names in the same startup locale as the surrounding copy.
    pub fn current() -> Self {
        Self::for_locale(&rust_i18n::locale())
    }

    /// Look up explicit-locale context without modifying global locale state.
    fn for_locale(locale: &str) -> Self {
        Self {
            selected_device: rust_i18n::t!("failure.selected_device", locale = locale),
            selected_channel: rust_i18n::t!("failure.selected_channel", locale = locale),
        }
    }
}

/// Name a channel whose guide number is known but whose lineup name is unavailable.
pub fn channel_number(number: &str) -> Cow<'static, str> {
    channel_number_in(&rust_i18n::locale(), number)
}

/// Interpolate an admitted guide number once as a literal value.
fn channel_number_in(locale: &str, number: &str) -> Cow<'static, str> {
    rust_i18n::t!("failure.channel_number", locale = locale, number = number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::localization::SUPPORTED_LOCALES;

    /// All categories retain a translated recovery message and literal display values.
    #[test]
    fn typed_categories_and_literal_context_work_in_every_catalog() {
        let channel = "7.1 News & <i>%{device}</i>";
        let device = "Tuner %{channel} <b>Room</b>";
        for locale in SUPPORTED_LOCALES {
            for failure in PlaybackPipelineFailure::ALL {
                let text = pipeline_in(locale, failure, channel, device);
                assert!(
                    !text.title.is_empty() && !text.description.is_empty(),
                    "{locale}"
                );
                assert!(text.description.contains(device), "{locale}");
                if failure != PlaybackPipelineFailure::TunerBusy {
                    assert!(text.description.contains(channel), "{locale}");
                }
                assert!(!text.title.contains("failure."), "{locale}");
            }
            for text in [generic_in(locale), stop_in(locale)] {
                assert!(
                    !text.title.is_empty() && !text.description.is_empty(),
                    "{locale}"
                );
                assert!(!text.description.contains("%{"), "{locale}");
            }
            let context = ContextLabels::for_locale(locale);
            assert!(!context.selected_device.is_empty() && !context.selected_channel.is_empty());
            assert!(channel_number_in(locale, "7.1").contains("7.1"));
        }
        assert_eq!(
            pipeline_in("de", PlaybackPipelineFailure::TunerBusy, "7.1", "Tuner").title,
            "Kein Tuner verfügbar"
        );
        assert_eq!(
            stop_in("fr").title,
            "Impossible d’arrêter la télévision en direct"
        );
        assert_eq!(channel_number_in("de", "7.1"), "Kanal 7.1");
        assert_eq!(generic_in("unsupported"), generic_in("en"));
    }

    /// Codec identifiers are preserved while their media kind is translated.
    #[test]
    fn every_known_codec_uses_the_correct_media_wording() {
        let cases = [
            (MissingMedia::Mpeg2Video, "MPEG-2", "video", "Videodecoder"),
            (MissingMedia::H264Video, "H.264", "video", "Videodecoder"),
            (MissingMedia::HevcVideo, "HEVC", "video", "Videodecoder"),
            (MissingMedia::MpegAudio, "MPEG", "audio", "Audiodecoder"),
            (MissingMedia::AacAudio, "AAC", "audio", "Audiodecoder"),
            (MissingMedia::Ac3Audio, "AC-3", "audio", "Audiodecoder"),
            (MissingMedia::Eac3Audio, "E-AC-3", "audio", "Audiodecoder"),
            (MissingMedia::Ac4Audio, "AC-4", "audio", "Audiodecoder"),
        ];
        assert_eq!(cases.len() + 1, MissingMedia::ALL.len());
        for (media, codec, english_kind, german_kind) in cases {
            for locale in SUPPORTED_LOCALES {
                let description = missing_description(locale, media, "News", "Tuner");
                assert!(description.contains(codec), "{locale}");
                assert!(description.contains("News") && description.contains("Tuner"));
                assert!(!description.contains("%{"), "{locale}");
            }
            assert!(
                missing_description("en", media, "News", "Tuner")
                    .contains(&format!("{codec} {english_kind} decoder"))
            );
            assert!(missing_description("de", media, "News", "Tuner").contains(german_kind));
        }
        let unknown = missing_description("en", MissingMedia::Unknown, "News", "Tuner");
        assert!(unknown.contains("codec or GStreamer plugin"));
        assert!(!unknown.contains("Unknown"));
    }
}
