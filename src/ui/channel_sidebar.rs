//! Virtualized channel sidebar for exactly one selected HDHomeRun device.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, LineupFailure, OperationGeneration, SelectedLineupState,
    SelectedLineupStatus, StreamSelection,
};
use balun::domain::ChannelKey;

use super::objects::ChannelRowObject;

const STATUS_PAGE_NAME: &str = "status";
const CHANNEL_LIST_PAGE_NAME: &str = "channels";

/// GTK parts for the selected-device channel pane.
#[derive(Clone)]
pub(crate) struct ChannelSidebar {
    root: adw::ToolbarView,
    store: gtk::gio::ListStore,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    applying_snapshot: Rc<Cell<bool>>,
    activation_generation: Rc<Cell<Option<OperationGeneration>>>,
}

impl ChannelSidebar {
    #[must_use]
    pub(crate) fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// Connect one URL-free activation intent for a row belonging to the
    /// exact complete lineup generation currently applied to this sidebar.
    ///
    /// Selection alone remains inert. GTK emits this signal for the standard
    /// double-click and keyboard activation paths because single-click
    /// activation stays disabled on the list.
    pub(crate) fn connect_channel_activated<F>(&self, activate: F) -> gtk::glib::SignalHandlerId
    where
        F: Fn(StreamSelection) + 'static,
    {
        let activation_generation = Rc::clone(&self.activation_generation);
        let applying_snapshot = Rc::clone(&self.applying_snapshot);
        self.list.connect_activate(move |list, position| {
            if applying_snapshot.get() {
                return;
            }
            let row = list
                .model()
                .and_then(|model| model.item(position))
                .and_then(|item| item.downcast::<ChannelRowObject>().ok());
            let Some(selection) = row
                .as_ref()
                .and_then(|row| activation_selection(row, activation_generation.get()))
            else {
                return;
            };
            activate(selection);
        })
    }

    /// Atomically replace the channel model with the lineup belonging to the
    /// snapshot's selected DeviceID.
    ///
    /// Any inert local highlight is restored by its complete ChannelKey, never
    /// by a list position. Protected rows remain visible but unselectable.
    pub(crate) fn apply_snapshot(&self, snapshot: &ApplicationSnapshot) {
        let prior_selection = self.selected_channel_key();
        let lineup = snapshot.selected_lineup();
        let channels = if lineup.status() == SelectedLineupStatus::Ready {
            lineup.channels()
        } else {
            &[]
        };
        let selected_position = prior_selection.as_ref().and_then(|selected| {
            channels
                .iter()
                .position(|channel| channel.key() == selected && !channel.is_drm())
                .and_then(|position| u32::try_from(position).ok())
        });
        let rows = channels
            .iter()
            .map(ChannelRowObject::from_summary)
            .collect::<Vec<_>>();
        let activation_generation =
            (lineup.status() == SelectedLineupStatus::Ready).then_some(lineup.generation());

        let _applying = SnapshotApplicationGuard::enter(Rc::clone(&self.applying_snapshot));
        // Revoke the old row authority before replacing any model item. If a
        // nested GTK callback attempts activation during replacement, both
        // the application guard and the absent generation fail closed.
        self.activation_generation.set(None);
        self.store.splice(0, self.store.n_items(), &rows);
        self.selection
            .set_selected(selected_position.unwrap_or(gtk::INVALID_LIST_POSITION));
        self.activation_generation.set(activation_generation);

        let show_list = lineup.status() == SelectedLineupStatus::Ready && !rows.is_empty();
        let loading = lineup.status() == SelectedLineupStatus::Loading;
        self.spinner.set_visible(!show_list && loading);
        self.spinner.set_spinning(!show_list && loading);
        apply_empty_presentation(&self.status, lineup);
        self.stack.set_visible_child_name(if show_list {
            CHANNEL_LIST_PAGE_NAME
        } else {
            STATUS_PAGE_NAME
        });
    }

    fn selected_channel_key(&self) -> Option<ChannelKey> {
        self.selection
            .selected_item()?
            .downcast::<ChannelRowObject>()
            .ok()?
            .key()
    }
}

/// Build the channel pane without inventing lineup or guide data.
#[must_use]
pub(crate) fn build() -> ChannelSidebar {
    let store = gtk::gio::ListStore::new::<ChannelRowObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let factory = channel_factory();
    let list = gtk::ListView::builder()
        .model(&selection)
        .factory(&factory)
        .single_click_activate(false)
        .css_classes(["navigation-sidebar"])
        .vexpand(true)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();

    let spinner = gtk::Spinner::builder().visible(false).build();
    let status = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("Select a device")
        .description("Channels for the selected HDHomeRun device will appear here.")
        .child(&spinner)
        .vexpand(true)
        .build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vhomogeneous(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    stack.add_named(&status, Some(STATUS_PAGE_NAME));
    stack.add_named(&scrolled, Some(CHANNEL_LIST_PAGE_NAME));
    stack.set_visible_child_name(STATUS_PAGE_NAME);

    let root = adw::ToolbarView::new();
    root.add_top_bar(&adw::HeaderBar::new());
    root.set_content(Some(&stack));

    ChannelSidebar {
        root,
        store,
        selection,
        list,
        stack,
        status,
        spinner,
        applying_snapshot: Rc::new(Cell::new(false)),
        activation_generation: Rc::new(Cell::new(None)),
    }
}

fn activation_selection(
    row: &ChannelRowObject,
    generation: Option<OperationGeneration>,
) -> Option<StreamSelection> {
    if row.is_drm() {
        return None;
    }
    Some(StreamSelection::new(row.key()?, generation?))
}

fn channel_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let number = gtk::Label::builder()
            .halign(gtk::Align::End)
            .width_chars(5)
            .xalign(1.0)
            .css_classes(["numeric"])
            .build();
        let name = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let favorite = gtk::Image::builder().pixel_size(16).build();
        let drm = gtk::Image::builder().pixel_size(16).build();
        let hd = gtk::Label::builder()
            .css_classes(["caption", "dim-label"])
            .build();

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        row.append(&number);
        row.append(&name);
        row.append(&favorite);
        row.append(&drm);
        row.append(&hd);
        list_item.set_child(Some(&row));
        reset_channel_list_item(list_item);
    });
    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        reset_channel_list_item(list_item);

        let Some(model_row) = list_item.item().and_downcast::<ChannelRowObject>() else {
            return;
        };
        let Some((row, number, name, favorite, drm, hd)) = channel_row_widgets(list_item) else {
            return;
        };

        let number_text = model_row.number();
        let name_text = model_row.name();
        number.set_text(&number_text);
        number.set_visible(true);
        name.set_text(&name_text);
        name.set_visible(true);

        favorite.set_icon_name(Some("starred-symbolic"));
        favorite.set_tooltip_text(Some("Favorite channel"));
        favorite.set_visible(model_row.is_favorite());
        drm.set_icon_name(Some("changes-prevent-symbolic"));
        drm.set_tooltip_text(Some("Protected channel"));
        drm.set_visible(model_row.is_drm());
        hd.set_text("HD");
        hd.set_tooltip_text(Some("High definition"));
        hd.set_visible(model_row.is_hd());
        if model_row.is_drm() {
            name.add_css_class("dim-label");
        }

        let flags = [
            model_row.is_favorite().then_some("favorite"),
            model_row.is_drm().then_some("protected"),
            model_row.is_hd().then_some("high definition"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        let accessible = if flags.is_empty() {
            format!("{number_text}, {name_text}")
        } else {
            format!("{number_text}, {name_text}, {flags}")
        };
        row.set_tooltip_text(Some(&accessible));
        list_item.set_accessible_label(&accessible);
        let can_activate = !model_row.is_drm() && model_row.key().is_some();
        list_item.set_selectable(can_activate);
        list_item.set_activatable(can_activate);
    });
    factory.connect_unbind(|_, object| {
        if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
            reset_channel_list_item(list_item);
        }
    });
    factory.connect_teardown(|_, object| {
        if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
            reset_channel_list_item(list_item);
            list_item.set_child(gtk::Widget::NONE);
        }
    });
    factory
}

type ChannelRowWidgets = (
    gtk::Box,
    gtk::Label,
    gtk::Label,
    gtk::Image,
    gtk::Image,
    gtk::Label,
);

fn channel_row_widgets(list_item: &gtk::ListItem) -> Option<ChannelRowWidgets> {
    let row = list_item.child()?.downcast::<gtk::Box>().ok()?;
    let number = row.first_child()?.downcast::<gtk::Label>().ok()?;
    let name = number.next_sibling()?.downcast::<gtk::Label>().ok()?;
    let favorite = name.next_sibling()?.downcast::<gtk::Image>().ok()?;
    let drm = favorite.next_sibling()?.downcast::<gtk::Image>().ok()?;
    let hd = drm.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some((row, number, name, favorite, drm, hd))
}

fn reset_channel_list_item(list_item: &gtk::ListItem) {
    if let Some((row, number, name, favorite, drm, hd)) = channel_row_widgets(list_item) {
        row.set_tooltip_text(None);
        number.set_text("");
        number.set_tooltip_text(None);
        number.set_visible(false);
        name.set_text("");
        name.set_tooltip_text(None);
        name.set_visible(false);
        name.remove_css_class("dim-label");
        favorite.set_icon_name(None::<&str>);
        favorite.set_tooltip_text(None);
        favorite.set_visible(false);
        drm.set_icon_name(None::<&str>);
        drm.set_tooltip_text(None);
        drm.set_visible(false);
        hd.set_text("");
        hd.set_tooltip_text(None);
        hd.set_visible(false);
    }
    list_item.set_accessible_label("");
    list_item.set_selectable(false);
    list_item.set_activatable(false);
}

fn apply_empty_presentation(status: &adw::StatusPage, lineup: &SelectedLineupState) {
    match lineup.status() {
        SelectedLineupStatus::Unselected => {
            status.set_icon_name(Some("view-list-symbolic"));
            status.set_title("Select a device");
            status.set_description(Some(
                "Channels for the selected HDHomeRun device will appear here.",
            ));
        }
        SelectedLineupStatus::Loading => {
            status.set_icon_name(Some("view-refresh-symbolic"));
            status.set_title("Loading channels");
            status.set_description(Some("Reading the selected device's channel lineup."));
        }
        SelectedLineupStatus::Ready => {
            status.set_icon_name(Some("view-list-symbolic"));
            status.set_title("No channels available");
            status.set_description(Some(
                "The selected device returned an empty channel lineup.",
            ));
        }
        SelectedLineupStatus::Failed(failure) => {
            status.set_icon_name(Some("dialog-error-symbolic"));
            status.set_title("Channels could not be loaded");
            status.set_description(Some(lineup_failure_description(failure)));
        }
    }
}

fn lineup_failure_description(failure: LineupFailure) -> &'static str {
    match failure {
        LineupFailure::NoSupportedLocator => {
            "The selected device has no supported reachable address."
        }
        LineupFailure::Unreachable => "The selected device could not be reached.",
        LineupFailure::IdentityMismatch => {
            "The responder did not match the selected HDHomeRun device."
        }
        LineupFailure::InvalidMetadata => "The selected device returned invalid metadata.",
        LineupFailure::InvalidLineup => "The selected device returned an invalid channel lineup.",
        LineupFailure::Internal => "Channel loading stopped because of an internal error.",
    }
}

struct SnapshotApplicationGuard {
    flag: Rc<Cell<bool>>,
    previous: bool,
}

impl SnapshotApplicationGuard {
    fn enter(flag: Rc<Cell<bool>>) -> Self {
        let previous = flag.replace(true);
        Self { flag, previous }
    }
}

impl Drop for SnapshotApplicationGuard {
    fn drop(&mut self) {
        self.flag.set(self.previous);
    }
}

#[cfg(test)]
mod tests {
    use balun::controller::ChannelSummary;
    use balun::domain::{DeviceId, GuideNumber};

    use super::*;

    fn channel_row(drm: bool) -> ChannelRowObject {
        let key = ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        );
        ChannelRowObject::from_summary(
            &ChannelSummary::new(key, "Synthetic News".to_owned(), false, drm, true).unwrap(),
        )
    }

    #[test]
    fn activation_retains_the_exact_applied_generation_and_channel_key() {
        let row = channel_row(false);
        let generation = OperationGeneration::new(17);

        let selection = activation_selection(&row, Some(generation)).unwrap();

        assert_eq!(selection.channel_key(), &row.key().unwrap());
        assert_eq!(selection.selection_generation(), generation);
    }

    #[test]
    fn activation_fails_closed_without_ready_authority_or_for_protected_rows() {
        assert!(activation_selection(&channel_row(false), None).is_none());
        let protected = channel_row(true);
        assert!(activation_selection(&protected, Some(OperationGeneration::new(17))).is_none());
    }

    #[test]
    fn lineup_failures_have_stable_non_endpoint_descriptions() {
        assert_eq!(
            lineup_failure_description(LineupFailure::NoSupportedLocator),
            "The selected device has no supported reachable address."
        );
        assert_eq!(
            lineup_failure_description(LineupFailure::IdentityMismatch),
            "The responder did not match the selected HDHomeRun device."
        );
        assert_eq!(
            lineup_failure_description(LineupFailure::InvalidLineup),
            "The selected device returned an invalid channel lineup."
        );
    }
}
