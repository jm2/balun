//! Virtualized HDHomeRun device sidebar.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, DeviceSummary, DiscoveryFailure, DiscoveryKind, DiscoveryStatus,
    NetworkChangeSummary, OperationGeneration,
};
use balun::localization::subnet_search::{self, SubnetEntryLabels};

use super::objects::DeviceRowObject;

const STATUS_PAGE_NAME: &str = "status";
/// How long a successful reply stays in the top banner.
const ANNOUNCEMENT_DURATION: Duration = Duration::from_secs(3);

/// Main-context reaction to a secondary click on a device row: the row's
/// list position, the row widget, and the click point in row coordinates.
type DeviceContextHandler = dyn Fn(u32, &gtk::Widget, f64, f64);
type SharedDeviceContextHandler = Rc<RefCell<Option<Box<DeviceContextHandler>>>>;
/// Row widgets built by the factory and their list items, so the keyboard
/// shortcut can find the focused row's position and a row whose text
/// changed in place can be redrawn.
type BoundRows = Rc<
    RefCell<
        Vec<(
            gtk::glib::WeakRef<gtk::Widget>,
            gtk::glib::WeakRef<gtk::ListItem>,
        )>,
    >,
>;
const DEVICE_LIST_PAGE_NAME: &str = "devices";

/// GTK parts for the device pane.
///
/// Discovery buttons are intentionally exposed but left disconnected here.
/// The application bridge decides when a bounded discovery command is
/// admitted; constructing this sidebar performs no network work.
#[derive(Clone)]
pub(crate) struct DeviceSidebar {
    root: adw::ToolbarView,
    store: gtk::gio::ListStore,
    selection: gtk::SingleSelection,
    list: gtk::ListView,
    stack: gtk::Stack,
    status: adw::StatusPage,
    terminal_banner: adw::Banner,
    spinner: gtk::Spinner,
    cancel_discovery_button: gtk::Button,
    exact_discovery_button: gtk::Button,
    subnet_search_button: gtk::Button,
    refresh_button: gtk::Button,
    device_context: SharedDeviceContextHandler,
    bound_rows: BoundRows,
    applying_snapshot: Rc<Cell<bool>>,
    /// The last network-change sequence shown, so the notice appears once
    /// per reconciliation and yields to the next publication.
    network_sequence: Rc<Cell<u64>>,
    /// Bumped whenever the banner changes, so an announcement's timer only
    /// clears the banner it was started for.
    banner_epoch: Rc<Cell<u64>>,
    /// The exact operation whose successful reply was announced, so later
    /// snapshots of the same outcome do not show it again.
    announced_generation: Rc<Cell<Option<OperationGeneration>>>,
}

impl DeviceSidebar {
    #[must_use]
    pub(crate) fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    #[must_use]
    pub(crate) fn refresh_button(&self) -> &gtk::Button {
        &self.refresh_button
    }

    #[must_use]
    pub(crate) fn exact_discovery_button(&self) -> &gtk::Button {
        &self.exact_discovery_button
    }

    #[must_use]
    pub(crate) fn subnet_search_button(&self) -> &gtk::Button {
        &self.subnet_search_button
    }

    #[must_use]
    pub(crate) fn cancel_discovery_button(&self) -> &gtk::Button {
        &self.cancel_discovery_button
    }

    #[must_use]
    pub(crate) fn selection(&self) -> &gtk::SingleSelection {
        &self.selection
    }

    /// Connect an activation callback invoked when any device row is activated
    /// via Enter key or single click.
    pub(crate) fn connect_device_activated<F>(&self, callback: F)
    where
        F: Fn(u32) + 'static,
    {
        self.list.connect_activate(move |_, position| {
            callback(position);
        });
    }

    fn reveal_banner(&self, title: &str) -> u64 {
        let epoch = self.banner_epoch.get().wrapping_add(1);
        self.banner_epoch.set(epoch);
        self.terminal_banner.set_title(title);
        self.terminal_banner.set_revealed(true);
        epoch
    }

    fn hide_banner(&self) {
        self.banner_epoch
            .set(self.banner_epoch.get().wrapping_add(1));
        self.terminal_banner.set_revealed(false);
        self.terminal_banner.set_title("");
    }

    /// Show good news briefly. The timer clears the banner only while it
    /// still shows this announcement; anything shown since keeps its place.
    fn announce_banner(&self, title: &str) {
        let epoch = self.reveal_banner(title);
        let banner = self.terminal_banner.downgrade();
        let banner_epoch = Rc::clone(&self.banner_epoch);
        gtk::glib::timeout_add_local_once(ANNOUNCEMENT_DURATION, move || {
            if banner_epoch.get() == epoch
                && let Some(banner) = banner.upgrade()
            {
                banner.set_revealed(false);
                banner.set_title("");
            }
        });
    }

    /// Run `callback` when a device row is secondary-clicked (right-click).
    /// The bridge decides whether that device has anything to forget; the
    /// row itself never changes.
    pub(crate) fn connect_device_context<F>(&self, callback: F)
    where
        F: Fn(u32, &gtk::Widget, f64, f64) + 'static,
    {
        *self.device_context.borrow_mut() = Some(Box::new(callback));
    }

    /// Share the non-GObject reentrancy flag with the window bridge without
    /// making a GTK selection callback retain this complete sidebar.
    #[must_use]
    pub(crate) fn snapshot_application_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.applying_snapshot)
    }

    /// Show every device and status field from one immutable, URL-free
    /// controller publication.
    ///
    /// Numeric list positions are never retained. The authoritative DeviceID
    /// is resolved in the updated model before selection is restored.
    pub(crate) fn apply_snapshot(&self, snapshot: &ApplicationSnapshot) {
        let selected_position = snapshot.selected_device().and_then(|selected| {
            snapshot
                .devices()
                .iter()
                .position(|device| device.device_id() == selected)
                .and_then(|position| u32::try_from(position).ok())
        });

        let _applying = SnapshotApplicationGuard::enter(Rc::clone(&self.applying_snapshot));
        // Replacing a row unparents its widget, so GTK moves keyboard focus
        // to the first row and closes any popover on it. A listed device
        // keeps its row object: a snapshot that lists the same devices (a
        // lineup load, discovery progress) leaves the model alone, and GTK
        // keeps the widgets of retained rows when a device comes or goes.
        let current = (0..self.store.n_items())
            .filter_map(|position| self.store.item(position).and_downcast::<DeviceRowObject>())
            .collect::<Vec<_>>();
        let (rows, refreshed) = reconcile_rows(&current, snapshot.devices());
        if rows != current {
            self.store.splice(0, self.store.n_items(), &rows);
        }
        redraw_rows(&self.bound_rows, &refreshed);
        self.selection
            .set_selected(selected_position.unwrap_or(gtk::INVALID_LIST_POSITION));

        let discovery = snapshot.discovery();
        let actions = discovery_actions_presentation(discovery.status());
        self.refresh_button.set_sensitive(actions.start_sensitive);
        self.exact_discovery_button
            .set_sensitive(actions.start_sensitive);
        let subnet = subnet_action_presentation(
            actions.start_sensitive,
            snapshot.observation().generation().is_some(),
        );
        self.subnet_search_button.set_sensitive(subnet.sensitive);
        self.subnet_search_button
            .set_tooltip_text(Some(&subnet_action_tooltip(subnet.available)));
        self.cancel_discovery_button
            .set_sensitive(actions.cancel_sensitive);
        self.cancel_discovery_button
            .set_visible(actions.cancel_visible);

        let show_status = rows.is_empty();
        let refreshing = discovery.status() == DiscoveryStatus::Refreshing;
        let title = terminal_banner_title(discovery.kind(), discovery.status(), !show_status);
        match plan_terminal_banner(
            discovery.kind(),
            discovery.status(),
            title.as_deref(),
            discovery.generation(),
            self.announced_generation.get(),
            self.terminal_banner.title().as_str(),
        ) {
            BannerChange::Hide => {
                self.announced_generation.set(None);
                self.hide_banner();
            }
            BannerChange::Clear => self.hide_banner(),
            BannerChange::Reveal(title) => {
                self.announced_generation.set(None);
                self.reveal_banner(title);
            }
            BannerChange::Announce(title) => {
                self.announced_generation.set(Some(discovery.generation()));
                self.announce_banner(title);
            }
            BannerChange::Keep => {}
        }
        let network = snapshot.network();
        if self.network_sequence.replace(network.sequence()) != network.sequence()
            && let Some(title) = network_change_banner_title(network)
        {
            self.reveal_banner(title);
        }
        self.spinner.set_visible(show_status && refreshing);
        self.spinner.set_spinning(show_status && refreshing);
        apply_empty_presentation(
            &self.status,
            discovery.kind(),
            discovery.status(),
            discovery.issue_count(),
        );
        self.stack.set_visible_child_name(if show_status {
            STATUS_PAGE_NAME
        } else {
            DEVICE_LIST_PAGE_NAME
        });
    }
}

/// The rows for `devices` in snapshot order, reusing the object that already
/// shows each DeviceID (its text updated in place), and the reused rows whose
/// text changed. Only a newly listed device gets a new object.
fn reconcile_rows(
    current: &[DeviceRowObject],
    devices: &[DeviceSummary],
) -> (Vec<DeviceRowObject>, Vec<DeviceRowObject>) {
    let mut refreshed = Vec::new();
    let rows = devices
        .iter()
        .map(|device| {
            let Some(row) = current
                .iter()
                .find(|row| row.device_id() == Some(device.device_id()))
            else {
                return DeviceRowObject::from_summary(device);
            };
            if row.refresh(device) {
                refreshed.push(row.clone());
            }
            row.clone()
        })
        .collect();
    (rows, refreshed)
}

/// Build the device pane without starting discovery or any other network work.
#[must_use]
pub(crate) fn build() -> DeviceSidebar {
    let store = gtk::gio::ListStore::new::<DeviceRowObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let device_context: SharedDeviceContextHandler = Rc::default();
    let bound_rows: BoundRows = Rc::default();
    let factory = device_factory(&device_context, &bound_rows);
    let list = gtk::ListView::builder()
        .model(&selection)
        .factory(&factory)
        .single_click_activate(false)
        .css_classes(["navigation-sidebar"])
        .accessible_role(gtk::AccessibleRole::List)
        .vexpand(true)
        .build();
    list.update_property(&[gtk::accessible::Property::Label("HDHomeRun devices")]);
    list.add_controller(context_shortcut(&list, &bound_rows, &device_context));
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();

    let spinner = gtk::Spinner::builder()
        .visible(false)
        .accessible_role(gtk::AccessibleRole::ProgressBar)
        .build();
    spinner.update_property(&[gtk::accessible::Property::Label("Discovering devices")]);
    let status = adw::StatusPage::builder()
        .icon_name("network-wired-symbolic")
        .title("No HDHomeRun devices")
        .description("Choose Refresh to search your local network.")
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
    stack.add_named(&scrolled, Some(DEVICE_LIST_PAGE_NAME));
    stack.set_visible_child_name(STATUS_PAGE_NAME);

    let find_title = balun::localization::device_dialogs::FindLabels::current().title;
    let exact_discovery_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(&*find_title)
        .css_classes(["flat"])
        .build();
    exact_discovery_button.update_property(&[gtk::accessible::Property::Label(&find_title)]);
    // Offered only once a snapshot reports healthy network observation.
    let subnet_labels = SubnetEntryLabels::current();
    let subnet_search_button = gtk::Button::builder()
        .icon_name("network-workgroup-symbolic")
        .tooltip_text(&*subnet_labels.unavailable)
        .css_classes(["flat"])
        .sensitive(false)
        .build();
    subnet_search_button
        .update_property(&[gtk::accessible::Property::Label(&subnet_labels.button)]);
    let refresh_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh devices (F5)")
        .css_classes(["flat"])
        .build();
    refresh_button.update_property(&[
        gtk::accessible::Property::Label("Refresh devices"),
        gtk::accessible::Property::KeyShortcuts("F5 Control+r"),
    ]);
    let cancel_discovery_button = gtk::Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text("Stop device discovery")
        .css_classes(["flat"])
        .sensitive(false)
        .visible(false)
        .build();
    cancel_discovery_button
        .update_property(&[gtk::accessible::Property::Label("Stop device discovery")]);
    let discovery_actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    discovery_actions.append(&exact_discovery_button);
    discovery_actions.append(&subnet_search_button);
    discovery_actions.append(&refresh_button);
    discovery_actions.append(&cancel_discovery_button);
    let header = adw::HeaderBar::new();
    // The automatic NavigationPage title ellipsizes to fit beside macOS's
    // window controls. Keep this short title's full minimum width so the
    // split view allocates enough space for "Devices" and the buttons.
    let title = gtk::Label::builder()
        .label(balun::localization::controls::NavigationLabels::current().devices)
        .css_classes(["title"])
        .build();
    header.set_title_widget(Some(&title));
    let app_menu = gtk::gio::Menu::new();
    app_menu.append(Some(&balun::localization::about_label()), Some("app.about"));
    app_menu.append(Some(&balun::localization::quit_label()), Some("app.quit"));
    let main_menu_label = balun::localization::main_menu_label();
    let app_menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(&*main_menu_label)
        .menu_model(&app_menu)
        .build();
    app_menu_button.update_property(&[gtk::accessible::Property::Label(&main_menu_label)]);
    header.pack_start(&app_menu_button);
    header.pack_end(&discovery_actions);

    let terminal_banner = adw::Banner::builder().revealed(false).build();

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.add_top_bar(&terminal_banner);
    root.set_content(Some(&stack));

    DeviceSidebar {
        root,
        store,
        selection,
        list,
        stack,
        status,
        terminal_banner,
        spinner,
        cancel_discovery_button,
        exact_discovery_button,
        subnet_search_button,
        refresh_button,
        device_context,
        bound_rows,
        applying_snapshot: Rc::new(Cell::new(false)),
        network_sequence: Rc::new(Cell::new(0)),
        banner_epoch: Rc::new(Cell::new(0)),
        announced_generation: Rc::new(Cell::new(None)),
    }
}

/// Synthesize the list view's activation signal from a single primary click.
/// The keyboard path to the same action: Menu or Shift+F10 while a device
/// row has focus reports that row with its centre as the anchor point.
fn context_shortcut(
    list: &gtk::ListView,
    bound_rows: &BoundRows,
    handler: &SharedDeviceContextHandler,
) -> gtk::ShortcutController {
    let controller = gtk::ShortcutController::new();
    controller.set_scope(gtk::ShortcutScope::Local);
    let list = list.downgrade();
    let bound_rows = Rc::clone(bound_rows);
    let handler = Rc::clone(handler);
    let action = gtk::CallbackAction::new(move |_, _| {
        let Some(row) = list
            .upgrade()
            .and_then(|list| list.focus_child())
            .and_then(|item| item.first_child())
        else {
            return gtk::glib::Propagation::Proceed;
        };
        let position = {
            let mut rows = bound_rows.borrow_mut();
            rows.retain(|(bound, _)| bound.upgrade().is_some());
            rows.iter().find_map(|(bound, item)| {
                (bound.upgrade()? == row).then(|| item.upgrade().map(|item| item.position()))?
            })
        };
        let Some(position) = position.filter(|position| *position != gtk::INVALID_LIST_POSITION)
        else {
            return gtk::glib::Propagation::Proceed;
        };
        if let Some(handler) = handler.borrow().as_ref() {
            handler(
                position,
                &row,
                f64::from(row.width()) / 2.0,
                f64::from(row.height()) / 2.0,
            );
        }
        gtk::glib::Propagation::Stop
    });
    controller.add_shortcut(gtk::Shortcut::new(
        gtk::ShortcutTrigger::parse_string("Menu|<Shift>F10"),
        Some(action),
    ));
    controller
}

/// Report a secondary click on a bound row so the bridge can offer to forget
/// the device it shows. Nothing is reported for an unbound placeholder.
fn secondary_click(
    list_item: &gtk::ListItem,
    handler: &SharedDeviceContextHandler,
) -> gtk::GestureClick {
    let gesture = gtk::GestureClick::builder()
        .button(gtk::gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Bubble)
        .build();
    let list_item = list_item.downgrade();
    let handler = Rc::clone(handler);
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(list_item) = list_item.upgrade() else {
            return;
        };
        let position = list_item.position();
        if position == gtk::INVALID_LIST_POSITION || list_item.item().is_none() {
            return;
        }
        let Some(row) = gesture.widget() else {
            return;
        };
        if let Some(handler) = handler.borrow().as_ref() {
            handler(position, &row, x, y);
        }
    });
    gesture
}

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
        if n_press != 1 || !list_item.is_activatable() {
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

fn device_factory(
    device_context: &SharedDeviceContextHandler,
    bound_rows: &BoundRows,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    let device_context = Rc::clone(device_context);
    let bound_rows = Rc::clone(bound_rows);
    factory.connect_setup(move |_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };

        let icon = gtk::Image::builder().pixel_size(20).build();
        let title = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        let subtitle = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption"])
            .build();
        let labels = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        labels.append(&title);
        labels.append(&subtitle);

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        row.append(&icon);
        row.append(&labels);
        row.add_controller(single_click_activation(list_item));
        row.add_controller(secondary_click(list_item, &device_context));
        bound_rows.borrow_mut().push((
            row.upcast_ref::<gtk::Widget>().downgrade(),
            list_item.downgrade(),
        ));
        list_item.set_child(Some(&row));
        reset_device_list_item(list_item);
    });
    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        reset_device_list_item(list_item);
        present_device_row(list_item);
    });
    factory.connect_unbind(|_, object| {
        if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
            reset_device_list_item(list_item);
        }
    });
    factory.connect_teardown(|_, object| {
        if let Some(list_item) = object.downcast_ref::<gtk::ListItem>() {
            reset_device_list_item(list_item);
            list_item.set_child(gtk::Widget::NONE);
        }
    });
    factory
}

/// Show the bound row's device. Every field is set, so this also redraws a
/// row whose text changed in place.
fn present_device_row(list_item: &gtk::ListItem) {
    let Some(model_row) = list_item.item().and_downcast::<DeviceRowObject>() else {
        return;
    };
    let Some((row, icon, title, subtitle)) = device_row_widgets(list_item) else {
        return;
    };

    let title_text = model_row.title();
    let subtitle_text = model_row.subtitle();
    icon.set_icon_name(Some("network-server-symbolic"));
    icon.set_visible(true);
    icon.set_tooltip_text(Some("HDHomeRun device"));
    title.set_text(&title_text);
    title.set_visible(true);
    subtitle.set_text(&subtitle_text);
    subtitle.set_visible(!subtitle_text.is_empty());
    row.set_tooltip_text(Some(&format!("{title_text}\n{subtitle_text}")));
    let label = if subtitle_text.is_empty() {
        title_text.clone()
    } else {
        format!("{title_text}, {subtitle_text}")
    };
    list_item.set_accessible_label(&label);
    list_item.set_selectable(true);
    list_item.set_activatable(true);
}

/// Redraw the bound rows showing `refreshed`. GTK binds a row once per
/// object and keeps the widget of a retained object, so it never rebinds
/// text changed in place.
fn redraw_rows(bound_rows: &BoundRows, refreshed: &[DeviceRowObject]) {
    if refreshed.is_empty() {
        return;
    }
    let list_items = {
        let mut rows = bound_rows.borrow_mut();
        rows.retain(|(bound, _)| bound.upgrade().is_some());
        rows.iter()
            .filter_map(|(_, item)| item.upgrade())
            .collect::<Vec<_>>()
    };
    for list_item in list_items {
        if list_item
            .item()
            .and_downcast::<DeviceRowObject>()
            .is_some_and(|row| refreshed.contains(&row))
        {
            present_device_row(&list_item);
        }
    }
}

fn device_row_widgets(
    list_item: &gtk::ListItem,
) -> Option<(gtk::Box, gtk::Image, gtk::Label, gtk::Label)> {
    let row = list_item.child()?.downcast::<gtk::Box>().ok()?;
    let icon = row.first_child()?.downcast::<gtk::Image>().ok()?;
    let labels = icon.next_sibling()?.downcast::<gtk::Box>().ok()?;
    let title = labels.first_child()?.downcast::<gtk::Label>().ok()?;
    let subtitle = title.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some((row, icon, title, subtitle))
}

fn reset_device_list_item(list_item: &gtk::ListItem) {
    if let Some((row, icon, title, subtitle)) = device_row_widgets(list_item) {
        row.set_tooltip_text(None);
        icon.set_icon_name(None::<&str>);
        icon.set_tooltip_text(None);
        icon.set_visible(false);
        title.set_text("");
        title.set_tooltip_text(None);
        title.set_visible(false);
        subtitle.set_text("");
        subtitle.set_tooltip_text(None);
        subtitle.set_visible(false);
    }
    list_item.set_accessible_label("");
    list_item.set_selectable(false);
    list_item.set_activatable(false);
}

fn apply_empty_presentation(
    status: &adw::StatusPage,
    discovery_kind: DiscoveryKind,
    discovery_status: DiscoveryStatus,
    issue_count: u16,
) {
    let presentation = discovery_presentation(discovery_kind, discovery_status, issue_count);
    status.set_icon_name(Some(presentation.icon_name));
    status.set_title(&presentation.title);
    status.set_description(Some(&presentation.description));
}

/// How the top banner reacts to a discovery outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BannerChange<'a> {
    /// Nothing to show; clear whatever is there.
    Hide,
    /// A condition worth keeping until the discovery state changes.
    Reveal(&'a str),
    /// Good news: show it, then let a timer clear it.
    Announce(&'a str),
    /// A notice borrowed the banner after the announcement; clear it but
    /// keep the announcement on record so it is not repeated.
    Clear,
    /// The announced outcome is unchanged; leave the timer to it.
    Keep,
}

/// A successful exact reply is announced once per operation and then
/// clears itself; later snapshots of the same operation leave it alone,
/// whether the timer has run or not. Every other title stays until the
/// discovery state changes, and a notice that borrowed the banner after the
/// announcement is cleared on the next snapshot, as before.
fn plan_terminal_banner<'a>(
    kind: DiscoveryKind,
    status: DiscoveryStatus,
    title: Option<&'a str>,
    generation: OperationGeneration,
    announced: Option<OperationGeneration>,
    shown_title: &str,
) -> BannerChange<'a> {
    let Some(title) = title else {
        return BannerChange::Hide;
    };
    if status != DiscoveryStatus::Ready || kind == DiscoveryKind::Local {
        return BannerChange::Reveal(title);
    }
    if announced != Some(generation) {
        return BannerChange::Announce(title);
    }
    if shown_title == title || shown_title.is_empty() {
        BannerChange::Keep
    } else {
        BannerChange::Clear
    }
}

/// Keep relevant terminal discovery outcomes visible when retained device
/// rows make the empty-state page unavailable. Copy is fixed and the
/// controller snapshot intentionally contains no target address.
fn terminal_banner_title(
    kind: DiscoveryKind,
    status: DiscoveryStatus,
    has_device_rows: bool,
) -> Option<Cow<'static, str>> {
    if !has_device_rows {
        return None;
    }
    if kind == DiscoveryKind::Subnet || matches!(status, DiscoveryStatus::Incomplete(_)) {
        return subnet_search::banner(status);
    }

    let title = match (kind, status) {
        (DiscoveryKind::Exact, DiscoveryStatus::Ready) => Some("HDHomeRun device reply received."),
        (DiscoveryKind::Exact, DiscoveryStatus::NoResponse) => {
            Some("No valid HDHomeRun reply was received.")
        }
        (
            DiscoveryKind::Exact,
            DiscoveryStatus::Failed(DiscoveryFailure::ExactTargetLimitReached),
        ) => Some("Device address limit reached for this session."),
        (DiscoveryKind::Exact, DiscoveryStatus::Failed(_)) => {
            Some("Exact-address device search failed.")
        }
        (DiscoveryKind::Local, DiscoveryStatus::NoResponse) => {
            Some("No valid HDHomeRun replies were received.")
        }
        (DiscoveryKind::Local, DiscoveryStatus::Failed(_)) => {
            Some("Local device discovery failed.")
        }
        (_, DiscoveryStatus::Idle | DiscoveryStatus::Refreshing | DiscoveryStatus::Ready)
        | (DiscoveryKind::Subnet, _)
        | (_, DiscoveryStatus::Incomplete(_)) => None,
    };
    title.map(Cow::Borrowed)
}

/// A brief notice for the snapshot that reconciled a network change, shown
/// only when it retired evidence. The summary carries counts and nothing else.
fn network_change_banner_title(network: NetworkChangeSummary) -> Option<&'static str> {
    if network.sequence() == 0 {
        return None;
    }
    if network.removed_devices() > 0 {
        Some("Network changed; devices that lost every address were removed.")
    } else if network.expired_locators() > 0 {
        Some("Network changed; stale device addresses were dropped.")
    } else {
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiscoveryPresentation {
    icon_name: &'static str,
    title: Cow<'static, str>,
    description: Cow<'static, str>,
}

/// Whether the subnet action can start a search, and whether observation
/// makes subnet search available at all.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SubnetActionPresentation {
    sensitive: bool,
    available: bool,
}

/// Whether `snapshot` lets the subnet action start a search, for restoring
/// the action after a start that could not be queued.
pub(crate) fn subnet_search_sensitive(snapshot: &ApplicationSnapshot) -> bool {
    subnet_action_presentation(
        discovery_actions_presentation(snapshot.discovery().status()).start_sensitive,
        snapshot.observation().generation().is_some(),
    )
    .sensitive
}

/// Subnet search needs an idle discovery lane and healthy observation.
fn subnet_action_presentation(start_sensitive: bool, observing: bool) -> SubnetActionPresentation {
    SubnetActionPresentation {
        sensitive: start_sensitive && observing,
        available: observing,
    }
}

fn subnet_action_tooltip(available: bool) -> Cow<'static, str> {
    let labels = SubnetEntryLabels::current();
    if available {
        labels.button
    } else {
        labels.unavailable
    }
}

/// The empty-list copy for a subnet search, which never stops short of its
/// scope without saying so.
fn subnet_presentation(status: DiscoveryStatus) -> DiscoveryPresentation {
    let (title, description) = subnet_search::status(status);
    DiscoveryPresentation {
        icon_name: match status {
            DiscoveryStatus::Idle => "process-stop-symbolic",
            DiscoveryStatus::Refreshing => "network-transmit-receive-symbolic",
            DiscoveryStatus::Ready => "network-workgroup-symbolic",
            DiscoveryStatus::NoResponse => "network-offline-symbolic",
            DiscoveryStatus::Incomplete(_) => "dialog-warning-symbolic",
            DiscoveryStatus::Failed(_) => "dialog-error-symbolic",
        },
        title,
        description,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiscoveryActionsPresentation {
    start_sensitive: bool,
    cancel_visible: bool,
    cancel_sensitive: bool,
}

fn discovery_actions_presentation(status: DiscoveryStatus) -> DiscoveryActionsPresentation {
    let refreshing = status == DiscoveryStatus::Refreshing;
    DiscoveryActionsPresentation {
        start_sensitive: !refreshing,
        cancel_visible: refreshing,
        cancel_sensitive: refreshing,
    }
}

fn discovery_presentation(
    kind: DiscoveryKind,
    status: DiscoveryStatus,
    issue_count: u16,
) -> DiscoveryPresentation {
    if kind == DiscoveryKind::Subnet || matches!(status, DiscoveryStatus::Incomplete(_)) {
        return subnet_presentation(status);
    }
    let fixed = |icon_name, title, description| DiscoveryPresentation {
        icon_name,
        title: Cow::Borrowed(title),
        description: Cow::Borrowed(description),
    };
    match (kind, status) {
        (DiscoveryKind::Exact, DiscoveryStatus::Idle) => fixed(
            "process-stop-symbolic",
            "Device search stopped",
            "No exact-address discovery request is running.",
        ),
        (DiscoveryKind::Exact, DiscoveryStatus::Refreshing) => fixed(
            "network-transmit-receive-symbolic",
            "Finding HDHomeRun device",
            "Waiting for a reply from the entered address.",
        ),
        (DiscoveryKind::Exact, DiscoveryStatus::Ready) => fixed(
            "network-wired-symbolic",
            "Device reply received",
            "The exact-address discovery request completed.",
        ),
        (DiscoveryKind::Exact, DiscoveryStatus::NoResponse) => fixed(
            "network-offline-symbolic",
            "No valid HDHomeRun reply received",
            "Check that the entered address is reachable, then try again.",
        ),
        (DiscoveryKind::Local, DiscoveryStatus::Refreshing) => fixed(
            "network-transmit-receive-symbolic",
            "Searching for HDHomeRun devices",
            "Waiting for replies from this local network.",
        ),
        (DiscoveryKind::Local, DiscoveryStatus::Ready) => fixed(
            "network-offline-symbolic",
            "No HDHomeRun devices found",
            if issue_count == 0 {
                "Check that a tuner is reachable, then refresh again."
            } else {
                "No usable tuner was found; one or more replies were ignored."
            },
        ),
        (DiscoveryKind::Local, DiscoveryStatus::NoResponse) => fixed(
            "network-offline-symbolic",
            "No valid HDHomeRun replies received",
            "Check that a tuner is reachable, then refresh again.",
        ),
        (kind, DiscoveryStatus::Failed(failure)) => fixed(
            "dialog-error-symbolic",
            match kind {
                DiscoveryKind::Exact => "Device search failed",
                DiscoveryKind::Local | DiscoveryKind::Subnet => "Device discovery failed",
            },
            discovery_failure_description(kind, failure),
        ),
        // Local idle, and the subnet states handled above.
        _ => fixed(
            "network-wired-symbolic",
            "No HDHomeRun devices",
            "Choose Refresh to search your local network.",
        ),
    }
}

fn discovery_failure_description(kind: DiscoveryKind, failure: DiscoveryFailure) -> &'static str {
    match (kind, failure) {
        (DiscoveryKind::Local, DiscoveryFailure::InterfaceEnumeration) => {
            "Balun could not inspect this computer's network interfaces."
        }
        (DiscoveryKind::Exact, DiscoveryFailure::InterfaceEnumeration) => {
            "The exact-address device search could not be completed."
        }
        (DiscoveryKind::Local, DiscoveryFailure::Network) => {
            "The local discovery scan could not be completed."
        }
        (DiscoveryKind::Exact, DiscoveryFailure::Network) => {
            "The exact-address discovery request could not be completed."
        }
        (_, DiscoveryFailure::ExactTargetLimitReached) => {
            "This session has reached its limit for distinct device addresses."
        }
        // Subnet failures have their own translated copy; these never reach
        // a local or exact operation.
        (
            _,
            DiscoveryFailure::Internal
            | DiscoveryFailure::SubnetUnavailable
            | DiscoveryFailure::SubnetConfirmationStale
            | DiscoveryFailure::NetworkChanged,
        )
        | (DiscoveryKind::Subnet, _) => "Device discovery stopped because of an internal error.",
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
    use super::*;

    #[test]
    fn successful_replies_are_announced_once_and_other_outcomes_persist() {
        const REPLY: &str = "HDHomeRun device reply received.";
        let reply = Some(REPLY);
        let first = OperationGeneration::new(3);
        let second = OperationGeneration::new(4);
        let exact_ready = |announced, shown| {
            plan_terminal_banner(
                DiscoveryKind::Exact,
                DiscoveryStatus::Ready,
                reply,
                first,
                announced,
                shown,
            )
        };
        assert_eq!(exact_ready(None, ""), BannerChange::Announce(REPLY));
        assert_eq!(exact_ready(Some(second), ""), BannerChange::Announce(REPLY));
        assert_eq!(
            exact_ready(Some(first), REPLY),
            BannerChange::Keep,
            "the same outcome is left to its timer"
        );
        assert_eq!(
            exact_ready(
                Some(first),
                "Network changed; stale device addresses were dropped."
            ),
            BannerChange::Clear,
            "a notice that borrowed the banner is cleared on the next snapshot"
        );
        assert_eq!(
            exact_ready(Some(first), ""),
            BannerChange::Keep,
            "once the timer has run, the same operation is not announced again"
        );
        assert_eq!(
            plan_terminal_banner(
                DiscoveryKind::Exact,
                DiscoveryStatus::NoResponse,
                Some("No valid HDHomeRun reply was received."),
                first,
                Some(first),
                "",
            ),
            BannerChange::Reveal("No valid HDHomeRun reply was received.")
        );
        assert_eq!(
            plan_terminal_banner(
                DiscoveryKind::Local,
                DiscoveryStatus::Ready,
                None,
                first,
                Some(first),
                REPLY,
            ),
            BannerChange::Hide
        );
    }

    #[test]
    fn network_change_notice_appears_only_when_evidence_was_retired() {
        assert_eq!(
            network_change_banner_title(NetworkChangeSummary::INITIAL),
            None
        );
        assert_eq!(
            network_change_banner_title(NetworkChangeSummary::new(1, 0, 0)),
            None
        );
        assert_eq!(
            network_change_banner_title(NetworkChangeSummary::new(2, 0, 3)),
            Some("Network changed; stale device addresses were dropped.")
        );
        assert_eq!(
            network_change_banner_title(NetworkChangeSummary::new(3, 1, 3)),
            Some("Network changed; devices that lost every address were removed.")
        );
    }

    #[test]
    fn discovery_failures_map_to_bounded_user_facing_copy() {
        assert_eq!(
            discovery_failure_description(
                DiscoveryKind::Local,
                DiscoveryFailure::InterfaceEnumeration
            ),
            "Balun could not inspect this computer's network interfaces."
        );
        assert_eq!(
            discovery_failure_description(DiscoveryKind::Local, DiscoveryFailure::Network),
            "The local discovery scan could not be completed."
        );
        assert_eq!(
            discovery_failure_description(DiscoveryKind::Local, DiscoveryFailure::Internal),
            "Device discovery stopped because of an internal error."
        );
        assert_eq!(
            discovery_failure_description(
                DiscoveryKind::Exact,
                DiscoveryFailure::ExactTargetLimitReached
            ),
            "This session has reached its limit for distinct device addresses."
        );
    }

    #[test]
    fn exact_discovery_copy_is_address_free_and_never_describes_a_local_scan() {
        let sentinel = "198.51.100.247";
        for status in [
            DiscoveryStatus::Idle,
            DiscoveryStatus::Refreshing,
            DiscoveryStatus::Ready,
            DiscoveryStatus::NoResponse,
            DiscoveryStatus::Failed(DiscoveryFailure::Network),
        ] {
            let presentation = discovery_presentation(DiscoveryKind::Exact, status, 0);
            let copy = format!(
                "{} {} {}",
                presentation.icon_name, presentation.title, presentation.description
            );
            assert!(!copy.contains(sentinel));
            assert!(!copy.to_ascii_lowercase().contains("local"));
            assert!(!copy.contains("local network"));
            assert!(!copy.contains("local discovery"));
        }

        let stopped = discovery_presentation(DiscoveryKind::Exact, DiscoveryStatus::Idle, 0);
        assert_eq!(stopped.title, "Device search stopped");
        assert_eq!(
            stopped.description,
            "No exact-address discovery request is running."
        );

        let no_response =
            discovery_presentation(DiscoveryKind::Exact, DiscoveryStatus::NoResponse, 0);
        assert_eq!(no_response.title, "No valid HDHomeRun reply received");
        assert_eq!(
            no_response.description,
            "Check that the entered address is reachable, then try again."
        );
    }

    #[test]
    fn discovery_actions_offer_only_stop_while_work_is_refreshing() {
        assert_eq!(
            discovery_actions_presentation(DiscoveryStatus::Refreshing),
            DiscoveryActionsPresentation {
                start_sensitive: false,
                cancel_visible: true,
                cancel_sensitive: true,
            }
        );

        for status in [
            DiscoveryStatus::Idle,
            DiscoveryStatus::Ready,
            DiscoveryStatus::NoResponse,
            DiscoveryStatus::Failed(DiscoveryFailure::Network),
        ] {
            assert_eq!(
                discovery_actions_presentation(status),
                DiscoveryActionsPresentation {
                    start_sensitive: true,
                    cancel_visible: false,
                    cancel_sensitive: false,
                }
            );
        }
    }

    #[test]
    fn retained_rows_keep_exact_terminal_outcomes_visible_and_address_free() {
        let sentinel = "198.51.100.247";
        for (status, expected) in [
            (
                DiscoveryStatus::NoResponse,
                "No valid HDHomeRun reply was received.",
            ),
            (
                DiscoveryStatus::Failed(DiscoveryFailure::Network),
                "Exact-address device search failed.",
            ),
            (
                DiscoveryStatus::Failed(DiscoveryFailure::ExactTargetLimitReached),
                "Device address limit reached for this session.",
            ),
        ] {
            let title = terminal_banner_title(DiscoveryKind::Exact, status, true)
                .expect("retained rows require an exact terminal outcome banner");
            assert_eq!(title, expected);
            assert!(!title.contains(sentinel));
        }

        assert_eq!(
            terminal_banner_title(DiscoveryKind::Exact, DiscoveryStatus::NoResponse, false),
            None
        );
        assert_eq!(
            terminal_banner_title(DiscoveryKind::Exact, DiscoveryStatus::Refreshing, true),
            None
        );
        assert_eq!(
            terminal_banner_title(DiscoveryKind::Local, DiscoveryStatus::Ready, true),
            None
        );
    }

    #[test]
    fn retained_rows_keep_local_discovery_failures_visible() {
        assert_eq!(
            terminal_banner_title(
                DiscoveryKind::Local,
                DiscoveryStatus::Failed(DiscoveryFailure::Network),
                true
            )
            .as_deref(),
            Some("Local device discovery failed.")
        );
        assert_eq!(
            terminal_banner_title(
                DiscoveryKind::Local,
                DiscoveryStatus::Failed(DiscoveryFailure::Network),
                false
            ),
            None
        );
    }

    #[test]
    fn unchanged_devices_keep_their_rows_and_changed_ones_refresh_in_place() {
        use balun::domain::DeviceId;

        let device = |id, name: Option<&str>| {
            DeviceSummary::new(
                DeviceId::new(id).unwrap(),
                name.map(str::to_owned),
                None,
                Some(2),
                "192.0.2.10:65001".parse().unwrap(),
                vec!["192.0.2.10:65001".parse().unwrap()],
            )
            .unwrap()
        };
        let listed = [device(0x105A_1232, None), device(0x105B_1233, None)];
        let (rows, refreshed) = reconcile_rows(&[], &listed);
        assert_eq!(rows.len(), 2);
        assert!(refreshed.is_empty(), "new rows are drawn when bound");

        let (same, refreshed) = reconcile_rows(&rows, &listed);
        assert_eq!(same, rows, "the same devices keep the same row objects");
        assert!(refreshed.is_empty());

        // A loaded lineup names the selected device; its row keeps its
        // object (and so its widget and focus) and is redrawn.
        let named = [device(0x105A_1232, Some("Den")), listed[1].clone()];
        let (same, refreshed) = reconcile_rows(&rows, &named);
        assert_eq!(same, rows);
        assert_eq!(refreshed, [rows[0].clone()]);
        assert_eq!(rows[0].title(), "Den · 105A1232");

        let added = [
            named[0].clone(),
            device(0x105A_1243, None),
            named[1].clone(),
        ];
        let (grown, refreshed) = reconcile_rows(&rows, &added);
        assert_eq!(grown.len(), 3);
        assert_eq!(grown[0], rows[0]);
        assert_eq!(grown[2], rows[1], "a retained device keeps its row");
        assert_eq!(
            grown[1].device_id(),
            Some(DeviceId::new(0x105A_1243).unwrap())
        );
        assert!(refreshed.is_empty());

        let (shrunk, _) = reconcile_rows(&grown, &added[1..]);
        assert_eq!(shrunk, grown[1..]);
    }

    #[test]
    fn subnet_search_is_offered_only_with_an_idle_lane_and_healthy_observation() {
        for (start_sensitive, observing, sensitive) in [
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, false, false),
        ] {
            assert_eq!(
                subnet_action_presentation(start_sensitive, observing),
                SubnetActionPresentation {
                    sensitive,
                    available: observing,
                }
            );
        }
        use balun::discovery::{ObservationGeneration, ObservationState};
        let ready = ApplicationSnapshot::initial()
            .with_observation(ObservationState::Ready(ObservationGeneration::FIRST));
        assert!(subnet_search_sensitive(&ready));
        assert!(!subnet_search_sensitive(&ApplicationSnapshot::initial()));
        let labels = SubnetEntryLabels::current();
        assert_eq!(subnet_action_tooltip(true), labels.button);
        assert_eq!(subnet_action_tooltip(false), labels.unavailable);
    }

    #[test]
    fn subnet_outcomes_have_their_own_copy_and_never_look_complete_when_cut_short() {
        use balun::controller::DiscoveryIncomplete;

        let incomplete = [
            DiscoveryStatus::Incomplete(DiscoveryIncomplete::Deadline),
            DiscoveryStatus::Incomplete(DiscoveryIncomplete::DeviceLimit),
            DiscoveryStatus::Incomplete(DiscoveryIncomplete::Unprobed),
        ];
        let complete = discovery_presentation(DiscoveryKind::Subnet, DiscoveryStatus::Ready, 0);
        let empty = discovery_presentation(DiscoveryKind::Subnet, DiscoveryStatus::NoResponse, 0);
        for status in incomplete {
            let presentation = discovery_presentation(DiscoveryKind::Subnet, status, 0);
            assert_eq!(presentation.icon_name, "dialog-warning-symbolic");
            assert_ne!(presentation.title, complete.title);
            assert_ne!(presentation.title, empty.title);
            let banner = terminal_banner_title(DiscoveryKind::Subnet, status, true)
                .expect("an incomplete search stays visible above listed devices");
            assert_eq!(
                plan_terminal_banner(
                    DiscoveryKind::Subnet,
                    status,
                    Some(&banner),
                    OperationGeneration::new(1),
                    None,
                    "",
                ),
                BannerChange::Reveal(&banner)
            );
        }
        for failure in [
            DiscoveryFailure::SubnetUnavailable,
            DiscoveryFailure::SubnetConfirmationStale,
            DiscoveryFailure::NetworkChanged,
            DiscoveryFailure::Internal,
        ] {
            let status = DiscoveryStatus::Failed(failure);
            let presentation = discovery_presentation(DiscoveryKind::Subnet, status, 0);
            assert_eq!(presentation.icon_name, "dialog-error-symbolic");
            assert!(terminal_banner_title(DiscoveryKind::Subnet, status, true).is_some());
            assert!(terminal_banner_title(DiscoveryKind::Subnet, status, false).is_none());
        }
        for status in [DiscoveryStatus::Idle, DiscoveryStatus::Refreshing] {
            assert!(terminal_banner_title(DiscoveryKind::Subnet, status, true).is_none());
            assert!(
                !discovery_presentation(DiscoveryKind::Subnet, status, 0)
                    .title
                    .is_empty()
            );
        }
        // A completed search is announced once, like an exact reply.
        let done = terminal_banner_title(DiscoveryKind::Subnet, DiscoveryStatus::Ready, true)
            .expect("a completed search is announced");
        assert_eq!(
            plan_terminal_banner(
                DiscoveryKind::Subnet,
                DiscoveryStatus::Ready,
                Some(&done),
                OperationGeneration::new(2),
                None,
                "",
            ),
            BannerChange::Announce(&done)
        );
        assert_eq!(
            discovery_failure_description(DiscoveryKind::Subnet, DiscoveryFailure::Network),
            "Device discovery stopped because of an internal error."
        );
    }

    #[test]
    fn nested_application_guard_restores_the_prior_state() {
        let flag = Rc::new(Cell::new(false));
        let outer = SnapshotApplicationGuard::enter(Rc::clone(&flag));
        assert!(flag.get());
        {
            let _inner = SnapshotApplicationGuard::enter(Rc::clone(&flag));
            assert!(flag.get());
        }
        assert!(flag.get());
        drop(outer);
        assert!(!flag.get());
    }

    #[test]
    #[ignore = "requires the isolated display supplied by scripts/test-desktop-lifecycle.sh"]
    fn device_sidebar_accessibility_contract() {
        adw::init().expect("initialize libadwaita for device accessibility test");
        let sidebar = build();
        assert_eq!(sidebar.list.accessible_role(), gtk::AccessibleRole::List);
        assert!(sidebar.list.is_focusable());
        assert!(!sidebar.list.is_single_click_activate());
        assert_eq!(
            sidebar.cancel_discovery_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            sidebar.exact_discovery_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            sidebar.subnet_search_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert!(
            !sidebar.subnet_search_button.is_sensitive(),
            "subnet search waits for healthy observation"
        );

        // Healthy observation offers the action; a running subnet search
        // offers only Stop, and its progress copy is the subnet's own.
        use balun::controller::{DiscoveryState, SelectedLineupState, SnapshotRevision};
        use balun::discovery::{ObservationGeneration, ObservationState};
        let snapshot = |revision, discovery| {
            ApplicationSnapshot::new(
                SnapshotRevision::new(revision),
                OperationGeneration::new(1),
                OperationGeneration::INITIAL,
                discovery,
                [],
                None,
                SelectedLineupState::unselected(OperationGeneration::INITIAL),
            )
            .unwrap()
            .with_observation(ObservationState::Ready(ObservationGeneration::FIRST))
        };
        let generation = OperationGeneration::new(1);
        sidebar.apply_snapshot(&snapshot(
            1,
            DiscoveryState::idle_for(generation, DiscoveryKind::Local),
        ));
        assert!(sidebar.subnet_search_button.is_sensitive());
        assert_eq!(
            sidebar.subnet_search_button.tooltip_text().as_deref(),
            Some(&*SubnetEntryLabels::current().button)
        );
        sidebar.apply_snapshot(&snapshot(
            2,
            DiscoveryState::refreshing_for(generation, DiscoveryKind::Subnet),
        ));
        assert!(!sidebar.subnet_search_button.is_sensitive());
        assert!(sidebar.cancel_discovery_button.is_visible());
        assert!(sidebar.cancel_discovery_button.is_sensitive());
        assert_eq!(
            sidebar.status.title().as_str(),
            subnet_search::status(DiscoveryStatus::Refreshing).0
        );
        assert_eq!(
            sidebar.refresh_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            sidebar.spinner.accessible_role(),
            gtk::AccessibleRole::ProgressBar
        );
    }

    #[test]
    fn device_row_accessible_label_formatting() {
        let format_label = |title: &str, subtitle: &str| {
            if subtitle.is_empty() {
                title.to_string()
            } else {
                format!("{title}, {subtitle}")
            }
        };

        assert_eq!(
            format_label("HDHomeRun CONNECT", "10800000"),
            "HDHomeRun CONNECT, 10800000"
        );
        assert_eq!(format_label("HDHomeRun PRIME", ""), "HDHomeRun PRIME");
    }
}
