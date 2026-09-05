//! Top-level adaptive three-pane window and controller/GLib bridge.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, ControllerCommand, ControllerHandle, ControllerRuntime, DiscoveryFailure,
    DiscoveryKind, DiscoveryState, DiscoveryStatus, ExactSearchOutcome, ExactTargetTracker,
    HostnameResolutionReceiver, RediscoveryQueue,
};
use balun::discovery::{
    DiscoveryEntry, ExactDiscoveryTarget, HostnameResolutionError, HostnameTarget,
    RoutedScanTrigger,
};
use balun::playback::{PlaybackInitializationError, PlaybackRuntime};
use balun::settings::RememberedTarget;

use super::device_sidebar::FORGET_ROUTED_APPROVALS_ACTION;
use super::objects::DeviceRowObject;
use super::routed_flow::{RoutedApprovalFlow, RoutedFlowAction};
use super::settings_session::SettingsSession;
use super::{
    channel_sidebar, device_sidebar, exact_discovery_dialog, player_view, routed_approval_dialog,
};

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
    outer_content_can_pop: bool,
    inner_content_can_pop: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ResponsiveLayoutDecision {
    outer_collapsed: bool,
    inner_collapsed: bool,
    outer_show_content: bool,
    inner_show_content: bool,
    outer_content_can_pop: bool,
    inner_content_can_pop: bool,
}

impl ResponsiveLayoutDecision {
    const fn devices_presented(self) -> bool {
        !self.outer_collapsed || !self.outer_show_content
    }
}

impl ResponsiveLayoutState {
    const fn decision(self) -> ResponsiveLayoutDecision {
        ResponsiveLayoutDecision {
            outer_collapsed: self.medium_width || self.fullscreen,
            inner_collapsed: self.compact_width || self.fullscreen,
            outer_show_content: self.fullscreen || self.outer_show_content,
            inner_show_content: self.fullscreen || self.inner_show_content,
            outer_content_can_pop: !self.fullscreen && self.outer_content_can_pop,
            inner_content_can_pop: !self.fullscreen && self.inner_content_can_pop,
        }
    }
}

#[derive(Clone)]
struct ResponsiveLayout {
    state: Rc<Cell<ResponsiveLayoutState>>,
    outer: adw::NavigationSplitView,
    outer_content: adw::NavigationPage,
    inner: adw::NavigationSplitView,
    inner_content: adw::NavigationPage,
    player_view: Weak<player_view::PlayerView>,
}

#[derive(Clone)]
struct PlayerNavigation {
    state: Weak<Cell<ResponsiveLayoutState>>,
    inner: gtk::glib::WeakRef<adw::NavigationSplitView>,
}

impl PlayerNavigation {
    fn show_player(&self) {
        let Some(state) = self.state.upgrade() else {
            return;
        };
        let mut current = state.get();
        current.inner_show_content = true;
        state.set(current);
        if let Some(inner) = self.inner.upgrade() {
            inner.set_show_content(true);
        }
    }
}

impl ResponsiveLayout {
    fn new(
        outer: &adw::NavigationSplitView,
        outer_content: &adw::NavigationPage,
        inner: &adw::NavigationSplitView,
        inner_content: &adw::NavigationPage,
        player_view: &Rc<player_view::PlayerView>,
    ) -> Self {
        let layout = Self {
            state: Rc::new(Cell::new(ResponsiveLayoutState {
                medium_width: false,
                compact_width: false,
                fullscreen: false,
                outer_show_content: outer.shows_content(),
                inner_show_content: inner.shows_content(),
                outer_content_can_pop: outer_content.can_pop(),
                inner_content_can_pop: inner_content.can_pop(),
            })),
            outer: outer.clone(),
            outer_content: outer_content.clone(),
            inner: inner.clone(),
            inner_content: inner_content.clone(),
            player_view: Rc::downgrade(player_view),
        };

        // Native back buttons, shortcuts, mouse buttons, and swipe gestures
        // update show-content directly. Retain those user choices without
        // allowing fullscreen's forced player-only presentation to replace
        // the values that must be restored on exit.
        {
            let state = Rc::downgrade(&layout.state);
            layout.outer.connect_show_content_notify(move |outer| {
                let Some(state) = state.upgrade() else {
                    return;
                };
                let mut current = state.get();
                if !current.fullscreen {
                    current.outer_show_content = outer.shows_content();
                    state.set(current);
                }
            });
        }
        {
            let state = Rc::downgrade(&layout.state);
            layout.inner.connect_show_content_notify(move |inner| {
                let Some(state) = state.upgrade() else {
                    return;
                };
                let mut current = state.get();
                if !current.fullscreen {
                    current.inner_show_content = inner.shows_content();
                    state.set(current);
                }
            });
        }

        layout
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

    fn show_channels(&self) {
        let mut state = self.state.get();
        state.outer_show_content = true;
        state.inner_show_content = false;
        self.state.set(state);
        self.apply();
    }

    fn player_navigation(&self) -> PlayerNavigation {
        PlayerNavigation {
            state: Rc::downgrade(&self.state),
            inner: self.inner.downgrade(),
        }
    }

    fn set_fullscreen(&self, fullscreen: bool) {
        let mut state = self.state.get();
        if fullscreen && !state.fullscreen {
            // Retain the exact navigation pages visible when the compositor
            // confirms entry. Snapshot changes may still replace the outer
            // preference while fullscreen, and will be applied on exit.
            state.outer_show_content = self.outer.shows_content();
            state.inner_show_content = self.inner.shows_content();
            state.outer_content_can_pop = self.outer_content.can_pop();
            state.inner_content_can_pop = self.inner_content.can_pop();
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

    fn devices_presented(&self) -> bool {
        self.state.get().decision().devices_presented()
    }

    fn apply(&self) {
        let decision = self.state.get().decision();
        self.outer.set_collapsed(decision.outer_collapsed);
        self.inner.set_collapsed(decision.inner_collapsed);
        self.outer.set_show_content(decision.outer_show_content);
        self.inner.set_show_content(decision.inner_show_content);
        self.outer_content
            .set_can_pop(decision.outer_content_can_pop);
        self.inner_content
            .set_can_pop(decision.inner_content_can_pop);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowKeyAction {
    ToggleFullscreen,
    ExitFullscreen,
    FocusSearch,
    RefreshDevices,
    Ignore,
}

fn window_key_action(
    key: gtk::gdk::Key,
    modifiers: gtk::gdk::ModifierType,
    fullscreen: bool,
) -> WindowKeyAction {
    use gtk::gdk::{Key, ModifierType};

    // Filter out ambient lock/legacy modifier bits (Caps Lock, Num Lock, etc.)
    // and accept only exact unmodified or single-modifier bindings.
    let significant = modifiers
        & (ModifierType::SHIFT_MASK
            | ModifierType::CONTROL_MASK
            | ModifierType::ALT_MASK
            | ModifierType::SUPER_MASK
            | ModifierType::HYPER_MASK
            | ModifierType::META_MASK);

    if significant.is_empty() {
        if key == Key::F11 {
            return WindowKeyAction::ToggleFullscreen;
        }
        if key == Key::Escape && fullscreen {
            return WindowKeyAction::ExitFullscreen;
        }
        if key == Key::F5 {
            return WindowKeyAction::RefreshDevices;
        }
    } else if significant == ModifierType::CONTROL_MASK {
        if key == Key::f || key == Key::F {
            return WindowKeyAction::FocusSearch;
        }
        if key == Key::r || key == Key::R {
            return WindowKeyAction::RefreshDevices;
        }
    }

    WindowKeyAction::Ignore
}

const fn shortcut_target_available(visible: bool, sensitive: bool) -> bool {
    visible && sensitive
}

const fn refresh_shortcut_available(
    visible: bool,
    sensitive: bool,
    devices_presented: bool,
) -> bool {
    shortcut_target_available(visible, sensitive) && devices_presented
}

/// Build Balun's single application window.
pub(crate) fn build(
    application: &adw::Application,
    controller: ControllerRuntime,
    playback: Result<PlaybackRuntime, PlaybackInitializationError>,
    settings: SettingsSession,
    shutdown_failed: Rc<Cell<bool>>,
) -> adw::ApplicationWindow {
    let settings = Rc::new(settings);
    let window_state = settings.window();
    let device_sidebar = device_sidebar::build();
    let channel_sidebar = channel_sidebar::build();
    let player_view = Rc::new(player_view::build(playback));
    player_view.connect_stop_control();
    player_view.connect_audio_controls();
    player_view.connect_session_state();

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

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&device_and_content));
    let window = adw::ApplicationWindow::builder()
        .application(application)
        .title("Balun")
        .default_width(i32::try_from(window_state.width()).unwrap_or(i32::MAX))
        .default_height(i32::try_from(window_state.height()).unwrap_or(i32::MAX))
        .content(&toasts)
        .build();
    if window_state.maximized() {
        window.maximize();
    }
    let layout = ResponsiveLayout::new(
        &device_and_content,
        &channel_and_player_page,
        &channel_and_player,
        &player_page,
        &player_view,
    );

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
    let exact_tracker = Rc::new(RefCell::new(ExactTargetTracker::new()));
    let remembered = settings.remembered_targets();
    let rediscovery = Rc::new(RefCell::new(RediscoveryQueue::new(
        remembered.iter().filter_map(|target| match target {
            RememberedTarget::Address(address) => Some(*address),
            RememberedTarget::Hostname(_) => None,
        }),
    )));
    let wiring = Rc::new(RediscoveryWiring {
        settings: Rc::clone(&settings),
        exact_tracker: Rc::clone(&exact_tracker),
        rediscovery: Rc::clone(&rediscovery),
        hostnames: Rc::new(RefCell::new(HostnameProbes::default())),
        controller: handle.clone(),
        accepted: Rc::clone(&accepted),
        toasts: toasts.downgrade(),
    });

    connect_refresh(&device_sidebar, &handle);
    connect_exact_discovery(&window, &device_sidebar, &handle, &exact_tracker, &wiring);
    connect_cancel_discovery(&device_sidebar, &handle, &wiring);
    let routed_ui = Rc::new(RoutedUi::new(&window, Rc::clone(&wiring)));
    connect_routed_discovery(&window, &device_sidebar, &routed_ui);
    connect_device_selection(&device_sidebar, &handle, &accepted, &player_view, &layout);
    connect_channel_activation(
        &channel_sidebar,
        &handle,
        &player_view,
        layout.player_navigation(),
        &accepted,
    );
    connect_window_shortcuts(
        &window,
        &player_view,
        &layout,
        Some(device_sidebar.refresh_button().clone()),
        Some(channel_sidebar.search_entry().clone()),
    );
    spawn_snapshot_reducer(
        snapshots,
        Rc::clone(&accepted),
        device_sidebar,
        channel_sidebar,
        layout,
        Rc::downgrade(&player_view),
        SnapshotReactions {
            rediscovery: Rc::clone(&wiring),
            routed_ui,
        },
    );
    // One bounded local discovery runs at launch so the sidebar fills without
    // a click; nothing rescans on its own afterwards. The remembered queue is
    // told to await that operation before any name is resolved, so a name
    // that resolves before the Refreshing snapshot lands cannot supersede it;
    // remembered addresses and names are probed once the lane settles, and a
    // remembered name is resolved again before it is probed. If the launch
    // refresh cannot be queued, the remembered probes start immediately.
    let launch_generation = accepted.borrow().discovery().generation();
    if handle
        .try_send(ControllerCommand::RefreshLocalDiscovery)
        .is_ok()
    {
        rediscovery.borrow_mut().await_operation(launch_generation);
    } else {
        advance_rediscovery(&wiring, accepted.borrow().discovery());
    }
    wiring
        .hostnames
        .borrow_mut()
        .unresolved
        .extend(remembered.into_iter().filter_map(|target| match target {
            RememberedTarget::Hostname(host) => Some(host),
            RememberedTarget::Address(_) => None,
        }));
    submit_unresolved_hostnames(&wiring);
    connect_joined_shutdown(&window, controller, player_view, settings, shutdown_failed);

    window
}

fn connect_channel_activation(
    sidebar: &channel_sidebar::ChannelSidebar,
    controller: &ControllerHandle,
    player_view: &Rc<player_view::PlayerView>,
    navigation: PlayerNavigation,
    accepted: &Rc<RefCell<Arc<ApplicationSnapshot>>>,
) {
    let controller = controller.clone();
    let player_view = Rc::clone(player_view);
    let accepted = Rc::clone(accepted);
    sidebar.connect_channel_activated(move |selection| {
        // Name the device and channel from the accepted snapshot so progress
        // and failure copy can say which tuner and channel they mean.
        let context =
            player_view::TuneContext::from_snapshot(&accepted.borrow(), selection.channel_key());
        // Present the connecting, video, or failure state on compact layouts
        // instead of leaving the player hidden behind the channel list.
        navigation.show_player();
        player_view.activate_channel(&controller, selection, context);
    });
}

fn connect_window_shortcuts(
    window: &adw::ApplicationWindow,
    player_view: &Rc<player_view::PlayerView>,
    layout: &ResponsiveLayout,
    refresh_button: Option<gtk::Button>,
    search_entry: Option<gtk::SearchEntry>,
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

    let previous_focus: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Widget>>>> =
        Rc::new(RefCell::new(None));
    let previous_focus_for_keys = Rc::clone(&previous_focus);

    let key_controller = gtk::EventControllerKey::new();
    key_controller.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let window_weak = window.downgrade();
    let layout_for_keys = layout.clone();
    key_controller.connect_key_pressed(move |_, key, _keycode, modifiers| {
        let Some(window) = window_weak.upgrade() else {
            return gtk::glib::Propagation::Proceed;
        };
        match window_key_action(key, modifiers, window.is_fullscreen()) {
            WindowKeyAction::ToggleFullscreen => {
                if window.is_fullscreen() {
                    window.unfullscreen();
                } else {
                    window.fullscreen();
                }
                gtk::glib::Propagation::Stop
            }
            WindowKeyAction::ExitFullscreen => {
                window.unfullscreen();
                gtk::glib::Propagation::Stop
            }
            WindowKeyAction::FocusSearch => {
                let Some(search_entry) = search_entry.as_ref() else {
                    return gtk::glib::Propagation::Stop;
                };
                if !shortcut_target_available(
                    search_entry.is_visible(),
                    search_entry.is_sensitive(),
                ) {
                    return gtk::glib::Propagation::Stop;
                }
                if window.is_fullscreen() {
                    previous_focus_for_keys
                        .replace(Some(search_entry.upcast_ref::<gtk::Widget>().downgrade()));
                    window.unfullscreen();
                }
                layout_for_keys.show_channels();
                let _ = search_entry.grab_focus();
                gtk::glib::Propagation::Stop
            }
            WindowKeyAction::RefreshDevices => {
                if let Some(refresh_button) = refresh_button.as_ref()
                    && refresh_shortcut_available(
                        refresh_button.is_visible(),
                        refresh_button.is_sensitive(),
                        layout_for_keys.devices_presented(),
                    )
                {
                    refresh_button.emit_clicked();
                }
                gtk::glib::Propagation::Stop
            }
            WindowKeyAction::Ignore => gtk::glib::Propagation::Proceed,
        }
    });
    window.add_controller(key_controller);
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
    let routed_discovery_button = sidebar.routed_discovery_button().clone();
    sidebar.refresh_button().connect_clicked(move |button| {
        // Close the tiny interval before the Refreshing snapshot arrives so a
        // fast double-click cannot enqueue redundant supersessions. Local,
        // exact, and routed discovery share one supersedable lane, so every
        // start control closes together.
        button.set_sensitive(false);
        exact_discovery_button.set_sensitive(false);
        routed_discovery_button.set_sensitive(false);
        match controller.try_send(ControllerCommand::RefreshLocalDiscovery) {
            Ok(()) => {
                cancel_discovery_button.set_visible(true);
                cancel_discovery_button.set_sensitive(true);
            }
            Err(_) => {
                button.set_sensitive(true);
                exact_discovery_button.set_sensitive(true);
                routed_discovery_button.set_sensitive(true);
            }
        }
    });
}

fn connect_exact_discovery(
    window: &adw::ApplicationWindow,
    sidebar: &device_sidebar::DeviceSidebar,
    controller: &ControllerHandle,
    exact_tracker: &Rc<RefCell<ExactTargetTracker>>,
    wiring: &Rc<RediscoveryWiring>,
) {
    let controller = controller.clone();
    let exact_tracker = Rc::clone(exact_tracker);
    let wiring = Rc::clone(wiring);
    let cancel_discovery_button = sidebar.cancel_discovery_button().clone();
    let dialog_open = Rc::new(Cell::new(false));
    let refresh_button = sidebar.refresh_button().clone();
    let routed_discovery_button = sidebar.routed_discovery_button().clone();
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
            let admitted_tracker = Rc::clone(&exact_tracker);
            let admitted_wiring = Rc::clone(&wiring);
            let admitted_cancel_button = cancel_discovery_button.clone();
            let admitted_exact_button = button.clone();
            let admitted_refresh_button = refresh_button.clone();
            let admitted_routed_button = routed_discovery_button.clone();
            let closed_dialog_open = Rc::clone(&dialog_open);
            exact_discovery_dialog::present(
                &window,
                move |entry| {
                    let target = match entry {
                        DiscoveryEntry::Address(target) => target,
                        DiscoveryEntry::Hostname(host) => {
                            // A name is resolved on the controller runtime and
                            // its addresses join the paced probe queue.
                            resolve_hostname_into_queue(Rc::clone(&admitted_wiring), host, true);
                            return;
                        }
                    };
                    // Exact, local, and routed discovery share one
                    // supersedable lane. Disable every start action before
                    // the Refreshing publication closes the small
                    // re-admission interval.
                    admitted_exact_button.set_sensitive(false);
                    admitted_refresh_button.set_sensitive(false);
                    admitted_routed_button.set_sensitive(false);
                    match admitted_controller.try_discover_exact(target) {
                        Ok(ticket) => {
                            // Remember the address only once this search
                            // reports a valid reply from it.
                            admitted_tracker.borrow_mut().admit(target, ticket);
                            admitted_cancel_button.set_visible(true);
                            admitted_cancel_button.set_sensitive(true);
                            admitted_wiring
                                .toast("Searching the entered address for an HDHomeRun device.");
                        }
                        Err(_) => {
                            admitted_exact_button.set_sensitive(true);
                            admitted_refresh_button.set_sensitive(true);
                            admitted_routed_button.set_sensitive(true);
                            admitted_wiring
                                .toast("Balun is busy; try the device address again in a moment.");
                        }
                    }
                },
                move || closed_dialog_open.set(false),
            );
        });
}

/// Main-context state behind the routed search button: the approval flow,
/// the window that hosts the approval dialog, and the shared toast and
/// controller wiring.
struct RoutedUi {
    flow: RefCell<RoutedApprovalFlow>,
    window: gtk::glib::WeakRef<adw::ApplicationWindow>,
    wiring: Rc<RediscoveryWiring>,
    dialog_open: Cell<bool>,
}

impl RoutedUi {
    fn new(window: &adw::ApplicationWindow, wiring: Rc<RediscoveryWiring>) -> Self {
        Self {
            flow: RefCell::new(RoutedApprovalFlow::new()),
            window: window.downgrade(),
            wiring,
            dialog_open: Cell::new(false),
        }
    }

    fn send(&self, command: ControllerCommand) -> bool {
        if self.wiring.controller.try_send(command).is_ok() {
            true
        } else {
            self.wiring
                .toast("Balun is busy; try the routed search again in a moment.");
            false
        }
    }
}

/// Connect the routed search button and the forget-approvals action. A
/// click runs the approved scan; the first refusal opens the approval
/// dialog through the snapshot reducer.
fn connect_routed_discovery(
    window: &adw::ApplicationWindow,
    sidebar: &device_sidebar::DeviceSidebar,
    routed_ui: &Rc<RoutedUi>,
) {
    let cancel_discovery_button = sidebar.cancel_discovery_button().clone();
    let exact_discovery_button = sidebar.exact_discovery_button().clone();
    let refresh_button = sidebar.refresh_button().clone();
    let ui = Rc::clone(routed_ui);
    sidebar
        .routed_discovery_button()
        .connect_clicked(move |button| {
            // Routed, exact, and local discovery share one supersedable lane.
            button.set_sensitive(false);
            exact_discovery_button.set_sensitive(false);
            refresh_button.set_sensitive(false);
            ui.flow.borrow_mut().user_requested_run();
            if ui.send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            )) {
                cancel_discovery_button.set_visible(true);
                cancel_discovery_button.set_sensitive(true);
            } else {
                button.set_sensitive(true);
                exact_discovery_button.set_sensitive(true);
                refresh_button.set_sensitive(true);
            }
        });

    let forget = gtk::gio::SimpleAction::new(FORGET_ROUTED_APPROVALS_ACTION, None);
    let ui = Rc::clone(routed_ui);
    forget.connect_activate(move |_, _| {
        ui.flow.borrow_mut().dialog_dismissed();
        if ui.send(ControllerCommand::RevokeRoutedApprovals) {
            ui.wiring
                .toast("Routed approvals will be forgotten; approve a search to use them again.");
        }
    });
    window.add_action(&forget);
}

/// Advance the approval flow with one accepted snapshot.
fn react_to_routed(ui: &Rc<RoutedUi>, snapshot: &ApplicationSnapshot) {
    let action = ui.flow.borrow_mut().observe(snapshot);
    match action {
        RoutedFlowAction::Nothing => {}
        RoutedFlowAction::Propose => {
            if !ui.send(ControllerCommand::ProposeRoutedDiscovery) {
                ui.flow.borrow_mut().dialog_dismissed();
            }
        }
        RoutedFlowAction::Run => {
            let _ = ui.send(ControllerCommand::RunRoutedDiscovery(
                RoutedScanTrigger::ExplicitRefresh,
            ));
        }
        RoutedFlowAction::ShowDialog(proposal) => {
            if ui.dialog_open.replace(true) {
                return;
            }
            let receiver = match ui
                .wiring
                .controller
                .try_routed_proposal_origins(proposal.token())
            {
                Ok(receiver) => receiver,
                Err(_) => {
                    ui.dialog_open.set(false);
                    ui.flow.borrow_mut().dialog_dismissed();
                    ui.wiring
                        .toast("Balun is busy; try the routed search again in a moment.");
                    return;
                }
            };
            let ui = Rc::clone(ui);
            gtk::glib::MainContext::default().spawn_local(async move {
                let origins = match receiver.receive().await {
                    Ok(origins) => origins,
                    Err(_) => {
                        ui.dialog_open.set(false);
                        ui.flow.borrow_mut().dialog_dismissed();
                        ui.wiring
                            .toast("The routed proposal changed; try the search again.");
                        return;
                    }
                };
                let Some(window) = ui.window.upgrade() else {
                    ui.dialog_open.set(false);
                    ui.flow.borrow_mut().dialog_dismissed();
                    return;
                };
                let approve_ui = Rc::clone(&ui);
                let closed_ui = Rc::clone(&ui);
                routed_approval_dialog::present(
                    &window,
                    proposal,
                    &origins,
                    move || {
                        approve_ui
                            .flow
                            .borrow_mut()
                            .dialog_approved(proposal.token());
                        if !approve_ui
                            .send(ControllerCommand::ApproveRoutedDiscovery(proposal.token()))
                        {
                            approve_ui.flow.borrow_mut().dialog_dismissed();
                        }
                    },
                    move || {
                        closed_ui.dialog_open.set(false);
                        closed_ui.flow.borrow_mut().dialog_dismissed();
                    },
                );
            });
        }
    }
}

fn connect_cancel_discovery(
    sidebar: &device_sidebar::DeviceSidebar,
    controller: &ControllerHandle,
    wiring: &Rc<RediscoveryWiring>,
) {
    let controller = controller.clone();
    let wiring = Rc::clone(wiring);
    sidebar
        .cancel_discovery_button()
        .connect_clicked(move |button| {
            button.set_sensitive(false);
            // Stop also drains any launch probes still waiting for the lane,
            // including remembered names not yet handed to the controller.
            wiring.rediscovery.borrow_mut().cancel();
            wiring.hostnames.borrow_mut().unresolved.clear();
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
    layout: &ResponsiveLayout,
) {
    let applying_snapshot = sidebar.snapshot_application_flag();
    let accepted = Rc::clone(accepted);
    let controller = controller.clone();
    let selection = sidebar.selection().clone();
    let accepted_for_activated = Rc::clone(&accepted);
    let layout_for_activated = layout.clone();
    sidebar.connect_device_activated(move |position| {
        let target_device = selection
            .item(position)
            .and_then(|item| item.downcast::<DeviceRowObject>().ok())
            .and_then(|row| row.device_id());
        let authoritative = accepted_for_activated.borrow().selected_device();
        if target_device.is_some() && target_device == authoritative {
            // The row activated is already the admitted, authoritative device.
            // Selection change notification will not fire, so restore the
            // channel lineup view directly.
            layout_for_activated.set_device_selected(true);
            layout_for_activated.show_channels();
        } else {
            // Selecting a different device triggers selection-notify, which
            // gates channel navigation on controller admission.
            selection.set_selected(position);
        }
    });
    let player_view = Rc::downgrade(player_view);
    let layout_for_notify = layout.clone();
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
                    if selected_device.is_some() {
                        layout_for_notify.set_device_selected(true);
                        layout_for_notify.show_channels();
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

/// Main-context state the snapshot reducer needs to remember reachable
/// addresses and names and to pace queued probes.
struct RediscoveryWiring {
    settings: Rc<SettingsSession>,
    exact_tracker: Rc<RefCell<ExactTargetTracker>>,
    rediscovery: Rc<RefCell<RediscoveryQueue>>,
    hostnames: Rc<RefCell<HostnameProbes>>,
    controller: ControllerHandle,
    accepted: Rc<RefCell<Arc<ApplicationSnapshot>>>,
    toasts: gtk::glib::WeakRef<adw::ToastOverlay>,
}

/// Which queued addresses came from which user-entered hostname, so the
/// name (not its transient address) is remembered once one of them answers.
#[derive(Default)]
struct HostnameProbes {
    pending: HashMap<ExactDiscoveryTarget, HostnameTarget>,
    /// Remembered names not yet handed to the controller for resolution.
    /// The command queue is bounded, so they are submitted a few at a time
    /// and the remainder waits for the next accepted snapshot.
    unresolved: VecDeque<HostnameTarget>,
}

impl RediscoveryWiring {
    fn remember(&self, target: RememberedTarget) {
        if let Some(pending_save) = self.settings.remember_target(target) {
            // The session's writer runs the flushing write off the main
            // context, one save at a time, so snapshot reduction never
            // stalls and no older document can land after this one.
            self.settings.save(pending_save);
        }
    }

    fn toast(&self, text: &str) {
        if let Some(toasts) = self.toasts.upgrade() {
            toasts.add_toast(adw::Toast::new(text));
        }
    }
}

/// Feed one discovery state to the probe queue: remember a queued address
/// (or the name it came from) once its probe answered, then send the next
/// queued target if the lane is idle.
fn advance_rediscovery(wiring: &Rc<RediscoveryWiring>, discovery: DiscoveryState) {
    submit_unresolved_hostnames(wiring);
    let step = wiring.rediscovery.borrow_mut().advance(discovery);
    if let Some(target) = step.reachable {
        let host = {
            let mut hostnames = wiring.hostnames.borrow_mut();
            let host = hostnames.pending.remove(&target);
            if let Some(host) = &host {
                hostnames.pending.retain(|_, pending| pending != host);
            }
            host
        };
        wiring.remember(match host {
            Some(host) => RememberedTarget::Hostname(host),
            None => RememberedTarget::Address(target),
        });
    }
    if let Some(target) = step.send
        && wiring
            .controller
            .try_send(ControllerCommand::DiscoverExact(target))
            .is_err()
    {
        wiring.rediscovery.borrow_mut().send_failed(target);
    }
}

/// Resolve `host` on the controller runtime and queue its addresses. A
/// user-entered name is remembered only after one of them answers; a
/// remembered name is already known, so a failure only shows a notice.
fn resolve_hostname_into_queue(wiring: Rc<RediscoveryWiring>, host: HostnameTarget, entered: bool) {
    let receiver = match wiring.controller.try_resolve_hostname(host.clone()) {
        Ok(receiver) => receiver,
        Err(_) => {
            wiring.toast("Balun is busy; try the device name again in a moment.");
            return;
        }
    };
    await_resolution(wiring, host, entered, receiver);
}

/// Hand remembered names to the controller until its command queue is full;
/// whatever remains is retried from the next accepted snapshot, so a long
/// remembered list never drops a name at launch.
fn submit_unresolved_hostnames(wiring: &Rc<RediscoveryWiring>) {
    loop {
        let Some(host) = wiring.hostnames.borrow_mut().unresolved.pop_front() else {
            return;
        };
        match wiring.controller.try_resolve_hostname(host.clone()) {
            Ok(receiver) => await_resolution(Rc::clone(wiring), host, false, receiver),
            Err(_) => {
                wiring.hostnames.borrow_mut().unresolved.push_front(host);
                return;
            }
        }
    }
}

fn await_resolution(
    wiring: Rc<RediscoveryWiring>,
    host: HostnameTarget,
    entered: bool,
    receiver: HostnameResolutionReceiver,
) {
    gtk::glib::MainContext::default().spawn_local(async move {
        match receiver.receive().await {
            Ok(addresses) => {
                if entered {
                    let mut hostnames = wiring.hostnames.borrow_mut();
                    for address in &addresses {
                        hostnames.pending.insert(*address, host.clone());
                    }
                }
                wiring.rediscovery.borrow_mut().enqueue(addresses);
                let discovery = wiring.accepted.borrow().discovery();
                advance_rediscovery(&wiring, discovery);
            }
            Err(error) => {
                wiring.toast(resolution_failure_copy(error));
                // A finished resolution is the other moment queue capacity
                // becomes available, so retry deferred names here too.
                submit_unresolved_hostnames(&wiring);
            }
        }
    });
}

const fn resolution_failure_copy(error: HostnameResolutionError) -> &'static str {
    match error {
        HostnameResolutionError::Timeout => "Resolving the device name timed out.",
        HostnameResolutionError::Lookup(_) => "The device name could not be resolved.",
        HostnameResolutionError::NoUsableAddress => {
            "The device name has no usable unicast address."
        }
        HostnameResolutionError::ControllerStopped => "Balun is shutting down.",
    }
}

const EXACT_SEARCH_REPLACED_COPY: &str =
    "The exact-address device search was replaced before it completed.";

const fn exact_settlement_failure_toast(discovery: DiscoveryState) -> Option<&'static str> {
    match (discovery.kind(), discovery.status()) {
        (DiscoveryKind::Exact, DiscoveryStatus::NoResponse) => {
            Some("No valid HDHomeRun reply was received from the entered address.")
        }
        (
            DiscoveryKind::Exact,
            DiscoveryStatus::Failed(DiscoveryFailure::ExactTargetLimitReached),
        ) => Some("The device address limit was reached for this session."),
        (DiscoveryKind::Exact, DiscoveryStatus::Failed(_)) => {
            Some("The exact-address device search failed.")
        }
        (DiscoveryKind::Exact, DiscoveryStatus::Idle) => {
            Some("The exact-address device search was stopped.")
        }
        (DiscoveryKind::Exact, DiscoveryStatus::Ready | DiscoveryStatus::Refreshing) => None,
        _ => Some(EXACT_SEARCH_REPLACED_COPY),
    }
}

/// Main-context reactions the reducer runs after accepting a snapshot.
struct SnapshotReactions {
    rediscovery: Rc<RediscoveryWiring>,
    routed_ui: Rc<RoutedUi>,
}

fn spawn_snapshot_reducer(
    mut snapshots: tokio::sync::watch::Receiver<Arc<ApplicationSnapshot>>,
    accepted: Rc<RefCell<Arc<ApplicationSnapshot>>>,
    device_sidebar: device_sidebar::DeviceSidebar,
    channel_sidebar: channel_sidebar::ChannelSidebar,
    layout: ResponsiveLayout,
    player_view: Weak<player_view::PlayerView>,
    reactions: SnapshotReactions,
) {
    let SnapshotReactions {
        rediscovery,
        routed_ui,
    } = reactions;
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

            if selection_changed {
                // A newly authoritative device selection starts at its own
                // channel list even if the previous device was playing.
                layout.show_channels();
            }

            device_sidebar.apply_snapshot(&candidate);
            channel_sidebar.apply_snapshot(&candidate);
            layout.set_device_selected(candidate.selected_device().is_some());
            let discovery = candidate.discovery();
            let exact_searches = candidate.exact_searches();
            accepted.replace(candidate);

            let outcome = rediscovery
                .exact_tracker
                .borrow_mut()
                .observe(discovery, exact_searches);
            match outcome {
                ExactSearchOutcome::Pending => {}
                ExactSearchOutcome::Reachable(target) => {
                    rediscovery.remember(RememberedTarget::Address(target));
                    rediscovery.toast("HDHomeRun device found.");
                }
                ExactSearchOutcome::Settled => {
                    if let Some(copy) = exact_settlement_failure_toast(discovery) {
                        rediscovery.toast(copy);
                    }
                }
                ExactSearchOutcome::Superseded => rediscovery.toast(EXACT_SEARCH_REPLACED_COPY),
            }
            advance_rediscovery(&rediscovery, discovery);
            react_to_routed(&routed_ui, &accepted.borrow());
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
    settings: Rc<SettingsSession>,
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

        // Capture geometry while the window is still mapped and interactive.
        // The durable write queues on the settings writer behind any save
        // still in flight, so its file flushes never stall the main loop.
        if let Some(pending_save) = settings.stage_window(window) {
            settings.save(pending_save);
        }
        window.set_sensitive(false);
        let controller = controller.borrow_mut().take();
        let retained_player_view = player_view.borrow_mut().take();
        if let Some(controller) = &controller {
            controller.begin_shutdown();
        }
        if retained_player_view
            .as_ref()
            .is_some_and(|player_view| player_view.shut_down().is_err())
        {
            shutdown_failed.set(true);
            eprintln!("Balun playback shutdown failed");
        }

        let shutdown_complete = Rc::clone(&shutdown_complete);
        let shutdown_failed = Rc::clone(&shutdown_failed);
        let settings = Rc::clone(&settings);
        let window = window.downgrade();
        gtk::glib::MainContext::default().spawn_local(async move {
            // Retain GTK and playback ownership on this local future while
            // only the controller join moves to the blocking worker. The
            // window closes only after the join and every queued settings
            // write finish, so the process never exits ahead of a write.
            let _retained_player_view = retained_player_view;
            let worker = gtk::gio::spawn_blocking(move || {
                controller.map_or(Ok(()), ControllerRuntime::join)
            });
            settings.drain().await;
            match worker.await {
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
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use balun::controller::{
        DiscoveryFailure, DiscoveryFuture, DiscoveryService, DiscoveryStatus, SelectedLineupStatus,
    };
    use balun::discovery::{
        DiscoveryClient, DiscoveryError, DiscoveryMethod, DiscoveryObservation, DiscoveryReport,
        ExactDiscoveryTarget, ProbeConfig,
    };
    use balun::domain::DeviceId;
    use balun::hdhr::protocol::{
        DEVICE_TYPE_TUNER, DISCOVERY_UDP_PORT, FRAME_OVERHEAD, MAX_PACKET_SIZE, TAG_BASE_URL,
        TAG_DEVICE_ID, TAG_DEVICE_TYPE, TAG_LINEUP_URL, TAG_TUNER_COUNT, TYPE_DISCOVER_REPLY,
        TYPE_DISCOVER_REQUEST,
    };
    use tokio_util::sync::CancellationToken;

    use super::*;
    use balun::settings::{DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH};

    #[test]
    fn exact_address_settlement_has_bounded_failure_feedback() {
        let generation = balun::controller::OperationGeneration::new(2);
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::exact_no_response(generation, 0)),
            Some("No valid HDHomeRun reply was received from the entered address.")
        );
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::failed_for(
                generation,
                DiscoveryKind::Exact,
                DiscoveryFailure::Network,
            )),
            Some("The exact-address device search failed.")
        );
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::failed_for(
                generation,
                DiscoveryKind::Exact,
                DiscoveryFailure::ExactTargetLimitReached,
            )),
            Some("The device address limit was reached for this session.")
        );
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::idle_for(
                generation,
                DiscoveryKind::Exact,
            )),
            Some("The exact-address device search was stopped.")
        );
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::ready_for(
                generation,
                DiscoveryKind::Exact,
                0,
            )),
            None
        );
        assert_eq!(
            exact_settlement_failure_toast(DiscoveryState::ready_for(
                generation,
                DiscoveryKind::Local,
                0,
            )),
            Some("The exact-address device search was replaced before it completed.")
        );
    }

    #[test]
    fn window_keyboard_contract_filters_modifiers_and_ambient_locks() {
        use gtk::gdk::{Key, ModifierType};

        assert_eq!(
            window_key_action(Key::F11, ModifierType::empty(), false),
            WindowKeyAction::ToggleFullscreen
        );
        assert_eq!(
            window_key_action(Key::F11, ModifierType::LOCK_MASK, true),
            WindowKeyAction::ToggleFullscreen
        );
        let ambient_mod2 = ModifierType::from_bits_retain(1 << 4);
        assert_eq!(
            window_key_action(Key::F11, ambient_mod2, false),
            WindowKeyAction::ToggleFullscreen
        );
        assert_eq!(
            window_key_action(Key::Escape, ModifierType::empty(), true),
            WindowKeyAction::ExitFullscreen
        );
        assert_eq!(
            window_key_action(Key::Escape, ModifierType::LOCK_MASK, true),
            WindowKeyAction::ExitFullscreen
        );

        // F5 refreshes devices
        assert_eq!(
            window_key_action(Key::F5, ModifierType::empty(), false),
            WindowKeyAction::RefreshDevices
        );
        assert_eq!(
            window_key_action(Key::F5, ModifierType::LOCK_MASK, false),
            WindowKeyAction::RefreshDevices
        );

        // Ctrl+F focuses search
        assert_eq!(
            window_key_action(Key::f, ModifierType::CONTROL_MASK, false),
            WindowKeyAction::FocusSearch
        );
        assert_eq!(
            window_key_action(Key::F, ModifierType::CONTROL_MASK, false),
            WindowKeyAction::FocusSearch
        );
        assert_eq!(
            window_key_action(
                Key::f,
                ModifierType::CONTROL_MASK | ModifierType::LOCK_MASK,
                true
            ),
            WindowKeyAction::FocusSearch
        );

        // Ctrl+R refreshes devices
        assert_eq!(
            window_key_action(Key::r, ModifierType::CONTROL_MASK, false),
            WindowKeyAction::RefreshDevices
        );
        assert_eq!(
            window_key_action(Key::R, ModifierType::CONTROL_MASK, false),
            WindowKeyAction::RefreshDevices
        );
        assert_eq!(
            window_key_action(
                Key::r,
                ModifierType::CONTROL_MASK | ModifierType::LOCK_MASK,
                false
            ),
            WindowKeyAction::RefreshDevices
        );

        // Modified F11, Escape, F5 are ignored
        for modifiers in [
            ModifierType::SHIFT_MASK,
            ModifierType::CONTROL_MASK,
            ModifierType::ALT_MASK,
            ModifierType::SUPER_MASK,
            ModifierType::HYPER_MASK,
            ModifierType::META_MASK,
        ] {
            assert_eq!(
                window_key_action(Key::F11, modifiers, false),
                WindowKeyAction::Ignore
            );
            assert_eq!(
                window_key_action(Key::Escape, modifiers, true),
                WindowKeyAction::Ignore
            );
            assert_eq!(
                window_key_action(Key::F5, modifiers, false),
                WindowKeyAction::Ignore
            );
        }

        // Additional modifier combinations with Ctrl are ignored
        assert_eq!(
            window_key_action(
                Key::f,
                ModifierType::CONTROL_MASK | ModifierType::SHIFT_MASK,
                false
            ),
            WindowKeyAction::Ignore
        );
        assert_eq!(
            window_key_action(
                Key::r,
                ModifierType::CONTROL_MASK | ModifierType::ALT_MASK,
                false
            ),
            WindowKeyAction::Ignore
        );

        assert_eq!(
            window_key_action(Key::Escape, ModifierType::empty(), false),
            WindowKeyAction::Ignore
        );
        assert_eq!(
            window_key_action(Key::F10, ModifierType::empty(), true),
            WindowKeyAction::Ignore
        );
    }

    #[test]
    fn shortcuts_only_activate_visible_sensitive_targets() {
        assert!(shortcut_target_available(true, true));
        assert!(!shortcut_target_available(false, true));
        assert!(!shortcut_target_available(true, false));
        assert!(!shortcut_target_available(false, false));

        assert!(refresh_shortcut_available(true, true, true));
        assert!(!refresh_shortcut_available(true, true, false));
        assert!(!refresh_shortcut_available(false, true, true));
        assert!(!refresh_shortcut_available(true, false, true));
    }

    #[test]
    fn refresh_page_availability_follows_responsive_navigation() {
        let mut state = ResponsiveLayoutState {
            medium_width: false,
            compact_width: false,
            fullscreen: false,
            outer_show_content: true,
            inner_show_content: true,
            outer_content_can_pop: true,
            inner_content_can_pop: true,
        };
        assert!(state.decision().devices_presented());
        state.medium_width = true;
        assert!(!state.decision().devices_presented());
        state.outer_show_content = false;
        assert!(state.decision().devices_presented());
        state.fullscreen = true;
        assert!(!state.decision().devices_presented());
    }

    #[test]
    fn fullscreen_layout_forces_player_then_restores_responsive_preferences() {
        let mut state = ResponsiveLayoutState {
            medium_width: false,
            compact_width: false,
            fullscreen: false,
            outer_show_content: true,
            inner_show_content: false,
            outer_content_can_pop: true,
            inner_content_can_pop: true,
        };
        assert_eq!(
            state.decision(),
            ResponsiveLayoutDecision {
                outer_collapsed: false,
                inner_collapsed: false,
                outer_show_content: true,
                inner_show_content: false,
                outer_content_can_pop: true,
                inner_content_can_pop: true,
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
                outer_content_can_pop: true,
                inner_content_can_pop: true,
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
                outer_content_can_pop: false,
                inner_content_can_pop: false,
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
                outer_content_can_pop: true,
                inner_content_can_pop: true,
            }
        );
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this compositor-dependent Wayland contract.
    #[test]
    #[ignore = "requires the isolated Wayland compositor and D-Bus session supplied by scripts/test-desktop-lifecycle.sh"]
    fn wayland_fullscreen_round_trip_protects_and_restores_navigation() {
        assert_eq!(
            std::env::var("GDK_BACKEND").as_deref(),
            Ok("wayland"),
            "this smoke must exercise GTK's Wayland backend"
        );

        let application = adw::Application::builder()
            .application_id("io.github.jm2.Balun.FullscreenSmoke")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        let completed = Rc::new(Cell::new(false));
        let failure = Rc::new(RefCell::new(None::<String>));
        let activated = Rc::new(Cell::new(false));

        {
            let completed = Rc::clone(&completed);
            let failure = Rc::clone(&failure);
            let activated = Rc::clone(&activated);
            application.connect_activate(move |application| {
                if activated.replace(true) {
                    return;
                }
                gtk::Settings::default()
                    .expect("fullscreen smoke requires display settings")
                    .set_gtk_enable_animations(false);

                let channel_focus = gtk::Button::with_label("Channels");
                let channel_page = adw::NavigationPage::new(&channel_focus, "Channels");
                let player_view = Rc::new(player_view::build(Err(
                    PlaybackInitializationError::InitializationFailed,
                )));
                let player_page = adw::NavigationPage::new(player_view.root(), "Live TV");
                let inner = adw::NavigationSplitView::builder()
                    .sidebar(&channel_page)
                    .content(&player_page)
                    .build();
                let outer_content =
                    adw::NavigationPage::new(&inner, "Channels and live TV");
                let device_page =
                    adw::NavigationPage::new(&gtk::Label::new(Some("Devices")), "Devices");
                let outer = adw::NavigationSplitView::builder()
                    .sidebar(&device_page)
                    .content(&outer_content)
                    .build();
                outer.set_show_content(true);
                inner.set_show_content(false);
                outer_content.set_can_pop(false);
                player_page.set_can_pop(true);

                let window = adw::ApplicationWindow::builder()
                    .application(application)
                    .title("Balun fullscreen round-trip proof")
                    .default_width(640)
                    .default_height(480)
                    .content(&outer)
                    .build();
                let layout = ResponsiveLayout::new(
                    &outer,
                    &outer_content,
                    &inner,
                    &player_page,
                    &player_view,
                );
                layout.set_compact_width(true);
                connect_window_shortcuts(&window, &player_view, &layout, None, None);

                let phase = Rc::new(Cell::new(0_u8));
                let start = {
                    let application = application.clone();
                    let failure = Rc::clone(&failure);
                    let phase = Rc::clone(&phase);
                    let layout = layout.clone();
                    let inner = inner.clone();
                    let channel_focus = channel_focus.clone();
                    let player_view = Rc::clone(&player_view);
                    let window = window.downgrade();
                    Rc::new(move || {
                        if phase.replace(1) != 0 {
                            return;
                        }
                        eprintln!("[balun] fullscreen smoke: prepare normal navigation");
                        let Some(window) = window.upgrade() else {
                            failure.replace(Some(
                                "fullscreen smoke window disappeared before entry".into(),
                            ));
                            application.quit();
                            return;
                        };
                        let navigation = layout.player_navigation();
                        navigation.show_player();
                        if !inner.shows_content() {
                            failure.replace(Some(
                                "compact channel activation did not present the player".into(),
                            ));
                            application.quit();
                            return;
                        }
                        layout.show_channels();
                        if inner.shows_content() {
                            failure.replace(Some(
                                "authoritative device selection did not restore Channels".into(),
                            ));
                            application.quit();
                            return;
                        }
                        navigation.show_player();
                        if !inner.shows_content() {
                            failure.replace(Some(
                                "activation after device selection did not restore the player"
                                    .into(),
                            ));
                            application.quit();
                            return;
                        }

                        // Exercise the same native property change emitted by
                        // Back/pop, then prove it updates the retained reducer
                        // preference before fullscreen forces the player.
                        inner.set_show_content(false);
                        layout.set_medium_width(false);
                        if inner.shows_content() || layout.state.get().inner_show_content {
                            failure.replace(Some(
                                "native Back/pop was not retained outside fullscreen".into(),
                            ));
                            application.quit();
                            return;
                        }
                        if !channel_focus.grab_focus()
                            || gtk::prelude::GtkWindowExt::focus(&window).as_ref()
                                != Some(channel_focus.upcast_ref())
                        {
                            failure.replace(Some(
                                "channel focus could not be established before fullscreen".into(),
                            ));
                            application.quit();
                            return;
                        }
                        player_view.fullscreen_button().emit_clicked();
                    }) as Rc<dyn Fn()>
                };

                {
                    let application = application.clone();
                    let completed = Rc::clone(&completed);
                    let failure = Rc::clone(&failure);
                    let phase = Rc::clone(&phase);
                    let start = Rc::clone(&start);
                    let layout = layout.clone();
                    let outer = outer.clone();
                    let outer_content = outer_content.clone();
                    let inner = inner.clone();
                    let player_page = player_page.clone();
                    let player_view = Rc::clone(&player_view);
                    let channel_focus = channel_focus.clone();
                    window.connect_fullscreened_notify(move |window| {
                        eprintln!(
                            "[balun] fullscreen smoke: notify phase={} fullscreen={}",
                            phase.get(),
                            window.is_fullscreen()
                        );
                        match (phase.get(), window.is_fullscreen()) {
                            (0, false) => {
                                let start = Rc::clone(&start);
                                gtk::glib::idle_add_local_once(move || start());
                            }
                            (1, true) => {
                                if !layout.is_fullscreen()
                                    || !outer.is_collapsed()
                                    || !inner.is_collapsed()
                                    || !outer.shows_content()
                                    || !inner.shows_content()
                                    || outer_content.can_pop()
                                    || player_page.can_pop()
                                    || gtk::prelude::GtkWindowExt::focus(window).as_ref()
                                        != Some(player_view.fullscreen_button().upcast_ref())
                                {
                                    failure.replace(Some(
                                        "confirmed fullscreen did not protect navigation and focus the exit control".into(),
                                    ));
                                    let application = application.clone();
                                    gtk::glib::idle_add_local_once(move || application.quit());
                                    return;
                                }
                                phase.set(2);
                                let fullscreen_button =
                                    player_view.fullscreen_button().clone();
                                gtk::glib::idle_add_local_once(move || {
                                    eprintln!(
                                        "[balun] fullscreen smoke: request confirmed exit"
                                    );
                                    fullscreen_button.emit_clicked();
                                });
                            }
                            (2, false) => {
                                if layout.is_fullscreen()
                                    || outer.is_collapsed()
                                    || !outer.shows_content()
                                    || !inner.is_collapsed()
                                    || inner.shows_content()
                                    || outer_content.can_pop()
                                    || !player_page.can_pop()
                                    || gtk::prelude::GtkWindowExt::focus(window).as_ref()
                                        != Some(channel_focus.upcast_ref())
                                {
                                    failure.replace(Some(
                                        "fullscreen exit did not restore navigation and focus exactly".into(),
                                    ));
                                    let application = application.clone();
                                    gtk::glib::idle_add_local_once(move || application.quit());
                                    return;
                                }
                                phase.set(3);
                                completed.set(true);
                                let application = application.clone();
                                let window = window.downgrade();
                                gtk::glib::idle_add_local_once(move || {
                                    eprintln!(
                                        "[balun] fullscreen smoke: close after confirmed exit"
                                    );
                                    if let Some(window) = window.upgrade() {
                                        window.close();
                                    }
                                    gtk::glib::idle_add_local_once(move || application.quit());
                                });
                            }
                            _ => {}
                        }
                    });
                }

                {
                    let start = Rc::clone(&start);
                    let window_weak = window.downgrade();
                    window.connect_map(move |_| {
                        let start = Rc::clone(&start);
                        let window_weak = window_weak.clone();
                        gtk::glib::idle_add_local_once(move || {
                            let Some(window) = window_weak.upgrade() else {
                                return;
                            };
                            if window.is_fullscreen() {
                                // Normalize kiosk-style compositor policy
                                // before proving a complete enter/exit cycle.
                                window.unfullscreen();
                            } else {
                                start();
                            }
                        });
                    });
                }

                {
                    let application = application.clone();
                    let completed = Rc::clone(&completed);
                    let failure = Rc::clone(&failure);
                    gtk::glib::timeout_add_local_once(Duration::from_secs(10), move || {
                        if !completed.get() && failure.borrow().is_none() {
                            failure.replace(Some(
                                "Wayland compositor did not confirm the fullscreen round trip"
                                    .into(),
                            ));
                            application.quit();
                        }
                    });
                }

                window.present();
            });
        }

        let exit_code = application.run_with_args(&["balun-fullscreen-smoke"]);
        assert_eq!(exit_code, gtk::glib::ExitCode::SUCCESS);
        assert!(
            failure.borrow().is_none(),
            "{}",
            failure.borrow().as_deref().unwrap_or_default()
        );
        assert!(completed.get(), "fullscreen smoke did not complete");
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

    /// Checksum-valid synthetic identity of the loopback discovery responder.
    const WINDOW_SMOKE_DEVICE_ID: u32 = 0x105A_1232;
    /// Checksum-valid second synthetic identity for the device-change lane.
    const WINDOW_SMOKE_SECOND_DEVICE_ID: u32 = 0x205B_0144;
    /// Synthetic discovery source of the unreachable second device.
    const WINDOW_SMOKE_SECOND_SOURCE_PORT: u16 = 65_002;
    /// Bound for the whole phase-driven window release flow.
    const WINDOW_SMOKE_PHASE_BOUND: Duration = Duration::from_secs(45);
    /// Outer watchdog bound; the lifecycle script still enforces its own.
    const WINDOW_SMOKE_WATCHDOG: Duration = Duration::from_secs(50);

    fn window_smoke_device_id() -> DeviceId {
        DeviceId::new(WINDOW_SMOKE_DEVICE_ID)
            .expect("the loopback responder identity is checksum-valid")
    }

    fn window_smoke_second_device_id() -> DeviceId {
        DeviceId::new(WINDOW_SMOKE_SECOND_DEVICE_ID)
            .expect("the second synthetic identity is checksum-valid")
    }

    fn window_smoke_probe_config() -> ProbeConfig {
        ProbeConfig::new(1, Duration::from_millis(200), 16, 4)
            .expect("the fixed window-smoke probe budget is valid")
    }

    fn window_smoke_discovery_failure(error: DiscoveryError) -> DiscoveryFailure {
        match error {
            DiscoveryError::Interfaces(_) => DiscoveryFailure::InterfaceEnumeration,
            DiscoveryError::Io { .. } | DiscoveryError::ShortSend { .. } => {
                DiscoveryFailure::Network
            }
            DiscoveryError::InvalidEndpoint { .. }
            | DiscoveryError::Task(_)
            | DiscoveryError::RoutedScanDeadline { .. }
            | DiscoveryError::Cancelled
            | DiscoveryError::Protocol(_) => DiscoveryFailure::Internal,
        }
    }

    fn window_smoke_second_observation() -> DiscoveryObservation {
        DiscoveryObservation {
            device_id: window_smoke_second_device_id(),
            source: SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                WINDOW_SMOKE_SECOND_SOURCE_PORT,
            ),
            method: DiscoveryMethod::Targeted,
            interface: None,
            device_types: vec![DEVICE_TYPE_TUNER],
            tuner_count: Some(1),
            advertised_base_url: None,
            advertised_lineup_url: None,
        }
    }

    /// Stateful discovery lane behind the window smoke: odd refreshes run the
    /// real targeted probe against the loopback responder and retain one
    /// hand-built observation for the second synthetic device; the second
    /// refresh reports the device gone (the mutation lane).
    struct WindowSmokeDiscovery {
        target: SocketAddr,
        refresh_calls: Arc<AtomicUsize>,
    }

    impl DiscoveryService for WindowSmokeDiscovery {
        fn discover_local(&self, cancellation: CancellationToken) -> DiscoveryFuture {
            let client = DiscoveryClient::new(window_smoke_probe_config());
            let target = self.target;
            let refresh_calls = Arc::clone(&self.refresh_calls);
            Box::pin(async move {
                let call = refresh_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call.is_multiple_of(2) {
                    return Ok::<_, DiscoveryFailure>(DiscoveryReport::default());
                }
                let mut report = client
                    .discover_target(target, None, &cancellation)
                    .await
                    .map_err(window_smoke_discovery_failure)?;
                report.observations.push(window_smoke_second_observation());
                Ok(report)
            })
        }

        fn discover_exact(
            &self,
            _target: ExactDiscoveryTarget,
            _expected_device: Option<DeviceId>,
            _cancellation: CancellationToken,
        ) -> DiscoveryFuture {
            // The window smoke never issues exact-address discovery; like the
            // packet-free application smoke, answer with an empty report.
            Box::pin(async { Ok::<_, DiscoveryFailure>(DiscoveryReport::default()) })
        }
    }

    /// Loopback UDP responder impersonating one HDHomeRun tuner on the fixed
    /// discovery port. It advertises production-shaped port-80 metadata URLs;
    /// nothing serves them, so a real selection settles `Unreachable` without
    /// ever contacting a tuner.
    struct WindowSmokeResponder {
        target: SocketAddr,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl WindowSmokeResponder {
        fn start() -> Self {
            let target = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DISCOVERY_UDP_PORT);
            let socket = UdpSocket::bind(target).expect("bind the loopback discovery responder");
            let stop = Arc::new(AtomicBool::new(false));
            let reply = encode_window_smoke_discover_reply();
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
                let mut buffer = [0_u8; MAX_PACKET_SIZE];
                while !worker_stop.load(Ordering::Acquire) {
                    match socket.recv_from(&mut buffer) {
                        Ok((received, peer)) => {
                            if received >= 2
                                && u16::from_be_bytes([buffer[0], buffer[1]])
                                    == TYPE_DISCOVER_REQUEST
                            {
                                let _ = socket.send_to(&reply, peer);
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => return,
                    }
                }
            });
            Self {
                target,
                stop,
                worker: Some(worker),
            }
        }
    }

    impl Drop for WindowSmokeResponder {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn encode_window_smoke_discover_reply() -> Vec<u8> {
        let mut payload = Vec::new();
        push_window_smoke_tlv(
            &mut payload,
            TAG_DEVICE_TYPE,
            &DEVICE_TYPE_TUNER.to_be_bytes(),
        );
        push_window_smoke_tlv(
            &mut payload,
            TAG_DEVICE_ID,
            &WINDOW_SMOKE_DEVICE_ID.to_be_bytes(),
        );
        push_window_smoke_tlv(&mut payload, TAG_TUNER_COUNT, &[2]);
        push_window_smoke_tlv(&mut payload, TAG_BASE_URL, b"http://127.0.0.1:80");
        push_window_smoke_tlv(
            &mut payload,
            TAG_LINEUP_URL,
            b"http://127.0.0.1:80/lineup.json",
        );

        let payload_length = u16::try_from(payload.len())
            .expect("the smoke discovery payload stays far below the u16 maximum");
        let mut frame = Vec::with_capacity(payload.len() + FRAME_OVERHEAD);
        frame.extend_from_slice(&TYPE_DISCOVER_REPLY.to_be_bytes());
        frame.extend_from_slice(&payload_length.to_be_bytes());
        frame.extend_from_slice(&payload);
        let crc = crc32fast::hash(&frame);
        frame.extend_from_slice(&crc.to_le_bytes());
        frame
    }

    fn push_window_smoke_tlv(payload: &mut Vec<u8>, tag: u8, value: &[u8]) {
        payload.push(tag);
        if value.len() < 0x80 {
            payload.push(value.len() as u8);
        } else {
            payload.push(0x80 | (value.len() & 0x7F) as u8);
            payload.push((value.len() >> 7) as u8);
        }
        payload.extend_from_slice(value);
    }

    fn device_row_position(selection: &gtk::SingleSelection, wanted: DeviceId) -> Option<u32> {
        let model = selection.model()?;
        (0..model.n_items()).find(|position| {
            model
                .item(*position)
                .and_then(|item| item.downcast::<DeviceRowObject>().ok())
                .and_then(|row| row.device_id())
                == Some(wanted)
        })
    }

    fn fail_window_smoke(
        failure: &Rc<RefCell<Option<String>>>,
        application: &adw::Application,
        text: String,
    ) {
        if failure.borrow().is_none() {
            failure.replace(Some(text));
        }
        application.quit();
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this compositor-dependent Wayland contract.
    ///
    /// Crate-boundary note: the loopback fake *stream* device is a lib-test
    /// module, and the bin target enforces the production metadata-port
    /// policy whose loopback exemption compiles only into lib test builds, so
    /// this bin-side window proof cannot open a real tuner or host active
    /// playback. It instead drives the production window wiring end to end
    /// against the real controller and a real targeted discovery probe, and
    /// observes `PlayerView`'s own test-only stop counter directly: the
    /// sidebar-signal stop-on-admission for a user device change, the snapshot
    /// reducer's accepted-generation stop for the mutation that makes the
    /// selected device vanish, and the joined close shutdown of the controller
    /// and the playback session. The real-tuner release proofs for these same
    /// window stop paths (a live tuner observed `Closed` on device change,
    /// mutation, and close) live in `playback::fake_device_e2e`, and the
    /// sidebar admission stop itself is separately covered by the widget
    /// tests in this crate.
    #[test]
    #[ignore = "requires the isolated Wayland compositor and D-Bus session supplied by scripts/test-desktop-lifecycle.sh"]
    fn fake_device_window_releases_tuners_on_device_change_mutation_and_close() {
        assert_eq!(
            std::env::var("GDK_BACKEND").as_deref(),
            Ok("wayland"),
            "this smoke must exercise GTK's Wayland backend"
        );

        let responder = Rc::new(WindowSmokeResponder::start());
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let controller = ControllerRuntime::start(WindowSmokeDiscovery {
            target: responder.target,
            refresh_calls: Arc::clone(&refresh_calls),
        })
        .expect("start the window smoke controller against the loopback responder");

        let application = adw::Application::builder()
            .application_id("io.github.jm2.Balun.FakeDeviceWindowSmoke")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        let completed = Rc::new(Cell::new(false));
        let failure = Rc::new(RefCell::new(None::<String>));
        let shutdown_failed = Rc::new(Cell::new(false));
        let setup = Rc::new(RefCell::new(Some((Rc::clone(&responder), controller))));
        let activated = Rc::new(Cell::new(false));

        {
            let completed = Rc::clone(&completed);
            let failure = Rc::clone(&failure);
            let shutdown_failed = Rc::clone(&shutdown_failed);
            let setup = Rc::clone(&setup);
            let activated = Rc::clone(&activated);
            application.connect_activate(move |application| {
                if activated.replace(true) {
                    return;
                }
                gtk::Settings::default()
                    .expect("window release smoke requires display settings")
                    .set_gtk_enable_animations(false);

                let Some((_responder, controller)) = setup.borrow_mut().take() else {
                    fail_window_smoke(
                        &failure,
                        application,
                        "the window release smoke lost its controller setup".into(),
                    );
                    return;
                };
                let playback = match PlaybackRuntime::initialize() {
                    Ok(playback) => playback,
                    Err(_) => {
                        fail_window_smoke(
                            &failure,
                            application,
                            "the lifecycle harness must provide the playback runtime".into(),
                        );
                        return;
                    }
                };
                if !playback.capabilities().is_foundation_ready() {
                    fail_window_smoke(
                        &failure,
                        application,
                        "the lifecycle harness must install the complete playback foundation"
                            .into(),
                    );
                    return;
                }

                // Mirror build()'s production wiring while retaining the
                // panes the phase driver must reach as a user would.
                let device_sidebar = device_sidebar::build();
                let channel_sidebar = channel_sidebar::build();
                let player_view = Rc::new(player_view::build(Ok(playback)));
                player_view.connect_stop_control();
                player_view.connect_audio_controls();
                player_view.connect_session_state();

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
                    .default_width(i32::try_from(DEFAULT_WINDOW_WIDTH).expect("fits i32"))
                    .default_height(i32::try_from(DEFAULT_WINDOW_HEIGHT).expect("fits i32"))
                    .content(&device_and_content)
                    .build();
                let layout = ResponsiveLayout::new(
                    &device_and_content,
                    &channel_and_player_page,
                    &channel_and_player,
                    &player_page,
                    &player_view,
                );
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
                let compact_breakpoint =
                    adw::Breakpoint::new(adw::BreakpointCondition::new_length(
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
                    compact_breakpoint.connect_unapply(move |_| {
                        layout.set_compact_width(false);
                    });
                }
                window.add_breakpoint(compact_breakpoint);

                let handle = controller.handle();
                let mut reducer_snapshots = handle.subscribe();
                let initial = Arc::clone(&reducer_snapshots.borrow_and_update());
                device_sidebar.apply_snapshot(&initial);
                channel_sidebar.apply_snapshot(&initial);
                layout.set_device_selected(initial.selected_device().is_some());
                let accepted = Rc::new(RefCell::new(initial));
                let poll_snapshots = handle.subscribe();
                let settings = Rc::new(SettingsSession::open(None));
                let exact_tracker = Rc::new(RefCell::new(ExactTargetTracker::new()));
                let rediscovery = Rc::new(RefCell::new(RediscoveryQueue::new([])));
                let toasts = adw::ToastOverlay::new();
                let wiring = Rc::new(RediscoveryWiring {
                    settings: Rc::clone(&settings),
                    exact_tracker: Rc::clone(&exact_tracker),
                    rediscovery: Rc::clone(&rediscovery),
                    hostnames: Rc::new(RefCell::new(HostnameProbes::default())),
                    controller: handle.clone(),
                    accepted: Rc::clone(&accepted),
                    toasts: toasts.downgrade(),
                });

                connect_refresh(&device_sidebar, &handle);
                connect_exact_discovery(&window, &device_sidebar, &handle, &exact_tracker, &wiring);
                connect_cancel_discovery(&device_sidebar, &handle, &wiring);
                let routed_ui = Rc::new(RoutedUi::new(&window, Rc::clone(&wiring)));
                connect_routed_discovery(&window, &device_sidebar, &routed_ui);
                connect_device_selection(
                    &device_sidebar,
                    &handle,
                    &accepted,
                    &player_view,
                    &layout,
                );
                connect_channel_activation(
                    &channel_sidebar,
                    &handle,
                    &player_view,
                    layout.player_navigation(),
                    &accepted,
                );
                connect_window_shortcuts(
                    &window,
                    &player_view,
                    &layout,
                    Some(device_sidebar.refresh_button().clone()),
                    Some(channel_sidebar.search_entry().clone()),
                );
                spawn_snapshot_reducer(
                    reducer_snapshots,
                    Rc::clone(&accepted),
                    device_sidebar.clone(),
                    channel_sidebar,
                    layout,
                    Rc::downgrade(&player_view),
                    SnapshotReactions {
                        rediscovery: Rc::clone(&wiring),
                        routed_ui,
                    },
                );
                connect_joined_shutdown(
                    &window,
                    controller,
                    Rc::clone(&player_view),
                    settings,
                    Rc::clone(&shutdown_failed),
                );

                // Phase-driven state machine mirroring the fullscreen smoke:
                // bounded phases advance from real controller publications,
                // with a deadline watchdog and a fixed-copy failure channel.
                // The stop gates compare `PlayerView::stop` invocations
                // against a baseline taken while the snapshot reducer has
                // settled on the observed revision, so each cleanup path must
                // itself raise the count: the sidebar admission stop for the
                // user device change and the reducer's accepted-generation
                // stop for the mutation.
                let phase = Rc::new(Cell::new(0_u8));
                let device_selection = device_sidebar.selection().clone();
                let player_status = Rc::clone(&player_view);
                let driver_handle = handle.clone();
                let refresh_state = Arc::clone(&refresh_calls);
                let driver_completed = Rc::clone(&completed);
                let driver_failure = Rc::clone(&failure);
                let driver_application = application.clone();
                let window_weak = window.downgrade();
                let accepted_generation = Rc::clone(&accepted);
                let stop_baseline = Rc::new(Cell::new(0_u32));
                let deadline = Instant::now() + WINDOW_SMOKE_PHASE_BOUND;
                gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
                    if driver_completed.get() || driver_failure.borrow().is_some() {
                        return gtk::glib::ControlFlow::Continue;
                    }
                    if Instant::now() > deadline {
                        fail_window_smoke(
                            &driver_failure,
                            &driver_application,
                            "the window release phases did not advance within their bound".into(),
                        );
                        return gtk::glib::ControlFlow::Continue;
                    }
                    let snapshot = Arc::clone(&poll_snapshots.borrow());
                    let reducer_generation = accepted_generation.borrow().selection_generation();
                    let stop_calls = player_status.stop_call_count();
                    let row_count = device_selection
                        .model()
                        .map(|model| model.n_items())
                        .unwrap_or(0);
                    match phase.get() {
                        0 => {
                            if driver_handle
                                .try_send(ControllerCommand::RefreshLocalDiscovery)
                                .is_err()
                            {
                                fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the window smoke could not admit its first local refresh"
                                        .into(),
                                );
                                return gtk::glib::ControlFlow::Continue;
                            }
                            phase.set(1);
                        }
                        1 if row_count == 2
                            && snapshot.devices().len() == 2
                            && snapshot.discovery().status() == DiscoveryStatus::Ready =>
                        {
                            match device_row_position(&device_selection, window_smoke_device_id()) {
                                Some(position) => device_selection.set_selected(position),
                                None => fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the discovered loopback device row is missing".into(),
                                ),
                            }
                            phase.set(2);
                        }
                        2 if snapshot.selected_device() == Some(window_smoke_device_id())
                            && matches!(
                                snapshot.selected_lineup().status(),
                                SelectedLineupStatus::Failed(_)
                            )
                            && reducer_generation == snapshot.selection_generation() =>
                        {
                            if player_status.playback_status().label() != "Stopped" {
                                fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the idle player left its stopped presentation".into(),
                                );
                                return gtk::glib::ControlFlow::Continue;
                            }
                            // User device change: the sidebar signal's
                            // stop-on-admission fires here, and only the
                            // device change can raise the count past this
                            // reducer-settled baseline.
                            stop_baseline.set(stop_calls);
                            match device_row_position(
                                &device_selection,
                                window_smoke_second_device_id(),
                            ) {
                                Some(position) => device_selection.set_selected(position),
                                None => fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the second synthetic device row is missing".into(),
                                ),
                            }
                            phase.set(3);
                        }
                        3 if snapshot.selected_device()
                            == Some(window_smoke_second_device_id())
                            && matches!(
                                snapshot.selected_lineup().status(),
                                SelectedLineupStatus::Failed(_)
                            )
                            && reducer_generation == snapshot.selection_generation()
                            && stop_calls > stop_baseline.get() =>
                        {
                            // The admission stop (and any fail-safe repeat for
                            // the new selection generation) has fired. Baseline
                            // again with the reducer settled, so only the
                            // mutation's accepted-generation stop can raise it.
                            stop_baseline.set(stop_calls);
                            if driver_handle
                                .try_send(ControllerCommand::RefreshLocalDiscovery)
                                .is_err()
                            {
                                fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the window smoke could not admit its mutation refresh".into(),
                                );
                                return gtk::glib::ControlFlow::Continue;
                            }
                            phase.set(4);
                        }
                        4 if row_count == 0
                            && snapshot.devices().is_empty()
                            && snapshot.selected_device().is_none()
                            && snapshot.selected_lineup().status()
                                == SelectedLineupStatus::Unselected
                            && refresh_state.load(Ordering::SeqCst) == 2
                            && stop_calls > stop_baseline.get() =>
                        {
                            if player_status.playback_status().label() != "Stopped" {
                                fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the mutation did not settle the player presentation".into(),
                                );
                                return gtk::glib::ControlFlow::Continue;
                            }
                            let Some(window) = window_weak.upgrade() else {
                                fail_window_smoke(
                                    &driver_failure,
                                    &driver_application,
                                    "the window release smoke window disappeared before close"
                                        .into(),
                                );
                                return gtk::glib::ControlFlow::Continue;
                            };
                            // Joined close: the controller and playback
                            // session settle together here.
                            driver_completed.set(true);
                            window.close();
                            phase.set(5);
                        }
                        _ => {}
                    }
                    gtk::glib::ControlFlow::Continue
                });

                {
                    let completed = Rc::clone(&completed);
                    let failure = Rc::clone(&failure);
                    let application = application.clone();
                    gtk::glib::timeout_add_local_once(WINDOW_SMOKE_WATCHDOG, move || {
                        if !completed.get() && failure.borrow().is_none() {
                            failure.replace(Some(
                                "the window release smoke did not complete within its bound".into(),
                            ));
                            application.quit();
                        }
                    });
                }

                window.present();
            });
        }

        let exit_code = application.run_with_args(&["balun-fake-device-window-smoke"]);
        assert_eq!(exit_code, gtk::glib::ExitCode::SUCCESS);
        let failure_text = failure.borrow().clone();
        assert!(
            failure_text.is_none(),
            "{}",
            failure_text.unwrap_or_default()
        );
        assert!(
            completed.get(),
            "the window release smoke did not reach its close phase"
        );
        assert!(
            !shutdown_failed.get(),
            "joined controller and playback shutdown must succeed"
        );
    }
}
