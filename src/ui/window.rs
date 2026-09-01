//! Top-level adaptive three-pane window and controller/GLib bridge.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, ControllerCommand, ControllerHandle, ControllerRuntime,
};

use super::objects::DeviceRowObject;
use super::{channel_sidebar, device_sidebar, player_view};

const DEFAULT_WIDTH: i32 = 1_200;
const DEFAULT_HEIGHT: i32 = 720;
const DEVICE_SIDEBAR_MIN_WIDTH: f64 = 160.0;
const DEVICE_SIDEBAR_MAX_WIDTH: f64 = 220.0;
const CHANNEL_SIDEBAR_MIN_WIDTH: f64 = 240.0;
const CHANNEL_SIDEBAR_MAX_WIDTH: f64 = 360.0;
const COLLAPSE_DEVICE_SIDEBAR_AT: f64 = 1_000.0;
const COLLAPSE_CHANNEL_SIDEBAR_AT: f64 = 700.0;

/// Build Balun's single application window.
pub(crate) fn build(
    application: &adw::Application,
    controller: ControllerRuntime,
    shutdown_failed: Rc<Cell<bool>>,
) -> adw::ApplicationWindow {
    let device_sidebar = device_sidebar::build();
    let channel_sidebar = channel_sidebar::build();
    let player_view = player_view::build();

    let device_page = adw::NavigationPage::new(device_sidebar.root(), "Devices");
    let channel_page = adw::NavigationPage::new(channel_sidebar.root(), "Channels");
    let player_page = adw::NavigationPage::new(&player_view, "Live TV");

    let channel_and_player = adw::NavigationSplitView::builder()
        .sidebar(&channel_page)
        .content(&player_page)
        .min_sidebar_width(CHANNEL_SIDEBAR_MIN_WIDTH)
        .max_sidebar_width(CHANNEL_SIDEBAR_MAX_WIDTH)
        .sidebar_width_fraction(0.30)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .build();
    let channel_and_player_page =
        adw::NavigationPage::new(&channel_and_player, "Channels and live TV");

    let device_and_content = adw::NavigationSplitView::builder()
        .sidebar(&device_page)
        .content(&channel_and_player_page)
        .min_sidebar_width(DEVICE_SIDEBAR_MIN_WIDTH)
        .max_sidebar_width(DEVICE_SIDEBAR_MAX_WIDTH)
        .sidebar_width_fraction(0.18)
        .sidebar_width_unit(adw::LengthUnit::Sp)
        .build();

    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Balun")
        .default_width(DEFAULT_WIDTH)
        .default_height(DEFAULT_HEIGHT)
        .content(&device_and_content)
        .build();

    let medium_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_DEVICE_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    medium_breakpoint.add_setters(&[(&device_and_content, "collapsed", true)]);
    window.add_breakpoint(medium_breakpoint);

    let compact_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_CHANNEL_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    compact_breakpoint.add_setters(&[(&channel_and_player, "collapsed", true)]);
    window.add_breakpoint(compact_breakpoint);

    let handle = controller.handle();
    let mut snapshots = handle.subscribe();
    let initial = Arc::clone(&snapshots.borrow_and_update());
    device_sidebar.apply_snapshot(&initial);
    channel_sidebar.apply_snapshot(&initial);
    device_and_content.set_show_content(initial.selected_device().is_some());
    let accepted = Rc::new(RefCell::new(initial));

    connect_refresh(&device_sidebar, &handle);
    connect_device_selection(&device_sidebar, &handle, &accepted);
    spawn_snapshot_reducer(
        snapshots,
        Rc::clone(&accepted),
        device_sidebar,
        channel_sidebar,
        device_and_content,
    );
    connect_joined_shutdown(&window, controller, shutdown_failed);

    window
}

fn connect_refresh(sidebar: &device_sidebar::DeviceSidebar, controller: &ControllerHandle) {
    let controller = controller.clone();
    sidebar.refresh_button().connect_clicked(move |button| {
        // Close the tiny interval before the Refreshing snapshot arrives so a
        // fast double-click cannot enqueue redundant supersessions.
        button.set_sensitive(false);
        if controller
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .is_err()
        {
            button.set_sensitive(true);
        }
    });
}

fn connect_device_selection(
    sidebar: &device_sidebar::DeviceSidebar,
    controller: &ControllerHandle,
    accepted: &Rc<RefCell<Arc<ApplicationSnapshot>>>,
) {
    let applying_snapshot = sidebar.snapshot_application_flag();
    let accepted = Rc::clone(accepted);
    let controller = controller.clone();
    sidebar
        .selection()
        .connect_selected_notify(move |selection| {
            if applying_snapshot.get() {
                return;
            }

            let selected_device = selection
                .selected_item()
                .and_then(|item| item.downcast::<DeviceRowObject>().ok())
                .and_then(|row| row.device_id());
            let authoritative_device = accepted.borrow().selected_device();
            // Every user-originated change is a superseding intent. In
            // particular, returning to the last published selection must not
            // be suppressed: a different selection command can still be
            // queued or in flight on the controller thread.
            let command = selection_command(selected_device);
            if controller.try_send(command).is_err() {
                restore_device_selection(selection, authoritative_device, &applying_snapshot);
            }
        });
}

fn selection_command(selected_device: Option<balun::domain::DeviceId>) -> ControllerCommand {
    selected_device.map_or(
        ControllerCommand::ClearSelection,
        ControllerCommand::SelectDevice,
    )
}

fn restore_device_selection(
    selection: &gtk::SingleSelection,
    device_id: Option<balun::domain::DeviceId>,
    applying_snapshot: &Rc<Cell<bool>>,
) {
    let selected_position = device_id.and_then(|device_id| {
        let model = selection.model()?;
        (0..model.n_items()).find(|position| {
            model
                .item(*position)
                .and_then(|item| item.downcast::<DeviceRowObject>().ok())
                .and_then(|row| row.device_id())
                == Some(device_id)
        })
    });
    let previous = applying_snapshot.replace(true);
    selection.set_selected(selected_position.unwrap_or(gtk::INVALID_LIST_POSITION));
    applying_snapshot.set(previous);
}

fn spawn_snapshot_reducer(
    mut snapshots: tokio::sync::watch::Receiver<Arc<ApplicationSnapshot>>,
    accepted: Rc<RefCell<Arc<ApplicationSnapshot>>>,
    device_sidebar: device_sidebar::DeviceSidebar,
    channel_sidebar: channel_sidebar::ChannelSidebar,
    device_and_content: adw::NavigationSplitView,
) {
    gtk::glib::MainContext::default().spawn_local(async move {
        while snapshots.changed().await.is_ok() {
            let candidate = Arc::clone(&snapshots.borrow_and_update());
            let can_replace = {
                let current = accepted.borrow();
                candidate.can_replace(&current)
            };
            if !can_replace {
                continue;
            }

            device_sidebar.apply_snapshot(&candidate);
            channel_sidebar.apply_snapshot(&candidate);
            device_and_content.set_show_content(candidate.selected_device().is_some());
            accepted.replace(candidate);
        }
    });
}

fn connect_joined_shutdown(
    window: &adw::ApplicationWindow,
    controller: ControllerRuntime,
    shutdown_failed: Rc<Cell<bool>>,
) {
    let controller = Rc::new(RefCell::new(Some(controller)));
    let shutdown_started = Rc::new(Cell::new(false));
    let shutdown_complete = Rc::new(Cell::new(false));

    window.connect_close_request(move |window| {
        if shutdown_complete.get() {
            return gtk::glib::Propagation::Proceed;
        }
        if shutdown_started.replace(true) {
            return gtk::glib::Propagation::Stop;
        }

        window.set_sensitive(false);
        let Some(controller) = controller.borrow_mut().take() else {
            shutdown_complete.set(true);
            return gtk::glib::Propagation::Proceed;
        };
        controller.begin_shutdown();

        let shutdown_complete = Rc::clone(&shutdown_complete);
        let shutdown_failed = Rc::clone(&shutdown_failed);
        let window = window.downgrade();
        gtk::glib::MainContext::default().spawn_local(async move {
            match gtk::gio::spawn_blocking(move || controller.join()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    shutdown_failed.set(true);
                    eprintln!("Balun controller shutdown failed: {error}");
                }
                Err(_) => {
                    shutdown_failed.set(true);
                    eprintln!("Balun controller shutdown worker panicked");
                }
            }
            shutdown_complete.set(true);
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
        gtk::glib::Propagation::Stop
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_selection_changes_always_map_to_superseding_commands() {
        let device_id = balun::domain::DeviceId::new(0x105A_1232).unwrap();

        assert_eq!(
            selection_command(Some(device_id)),
            ControllerCommand::SelectDevice(device_id)
        );
        assert_eq!(selection_command(None), ControllerCommand::ClearSelection);
    }
}
