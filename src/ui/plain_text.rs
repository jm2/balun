//! Plain text at libadwaita sinks that always parse Pango markup.
//!
//! `AdwStatusPage` descriptions have no markup switch. Compose ordinary text
//! first, including device/channel names and translated copy, then escape once
//! at this rendering boundary. Titles and `GtkLabel::set_label` stay plain text;
//! banners, toasts and dialogs turn markup off instead.

use gtk::glib::GString;

/// Escape ordinary text for a markup-only description.
pub(crate) fn description_markup(text: &str) -> GString {
    gtk::glib::markup_escape_text(text)
}

/// Show ordinary text as a status page's description.
pub(crate) fn set_status_description(status: &adw::StatusPage, text: &str) {
    status.set_description(Some(&description_markup(text)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_metacharacters_render_literally() {
        assert_eq!(
            description_markup("Den & <b>Kitchen</b> 'A' \"B\"").as_str(),
            "Den &amp; &lt;b&gt;Kitchen&lt;/b&gt; &apos;A&apos; &quot;B&quot;"
        );
        assert_eq!(description_markup("plain").as_str(), "plain");
    }
}
