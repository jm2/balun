//! The addresses and names Balun probes at launch, each with a Forget action.
//!
//! Every entry shown here is one the user typed, so showing it back stays
//! within ADR-0002; nothing else about a device appears.

use std::rc::Rc;

use adw::prelude::*;
use balun::settings::RememberedTarget;

use super::settings_session::SettingsSession;

/// Present the remembered targets. `on_forgotten` runs after an entry is
/// removed and its save is staged.
pub(crate) fn present(
    parent: &adw::ApplicationWindow,
    settings: Rc<SettingsSession>,
    on_forgotten: impl Fn() + 'static,
) {
    let dialog = adw::Dialog::builder()
        .title("Remembered devices")
        .content_width(440)
        .build();
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let group = adw::PreferencesGroup::builder()
        .description(
            "Balun probes these at every launch. A forgotten entry is not probed again; a device \
             already listed stays until the next launch.",
        )
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();

    let targets = settings.remembered_targets();
    if targets.is_empty() {
        group.add(
            &adw::ActionRow::builder()
                .title("No remembered devices")
                .build(),
        );
    }
    let on_forgotten = Rc::new(on_forgotten);
    for target in targets {
        let (title, subtitle) = describe(&target);
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .build();
        let forget = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Forget")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        forget.update_property(&[gtk::accessible::Property::Label("Forget this device")]);
        let settings = Rc::clone(&settings);
        let on_forgotten = Rc::clone(&on_forgotten);
        let group_for_forget = group.downgrade();
        let row_for_forget = row.downgrade();
        forget.connect_clicked(move |_| {
            if let Some(save) = settings.forget_target(&target) {
                settings.save(save);
            }
            if let (Some(group), Some(row)) = (group_for_forget.upgrade(), row_for_forget.upgrade())
            {
                group.remove(&row);
            }
            on_forgotten();
        });
        row.add_suffix(&forget);
        group.add(&row);
    }

    toolbar.set_content(Some(&group));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(parent));
}

/// The entry as it was typed, with the kind of entry it is.
fn describe(target: &RememberedTarget) -> (String, &'static str) {
    match target {
        RememberedTarget::Address(address) => (address.ip_addr().to_string(), "Address"),
        RememberedTarget::Hostname(host) => (host.name().to_owned(), "Hostname"),
    }
}

#[cfg(test)]
mod tests {
    use balun::discovery::DiscoveryEntry;

    use super::*;

    #[test]
    fn entries_are_described_as_typed() {
        let Ok(DiscoveryEntry::Address(address)) = DiscoveryEntry::parse("192.0.2.40") else {
            panic!("address entry");
        };
        assert_eq!(
            describe(&RememberedTarget::Address(address)),
            ("192.0.2.40".to_owned(), "Address")
        );
        let Ok(DiscoveryEntry::Hostname(host)) = DiscoveryEntry::parse("Tuner.Example") else {
            panic!("hostname entry");
        };
        assert_eq!(
            describe(&RememberedTarget::Hostname(host)),
            ("tuner.example".to_owned(), "Hostname")
        );
    }
}
