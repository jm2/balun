//! Bounded, topology-redacting address-or-hostname discovery admission dialog.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use balun::discovery::{
    DiscoveryEntry, InvalidDiscoveryEntry, InvalidExactDiscoveryTarget, InvalidHostnameTarget,
    MAX_HOSTNAME_BYTES,
};

const CANCEL_RESPONSE: &str = "cancel";
const FIND_RESPONSE: &str = "find";

#[derive(Clone)]
struct Admission {
    entry: Option<DiscoveryEntry>,
    validation_message: Option<&'static str>,
}

/// Present one address-or-hostname admission dialog.
///
/// `on_admit` receives only the validated, canonical entry. Raw entry text
/// never crosses this module boundary and is never rendered in validation
/// copy, status text, or logs. `on_closed` runs for every response or dismiss.
pub(crate) fn present(
    parent: &adw::ApplicationWindow,
    on_admit: impl Fn(DiscoveryEntry) + 'static,
    on_closed: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading("Find device by address")
        .body(
            "Send one bounded HDHomeRun discovery request to a known IP address or hostname; Balun does not scan a range. Example: 192.168.1.20, fd00::20, or tuner.example.",
        )
        .close_response(CANCEL_RESPONSE)
        .default_response(FIND_RESPONSE)
        .build();
    dialog.add_response(CANCEL_RESPONSE, "Cancel");
    dialog.add_response(FIND_RESPONSE, "Find");
    dialog.set_response_appearance(FIND_RESPONSE, adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled(FIND_RESPONSE, false);

    let maximum_length =
        i32::try_from(MAX_HOSTNAME_BYTES).expect("hostname text bound must fit a GTK entry length");
    let entry = adw::EntryRow::builder()
        .title("IP address or hostname")
        .activates_default(true)
        .input_hints(gtk::InputHints::NO_EMOJI | gtk::InputHints::NO_SPELLCHECK)
        .max_length(maximum_length)
        .build();
    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let validation = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .justify(gtk::Justification::Left)
        .max_width_chars(48)
        .visible(false)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["error"])
        .build();
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .build();
    content.append(&group);
    content.append(&validation);
    dialog.set_extra_child(Some(&content));

    // The dialog owns the entry through its extra child. A strong dialog
    // capture here would complete a dialog -> entry -> handler -> dialog
    // cycle and retain the raw address after the dialog closes.
    // libadwaita closes an alert dialog before it emits the button's
    // `response` (`emit_response` calls `adw_dialog_close`, whose `closed`
    // signal runs synchronously), so the `closed` handler below has already
    // wiped the entry by the time the response handler runs. The validated
    // parse of the current text is therefore kept beside the entry on every
    // change and consumed at the response boundary; the widget text is never
    // re-read there. Only a parser-approved value is ever stored, and any
    // response, including cancel, drops it.
    let admitted: Rc<RefCell<Option<DiscoveryEntry>>> = Rc::new(RefCell::new(None));
    // Set once the dialog is closing: the close-time `set_text("")` below
    // emits `changed` synchronously and must not wipe the stored admission
    // before the response handler consumes it.
    let closing = Rc::new(Cell::new(false));

    let dialog_for_validation = dialog.downgrade();
    let validation_for_entry = validation.downgrade();
    let admitted_for_entry = Rc::clone(&admitted);
    let closing_for_entry = Rc::clone(&closing);
    entry.connect_changed(move |entry| {
        let Some(dialog) = upgrade_signal_target(&dialog_for_validation) else {
            return;
        };
        let Some(validation) = upgrade_signal_target(&validation_for_entry) else {
            return;
        };
        let admission = admission(entry.text().as_str());
        record_admission(closing_for_entry.get(), &admission, &admitted_for_entry);
        apply_admission(&dialog, entry, &validation, admission);
    });

    // Dialog-owned signal handlers keep only weak child references. The
    // normal widget hierarchy is the sole owner of address-bearing entry
    // state and can therefore release it as soon as the dialog is closed.
    let admitted_for_response = Rc::clone(&admitted);
    dialog.connect_response(None, move |_, response| {
        if let Some(entry) = take_admitted(response, &admitted_for_response) {
            on_admit(entry);
        }
    });
    let entry_for_close = entry.downgrade();
    dialog.connect_closed(move |_| {
        closing.set(true);
        if let Some(entry) = upgrade_signal_target(&entry_for_close) {
            entry.set_text("");
        }
        on_closed();
    });

    dialog.set_focus(Some(&entry));
    dialog.present(Some(parent));
}

/// Store the parser-approved value of the entry's current text after a user
/// edit. The close-time clear also emits `changed`, but it must leave the
/// value the pending response handler is about to consume in place.
fn record_admission(
    closing: bool,
    admission: &Admission,
    admitted: &RefCell<Option<DiscoveryEntry>>,
) {
    if !closing {
        admitted.replace(admission.entry.clone());
    }
}

/// Consume the stored admission for `response`: only the Find response admits
/// it, and every response, including cancel or close, clears it.
fn take_admitted(
    response: &str,
    admitted: &RefCell<Option<DiscoveryEntry>>,
) -> Option<DiscoveryEntry> {
    let entry = admitted.borrow_mut().take();
    (response == FIND_RESPONSE).then_some(entry).flatten()
}

fn upgrade_signal_target<T>(target: &gtk::glib::WeakRef<T>) -> Option<T>
where
    T: gtk::glib::object::ObjectType,
{
    target.upgrade()
}

fn apply_admission(
    dialog: &adw::AlertDialog,
    entry: &adw::EntryRow,
    validation: &gtk::Label,
    admission: Admission,
) {
    dialog.set_response_enabled(FIND_RESPONSE, admission.entry.is_some());
    if let Some(message) = admission.validation_message {
        validation.set_label(message);
        validation.set_visible(true);
        entry.add_css_class("error");
    } else {
        validation.set_label("");
        validation.set_visible(false);
        entry.remove_css_class("error");
    }
}

fn admission(value: &str) -> Admission {
    match DiscoveryEntry::parse(value) {
        Ok(entry) => Admission {
            entry: Some(entry),
            validation_message: None,
        },
        Err(InvalidDiscoveryEntry::Address(InvalidExactDiscoveryTarget::Empty))
        | Err(InvalidDiscoveryEntry::Hostname(InvalidHostnameTarget::Empty)) => Admission {
            entry: None,
            validation_message: None,
        },
        Err(InvalidDiscoveryEntry::Address(error)) => Admission {
            entry: None,
            validation_message: Some(validation_message(error)),
        },
        Err(InvalidDiscoveryEntry::Hostname(error)) => Admission {
            entry: None,
            validation_message: Some(hostname_validation_message(error)),
        },
    }
}

fn hostname_validation_message(error: InvalidHostnameTarget) -> &'static str {
    match error {
        InvalidHostnameTarget::Empty => "Enter a device IP address or hostname.",
        InvalidHostnameTarget::TooLong { .. } | InvalidHostnameTarget::ControlCharacter => {
            "Enter one hostname of letters, digits, hyphens, and dots."
        }
        InvalidHostnameTarget::InvalidSyntax => {
            "Enter a hostname without a URL, port, path, or range."
        }
        InvalidHostnameTarget::IpAddressLiteral => "Enter a usable unicast device address.",
    }
}

fn validation_message(error: InvalidExactDiscoveryTarget) -> &'static str {
    match error {
        InvalidExactDiscoveryTarget::Empty => "Enter a device IP address or hostname.",
        InvalidExactDiscoveryTarget::TooLong { .. }
        | InvalidExactDiscoveryTarget::ControlCharacter => {
            "Enter one usable numeric IPv4 or IPv6 address."
        }
        InvalidExactDiscoveryTarget::InvalidSyntax => {
            "Enter an IP address or hostname without a URL, port, or range."
        }
        InvalidExactDiscoveryTarget::UnicastRequired => "Enter a usable unicast device address.",
        InvalidExactDiscoveryTarget::Ipv4MappedIpv6Unsupported => {
            "Enter the IPv4 address directly."
        }
        InvalidExactDiscoveryTarget::LinkLocalIpv6ScopeRequired => {
            "Link-local IPv6 is not supported yet; use IPv4 or unscoped IPv6."
        }
        InvalidExactDiscoveryTarget::ScopedIpv6Unsupported => {
            "Scoped IPv6 is not supported yet; use IPv4 or unscoped IPv6."
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn weak_signal_capture_does_not_retain_its_owner() {
        // GtkWidget construction requires a display, but SimpleAction is a
        // display-free GObject. This exercises the same GLib WeakRef helper
        // used by the entry/dialog signal handlers and models the ownership
        // edge that previously formed the cycle.
        let owner = gtk::gio::SimpleAction::new("owner", None);
        let owner_lifetime = owner.downgrade();
        let owner_for_signal = owner.downgrade();
        let child = gtk::gio::SimpleAction::new("child", None);
        let upgraded_after_release = Rc::new(Cell::new(false));
        let signal_observation = Rc::clone(&upgraded_after_release);
        child.connect_activate(move |_, _| {
            signal_observation.set(upgrade_signal_target(&owner_for_signal).is_some());
        });

        drop(owner);
        assert!(upgrade_signal_target(&owner_lifetime).is_none());
        child.activate(None);
        assert!(!upgraded_after_release.get());
    }

    #[test]
    fn admission_survives_the_dialog_closing_before_its_response() {
        // libadwaita order for a button: `closed` (the entry is cleared) and
        // only then `response`, so the response boundary must not depend on
        // the widget text.
        let admitted = RefCell::new(None);
        record_admission(false, &admission("192.0.2.40"), &admitted);
        // Closing clears the entry, which re-enters `changed` with "" before
        // the response handler runs; that edit must not wipe the admission.
        record_admission(true, &admission(""), &admitted);
        assert!(
            admission("").entry.is_none(),
            "the cleared entry admits nothing"
        );
        assert_eq!(
            take_admitted(FIND_RESPONSE, &admitted),
            DiscoveryEntry::parse("192.0.2.40").ok()
        );
        // A user edit that empties the entry does drop the admission.
        record_admission(false, &admission("192.0.2.40"), &admitted);
        record_admission(false, &admission(""), &admitted);
        assert_eq!(take_admitted(FIND_RESPONSE, &admitted), None);
        assert_eq!(
            take_admitted(FIND_RESPONSE, &admitted),
            None,
            "an admission is consumed once"
        );

        admitted.replace(admission("tuner.example").entry);
        assert_eq!(
            take_admitted(CANCEL_RESPONSE, &admitted),
            None,
            "cancel admits nothing"
        );
        assert!(
            admitted.borrow().is_none(),
            "cancel also drops the stored entry"
        );
    }

    #[test]
    fn only_parser_approved_entries_cross_the_dialog_boundary() {
        let valid = admission("  192.0.2.40  ");
        assert_eq!(valid.entry, DiscoveryEntry::parse("192.0.2.40").ok());
        assert_eq!(valid.validation_message, None);
        let host = admission("Tuner.Example");
        assert_eq!(host.entry, DiscoveryEntry::parse("tuner.example").ok());
        assert_eq!(host.validation_message, None);

        for value in [
            "",
            "http://192.0.2.40/",
            "192.0.2.40:65001",
            "192.0.2.0/24",
            "127.0.0.1",
            "fe80::40%12",
            "tuner.example:5004",
            "tuner_example",
        ] {
            assert!(
                admission(value).entry.is_none(),
                "rejected text crossed dialog admission: {value:?}"
            );
        }
    }

    #[test]
    fn validation_copy_never_echoes_rejected_entry_text() {
        for value in [
            "private-tuner-name_example",
            "http://198.51.100.247/private-token",
            "198.51.100.247:65001",
        ] {
            let state = admission(value);
            let message = state
                .validation_message
                .expect("nonempty rejected input has fixed validation copy");
            assert!(!message.contains(value));
            assert!(!message.contains("198.51.100.247"));
            assert!(!message.contains("private-token"));
        }
    }

    #[test]
    fn entry_character_bound_is_derived_from_the_parser_byte_bound() {
        let gtk_bound = i32::try_from(MAX_HOSTNAME_BYTES).unwrap();
        assert_eq!(gtk_bound, 253);
    }
}
