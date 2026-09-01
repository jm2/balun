//! Virtualized HDHomeRun device sidebar.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use balun::controller::{ApplicationSnapshot, DiscoveryFailure, DiscoveryStatus};

use super::objects::DeviceRowObject;

const STATUS_PAGE_NAME: &str = "status";
const DEVICE_LIST_PAGE_NAME: &str = "devices";

/// GTK parts for the device pane.
///
/// The Refresh button is intentionally exposed but left disconnected here.
/// The application bridge decides when a bounded discovery command is
/// admitted; constructing this sidebar performs no network work.
#[derive(Clone)]
pub(crate) struct DeviceSidebar {
    root: adw::ToolbarView,
    store: gtk::gio::ListStore,
    selection: gtk::SingleSelection,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    refresh_button: gtk::Button,
    applying_snapshot: Rc<Cell<bool>>,
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
    pub(crate) fn selection(&self) -> &gtk::SingleSelection {
        &self.selection
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
        let refreshing = discovery.status() == DiscoveryStatus::Refreshing;
        self.refresh_button.set_sensitive(!refreshing);

        let show_status = rows.is_empty();
        self.spinner.set_visible(show_status && refreshing);
        self.spinner.set_spinning(show_status && refreshing);
        apply_empty_presentation(&self.status, discovery.status(), discovery.issue_count());
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

    let refresh_button = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Refresh devices")
        .css_classes(["flat"])
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&refresh_button);

    let root = adw::ToolbarView::new();
    root.add_top_bar(&header);
    root.set_content(Some(&stack));

    DeviceSidebar {
        root,
        store,
        selection,
        stack,
        status,
        spinner,
        refresh_button,
        applying_snapshot: Rc::new(Cell::new(false)),
    }
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
        list_item.set_accessible_label(&format!("{title_text}, {subtitle_text}"));
        list_item.set_selectable(true);
        list_item.set_activatable(false);
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
    discovery_status: DiscoveryStatus,
    issue_count: u16,
) {
    match discovery_status {
        DiscoveryStatus::Idle => {
            status.set_icon_name(Some("network-wired-symbolic"));
            status.set_title("No HDHomeRun devices");
            status.set_description(Some("Choose Refresh to search your local network."));
        }
        DiscoveryStatus::Refreshing => {
            status.set_icon_name(Some("network-transmit-receive-symbolic"));
            status.set_title("Searching for HDHomeRun devices");
            status.set_description(Some("Waiting for replies from this local network."));
        }
        DiscoveryStatus::Ready => {
            status.set_icon_name(Some("network-offline-symbolic"));
            status.set_title("No HDHomeRun devices found");
            status.set_description(Some(if issue_count == 0 {
                "Check that a tuner is reachable, then refresh again."
            } else {
                "No usable tuner was found; one or more replies were ignored."
            }));
        }
        DiscoveryStatus::Failed(failure) => {
            status.set_icon_name(Some("dialog-error-symbolic"));
            status.set_title("Device discovery failed");
            status.set_description(Some(discovery_failure_description(failure)));
        }
    }
}

fn discovery_failure_description(failure: DiscoveryFailure) -> &'static str {
    match failure {
        DiscoveryFailure::InterfaceEnumeration => {
            "Balun could not inspect this computer's network interfaces."
        }
        DiscoveryFailure::Network => "The local discovery scan could not be completed.",
        DiscoveryFailure::Internal => "Device discovery stopped because of an internal error.",
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
    fn discovery_failures_map_to_bounded_user_facing_copy() {
        assert_eq!(
            discovery_failure_description(DiscoveryFailure::InterfaceEnumeration),
            "Balun could not inspect this computer's network interfaces."
        );
        assert_eq!(
            discovery_failure_description(DiscoveryFailure::Network),
            "The local discovery scan could not be completed."
        );
        assert_eq!(
            discovery_failure_description(DiscoveryFailure::Internal),
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
}
