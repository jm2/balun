//! Typed-subnet entry and the confirmation shown before every search.
//!
//! The entry validates what the user types, previews the exact candidate
//! count and outbound request budget, and hands on only a parsed scope. The
//! confirmation shows that scope and budget as plain text, is bound to the
//! observation generation that was healthy when it opened, and yields at most
//! one consent. A network change closes it and invalidates any response still
//! queued, so it can never authorize a search in a later generation.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use balun::discovery::{
    ObservationGeneration, ObservationState, SubnetSearchConsent, TypedSubnetScope,
};
use balun::localization::subnet_search::{
    SubnetConfirmation, SubnetEntryLabels, preview, validation,
};

const CANCEL_RESPONSE: &str = "cancel";
const CONTINUE_RESPONSE: &str = "continue";
const FORGET_RESPONSE: &str = "forget";
const SEARCH_RESPONSE: &str = "search";
/// Room for surrounding spaces a paste may carry beyond the canonical text.
const ENTRY_MAX_CHARS: usize = TypedSubnetScope::MAX_TEXT_BYTES + 8;

/// What the entry's current text admits, and what to show under it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct EntryAdmission {
    scope: Option<TypedSubnetScope>,
    message: Option<Cow<'static, str>>,
    preview: Option<Cow<'static, str>>,
}

/// Parse the entry, ignoring only surrounding spaces a paste may bring.
fn entry_admission(text: &str) -> EntryAdmission {
    let text = text.trim_matches(|character: char| character.is_ascii_whitespace());
    if text.is_empty() {
        return EntryAdmission {
            scope: None,
            message: None,
            preview: None,
        };
    }
    match text.parse::<TypedSubnetScope>() {
        Ok(scope) => EntryAdmission {
            scope: Some(scope),
            message: None,
            preview: Some(preview(scope)),
        },
        Err(error) => EntryAdmission {
            scope: None,
            message: Some(validation(error)),
            preview: None,
        },
    }
}

/// Keep the parsed scope beside the entry. libadwaita closes a dialog before
/// it emits the button's response, and the close handler clears the entry,
/// so the response must never re-read the widget text.
fn record_scope(
    closing: bool,
    admission: &EntryAdmission,
    admitted: &Cell<Option<TypedSubnetScope>>,
) {
    if !closing {
        admitted.set(admission.scope);
    }
}

/// What the entry dialog's response asks for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryResponse {
    Continue(TypedSubnetScope),
    Forget,
    Dismiss,
}

/// Consume the stored scope; only Continue uses it, and any response clears it.
fn entry_response(response: &str, admitted: &Cell<Option<TypedSubnetScope>>) -> EntryResponse {
    let scope = admitted.take();
    match (response, scope) {
        (CONTINUE_RESPONSE, Some(scope)) => EntryResponse::Continue(scope),
        (FORGET_RESPONSE, _) => EntryResponse::Forget,
        _ => EntryResponse::Dismiss,
    }
}

/// Present the entry, prefilled with the remembered subnet as editable text.
/// Forget is offered only while a subnet is remembered.
pub(crate) fn present_entry(
    parent: &adw::ApplicationWindow,
    remembered: Option<TypedSubnetScope>,
    on_continue: impl Fn(TypedSubnetScope) + 'static,
    on_forget: impl Fn() + 'static,
    on_closed: impl Fn() + 'static,
) {
    let built = build_entry(remembered.is_some(), on_continue, on_forget, on_closed);
    built.dialog.set_focus(Some(&built.entry));
    built.dialog.present(Some(parent));
    built.prefill(remembered);
}

/// The entry dialog and the widgets its handlers update.
struct EntryDialog {
    dialog: adw::AlertDialog,
    entry: adw::EntryRow,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "display tests inspect the copy")
    )]
    message: gtk::Label,
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "display tests inspect the copy")
    )]
    summary: gtk::Label,
}

impl EntryDialog {
    /// Offer the remembered subnet as editable text. Presenting the dialog
    /// resets its labels, so this runs once it is shown.
    fn prefill(&self, remembered: Option<TypedSubnetScope>) {
        if let Some(remembered) = remembered {
            self.entry.set_text(&remembered.to_string());
        }
    }
}

fn build_entry(
    offer_forget: bool,
    on_continue: impl Fn(TypedSubnetScope) + 'static,
    on_forget: impl Fn() + 'static,
    on_closed: impl Fn() + 'static,
) -> EntryDialog {
    let labels = SubnetEntryLabels::current();
    let dialog = adw::AlertDialog::builder()
        .heading(&*labels.title)
        .heading_use_markup(false)
        .body(&*labels.description)
        .body_use_markup(false)
        .close_response(CANCEL_RESPONSE)
        .default_response(CONTINUE_RESPONSE)
        .build();
    dialog.add_response(CANCEL_RESPONSE, &labels.cancel);
    if offer_forget {
        dialog.add_response(FORGET_RESPONSE, &labels.forget);
        dialog.set_response_appearance(FORGET_RESPONSE, adw::ResponseAppearance::Destructive);
    }
    dialog.add_response(CONTINUE_RESPONSE, &labels.proceed);
    dialog.set_response_appearance(CONTINUE_RESPONSE, adw::ResponseAppearance::Suggested);
    dialog.set_response_enabled(CONTINUE_RESPONSE, false);

    let entry = adw::EntryRow::builder()
        .title(&*labels.entry)
        .activates_default(true)
        .input_hints(gtk::InputHints::NO_EMOJI | gtk::InputHints::NO_SPELLCHECK)
        .max_length(i32::try_from(ENTRY_MAX_CHARS).unwrap_or(i32::MAX))
        .build();
    let group = adw::PreferencesGroup::new();
    group.add(&entry);
    let label = || {
        gtk::Label::builder()
            .halign(gtk::Align::Start)
            .justify(gtk::Justification::Left)
            .max_width_chars(48)
            .visible(false)
            .wrap(true)
            .xalign(0.0)
            .use_markup(false)
            .build()
    };
    let message = label();
    message.add_css_class("error");
    let summary = label();
    summary.add_css_class("dim-label");
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_top(8)
        .build();
    content.append(&group);
    content.append(&message);
    content.append(&summary);
    dialog.set_extra_child(Some(&content));

    let admitted = Rc::new(Cell::new(None));
    let closing = Rc::new(Cell::new(false));
    // Weak captures: the dialog owns these widgets through its extra child.
    let dialog_for_entry = dialog.downgrade();
    let message_for_entry = message.downgrade();
    let summary_for_entry = summary.downgrade();
    let admitted_for_entry = Rc::clone(&admitted);
    let closing_for_entry = Rc::clone(&closing);
    entry.connect_changed(move |entry| {
        let (Some(dialog), Some(message), Some(summary)) = (
            dialog_for_entry.upgrade(),
            message_for_entry.upgrade(),
            summary_for_entry.upgrade(),
        ) else {
            return;
        };
        let admission = entry_admission(entry.text().as_str());
        record_scope(closing_for_entry.get(), &admission, &admitted_for_entry);
        dialog.set_response_enabled(CONTINUE_RESPONSE, admission.scope.is_some());
        show(&message, admission.message.as_deref());
        show(&summary, admission.preview.as_deref());
        if admission.message.is_some() {
            entry.add_css_class("error");
        } else {
            entry.remove_css_class("error");
        }
    });
    let admitted_for_response = Rc::clone(&admitted);
    dialog.connect_response(None, move |_, response| {
        match entry_response(response, &admitted_for_response) {
            EntryResponse::Continue(scope) => on_continue(scope),
            EntryResponse::Forget => on_forget(),
            EntryResponse::Dismiss => {}
        }
    });
    let entry_for_close = entry.downgrade();
    dialog.connect_closed(move |_| {
        closing.set(true);
        if let Some(entry) = entry_for_close.upgrade() {
            entry.set_text("");
        }
        on_closed();
    });

    EntryDialog {
        dialog,
        entry,
        message,
        summary,
    }
}

fn show(label: &gtk::Label, text: Option<&str>) {
    label.set_label(text.unwrap_or_default());
    label.set_visible(text.is_some());
}

/// One open confirmation: the generation it was shown under, whether it can
/// still authorize, and the dialog to close when it cannot.
pub(crate) struct PendingConfirmation {
    scope: TypedSubnetScope,
    generation: ObservationGeneration,
    valid: Cell<bool>,
    dialog: RefCell<Option<gtk::glib::WeakRef<adw::AlertDialog>>>,
}

impl PendingConfirmation {
    const fn new(scope: TypedSubnetScope, generation: ObservationGeneration) -> Self {
        Self {
            scope,
            generation,
            valid: Cell::new(true),
            dialog: RefCell::new(None),
        }
    }

    /// Whether the confirmation still matches `state`. Any other state, even
    /// a later healthy generation, invalidates it for good.
    pub(crate) fn observe(&self, state: ObservationState) -> bool {
        if !state.is_ready_for(self.generation) {
            self.valid.set(false);
        }
        self.valid.get()
    }

    /// The one consent a Search response yields while still valid.
    fn consent(&self, response: &str) -> Option<SubnetSearchConsent> {
        if response != SEARCH_RESPONSE || !self.valid.replace(false) {
            self.valid.set(false);
            return None;
        }
        Some(SubnetSearchConsent::confirm(self.scope, self.generation))
    }

    /// Close the dialog without a response; nothing it shows can authorize.
    pub(crate) fn close(&self) {
        self.valid.set(false);
        // Closing runs `closed` handlers synchronously; hold no borrow.
        let dialog = self
            .dialog
            .borrow()
            .as_ref()
            .and_then(gtk::glib::WeakRef::upgrade);
        if let Some(dialog) = dialog {
            dialog.force_close();
        }
    }
}

/// Present the confirmation for exactly `scope` and its budget, bound to
/// `generation`. Cancel is the default and closing declines.
pub(crate) fn present_confirmation(
    parent: &adw::ApplicationWindow,
    scope: TypedSubnetScope,
    generation: ObservationGeneration,
    on_search: impl Fn(SubnetSearchConsent) + 'static,
    on_closed: impl Fn() + 'static,
) -> Rc<PendingConfirmation> {
    let (dialog, pending) = build_confirmation(scope, generation, on_search, on_closed);
    dialog.present(Some(parent));
    pending
}

fn build_confirmation(
    scope: TypedSubnetScope,
    generation: ObservationGeneration,
    on_search: impl Fn(SubnetSearchConsent) + 'static,
    on_closed: impl Fn() + 'static,
) -> (adw::AlertDialog, Rc<PendingConfirmation>) {
    let confirmation = SubnetConfirmation::current(scope);
    let pending = Rc::new(PendingConfirmation::new(scope, generation));
    let dialog = adw::AlertDialog::builder()
        .heading(&*confirmation.heading)
        .heading_use_markup(false)
        .body(&*confirmation.body)
        .body_use_markup(false)
        .close_response(CANCEL_RESPONSE)
        .default_response(CANCEL_RESPONSE)
        .build();
    dialog.add_response(CANCEL_RESPONSE, &confirmation.cancel);
    dialog.add_response(SEARCH_RESPONSE, &confirmation.search);
    dialog.set_response_appearance(SEARCH_RESPONSE, adw::ResponseAppearance::Suggested);
    *pending.dialog.borrow_mut() = Some(dialog.downgrade());

    // libadwaita emits `closed` before `response`, and the window drops its
    // reference when the dialog closes, so the response keeps its own. The
    // pending value holds the dialog only weakly, so this forms no cycle.
    let responding = Rc::clone(&pending);
    dialog.connect_response(None, move |_, response| {
        if let Some(consent) = responding.consent(response) {
            on_search(consent);
        }
    });
    dialog.connect_closed(move |_| on_closed());
    (dialog, pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(value: u64) -> ObservationGeneration {
        ObservationGeneration::new(value).unwrap()
    }

    fn pending(text: &str, shown: u64) -> PendingConfirmation {
        PendingConfirmation::new(text.parse().unwrap(), generation(shown))
    }

    #[test]
    fn the_entry_admits_only_parsed_subnets_and_previews_their_budget() {
        let valid = entry_admission("  192.168.2.0/23 ");
        assert_eq!(valid.scope, "192.168.2.0/23".parse().ok());
        assert_eq!(valid.message, None);
        let preview = valid.preview.expect("a valid subnet is previewed");
        assert!(preview.contains("510") && preview.contains("1020"));

        assert_eq!(
            entry_admission(""),
            EntryAdmission {
                scope: None,
                message: None,
                preview: None
            }
        );
        for rejected in [
            "192.168.2.1/23",
            "10.0.0.0/22",
            "203.0.113.0/24",
            "127.0.0.0/24",
            "tuner.example",
            "fd00::/120",
        ] {
            let admission = entry_admission(rejected);
            assert_eq!(admission.scope, None, "{rejected}");
            assert_eq!(admission.preview, None);
            let message = admission.message.expect("a rejection is explained");
            assert!(!message.contains(rejected), "{rejected}");
        }
    }

    #[test]
    fn the_admitted_subnet_survives_the_close_before_its_response() {
        let admitted = Cell::new(None);
        record_scope(false, &entry_admission("10.0.0.0/24"), &admitted);
        // Closing clears the entry, which reports "" before the response.
        record_scope(true, &entry_admission(""), &admitted);
        assert_eq!(
            entry_response(CONTINUE_RESPONSE, &admitted),
            EntryResponse::Continue("10.0.0.0/24".parse().unwrap())
        );
        assert_eq!(
            entry_response(CONTINUE_RESPONSE, &admitted),
            EntryResponse::Dismiss,
            "consumed once"
        );
        record_scope(false, &entry_admission("10.0.0.0/24"), &admitted);
        record_scope(false, &entry_admission("10.0.0.1/24"), &admitted);
        assert_eq!(
            entry_response(CONTINUE_RESPONSE, &admitted),
            EntryResponse::Dismiss
        );
        record_scope(false, &entry_admission("10.0.0.0/24"), &admitted);
        assert_eq!(
            entry_response(FORGET_RESPONSE, &admitted),
            EntryResponse::Forget
        );
        assert_eq!(admitted.get(), None, "forgetting drops the entry");
        record_scope(false, &entry_admission("10.0.0.0/24"), &admitted);
        assert_eq!(
            entry_response(CANCEL_RESPONSE, &admitted),
            EntryResponse::Dismiss
        );
        assert_eq!(admitted.get(), None);
    }

    #[test]
    fn a_confirmation_yields_one_consent_for_its_scope_budget_and_generation() {
        let confirmation = pending("192.168.2.0/23", 3);
        assert!(confirmation.observe(ObservationState::Ready(generation(3))));
        let consent = confirmation
            .consent(SEARCH_RESPONSE)
            .expect("a valid Search response consents");
        assert_eq!(consent.scope(), "192.168.2.0/23".parse().unwrap());
        assert_eq!(consent.generation(), generation(3));
        assert!(
            confirmation.consent(SEARCH_RESPONSE).is_none(),
            "one consent"
        );

        let declined = pending("192.168.2.0/23", 3);
        assert!(declined.consent(CANCEL_RESPONSE).is_none());
        assert!(
            declined.consent(SEARCH_RESPONSE).is_none(),
            "declining ends it"
        );
    }

    #[test]
    fn a_network_change_invalidates_an_open_or_queued_confirmation_for_good() {
        for states in [
            vec![ObservationState::Unavailable],
            vec![ObservationState::Ready(generation(4))],
            // The network changes and the same topology returns.
            vec![
                ObservationState::Unavailable,
                ObservationState::Ready(generation(4)),
            ],
        ] {
            let confirmation = pending("10.0.0.0/24", 3);
            for state in states {
                assert!(!confirmation.observe(state));
            }
            assert!(!confirmation.observe(ObservationState::Ready(generation(3))));
            // A Search response already queued behind the change is refused.
            assert!(confirmation.consent(SEARCH_RESPONSE).is_none());
        }
        let closed = pending("10.0.0.0/24", 3);
        closed.close();
        assert!(closed.consent(SEARCH_RESPONSE).is_none());
    }

    #[test]
    fn the_entry_bound_leaves_room_for_the_longest_canonical_text() {
        assert!(ENTRY_MAX_CHARS > "192.168.255.255/32".len());
    }

    /// Preview, validation, plain-text copy, safe defaults, one consent, and
    /// invalidation on a real display, driven through the dialogs' own
    /// responses and the entry that activates the default one.
    #[test]
    #[ignore = "requires the isolated display supplied by scripts/test-desktop-lifecycle.sh"]
    fn subnet_dialogs_preview_confirm_once_and_close_when_the_network_changes() {
        adw::init().expect("initialize libadwaita for the subnet dialog test");
        let window = adw::ApplicationWindow::builder()
            .default_width(640)
            .default_height(480)
            .build();
        window.present();
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        let continued = Rc::new(Cell::new(None));
        let forgotten = Rc::new(Cell::new(0));
        let closed = Rc::new(Cell::new(0));
        let remembered: TypedSubnetScope = "10.0.0.0/24".parse().unwrap();
        let built = {
            let (continued, forgotten, closed) = (
                Rc::clone(&continued),
                Rc::clone(&forgotten),
                Rc::clone(&closed),
            );
            build_entry(
                true,
                move |scope| continued.set(Some(scope)),
                move || forgotten.set(forgotten.get() + 1),
                move || closed.set(closed.get() + 1),
            )
        };
        built.dialog.present(Some(&window));
        built.prefill(Some(remembered));
        assert_eq!(
            built.dialog.default_response().as_deref(),
            Some(CONTINUE_RESPONSE)
        );
        assert!(!built.dialog.is_heading_use_markup() && !built.dialog.is_body_use_markup());
        assert!(
            built.entry.activates_default(),
            "Enter continues from the entry"
        );
        assert_eq!(
            built.entry.text(),
            "10.0.0.0/24",
            "the remembered text is editable"
        );
        assert!(built.dialog.has_response(FORGET_RESPONSE));
        assert!(built.dialog.is_response_enabled(CONTINUE_RESPONSE));
        assert!(built.summary.is_visible() && built.summary.label().contains("508"));

        built.entry.set_text("10.0.0.1/24");
        assert!(!built.dialog.is_response_enabled(CONTINUE_RESPONSE));
        assert!(built.message.is_visible() && !built.summary.is_visible());
        assert!(built.entry.has_css_class("error"));
        assert!(!built.message.label().contains("10.0.0.1"));
        assert!(!built.message.uses_markup());

        built.entry.set_text("192.168.2.0/23");
        assert!(built.dialog.is_response_enabled(CONTINUE_RESPONSE));
        assert!(!built.entry.has_css_class("error"));
        assert!(built.summary.label().contains("510") && built.summary.label().contains("1020"));
        built
            .dialog
            .emit_by_name::<()>("response", &[&CONTINUE_RESPONSE]);
        assert_eq!(continued.get(), "192.168.2.0/23".parse().ok());
        built
            .dialog
            .emit_by_name::<()>("response", &[&FORGET_RESPONSE]);
        assert_eq!(forgotten.get(), 1);
        built.dialog.force_close();
        assert_eq!(closed.get(), 1);
        assert_eq!(built.entry.text(), "", "closing clears the entry");

        // The confirmation: plain text, Cancel by default, one consent.
        let scope: TypedSubnetScope = "192.168.2.0/23".parse().unwrap();
        let consents = Rc::new(Cell::new(0));
        let confirmation_closed = Rc::new(Cell::new(false));
        let (dialog, pending) = {
            let (consents, confirmation_closed) =
                (Rc::clone(&consents), Rc::clone(&confirmation_closed));
            build_confirmation(
                scope,
                generation(5),
                move |consent| {
                    assert_eq!(consent.scope(), scope);
                    consents.set(consents.get() + 1);
                },
                move || confirmation_closed.set(true),
            )
        };
        dialog.present(Some(&window));
        assert_eq!(dialog.default_response().as_deref(), Some(CANCEL_RESPONSE));
        assert_eq!(dialog.close_response(), CANCEL_RESPONSE);
        assert!(!dialog.is_heading_use_markup() && !dialog.is_body_use_markup());
        assert!(
            dialog
                .heading()
                .is_some_and(|heading| heading.contains("192.168.2.0/23"))
        );
        let body = dialog.body();
        assert!(body.contains("510") && body.contains("1020") && body.contains("64"));
        dialog.emit_by_name::<()>("response", &[&SEARCH_RESPONSE]);
        dialog.emit_by_name::<()>("response", &[&SEARCH_RESPONSE]);
        assert_eq!(consents.get(), 1, "one confirmation, one search");

        // A network change closes an open confirmation; a Search response
        // still queued behind it authorizes nothing.
        let (stale, pending_stale) = build_confirmation(
            scope,
            generation(5),
            {
                let consents = Rc::clone(&consents);
                move |_| consents.set(consents.get() + 1)
            },
            || {},
        );
        stale.present(Some(&window));
        assert!(!pending_stale.observe(ObservationState::Ready(generation(6))));
        pending_stale.close();
        stale.emit_by_name::<()>("response", &[&SEARCH_RESPONSE]);
        assert_eq!(consents.get(), 1);
        drop(pending);
        dialog.force_close();
        assert!(confirmation_closed.get());
    }
}
