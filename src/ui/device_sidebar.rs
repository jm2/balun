//! Virtualized HDHomeRun device sidebar.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, DiscoveryFailure, DiscoveryKind, DiscoveryStatus, NetworkChangeSummary,
    RoutedAvailability,
};

use super::objects::DeviceRowObject;

const STATUS_PAGE_NAME: &str = "status";
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
    routed_discovery_button: gtk::Button,
    routed_menu_button: gtk::MenuButton,
    refresh_button: gtk::Button,
    applying_snapshot: Rc<Cell<bool>>,
    /// The last network-change sequence shown, so the notice appears once
    /// per reconciliation and yields to the next publication.
    network_sequence: Rc<Cell<u64>>,
}

/// Window action that forgets every remembered routed approval.
pub(crate) const FORGET_ROUTED_APPROVALS_ACTION: &str = "forget-routed-approvals";

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
    pub(crate) fn cancel_discovery_button(&self) -> &gtk::Button {
        &self.cancel_discovery_button
    }

    #[must_use]
    pub(crate) fn routed_discovery_button(&self) -> &gtk::Button {
        &self.routed_discovery_button
    }

    #[must_use]
    pub(crate) fn selection(&self) -> &gtk::SingleSelection {
        &self.selection
    }

    /// Connect an activation callback invoked when any device row is activated
    /// via Enter key or single click.
    pub(crate) fn connect_device_activated<F>(&self, callback: F)
    where
        F: Fn() + 'static,
    {
        let selection = self.selection.clone();
        self.list.connect_activate(move |_, position| {
            selection.set_selected(position);
            callback();
        });
    }

    /// Share the non-GObject reentrancy flag with the window bridge without
    /// making a GTK selection callback retain this complete sidebar.
    #[must_use]
    pub(crate) fn snapshot_application_flag(&self) -> Rc<Cell<bool>> {
        Rc::clone(&self.applying_snapshot)
    }

    /// Replace every visible device and status field from one immutable,
    /// URL-free controller publication.
    ///
    /// Numeric list positions are never retained. The authoritative DeviceID
    /// is resolved in the replacement model before selection is restored.
    pub(crate) fn apply_snapshot(&self, snapshot: &ApplicationSnapshot) {
        let rows = snapshot
            .devices()
            .iter()
            .map(DeviceRowObject::from_summary)
            .collect::<Vec<_>>();
        let selected_position = snapshot.selected_device().and_then(|selected| {
            snapshot
                .devices()
                .iter()
                .position(|device| device.device_id() == selected)
                .and_then(|position| u32::try_from(position).ok())
        });

        let _applying = SnapshotApplicationGuard::enter(Rc::clone(&self.applying_snapshot));
        self.store.splice(0, self.store.n_items(), &rows);
        self.selection
            .set_selected(selected_position.unwrap_or(gtk::INVALID_LIST_POSITION));

        let discovery = snapshot.discovery();
        let actions = discovery_actions_presentation(discovery.status());
        self.refresh_button.set_sensitive(actions.start_sensitive);
        self.exact_discovery_button
            .set_sensitive(actions.start_sensitive);
        let routed_available = snapshot.routed().availability() == RoutedAvailability::Available;
        self.routed_discovery_button.set_visible(routed_available);
        self.routed_discovery_button
            .set_sensitive(actions.start_sensitive);
        self.routed_menu_button.set_visible(routed_available);
        self.cancel_discovery_button
            .set_sensitive(actions.cancel_sensitive);
        self.cancel_discovery_button
            .set_visible(actions.cancel_visible);

        let show_status = rows.is_empty();
        let refreshing = discovery.status() == DiscoveryStatus::Refreshing;
        apply_terminal_banner(
            &self.terminal_banner,
            discovery.kind(),
            discovery.status(),
            !show_status,
        );
        let network = snapshot.network();
        if self.network_sequence.replace(network.sequence()) != network.sequence()
            && let Some(title) = network_change_banner_title(network)
        {
            self.terminal_banner.set_title(title);
            self.terminal_banner.set_revealed(true);
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

/// Build the device pane without starting discovery or any other network work.
#[must_use]
pub(crate) fn build() -> DeviceSidebar {
    let store = gtk::gio::ListStore::new::<DeviceRowObject>();
    let selection = gtk::SingleSelection::new(Some(store.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

    let factory = device_factory();
    let list = gtk::ListView::builder()
        .model(&selection)
        .factory(&factory)
        .single_click_activate(false)
        .css_classes(["navigation-sidebar"])
        .accessible_role(gtk::AccessibleRole::List)
        .vexpand(true)
        .build();
    list.update_property(&[gtk::accessible::Property::Label("HDHomeRun devices")]);
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

    let exact_discovery_button = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Find device by address")
        .css_classes(["flat"])
        .build();
    exact_discovery_button
        .update_property(&[gtk::accessible::Property::Label("Find device by address")]);
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
    // Routed actions stay hidden until a snapshot says the platform offers
    // them; the menu holds the one destructive gesture, forgetting approvals.
    let routed_discovery_button = gtk::Button::builder()
        .icon_name("network-vpn-symbolic")
        .tooltip_text("Search routes behind your tunnel")
        .css_classes(["flat"])
        .visible(false)
        .build();
    routed_discovery_button.update_property(&[gtk::accessible::Property::Label(
        "Search routes behind your tunnel",
    )]);
    let routed_menu = gtk::gio::Menu::new();
    routed_menu.append(
        Some("Forget routed approvals"),
        Some(&format!("win.{FORGET_ROUTED_APPROVALS_ACTION}")),
    );
    let routed_menu_button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("More discovery options")
        .css_classes(["flat"])
        .menu_model(&routed_menu)
        .visible(false)
        .build();
    routed_menu_button
        .update_property(&[gtk::accessible::Property::Label("More discovery options")]);
    let discovery_actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    discovery_actions.append(&exact_discovery_button);
    discovery_actions.append(&routed_discovery_button);
    discovery_actions.append(&refresh_button);
    discovery_actions.append(&cancel_discovery_button);
    discovery_actions.append(&routed_menu_button);
    let header = adw::HeaderBar::new();
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
        routed_discovery_button,
        routed_menu_button,
        refresh_button,
        applying_snapshot: Rc::new(Cell::new(false)),
        network_sequence: Rc::new(Cell::new(0)),
    }
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

fn device_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, object| {
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
        list_item.set_child(Some(&row));
        reset_device_list_item(list_item);
    });
    factory.connect_bind(|_, object| {
        let Some(list_item) = object.downcast_ref::<gtk::ListItem>() else {
            return;
        };
        reset_device_list_item(list_item);

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
    status.set_title(presentation.title);
    status.set_description(Some(presentation.description));
}

fn apply_terminal_banner(
    banner: &adw::Banner,
    discovery_kind: DiscoveryKind,
    discovery_status: DiscoveryStatus,
    has_device_rows: bool,
) {
    if let Some(title) = terminal_banner_title(discovery_kind, discovery_status, has_device_rows) {
        banner.set_title(title);
        banner.set_revealed(true);
    } else {
        banner.set_revealed(false);
        banner.set_title("");
    }
}

/// Keep relevant terminal discovery outcomes visible when retained device
/// rows make the empty-state page unavailable. Copy is fixed and the
/// controller snapshot intentionally contains no target address.
fn terminal_banner_title(
    kind: DiscoveryKind,
    status: DiscoveryStatus,
    has_device_rows: bool,
) -> Option<&'static str> {
    if !has_device_rows {
        return None;
    }

    match (kind, status) {
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
        (DiscoveryKind::Routed, DiscoveryStatus::Ready) => Some("Routed discovery finished."),
        (DiscoveryKind::Routed, DiscoveryStatus::NoResponse) => {
            Some("No HDHomeRun replies from the approved routes.")
        }
        (DiscoveryKind::Routed, DiscoveryStatus::Failed(failure)) => {
            Some(routed_failure_banner_title(failure))
        }
        (_, DiscoveryStatus::Idle | DiscoveryStatus::Refreshing | DiscoveryStatus::Ready) => None,
    }
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

fn routed_failure_banner_title(failure: DiscoveryFailure) -> &'static str {
    match failure {
        DiscoveryFailure::RoutedNotApproved => "Routed discovery needs your approval.",
        DiscoveryFailure::RoutedCoolingDown => "Routed discovery is cooling down.",
        DiscoveryFailure::RoutedBusy => "Another routed scan is still reserved.",
        DiscoveryFailure::RoutedNoCandidates => "No tunnel route offers addresses to probe.",
        DiscoveryFailure::RoutedUnavailable => "Routed discovery is not available here.",
        DiscoveryFailure::RoutedProposalChanged => "The routed proposal changed; review it again.",
        DiscoveryFailure::RoutedUnconfirmed
        | DiscoveryFailure::InterfaceEnumeration
        | DiscoveryFailure::Network
        | DiscoveryFailure::ExactTargetLimitReached
        | DiscoveryFailure::Internal => "Routed discovery failed.",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DiscoveryPresentation {
    icon_name: &'static str,
    title: &'static str,
    description: &'static str,
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
    match (kind, status) {
        (DiscoveryKind::Local, DiscoveryStatus::Idle) => DiscoveryPresentation {
            icon_name: "network-wired-symbolic",
            title: "No HDHomeRun devices",
            description: "Choose Refresh to search your local network.",
        },
        (DiscoveryKind::Exact, DiscoveryStatus::Idle) => DiscoveryPresentation {
            icon_name: "process-stop-symbolic",
            title: "Device search stopped",
            description: "No exact-address discovery request is running.",
        },
        (DiscoveryKind::Local, DiscoveryStatus::Refreshing) => DiscoveryPresentation {
            icon_name: "network-transmit-receive-symbolic",
            title: "Searching for HDHomeRun devices",
            description: "Waiting for replies from this local network.",
        },
        (DiscoveryKind::Exact, DiscoveryStatus::Refreshing) => DiscoveryPresentation {
            icon_name: "network-transmit-receive-symbolic",
            title: "Finding HDHomeRun device",
            description: "Waiting for a reply from the entered address.",
        },
        (DiscoveryKind::Local, DiscoveryStatus::Ready) => DiscoveryPresentation {
            icon_name: "network-offline-symbolic",
            title: "No HDHomeRun devices found",
            description: if issue_count == 0 {
                "Check that a tuner is reachable, then refresh again."
            } else {
                "No usable tuner was found; one or more replies were ignored."
            },
        },
        (DiscoveryKind::Exact, DiscoveryStatus::Ready) => DiscoveryPresentation {
            icon_name: "network-wired-symbolic",
            title: "Device reply received",
            description: "The exact-address discovery request completed.",
        },
        (DiscoveryKind::Exact, DiscoveryStatus::NoResponse) => DiscoveryPresentation {
            icon_name: "network-offline-symbolic",
            title: "No valid HDHomeRun reply received",
            description: "Check that the entered address is reachable, then try again.",
        },
        (DiscoveryKind::Local, DiscoveryStatus::NoResponse) => DiscoveryPresentation {
            icon_name: "network-offline-symbolic",
            title: "No valid HDHomeRun replies received",
            description: "Check that a tuner is reachable, then refresh again.",
        },
        (DiscoveryKind::Routed, DiscoveryStatus::Idle) => DiscoveryPresentation {
            icon_name: "process-stop-symbolic",
            title: "Routed discovery stopped",
            description: "No routed discovery request is running.",
        },
        (DiscoveryKind::Routed, DiscoveryStatus::Refreshing) => DiscoveryPresentation {
            icon_name: "network-transmit-receive-symbolic",
            title: "Searching approved routes",
            description: "Probing the approved addresses behind your tunnel.",
        },
        (DiscoveryKind::Routed, DiscoveryStatus::Ready) => DiscoveryPresentation {
            icon_name: "network-offline-symbolic",
            title: "No HDHomeRun devices found behind the tunnel",
            description: if issue_count == 0 {
                "Every approved address was probed without a valid reply."
            } else {
                "No usable tuner was found; one or more probes were not completed."
            },
        },
        (DiscoveryKind::Routed, DiscoveryStatus::NoResponse) => DiscoveryPresentation {
            icon_name: "network-offline-symbolic",
            title: "No HDHomeRun replies from the approved routes",
            description: "Check that the tunnel is up and the remote tuner is powered.",
        },
        (kind, DiscoveryStatus::Failed(failure)) => DiscoveryPresentation {
            icon_name: "dialog-error-symbolic",
            title: match kind {
                DiscoveryKind::Local => "Device discovery failed",
                DiscoveryKind::Exact => "Device search failed",
                DiscoveryKind::Routed => "Routed discovery did not run",
            },
            description: discovery_failure_description(kind, failure),
        },
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
        (DiscoveryKind::Routed, DiscoveryFailure::InterfaceEnumeration) => {
            "Balun could not inspect this computer's routes."
        }
        (DiscoveryKind::Routed, DiscoveryFailure::Network) => {
            "The routed discovery scan could not be completed."
        }
        (_, DiscoveryFailure::ExactTargetLimitReached) => {
            "This session has reached its limit for distinct device addresses."
        }
        (_, DiscoveryFailure::RoutedUnavailable) => {
            "Routed discovery is not available on this system."
        }
        (_, DiscoveryFailure::RoutedNoCandidates) => {
            "No active tunnel route offers an address to probe."
        }
        (_, DiscoveryFailure::RoutedNotApproved) => {
            "Review and approve the routed proposal before it can run."
        }
        (_, DiscoveryFailure::RoutedBusy) => {
            "Wait for the current routed reservation to finish, then try again."
        }
        (_, DiscoveryFailure::RoutedCoolingDown) => {
            "Automatic routed discovery is cooling down; refresh to run it now."
        }
        (_, DiscoveryFailure::RoutedUnconfirmed) => {
            "The approval store could not confirm the reservation; try again shortly."
        }
        (_, DiscoveryFailure::RoutedProposalChanged) => {
            "The routed proposal changed since it was shown; review it again."
        }
        (_, DiscoveryFailure::Internal) => "Device discovery stopped because of an internal error.",
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
            ),
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
    fn routed_discovery_copy_is_topology_free_and_names_every_decision() {
        for status in [
            DiscoveryStatus::Idle,
            DiscoveryStatus::Refreshing,
            DiscoveryStatus::Ready,
            DiscoveryStatus::NoResponse,
            DiscoveryStatus::Failed(DiscoveryFailure::RoutedNotApproved),
        ] {
            let presentation = discovery_presentation(DiscoveryKind::Routed, status, 0);
            assert!(!presentation.title.is_empty());
            assert!(!presentation.description.is_empty());
            assert!(!presentation.title.to_ascii_lowercase().contains("local"));
            assert!(!presentation.description.contains("172."));
        }
        let failures = [
            DiscoveryFailure::RoutedUnavailable,
            DiscoveryFailure::RoutedNoCandidates,
            DiscoveryFailure::RoutedNotApproved,
            DiscoveryFailure::RoutedBusy,
            DiscoveryFailure::RoutedCoolingDown,
            DiscoveryFailure::RoutedUnconfirmed,
            DiscoveryFailure::RoutedProposalChanged,
            DiscoveryFailure::Network,
        ];
        let descriptions = failures
            .iter()
            .map(|failure| discovery_failure_description(DiscoveryKind::Routed, *failure))
            .collect::<Vec<_>>();
        let banners = failures
            .iter()
            .map(|failure| routed_failure_banner_title(*failure))
            .collect::<Vec<_>>();
        for (description, banner) in descriptions.iter().zip(&banners) {
            assert!(!description.is_empty());
            assert!(!banner.is_empty());
        }
        assert_eq!(
            descriptions
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            descriptions.len(),
            "every routed decision reads differently"
        );
        assert_eq!(
            terminal_banner_title(
                DiscoveryKind::Routed,
                DiscoveryStatus::Failed(DiscoveryFailure::RoutedCoolingDown),
                true
            ),
            Some("Routed discovery is cooling down.")
        );
        assert_eq!(
            terminal_banner_title(DiscoveryKind::Routed, DiscoveryStatus::Refreshing, true),
            None
        );
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
            sidebar.refresh_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            sidebar.routed_discovery_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert_eq!(
            sidebar.routed_menu_button.accessible_role(),
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
