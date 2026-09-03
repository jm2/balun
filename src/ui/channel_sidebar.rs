//! Virtualized channel sidebar for exactly one selected HDHomeRun device.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, LineupFailure, OperationGeneration, SelectedLineupState,
    SelectedLineupStatus, StreamSelection,
};
use balun::domain::{ChannelKey, DeviceId};

use super::objects::ChannelRowObject;

const STATUS_PAGE_NAME: &str = "status";
const CHANNEL_LIST_PAGE_NAME: &str = "channels";

/// GTK parts for the selected-device channel pane.
#[derive(Clone)]
pub(crate) struct ChannelSidebar {
    root: adw::ToolbarView,
    store: gtk::gio::ListStore,
    filtered: gtk::FilterListModel,
    filter: gtk::CustomFilter,
    criteria: Rc<RefCell<ChannelFilter>>,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    search: gtk::SearchEntry,
    favorites_toggle: gtk::ToggleButton,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    presentation: Rc<Cell<LineupPresentation>>,
    selected_device: Rc<Cell<Option<DeviceId>>>,
    applying_snapshot: Rc<Cell<bool>>,
    activation_generation: Rc<Cell<Option<OperationGeneration>>>,
}

/// Search text and favorites filter applied on top of the applied lineup.
///
/// Filtering only hides rows; every row keeps its exact ChannelKey and the
/// lineup generation, so activation and selection restore are unchanged.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ChannelFilter {
    query: String,
    favorites_only: bool,
}

impl ChannelFilter {
    /// Replace the search text; returns whether the effective query changed.
    pub(crate) fn set_query(&mut self, query: &str) -> bool {
        let normalized = query.trim().to_lowercase();
        if normalized == self.query {
            return false;
        }
        self.query = normalized;
        true
    }

    /// Show favorites only; returns whether the setting changed.
    pub(crate) fn set_favorites_only(&mut self, favorites_only: bool) -> bool {
        if self.favorites_only == favorites_only {
            return false;
        }
        self.favorites_only = favorites_only;
        true
    }

    /// Whether a channel passes: favorites only when requested, and the query
    /// as a case-insensitive prefix of the channel number or substring of
    /// the name.
    pub(crate) fn matches(&self, number: &str, name: &str, favorite: bool) -> bool {
        if self.favorites_only && !favorite {
            return false;
        }
        if self.query.is_empty() {
            return true;
        }
        number.to_lowercase().starts_with(&self.query) || name.to_lowercase().contains(&self.query)
    }
}

/// What the applied lineup allows the pane to show, independent of the
/// user's filter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LineupPresentation {
    Unselected,
    Loading,
    Ready { any_channels: bool },
    Failed(LineupFailure),
}

impl LineupPresentation {
    fn from_lineup(lineup: &SelectedLineupState) -> Self {
        match lineup.status() {
            SelectedLineupStatus::Unselected => Self::Unselected,
            SelectedLineupStatus::Loading => Self::Loading,
            SelectedLineupStatus::Ready => Self::Ready {
                any_channels: !lineup.channels().is_empty(),
            },
            SelectedLineupStatus::Failed(failure) => Self::Failed(failure),
        }
    }

    const fn has_channels(self) -> bool {
        matches!(self, Self::Ready { any_channels: true })
    }
}

impl ChannelSidebar {
    #[must_use]
    pub(crate) fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// Connect one URL-free activation intent for a row belonging to the
    /// exact complete lineup generation currently applied to this sidebar.
    ///
    /// Selection alone remains inert: keyboard navigation only moves the
    /// highlight. A single primary-button click on an activatable row
    /// activates it through the row's own gesture, and GTK still emits the
    /// signal for Enter and for its double-click path; the second activation
    /// of a double-click is dropped inside [`REPEAT_ACTIVATION_WINDOW`] so
    /// one tune request reaches the controller.
    pub(crate) fn connect_channel_activated<F>(&self, activate: F) -> gtk::glib::SignalHandlerId
    where
        F: Fn(StreamSelection) + 'static,
    {
        let activation_generation = Rc::clone(&self.activation_generation);
        let applying_snapshot = Rc::clone(&self.applying_snapshot);
        let last_activation: Rc<RefCell<Option<(StreamSelection, Instant)>>> =
            Rc::new(RefCell::new(None));
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
            let now = Instant::now();
            if is_repeat_activation(last_activation.borrow().as_ref(), &selection, now) {
                return;
            }
            *last_activation.borrow_mut() = Some((selection.clone(), now));
            activate(selection);
        })
    }

    /// Atomically replace the channel model with the lineup belonging to the
    /// snapshot's selected DeviceID.
    ///
    /// Any inert local highlight is restored by its complete ChannelKey, never
    /// by a list position. Protected rows remain visible but unselectable.
    /// Search text is cleared when a different device becomes selected.
    pub(crate) fn apply_snapshot(&self, snapshot: &ApplicationSnapshot) {
        let prior_selection = self.selected_channel_key();
        let lineup = snapshot.selected_lineup();
        let channels = if lineup.status() == SelectedLineupStatus::Ready {
            lineup.channels()
        } else {
            &[]
        };
        let rows = channels
            .iter()
            .map(ChannelRowObject::from_summary)
            .collect::<Vec<_>>();
        let activation_generation =
            (lineup.status() == SelectedLineupStatus::Ready).then_some(lineup.generation());

        let _applying = SnapshotApplicationGuard::enter(Rc::clone(&self.applying_snapshot));
        if self.selected_device.replace(snapshot.selected_device()) != snapshot.selected_device() {
            // A new device starts with its whole lineup visible; the
            // favorites toggle is a preference and stays as it was.
            self.search.set_text("");
            self.sync_criteria();
        }
        // Revoke the old row authority before replacing any model item. If a
        // nested GTK callback attempts activation during replacement, both
        // the application guard and the absent generation fail closed.
        self.activation_generation.set(None);
        self.store.splice(0, self.store.n_items(), &rows);
        self.restore_selection(prior_selection.as_ref());
        self.activation_generation.set(activation_generation);

        self.presentation
            .set(LineupPresentation::from_lineup(lineup));
        self.update_presentation();
    }

    fn selected_channel_key(&self) -> Option<ChannelKey> {
        self.selection
            .selected_item()?
            .downcast::<ChannelRowObject>()
            .ok()?
            .key()
    }

    /// Re-select `key` if it is still visible and activatable; otherwise
    /// leave nothing highlighted.
    fn restore_selection(&self, key: Option<&ChannelKey>) {
        let position = key.and_then(|key| self.visible_position(key));
        self.selection
            .set_selected(position.unwrap_or(gtk::INVALID_LIST_POSITION));
    }

    fn visible_position(&self, key: &ChannelKey) -> Option<u32> {
        (0..self.filtered.n_items()).find(|position| {
            self.filtered
                .item(*position)
                .and_downcast::<ChannelRowObject>()
                .is_some_and(|row| !row.is_drm() && row.key().as_ref() == Some(key))
        })
    }

    /// Copy the widget state into the filter criteria and re-run the filter
    /// only when something changed, keeping the highlighted channel if it is
    /// still visible.
    fn sync_criteria(&self) {
        let changed = {
            let mut criteria = self.criteria.borrow_mut();
            let query_changed = criteria.set_query(&self.search.text());
            let favorites_changed = criteria.set_favorites_only(self.favorites_toggle.is_active());
            query_changed || favorites_changed
        };
        if !changed {
            return;
        }
        let prior_selection = self.selected_channel_key();
        self.filter.changed(gtk::FilterChange::Different);
        self.restore_selection(prior_selection.as_ref());
        self.update_presentation();
    }

    fn update_presentation(&self) {
        let presentation = self.presentation.get();
        let has_channels = presentation.has_channels();
        let filtered_out = has_channels && self.filtered.n_items() == 0;
        let show_list = has_channels && !filtered_out;
        let loading = presentation == LineupPresentation::Loading;

        self.search.set_sensitive(has_channels);
        self.favorites_toggle.set_sensitive(has_channels);
        self.spinner.set_visible(!show_list && loading);
        self.spinner.set_spinning(!show_list && loading);
        if filtered_out {
            apply_filtered_out_presentation(&self.status, &self.criteria.borrow());
        } else {
            apply_empty_presentation(&self.status, presentation);
        }
        self.stack.set_visible_child_name(if show_list {
            CHANNEL_LIST_PAGE_NAME
        } else {
            STATUS_PAGE_NAME
        });
    }
}

/// Build the channel pane without inventing lineup or guide data.
#[must_use]
pub(crate) fn build() -> ChannelSidebar {
    let store = gtk::gio::ListStore::new::<ChannelRowObject>();
    let criteria = Rc::new(RefCell::new(ChannelFilter::default()));
    let filter = {
        let criteria = Rc::clone(&criteria);
        gtk::CustomFilter::new(move |item| {
            item.downcast_ref::<ChannelRowObject>().is_some_and(|row| {
                criteria
                    .borrow()
                    .matches(&row.number(), &row.name(), row.is_favorite())
            })
        })
    };
    let filtered = gtk::FilterListModel::new(Some(store.clone()), Some(filter.clone()));
    let selection = gtk::SingleSelection::new(Some(filtered.clone()));
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

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search channels")
        .tooltip_text("Search channels by number or name")
        .sensitive(false)
        .margin_start(6)
        .margin_end(6)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    search.update_property(&[gtk::accessible::Property::Label("Search channels")]);
    let favorites_toggle = gtk::ToggleButton::builder()
        .icon_name("starred-symbolic")
        .tooltip_text("Show favorite channels only")
        .sensitive(false)
        .build();
    favorites_toggle.update_property(&[gtk::accessible::Property::Label(
        "Show favorite channels only",
    )]);

    let header = adw::HeaderBar::new();
    header.pack_end(&favorites_toggle);
    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.add_top_bar(&search);
    root.set_content(Some(&stack));

    let sidebar = ChannelSidebar {
        root,
        store,
        filtered,
        filter,
        criteria,
        selection,
        list,
        search,
        favorites_toggle,
        stack,
        status,
        spinner,
        presentation: Rc::new(Cell::new(LineupPresentation::Unselected)),
        selected_device: Rc::new(Cell::new(None)),
        applying_snapshot: Rc::new(Cell::new(false)),
        activation_generation: Rc::new(Cell::new(None)),
    };
    {
        let sidebar = sidebar.clone();
        sidebar
            .search
            .clone()
            .connect_search_changed(move |_| sidebar.sync_criteria());
    }
    {
        let sidebar = sidebar.clone();
        sidebar
            .favorites_toggle
            .clone()
            .connect_toggled(move |_| sidebar.sync_criteria());
    }
    sidebar
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

/// A second activation of the same selection inside this window is the
/// double-click echo of a single click, not a new intent.
const REPEAT_ACTIVATION_WINDOW: Duration = Duration::from_millis(500);

fn is_repeat_activation(
    last: Option<&(StreamSelection, Instant)>,
    selection: &StreamSelection,
    now: Instant,
) -> bool {
    last.is_some_and(|(previous, at)| {
        previous == selection && now.saturating_duration_since(*at) <= REPEAT_ACTIVATION_WINDOW
    })
}

/// Whether one released primary-button press on a row should activate it.
const fn single_click_activates(n_press: i32, activatable: bool) -> bool {
    n_press == 1 && activatable
}

/// The gesture that turns one primary-button click on a bound row into the
/// list's ordinary `activate` signal, so the same URL-free callback handles
/// clicks, Enter, and GTK's double-click path. Bubble phase keeps GTK's own
/// press handling first, so the row is selected before it activates.
fn single_click_activation(list_item: &gtk::ListItem) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::builder()
        .button(gtk::gdk::BUTTON_PRIMARY)
        .propagation_phase(gtk::PropagationPhase::Bubble)
        .build();
    let list_item = list_item.downgrade();
    gesture.connect_released(move |gesture, n_press, _, _| {
        let Some(list_item) = list_item.upgrade() else {
            return;
        };
        if !single_click_activates(n_press, list_item.is_activatable()) {
            return;
        }
        let position = list_item.position();
        if position == gtk::INVALID_LIST_POSITION {
            return;
        }
        let Some(list) = gesture
            .widget()
            .and_then(|row| row.ancestor(gtk::ListView::static_type()))
            .and_downcast::<gtk::ListView>()
        else {
            return;
        };
        list.emit_by_name::<()>("activate", &[&position]);
    });
    gesture
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
        row.add_controller(single_click_activation(list_item));
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

fn apply_empty_presentation(status: &adw::StatusPage, presentation: LineupPresentation) {
    match presentation {
        LineupPresentation::Unselected => {
            status.set_icon_name(Some("view-list-symbolic"));
            status.set_title("Select a device");
            status.set_description(Some(
                "Channels for the selected HDHomeRun device will appear here.",
            ));
        }
        LineupPresentation::Loading => {
            status.set_icon_name(Some("view-refresh-symbolic"));
            status.set_title("Loading channels");
            status.set_description(Some("Reading the selected device's channel lineup."));
        }
        LineupPresentation::Ready { .. } => {
            status.set_icon_name(Some("view-list-symbolic"));
            status.set_title("No channels available");
            status.set_description(Some(
                "The selected device returned an empty channel lineup.",
            ));
        }
        LineupPresentation::Failed(failure) => {
            status.set_icon_name(Some("dialog-error-symbolic"));
            status.set_title("Channels could not be loaded");
            status.set_description(Some(lineup_failure_description(failure)));
        }
    }
}

fn apply_filtered_out_presentation(status: &adw::StatusPage, criteria: &ChannelFilter) {
    status.set_icon_name(Some("edit-find-symbolic"));
    status.set_title("No matching channels");
    status.set_description(Some(filtered_out_description(criteria)));
}

const fn filtered_out_description(criteria: &ChannelFilter) -> &'static str {
    if criteria.favorites_only && !criteria.query.is_empty() {
        "No favorite channel matches the search."
    } else if criteria.favorites_only {
        "The selected device has no favorite channels."
    } else {
        "No channel number or name matches the search."
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
    use std::cell::RefCell;

    use balun::controller::{ChannelSummary, DeviceSummary, DiscoveryState, SnapshotRevision};
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

    fn ready_snapshot() -> (
        ApplicationSnapshot,
        ChannelKey,
        ChannelKey,
        OperationGeneration,
    ) {
        let device_id = DeviceId::new(0x105A_1232).unwrap();
        let generation = OperationGeneration::new(23);
        let unprotected_key = ChannelKey::new(device_id, GuideNumber::new("7.1").unwrap());
        let protected_key = ChannelKey::new(device_id, GuideNumber::new("8.1").unwrap());
        let channels = [
            ChannelSummary::new(
                unprotected_key.clone(),
                "Synthetic News".to_owned(),
                false,
                false,
                true,
            )
            .unwrap(),
            ChannelSummary::new(
                protected_key.clone(),
                "Protected Test".to_owned(),
                false,
                true,
                true,
            )
            .unwrap(),
        ];
        let device = DeviceSummary::new(
            device_id,
            Some("Test tuner".to_owned()),
            Some("HDHomeRun".to_owned()),
            Some(2),
            "192.0.2.10:65001".parse().unwrap(),
            1,
        )
        .unwrap();
        let snapshot = ApplicationSnapshot::new(
            SnapshotRevision::new(7),
            OperationGeneration::new(5),
            generation,
            DiscoveryState::ready(OperationGeneration::new(5), 0),
            [device],
            Some(device_id),
            SelectedLineupState::ready(device_id, generation, channels).unwrap(),
        )
        .unwrap();
        (snapshot, unprotected_key, protected_key, generation)
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

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display-backed ListView signal contract.
    #[test]
    #[ignore = "requires the isolated display supplied by scripts/test-desktop-lifecycle.sh"]
    fn ready_listview_activation_is_inert_on_selection_and_exact_on_activate() {
        adw::init().expect("initialize libadwaita for channel activation smoke");
        let main_context = gtk::glib::MainContext::default();
        let _owner = main_context
            .acquire()
            .expect("acquire default main context for channel activation smoke");
        let sidebar = build();
        let (snapshot, unprotected_key, _protected_key, generation) = ready_snapshot();
        sidebar.apply_snapshot(&snapshot);
        let activations = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&activations);
        sidebar.connect_channel_activated(move |selection| {
            recorded.borrow_mut().push(selection);
        });

        let window = gtk::Window::builder()
            .title("Balun channel activation proof")
            .default_width(320)
            .default_height(480)
            .child(sidebar.root())
            .build();
        window.present();
        while main_context.pending() {
            main_context.iteration(false);
        }

        assert_eq!(sidebar.store.n_items(), 2);
        assert_eq!(
            sidebar.stack.visible_child_name().as_deref(),
            Some(CHANNEL_LIST_PAGE_NAME)
        );
        assert!(!sidebar.list.is_single_click_activate());
        assert!(sidebar.list.is_focusable());
        assert_eq!(sidebar.list.accessible_role(), gtk::AccessibleRole::List);

        // A single-click-equivalent selection changes only the inert local
        // highlight and must not allocate a tuner.
        sidebar.selection.set_selected(0);
        assert!(activations.borrow().is_empty());

        sidebar.list.emit_by_name::<()>("activate", &[&0_u32]);
        assert_eq!(activations.borrow().len(), 1);
        assert_eq!(activations.borrow()[0].channel_key(), &unprotected_key);
        assert_eq!(activations.borrow()[0].selection_generation(), generation);

        // Even a forged signal position for a protected visible row fails
        // closed in the same production callback.
        sidebar.list.emit_by_name::<()>("activate", &[&1_u32]);
        assert_eq!(activations.borrow().len(), 1);
        window.close();
    }

    #[test]
    fn a_single_primary_click_activates_only_activatable_rows() {
        assert!(single_click_activates(1, true));
        assert!(!single_click_activates(1, false));
        assert!(!single_click_activates(2, true));
        assert!(!single_click_activates(0, true));
    }

    #[test]
    fn the_double_click_echo_of_a_single_click_is_dropped() {
        let (_snapshot, key, protected, generation) = ready_snapshot();
        let selection = StreamSelection::new(key, generation);
        let start = Instant::now();
        assert!(!is_repeat_activation(None, &selection, start));
        let last = (selection.clone(), start);
        assert!(is_repeat_activation(
            Some(&last),
            &selection,
            start + Duration::from_millis(200)
        ));
        assert!(!is_repeat_activation(
            Some(&last),
            &selection,
            start + REPEAT_ACTIVATION_WINDOW + Duration::from_millis(1)
        ));
        let other = StreamSelection::new(protected, generation);
        assert!(!is_repeat_activation(Some(&last), &other, start));
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

    #[test]
    fn filter_matches_number_prefix_and_name_substring_case_insensitively() {
        let mut filter = ChannelFilter::default();
        assert!(filter.matches("7.1", "Synthetic News", false));

        assert!(filter.set_query("  SYN "));
        assert!(filter.matches("7.1", "Synthetic News", false));
        assert!(!filter.matches("8.1", "Protected Test", false));

        assert!(filter.set_query("7"));
        assert!(filter.matches("7.1", "Synthetic News", false));
        assert!(filter.matches("71.2", "Other", false));
        assert!(
            !filter.matches("17.1", "Other", false),
            "number matches by prefix only"
        );

        assert!(
            !filter.set_query(" 7 "),
            "an equivalent query is not a change"
        );
    }

    #[test]
    fn favorites_only_hides_non_favorites_and_composes_with_the_query() {
        let mut filter = ChannelFilter::default();
        assert!(filter.set_favorites_only(true));
        assert!(!filter.set_favorites_only(true));
        assert!(filter.matches("7.1", "Synthetic News", true));
        assert!(!filter.matches("7.1", "Synthetic News", false));

        assert!(filter.set_query("news"));
        assert!(filter.matches("7.1", "Synthetic News", true));
        assert!(!filter.matches("7.1", "Weather", true));
        assert_eq!(
            filtered_out_description(&filter),
            "No favorite channel matches the search."
        );
        assert!(filter.set_query(""));
        assert_eq!(
            filtered_out_description(&filter),
            "The selected device has no favorite channels."
        );
        assert!(filter.set_favorites_only(false));
        assert!(filter.set_query("x"));
        assert_eq!(
            filtered_out_description(&filter),
            "No channel number or name matches the search."
        );
    }

    #[test]
    fn presentation_follows_the_lineup_status() {
        let (snapshot, _, _, _) = ready_snapshot();
        assert_eq!(
            LineupPresentation::from_lineup(snapshot.selected_lineup()),
            LineupPresentation::Ready { any_channels: true }
        );
        assert!(LineupPresentation::Ready { any_channels: true }.has_channels());
        assert!(
            !LineupPresentation::Ready {
                any_channels: false
            }
            .has_channels()
        );
        assert!(!LineupPresentation::Loading.has_channels());
        assert_eq!(
            LineupPresentation::from_lineup(&SelectedLineupState::unselected(
                OperationGeneration::new(1)
            )),
            LineupPresentation::Unselected
        );
    }
}
