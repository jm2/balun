//! Top-level adaptive three-pane window and controller/GLib bridge.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, ControllerCommand, ControllerHandle, ControllerRuntime,
};
use balun::playback::{PlaybackInitializationError, PlaybackRuntime};

use super::objects::DeviceRowObject;
use super::{channel_sidebar, device_sidebar, exact_discovery_dialog, player_view};

const DEFAULT_WIDTH: i32 = 1_200;
const DEFAULT_HEIGHT: i32 = 720;
const DEVICE_SIDEBAR_MIN_WIDTH: f64 = 160.0;
const DEVICE_SIDEBAR_MAX_WIDTH: f64 = 220.0;
const CHANNEL_SIDEBAR_MIN_WIDTH: f64 = 240.0;
const CHANNEL_SIDEBAR_MAX_WIDTH: f64 = 360.0;
const COLLAPSE_DEVICE_SIDEBAR_AT: f64 = 1_000.0;
const COLLAPSE_CHANNEL_SIDEBAR_AT: f64 = 700.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResponsiveLayoutState {
    medium_width: bool,
    compact_width: bool,
    fullscreen: bool,
    outer_show_content: bool,
    inner_show_content: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResponsiveLayoutDecision {
    outer_collapsed: bool,
    inner_collapsed: bool,
    outer_show_content: bool,
    inner_show_content: bool,
}

impl ResponsiveLayoutState {
    const fn decision(self) -> ResponsiveLayoutDecision {
        ResponsiveLayoutDecision {
            outer_collapsed: self.medium_width || self.fullscreen,
            inner_collapsed: self.compact_width || self.fullscreen,
            outer_show_content: self.fullscreen || self.outer_show_content,
            inner_show_content: self.fullscreen || self.inner_show_content,
        }
    }
}

#[derive(Clone)]
struct ResponsiveLayout {
    state: Rc<Cell<ResponsiveLayoutState>>,
    outer: adw::NavigationSplitView,
    inner: adw::NavigationSplitView,
    player_view: Weak<player_view::PlayerView>,
}

impl ResponsiveLayout {
    fn new(
        outer: &adw::NavigationSplitView,
        inner: &adw::NavigationSplitView,
        player_view: &Rc<player_view::PlayerView>,
    ) -> Self {
        Self {
            state: Rc::new(Cell::new(ResponsiveLayoutState {
                medium_width: false,
                compact_width: false,
                fullscreen: false,
                outer_show_content: outer.shows_content(),
                inner_show_content: inner.shows_content(),
            })),
            outer: outer.clone(),
            inner: inner.clone(),
            player_view: Rc::downgrade(player_view),
        }
    }

    fn set_medium_width(&self, active: bool) {
        let mut state = self.state.get();
        state.medium_width = active;
        self.state.set(state);
        self.apply();
    }

    fn set_compact_width(&self, active: bool) {
        let mut state = self.state.get();
        state.compact_width = active;
        self.state.set(state);
        self.apply();
    }

    fn set_device_selected(&self, selected: bool) {
        let mut state = self.state.get();
        state.outer_show_content = selected;
        self.state.set(state);
        self.apply();
    }

    fn set_fullscreen(&self, fullscreen: bool) {
        let mut state = self.state.get();
        if fullscreen && !state.fullscreen {
            // Retain the exact navigation pages visible when the compositor
            // confirms entry. Snapshot changes may still replace the outer
            // preference while fullscreen, and will be applied on exit.
            state.outer_show_content = self.outer.shows_content();
            state.inner_show_content = self.inner.shows_content();
        }
        state.fullscreen = fullscreen;
        self.state.set(state);
        if let Some(player_view) = self.player_view.upgrade() {
            player_view.apply_fullscreen_presentation(fullscreen);
        }
        self.apply();
    }

    fn is_fullscreen(&self) -> bool {
        self.state.get().fullscreen
    }

    fn apply(&self) {
        let decision = self.state.get().decision();
        self.outer.set_collapsed(decision.outer_collapsed);
        self.inner.set_collapsed(decision.inner_collapsed);
        self.outer.set_show_content(decision.outer_show_content);
        self.inner.set_show_content(decision.inner_show_content);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FullscreenKeyAction {
    Toggle,
    Exit,
    Ignore,
}

fn fullscreen_key_action(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    fullscreen: bool,
) -> FullscreenKeyAction {
    use gtk::gdk::ModifierType;

    // Ignore ambient lock/legacy modifier bits and accept only exact
    // unmodified bindings. This keeps application and child-widget shortcuts
    // available instead of swallowing modified F11 or Escape.
    let effective = modifiers
        & (ModifierType::SHIFT_MASK
            | ModifierType::CONTROL_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK);
    if !effective.is_empty() {
        return FullscreenKeyAction::Ignore;
    }
    if key == gtk::gdk::Key::F11 {
        FullscreenKeyAction::Toggle
    } else if key == gtk::gdk::Key::Escape && fullscreen {
        FullscreenKeyAction::Exit
    } else {
        FullscreenKeyAction::Ignore
    }
}

/// Build Balun's single application window.
pub(crate) fn build(
    application: &adw::Application,
    controller: ControllerRuntime,
    playback: Result<PlaybackRuntime, PlaybackInitializationError>,
    shutdown_failed: Rc<Cell<bool>>,
) -> adw::ApplicationWindow {
    let device_sidebar = device_sidebar::build();
    let channel_sidebar = channel_sidebar::build();
    let player_view = Rc::new(player_view::build(playback));
    player_view.connect_stop_control();
    player_view.connect_audio_controls();

    let device_page = adw::NavigationPage::new(device_sidebar.root(), "Devices");
    let channel_page = adw::NavigationPage::new(channel_sidebar.root(), "Channels");
    let player_page = adw::NavigationPage::new(player_view.root(), "Live TV");

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
    let layout = ResponsiveLayout::new(&device_and_content, &channel_and_player, &player_view);

    let medium_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_DEVICE_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    {
        let layout = layout.clone();
        medium_breakpoint.connect_apply(move |_| layout.set_medium_width(true));
    }
    {
        let layout = layout.clone();
        medium_breakpoint.connect_unapply(move |_| layout.set_medium_width(false));
    }
    window.add_breakpoint(medium_breakpoint);

    let compact_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_CHANNEL_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    {
        let layout = layout.clone();
        compact_breakpoint.connect_apply(move |_| layout.set_compact_width(true));
    }
    {
        let layout = layout.clone();
        compact_breakpoint.connect_unapply(move |_| layout.set_compact_width(false));
    }
    window.add_breakpoint(compact_breakpoint);

    let handle = controller.handle();
    let mut snapshots = handle.subscribe();
    let initial = Arc::clone(&snapshots.borrow_and_update());
    device_sidebar.apply_snapshot(&initial);
    channel_sidebar.apply_snapshot(&initial);
    layout.set_device_selected(initial.selected_device().is_some());
    let accepted = Rc::new(RefCell::new(initial));

    connect_refresh(&device_sidebar, &handle);
    connect_exact_discovery(&window, &device_sidebar, &handle);
    connect_cancel_discovery(&device_sidebar, &handle);
    connect_device_selection(&device_sidebar, &handle, &accepted, &player_view);
    connect_channel_activation(&channel_sidebar, &handle, &player_view);
    connect_fullscreen(&window, &player_view, &layout);
    spawn_snapshot_reducer(
        snapshots,
        Rc::clone(&accepted),
        device_sidebar,
        channel_sidebar,
        layout,
        Rc::downgrade(&player_view),
    );
    connect_joined_shutdown(&window, controller, player_view, shutdown_failed);

    window
}

fn connect_channel_activation(
    sidebar: &channel_sidebar::ChannelSidebar,
    controller: &ControllerHandle,
    player_view: &Rc<player_view::PlayerView>,
) {
    let controller = controller.clone();
    let player_view = Rc::clone(player_view);
    sidebar.connect_channel_activated(move |selection| {
        player_view.activate_channel(&controller, selection);
    });
}

fn connect_fullscreen(
    window: &adw::ApplicationWindow,
    player_view: &Rc<player_view::PlayerView>,
    layout: &ResponsiveLayout,
) {
    let window_weak = window.downgrade();
    player_view.fullscreen_button().connect_clicked(move |_| {
        let Some(window) = window_weak.upgrade() else {
            return;
        };
        if window.is_fullscreen() {
            window.unfullscreen();
        } else {
            window.fullscreen();
        }
    });

    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let window_weak = window.downgrade();
    key_controller.connect_key_pressed(move |_, key, _keycode, modifiers| {
        let Some(window) = window_weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        match fullscreen_key_action(key, modifiers, window.is_fullscreen()) {
            FullscreenKeyAction::Toggle => {
                if window.is_fullscreen() {
                    window.unfullscreen();
                } else {
                    window.fullscreen();
                }
                gtk::glib::Propagation::Stop
            }
            FullscreenKeyAction::Exit => {
                window.unfullscreen();
                gtk::glib::Propagation::Stop
            }
            FullscreenKeyAction::Ignore => gtk::glib::Propagation::Proceed,
        }
    });
    window.add_controller(key_controller);

    let previous_focus: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Widget>>>> =
        Rc::new(RefCell::new(None));
    let confirmed_layout = layout.clone();
    let player_view = Rc::downgrade(player_view);
    window.connect_fullscreened_notify(move |window| {
        let fullscreen = window.is_fullscreen();
        let entering = fullscreen && !confirmed_layout.is_fullscreen();
        let leaving = !fullscreen && confirmed_layout.is_fullscreen();
        if entering {
            previous_focus.replace(
                gtk::prelude::GtkWindowExt::focus(window).map(|widget| widget.downgrade()),
            );
        }
        confirmed_layout.set_fullscreen(fullscreen);
        if entering {
            if let Some(player_view) = player_view.upgrade() {
                let _ = player_view.fullscreen_button().grab_focus();
            }
        } else if leaving
            && let Some(widget) = previous_focus
                .borrow_mut()
                .take()
                .and_then(|focus| focus.upgrade())
        {
            let _ = widget.grab_focus();
        }
    });

    // Initialize copy/layout from the actual window property, not a request.
    layout.set_fullscreen(window.is_fullscreen());
}

fn connect_refresh(sidebar: &device_sidebar::DeviceSidebar, controller: &ControllerHandle) {
    let controller = controller.clone();
    let cancel_discovery_button = sidebar.cancel_discovery_button().clone();
    let exact_discovery_button = sidebar.exact_discovery_button().clone();
    sidebar.refresh_button().connect_clicked(move |button| {
        // Close the tiny interval before the Refreshing snapshot arrives so a
        // fast double-click cannot enqueue redundant supersessions.
        button.set_sensitive(false);
        exact_discovery_button.set_sensitive(false);
        match controller.try_send(ControllerCommand::RefreshLocalDiscovery) {
            Ok(()) => {
                cancel_discovery_button.set_visible(true);
                cancel_discovery_button.set_sensitive(true);
            }
            Err(_) => {
                button.set_sensitive(true);
                exact_discovery_button.set_sensitive(true);
            }
        }
    });
}

fn connect_exact_discovery(
    window: &adw::ApplicationWindow,
    sidebar: &device_sidebar::DeviceSidebar,
    controller: &ControllerHandle,
) {
    let controller = controller.clone();
    let cancel_discovery_button = sidebar.cancel_discovery_button().clone();
    let dialog_open = Rc::new(Cell::new(false));
    let refresh_button = sidebar.refresh_button().clone();
    let window = window.downgrade();

    sidebar
        .exact_discovery_button()
        .connect_clicked(move |button| {
            if dialog_open.replace(true) {
                return;
            }
            let Some(window) = window.upgrade() else {
                dialog_open.set(false);
                return;
            };

            let admitted_controller = controller.clone();
            let admitted_cancel_button = cancel_discovery_button.clone();
            let admitted_exact_button = button.clone();
            let admitted_refresh_button = refresh_button.clone();
            let closed_dialog_open = Rc::clone(&dialog_open);
            exact_discovery_dialog::present(
                &window,
                move |target| {
                    // Exact and local discovery share one supersedable lane.
                    // Disable both actions before the Refreshing publication
                    // closes the small re-admission interval.
                    admitted_exact_button.set_sensitive(false);
                    admitted_refresh_button.set_sensitive(false);
                    match admitted_controller.try_send(ControllerCommand::DiscoverExact(target)) {
                        Ok(()) => {
                            admitted_cancel_button.set_visible(true);
                            admitted_cancel_button.set_sensitive(true);
                        }
                        Err(_) => {
                            admitted_exact_button.set_sensitive(true);
                            admitted_refresh_button.set_sensitive(true);
                        }
                    }
                },
                move || closed_dialog_open.set(false),
            );
        });
}

fn connect_cancel_discovery(
    sidebar: &device_sidebar::DeviceSidebar,
    controller: &ControllerHandle,
) {
    let controller = controller.clone();
    sidebar
        .cancel_discovery_button()
        .connect_clicked(move |button| {
            button.set_sensitive(false);
            if controller
                .try_send(ControllerCommand::CancelDiscovery)
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
    player_view: &Rc<player_view::PlayerView>,
) {
    let applying_snapshot = sidebar.snapshot_application_flag();
    let accepted = Rc::clone(accepted);
    let controller = controller.clone();
    let player_view = Rc::downgrade(player_view);
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
            match controller.try_send(command) {
                Ok(()) => {
                    // Release the old tuner immediately after the superseding
                    // intent is admitted; do not wait for a lineup worker to
                    // cancel and publish the next selection generation.
                    if let Some(player_view) = player_view.upgrade() {
                        let _ = player_view.stop();
                    }
                }
                Err(_) => {
                    restore_device_selection(selection, authoritative_device, &applying_snapshot);
                }
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
    layout: ResponsiveLayout,
    player_view: Weak<player_view::PlayerView>,
) {
    gtk::glib::MainContext::default().spawn_local(async move {
        while snapshots.changed().await.is_ok() {
            let candidate = Arc::clone(&snapshots.borrow_and_update());
            let (can_replace, selection_changed) = {
                let current = accepted.borrow();
                (
                    candidate.can_replace(&current),
                    candidate.selection_generation() > current.selection_generation(),
                )
            };
            if !can_replace {
                continue;
            }

            if selection_changed && let Some(player_view) = player_view.upgrade() {
                let _ = player_view.stop();
            }

            device_sidebar.apply_snapshot(&candidate);
            channel_sidebar.apply_snapshot(&candidate);
            layout.set_device_selected(candidate.selected_device().is_some());
            accepted.replace(candidate);
        }

        // The controller watch can also close because its actor failed. Its
        // independent GStreamer owner must not keep a tuner open afterward.
        if let Some(player_view) = player_view.upgrade() {
            let _ = player_view.stop();
        }
    });
}

fn connect_joined_shutdown(
    window: &adw::ApplicationWindow,
    controller: ControllerRuntime,
    player_view: Rc<player_view::PlayerView>,
    shutdown_failed: Rc<Cell<bool>>,
) {
    let controller = Rc::new(RefCell::new(Some(controller)));
    let player_view = Rc::new(RefCell::new(Some(player_view)));
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
        let retained_player_view = player_view.borrow_mut().take();
        controller.begin_shutdown();
        if retained_player_view
            .as_ref()
            .is_some_and(|player_view| player_view.shut_down().is_err())
        {
            shutdown_failed.set(true);
            eprintln!("Balun playback shutdown failed");
        }

        let shutdown_complete = Rc::clone(&shutdown_complete);
        let shutdown_failed = Rc::clone(&shutdown_failed);
        let window = window.downgrade();
        gtk::glib::MainContext::default().spawn_local(async move {
            // Retain GTK and playback ownership on this local future while
            // only the controller join moves to the blocking worker.
            let _retained_player_view = retained_player_view;
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
    fn fullscreen_keyboard_contract_filters_modifiers_and_ambient_locks() {
        use gtk::gdk::{Key, ModifierType};

        assert_eq!(
            fullscreen_key_action(Key::F11, ModifierType::empty(), false),
            FullscreenKeyAction::Toggle
        );
        assert_eq!(
            fullscreen_key_action(Key::F11, ModifierType::LOCK_MASK, true),
            FullscreenKeyAction::Toggle
        );
        let ambient_mod2 = ModifierType::from_bits_retain(1 << 4);
        assert_eq!(
            fullscreen_key_action(Key::F11, ambient_mod2, false),
            FullscreenKeyAction::Toggle
        );
        assert_eq!(
            fullscreen_key_action(Key::Escape, ModifierType::empty(), true),
            FullscreenKeyAction::Exit
        );
        assert_eq!(
            fullscreen_key_action(Key::Escape, ModifierType::LOCK_MASK, true),
            FullscreenKeyAction::Exit
        );

        for modifiers in [
            ModifierType::SHIFT_MASK,
            ModifierType::CONTROL_MASK,
            ModifierType::ALT_MASK,
            ModifierType::SUPER_MASK,
        ] {
            assert_eq!(
                fullscreen_key_action(Key::F11, modifiers, false),
                FullscreenKeyAction::Ignore
            );
            assert_eq!(
                fullscreen_key_action(Key::Escape, modifiers, true),
                FullscreenKeyAction::Ignore
            );
        }
        assert_eq!(
            fullscreen_key_action(Key::Escape, ModifierType::empty(), false),
            FullscreenKeyAction::Ignore
        );
        assert_eq!(
            fullscreen_key_action(Key::F10, ModifierType::empty(), true),
            FullscreenKeyAction::Ignore
        );
    }

    #[test]
    fn fullscreen_layout_forces_player_then_restores_responsive_preferences() {
        let mut state = ResponsiveLayoutState {
            medium_width: false,
            compact_width: false,
            fullscreen: false,
            outer_show_content: true,
            inner_show_content: false,
        };
        assert_eq!(
            state.decision(),
            ResponsiveLayoutDecision {
                outer_collapsed: false,
                inner_collapsed: false,
                outer_show_content: true,
                inner_show_content: false,
            }
        );

        state.medium_width = true;
        state.compact_width = true;
        assert_eq!(
            state.decision(),
            ResponsiveLayoutDecision {
                outer_collapsed: true,
                inner_collapsed: true,
                outer_show_content: true,
                inner_show_content: false,
            }
        );

        state.medium_width = false;
        state.compact_width = false;
        state.fullscreen = true;
        assert_eq!(
            state.decision(),
            ResponsiveLayoutDecision {
                outer_collapsed: true,
                inner_collapsed: true,
                outer_show_content: true,
                inner_show_content: true,
            }
        );

        // A selection clear while fullscreen is retained but cannot reveal a
        // sidebar until the compositor confirms exit.
        state.outer_show_content = false;
        assert!(state.decision().outer_show_content);
        state.fullscreen = false;
        assert_eq!(
            state.decision(),
            ResponsiveLayoutDecision {
                outer_collapsed: false,
                inner_collapsed: false,
                outer_show_content: false,
                inner_show_content: false,
            }
        );
    }

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
