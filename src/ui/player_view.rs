//! Live-TV picture shell and generation-owned playback-session owner.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use balun::controller::{ControllerHandle, StreamHandoff, StreamHandoffError, StreamSelection};
use balun::playback::{
    PlaybackCapabilities, PlaybackInitializationError, PlaybackRuntime, PlaybackSession,
    PlaybackSessionFailure, TuneCompletion, TuneRequest,
};

/// Main-context-owned player pane.
///
/// The native pipeline and GTK sink remain private to `PlaybackSession`; this
/// pane retains only that owner and the URI-opaque GDK paintable it publishes.
pub(crate) struct PlayerView {
    root: adw::ToolbarView,
    header: adw::HeaderBar,
    picture: gtk::Picture,
    status: adw::StatusPage,
    volume_adjustment: gtk::Adjustment,
    volume_scale: gtk::Scale,
    mute_button: gtk::ToggleButton,
    stop_button: gtk::Button,
    fullscreen_button: gtk::Button,
    idle_title: String,
    idle_description: String,
    session: Option<PlaybackSession>,
    updating_audio_controls: Cell<bool>,
    pending_response: RefCell<Option<gtk::glib::JoinHandle<()>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlayerAccessibilityPlan {
    volume_label: &'static str,
    mute_label: &'static str,
    unmute_label: &'static str,
    stop_label: &'static str,
    enter_fullscreen_label: &'static str,
    exit_fullscreen_label: &'static str,
    enter_fullscreen_shortcuts: &'static str,
    exit_fullscreen_shortcuts: &'static str,
}

const PLAYER_ACCESSIBILITY: PlayerAccessibilityPlan = PlayerAccessibilityPlan {
    volume_label: "Live TV volume",
    mute_label: "Mute live TV",
    unmute_label: "Unmute live TV",
    stop_label: "Stop live TV",
    enter_fullscreen_label: "Enter fullscreen",
    exit_fullscreen_label: "Exit fullscreen",
    enter_fullscreen_shortcuts: "F11",
    exit_fullscreen_shortcuts: "F11 Escape",
};

impl PlayerView {
    /// Return the widget rooted in the live-TV navigation page.
    pub(crate) const fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// Return the native pointer/Enter/Space fullscreen request control.
    pub(crate) const fn fullscreen_button(&self) -> &gtk::Button {
        &self.fullscreen_button
    }

    /// Reconcile presentation only after the application window reports its
    /// compositor-confirmed fullscreen state.
    pub(crate) fn apply_fullscreen_presentation(&self, fullscreen: bool) {
        let (icon, label, shortcuts) = if fullscreen {
            (
                "view-restore-symbolic",
                PLAYER_ACCESSIBILITY.exit_fullscreen_label,
                PLAYER_ACCESSIBILITY.exit_fullscreen_shortcuts,
            )
        } else {
            (
                "view-fullscreen-symbolic",
                PLAYER_ACCESSIBILITY.enter_fullscreen_label,
                PLAYER_ACCESSIBILITY.enter_fullscreen_shortcuts,
            )
        };
        self.fullscreen_button.set_icon_name(icon);
        self.fullscreen_button.set_tooltip_text(Some(label));
        self.fullscreen_button.update_property(&[
            gtk::accessible::Property::Label(label),
            gtk::accessible::Property::KeyShortcuts(shortcuts),
        ]);
        self.header.set_show_back_button(!fullscreen);
        self.root.set_extend_content_to_top_edge(fullscreen);
    }

    /// Synchronize the production picture with the session's current opaque
    /// paintable. The pipeline, sink, and URI never cross this boundary.
    pub(crate) fn sync_paintable(&self) -> Result<bool, PlaybackSessionFailure> {
        let paintable = match self.session.as_ref() {
            Some(session) => session.paintable()?,
            None => None,
        };
        Ok(self.apply_paintable(paintable.as_ref()))
    }

    /// Connect the accessible Stop control without retaining this owner in its
    /// GTK signal closure.
    pub(crate) fn connect_stop_control(self: &Rc<Self>) {
        let player_view = Rc::downgrade(self);
        self.stop_button.connect_clicked(move |button| {
            button.set_sensitive(false);
            if let Some(player_view) = player_view.upgrade() {
                let _ = player_view.stop();
            }
        });
    }

    /// Connect the native volume and mute widgets without retaining this owner
    /// in either GTK signal closure.
    pub(crate) fn connect_audio_controls(self: &Rc<Self>) {
        let player_view = Rc::downgrade(self);
        self.volume_adjustment
            .connect_value_changed(move |adjustment| {
                let Some(player_view) = player_view.upgrade() else {
                    return;
                };
                if player_view.updating_audio_controls.get() {
                    return;
                }
                player_view.apply_volume(adjustment.value() / 100.0);
            });

        let player_view = Rc::downgrade(self);
        self.mute_button.connect_toggled(move |button| {
            let Some(player_view) = player_view.upgrade() else {
                return;
            };
            if player_view.updating_audio_controls.get() {
                return;
            }
            player_view.apply_muted(button.is_active());
        });
    }

    /// Start one URL-free channel intent and consume the actor-private response
    /// only through the generation-owned playback session.
    pub(crate) fn activate_channel(
        self: &Rc<Self>,
        controller: &ControllerHandle,
        selection: StreamSelection,
    ) {
        self.abort_pending_response();

        let request = self
            .session
            .as_ref()
            .ok_or(PlaybackSessionFailure::ComponentsUnavailable)
            .and_then(|session| session.begin_tune(selection));
        // Even failed predecessor teardown must never leave its old frame in
        // the production picture.
        self.apply_paintable(None);
        let request = match request {
            Ok(request) => request,
            Err(failure) => {
                self.stop_button.set_sensitive(false);
                self.show_session_failure(failure);
                return;
            }
        };
        self.stop_button.set_sensitive(true);
        self.show_connecting();

        let receiver = match controller.try_request_stream(request.selection().clone()) {
            Ok(receiver) => receiver,
            Err(_) => {
                if let Some(session) = self.session.as_ref() {
                    let _ = session.cancel_tune(request);
                }
                self.stop_button.set_sensitive(false);
                self.show_playback_failure();
                return;
            }
        };

        let player_view = Rc::downgrade(self);
        let task = gtk::glib::MainContext::default().spawn_local(async move {
            let response = receiver.receive().await;
            let Some(player_view) = player_view.upgrade() else {
                return;
            };
            player_view.finish_tune(request, response);
        });
        self.pending_response.replace(Some(task));
    }

    /// Cancel pending resolution, hide any retained frame, and settle the
    /// current generation without making the session terminal.
    pub(crate) fn stop(&self) -> Result<(), PlaybackSessionFailure> {
        self.stop_button.set_sensitive(false);
        self.abort_pending_response();
        self.apply_paintable(None);
        let result = match self.session.as_ref() {
            Some(session) => session.stop(),
            None => Ok(()),
        };
        match result {
            Ok(()) => {
                self.show_idle();
                Ok(())
            }
            Err(failure) => {
                self.show_stop_failure();
                Err(failure)
            }
        }
    }

    /// Clear presentation and terminally settle the playback owner.
    pub(crate) fn shut_down(&self) -> Result<(), PlaybackSessionFailure> {
        self.stop_button.set_sensitive(false);
        self.set_audio_controls_sensitive(false);
        self.abort_pending_response();
        self.apply_paintable(None);
        match self.session.as_ref() {
            Some(session) => session.shut_down(),
            None => Ok(()),
        }
    }

    fn apply_paintable(&self, paintable: Option<&gtk::gdk::Paintable>) -> bool {
        self.picture.set_paintable(paintable);
        let has_video = paintable.is_some();
        self.status.set_visible(!has_video);
        has_video
    }

    fn finish_tune(
        &self,
        request: TuneRequest,
        response: Result<StreamHandoff, StreamHandoffError>,
    ) {
        let Some(session) = self.session.as_ref() else {
            drop(response);
            self.stop_button.set_sensitive(false);
            self.show_playback_failure();
            return;
        };
        match session.complete_tune(request, response) {
            Ok(TuneCompletion::Applied) => match self.sync_paintable() {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    self.apply_paintable(None);
                    self.stop_button.set_sensitive(false);
                    if session.stop().is_err() {
                        self.show_stop_failure();
                    } else {
                        self.show_playback_failure();
                    }
                }
            },
            Ok(TuneCompletion::Stale) => {
                // Reflect the current generation, which may already own a
                // successor paintable or may deliberately be idle.
                let _ = self.sync_paintable();
            }
            Err(failure) => {
                self.apply_paintable(None);
                self.stop_button.set_sensitive(false);
                self.show_session_failure(failure);
            }
        }
    }

    fn abort_pending_response(&self) {
        if let Some(task) = self.pending_response.borrow_mut().take() {
            task.abort();
        }
    }

    fn apply_volume(&self, volume: f64) {
        let Some(session) = self.session.as_ref() else {
            self.set_audio_controls_sensitive(false);
            return;
        };
        match session.set_volume(volume) {
            Ok(()) => {
                if let Ok(audio) = session.audio_state() {
                    self.sync_audio_controls(audio);
                }
            }
            Err(_) => self.restore_audio_controls(),
        }
    }

    fn apply_muted(&self, muted: bool) {
        let Some(session) = self.session.as_ref() else {
            self.set_audio_controls_sensitive(false);
            return;
        };
        match session.set_muted(muted) {
            Ok(()) => {
                if let Ok(audio) = session.audio_state() {
                    self.sync_audio_controls(audio);
                }
            }
            Err(_) => self.restore_audio_controls(),
        }
    }

    fn restore_audio_controls(&self) {
        match self
            .session
            .as_ref()
            .and_then(|session| session.audio_state().ok())
        {
            Some(audio) => self.sync_audio_controls(audio),
            None => self.set_audio_controls_sensitive(false),
        }
    }

    fn sync_audio_controls(&self, audio: balun::playback::PlaybackAudioState) {
        let previous = self.updating_audio_controls.replace(true);
        self.volume_adjustment.set_value(audio.volume() * 100.0);
        self.mute_button.set_active(audio.is_muted());
        update_mute_presentation(&self.mute_button, audio.volume(), audio.is_muted());
        self.updating_audio_controls.set(previous);
    }

    fn set_audio_controls_sensitive(&self, sensitive: bool) {
        self.volume_scale.set_sensitive(sensitive);
        self.mute_button.set_sensitive(sensitive);
    }

    fn show_connecting(&self) {
        self.status.set_title("Connecting");
        self.status
            .set_description(Some("Opening the selected channel on this device."));
        self.status.set_visible(true);
    }

    fn show_idle(&self) {
        self.status.set_title(&self.idle_title);
        self.status.set_description(Some(&self.idle_description));
        self.status.set_visible(true);
    }

    fn show_playback_failure(&self) {
        self.status.set_title("Unable to play channel");
        self.status.set_description(Some(
            "The selected channel could not be started. Device discovery and lineup inspection remain available.",
        ));
        self.status.set_visible(true);
    }

    fn show_stop_failure(&self) {
        self.set_audio_controls_sensitive(false);
        self.status.set_title("Unable to stop live TV");
        self.status.set_description(Some(
            "Playback could not be stopped cleanly. Close Balun before selecting another channel.",
        ));
        self.status.set_visible(true);
    }

    fn show_session_failure(&self, failure: PlaybackSessionFailure) {
        if failure == PlaybackSessionFailure::PipelineTeardown {
            self.show_stop_failure();
        } else {
            self.show_playback_failure();
        }
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        self.abort_pending_response();
    }
}

/// Build the player pane and inert session without creating a media pipeline.
pub(crate) fn build(runtime: Result<PlaybackRuntime, PlaybackInitializationError>) -> PlayerView {
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();

    let (title, description, session) = match runtime {
        Ok(runtime) => {
            let (title, description) = empty_state_copy(runtime.capabilities());
            (title, description, Some(PlaybackSession::new(runtime)))
        }
        Err(error) => (
            "Playback initialization unavailable",
            format!("{error}. Device discovery and lineup inspection remain available."),
            None,
        ),
    };
    let empty_state = adw::StatusPage::builder()
        .icon_name("video-display-symbolic")
        .title(title)
        .description(description.as_str())
        .vexpand(true)
        .build();

    let player = gtk::Overlay::new();
    player.set_child(Some(&picture));
    player.add_overlay(&empty_state);

    let audio_enabled = session
        .as_ref()
        .is_some_and(PlaybackSession::is_foundation_ready);
    let volume_adjustment = gtk::Adjustment::new(100.0, 0.0, 100.0, 5.0, 10.0, 0.0);
    let volume_scale = gtk::Scale::builder()
        .orientation(gtk::Orientation::Horizontal)
        .draw_value(false)
        .width_request(120)
        .valign(gtk::Align::Center)
        .focusable(true)
        .sensitive(audio_enabled)
        .tooltip_text(PLAYER_ACCESSIBILITY.volume_label)
        .adjustment(&volume_adjustment)
        .build();
    volume_scale.update_property(&[
        gtk::accessible::Property::Label(PLAYER_ACCESSIBILITY.volume_label),
        gtk::accessible::Property::Orientation(gtk::Orientation::Horizontal),
    ]);

    let mute_button = gtk::ToggleButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text(PLAYER_ACCESSIBILITY.mute_label)
        .focusable(true)
        .sensitive(audio_enabled)
        .build();
    mute_button.update_property(&[gtk::accessible::Property::Label(
        PLAYER_ACCESSIBILITY.mute_label,
    )]);

    let stop_button = gtk::Button::builder()
        .icon_name("media-playback-stop-symbolic")
        .tooltip_text("Stop live TV")
        .focusable(true)
        .sensitive(false)
        .build();
    stop_button.update_property(&[gtk::accessible::Property::Label(
        PLAYER_ACCESSIBILITY.stop_label,
    )]);
    let fullscreen_button = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text(PLAYER_ACCESSIBILITY.enter_fullscreen_label)
        .focusable(true)
        .build();
    fullscreen_button.update_property(&[
        gtk::accessible::Property::Label(PLAYER_ACCESSIBILITY.enter_fullscreen_label),
        gtk::accessible::Property::KeyShortcuts(PLAYER_ACCESSIBILITY.enter_fullscreen_shortcuts),
    ]);
    let controls = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk::Align::Center)
        .build();
    controls.append(&mute_button);
    controls.append(&volume_scale);
    controls.append(&stop_button);
    controls.append(&fullscreen_button);
    let header = adw::HeaderBar::new();
    header.pack_end(&controls);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&player));
    let view = PlayerView {
        root: toolbar,
        header,
        picture,
        status: empty_state,
        volume_adjustment,
        volume_scale,
        mute_button,
        stop_button,
        fullscreen_button,
        idle_title: title.to_owned(),
        idle_description: description,
        session,
        updating_audio_controls: Cell::new(false),
        pending_response: RefCell::new(None),
    };
    // Exercise the same narrow binding path used after a future tune. The
    // newly constructed session is inert, so this keeps the status visible.
    let _ = view.sync_paintable();
    view
}

fn update_mute_presentation(button: &gtk::ToggleButton, volume: f64, muted: bool) {
    let (icon, label) = if muted {
        (
            "audio-volume-muted-symbolic",
            PLAYER_ACCESSIBILITY.unmute_label,
        )
    } else {
        let icon = if volume > 0.66 {
            "audio-volume-high-symbolic"
        } else if volume > 0.33 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-low-symbolic"
        };
        (icon, PLAYER_ACCESSIBILITY.mute_label)
    };
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
}

fn empty_state_copy(capabilities: &PlaybackCapabilities) -> (&'static str, String) {
    if capabilities.is_foundation_ready() {
        return (
            "Select a channel",
            format!(
                "The GStreamer {} playback foundation is available; activate a channel to start live TV.",
                capabilities.runtime_version()
            ),
        );
    }

    let missing = capabilities
        .missing_required()
        .map(|factory| factory.name())
        .collect::<Vec<_>>()
        .join(", ");
    (
        "Playback components unavailable",
        format!("Required GStreamer factories are missing: {missing}."),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    struct DropProbe(Rc<Cell<bool>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display-dependent widget contract.
    #[test]
    #[ignore = "requires the isolated display supplied by scripts/test-desktop-lifecycle.sh"]
    fn opaque_paintable_binding_tracks_status_and_shutdown() {
        adw::init().expect("initialize libadwaita for player-view presentation smoke");
        let main_context = gtk::glib::MainContext::default();
        let _owner = main_context
            .acquire()
            .expect("acquire default main context for player-view smoke");
        let view = Rc::new(build(Err(
            PlaybackInitializationError::InitializationFailed,
        )));
        view.connect_stop_control();
        view.connect_audio_controls();

        assert_eq!(view.picture.content_fit(), gtk::ContentFit::Contain);
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), "Playback initialization unavailable");
        assert_eq!(
            view.stop_button.icon_name().as_deref(),
            Some("media-playback-stop-symbolic")
        );
        assert_eq!(
            view.stop_button.tooltip_text().as_deref(),
            Some("Stop live TV")
        );
        assert_eq!(
            view.stop_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert!(view.stop_button.is_focusable());
        assert!(!view.stop_button.is_sensitive());
        assert_eq!(
            view.volume_scale.accessible_role(),
            gtk::AccessibleRole::Slider
        );
        assert!(view.volume_scale.is_focusable());
        assert!(!view.volume_scale.is_sensitive());
        assert_eq!(
            view.volume_scale.orientation(),
            gtk::Orientation::Horizontal
        );
        assert_eq!(view.volume_adjustment.lower(), 0.0);
        assert_eq!(view.volume_adjustment.upper(), 100.0);
        assert_eq!(view.volume_adjustment.step_increment(), 5.0);
        assert_eq!(view.volume_adjustment.page_increment(), 10.0);
        assert_eq!(
            view.volume_scale.tooltip_text().as_deref(),
            Some(PLAYER_ACCESSIBILITY.volume_label)
        );
        assert_eq!(
            view.mute_button.accessible_role(),
            gtk::AccessibleRole::ToggleButton
        );
        assert!(view.mute_button.is_focusable());
        assert!(!view.mute_button.is_sensitive());
        assert!(!view.mute_button.is_active());
        assert_eq!(
            view.mute_button.tooltip_text().as_deref(),
            Some(PLAYER_ACCESSIBILITY.mute_label)
        );
        assert_eq!(
            view.fullscreen_button.accessible_role(),
            gtk::AccessibleRole::Button
        );
        assert!(view.fullscreen_button.is_focusable());
        assert!(view.fullscreen_button.is_sensitive());
        assert_eq!(
            view.fullscreen_button.icon_name().as_deref(),
            Some("view-fullscreen-symbolic")
        );
        assert_eq!(
            view.fullscreen_button.tooltip_text().as_deref(),
            Some(PLAYER_ACCESSIBILITY.enter_fullscreen_label)
        );
        assert!(!view.root.is_extend_content_to_top_edge());

        view.apply_fullscreen_presentation(true);
        assert_eq!(
            view.fullscreen_button.icon_name().as_deref(),
            Some("view-restore-symbolic")
        );
        assert_eq!(
            view.fullscreen_button.tooltip_text().as_deref(),
            Some(PLAYER_ACCESSIBILITY.exit_fullscreen_label)
        );
        assert!(view.root.is_extend_content_to_top_edge());
        assert!(!view.header.shows_back_button());
        view.apply_fullscreen_presentation(false);
        assert!(!view.root.is_extend_content_to_top_edge());
        assert!(view.header.shows_back_button());

        let bytes = gtk::glib::Bytes::from_static(&[0x18, 0x30, 0x48, 0xff]);
        let paintable =
            gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 4)
                .upcast::<gtk::gdk::Paintable>();
        assert!(view.apply_paintable(Some(&paintable)));
        assert_eq!(view.picture.paintable().as_ref(), Some(&paintable));
        assert!(!view.status.is_visible());

        let task_dropped = Rc::new(Cell::new(false));
        let drop_probe = DropProbe(Rc::clone(&task_dropped));
        let task = main_context.spawn_local(async move {
            let _drop_probe = drop_probe;
            std::future::pending::<()>().await;
        });
        view.pending_response.replace(Some(task));
        view.stop_button.set_sensitive(true);
        view.stop_button.emit_clicked();
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), "Playback initialization unavailable");
        assert!(!view.stop_button.is_sensitive());
        assert!(task_dropped.get());

        assert!(view.apply_paintable(Some(&paintable)));
        view.stop_button.set_sensitive(true);
        view.shut_down().unwrap();
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert!(!view.stop_button.is_sensitive());

        let retained_stop_button = view.stop_button.clone();
        let retained_mute_button = view.mute_button.clone();
        let retained_fullscreen_button = view.fullscreen_button.clone();
        let retained_adjustment = view.volume_adjustment.clone();
        let weak_view = Rc::downgrade(&view);
        drop(view);
        assert!(weak_view.upgrade().is_none());
        retained_stop_button.emit_clicked();
        retained_mute_button.set_active(true);
        retained_adjustment.set_value(25.0);
        retained_fullscreen_button.emit_clicked();
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display- and runtime-dependent control contract.
    #[test]
    #[ignore = "requires the isolated display and playback runtime supplied by scripts/test-desktop-lifecycle.sh"]
    fn accessible_audio_controls_update_the_session() {
        adw::init().expect("initialize libadwaita for player audio-control smoke");
        let main_context = gtk::glib::MainContext::default();
        let _owner = main_context
            .acquire()
            .expect("acquire default main context for player audio-control smoke");
        let runtime =
            PlaybackRuntime::initialize().expect("initialize production playback runtime");
        assert!(runtime.capabilities().is_foundation_ready());
        let view = Rc::new(build(Ok(runtime)));
        view.connect_audio_controls();

        assert!(view.volume_scale.is_sensitive());
        assert!(view.mute_button.is_sensitive());
        assert_eq!(
            view.session
                .as_ref()
                .unwrap()
                .audio_state()
                .unwrap()
                .volume(),
            1.0
        );

        view.volume_adjustment.set_value(45.0);
        let audio = view.session.as_ref().unwrap().audio_state().unwrap();
        assert_eq!(audio.volume(), 0.45);
        assert!(!audio.is_muted());
        assert_eq!(
            view.mute_button.icon_name().as_deref(),
            Some("audio-volume-medium-symbolic")
        );

        view.mute_button.emit_clicked();
        let audio = view.session.as_ref().unwrap().audio_state().unwrap();
        assert!(audio.is_muted());
        assert_eq!(
            view.mute_button.tooltip_text().as_deref(),
            Some(PLAYER_ACCESSIBILITY.unmute_label)
        );
        assert_eq!(
            view.mute_button.icon_name().as_deref(),
            Some("audio-volume-muted-symbolic")
        );

        view.volume_adjustment.set_value(65.0);
        let audio = view.session.as_ref().unwrap().audio_state().unwrap();
        assert_eq!(audio.volume(), 0.65);
        assert!(audio.is_muted());
        view.mute_button.emit_clicked();
        assert!(
            !view
                .session
                .as_ref()
                .unwrap()
                .audio_state()
                .unwrap()
                .is_muted()
        );

        view.shut_down().unwrap();
        assert!(!view.volume_scale.is_sensitive());
        assert!(!view.mute_button.is_sensitive());
    }

    #[test]
    fn accessibility_copy_plan_is_stable_and_unambiguous() {
        assert_eq!(
            PLAYER_ACCESSIBILITY,
            PlayerAccessibilityPlan {
                volume_label: "Live TV volume",
                mute_label: "Mute live TV",
                unmute_label: "Unmute live TV",
                stop_label: "Stop live TV",
                enter_fullscreen_label: "Enter fullscreen",
                exit_fullscreen_label: "Exit fullscreen",
                enter_fullscreen_shortcuts: "F11",
                exit_fullscreen_shortcuts: "F11 Escape",
            }
        );
        assert_ne!(
            PLAYER_ACCESSIBILITY.volume_label,
            PLAYER_ACCESSIBILITY.stop_label
        );
        assert_ne!(
            PLAYER_ACCESSIBILITY.mute_label,
            PLAYER_ACCESSIBILITY.unmute_label
        );
        assert_ne!(
            PLAYER_ACCESSIBILITY.enter_fullscreen_label,
            PLAYER_ACCESSIBILITY.exit_fullscreen_label
        );
    }
}
