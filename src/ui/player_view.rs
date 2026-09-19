//! Live-TV picture shell and generation-owned playback-session owner.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::idle_inhibitor::PlaybackInhibitor;
use adw::prelude::*;
use balun::controller::{
    ApplicationSnapshot, ControllerCommandError, ControllerHandle, StreamHandoff,
    StreamHandoffError, StreamSelection,
};
use balun::domain::ChannelKey;
use balun::localization::controls::PlayerLabels;
use balun::localization::playback_failure::{
    self, ContextLabels, FailureText as PlaybackFailureCopy,
};
use balun::localization::playback_startup;
use balun::localization::playback_status::{self, ProgressLabels};
use balun::playback::{
    MissingMedia, PlaybackInitializationError, PlaybackPipelineFailure, PlaybackRuntime,
    PlaybackSession, PlaybackSessionFailure, PlaybackSessionState, TuneCompletion, TuneRequest,
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
    playback_status: gtk::Label,
    idle_title: String,
    idle_description: String,
    session: Option<PlaybackSession>,
    tune_context: RefCell<Option<TuneContext>>,
    updating_audio_controls: Cell<bool>,
    pending_response: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    idle_inhibitor: RefCell<Option<PlaybackInhibitor>>,
    /// Test-only observation of `stop` invocations for release-wiring smokes.
    #[cfg(test)]
    stop_calls: Cell<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaybackPresentation {
    Stopped,
    Connecting,
    Playing,
    Buffering(u8),
    Failed(PlaybackSessionFailure),
    ShutDown,
    Unknown,
}

impl From<&PlaybackSessionState> for PlaybackPresentation {
    fn from(state: &PlaybackSessionState) -> Self {
        match state {
            PlaybackSessionState::Stopped => Self::Stopped,
            PlaybackSessionState::Resolving { .. } | PlaybackSessionState::Connecting { .. } => {
                Self::Connecting
            }
            PlaybackSessionState::Playing { .. } => Self::Playing,
            PlaybackSessionState::Buffering { percent, .. } => Self::Buffering((*percent).min(100)),
            PlaybackSessionState::Failed { failure, .. } => Self::Failed(*failure),
            PlaybackSessionState::ShutDown => Self::ShutDown,
            _ => Self::Unknown,
        }
    }
}

impl PlaybackPresentation {
    const fn keeps_awake(self) -> bool {
        matches!(self, Self::Playing | Self::Buffering(_))
    }
}

// GTK accessibility shortcut syntax is language-independent.
const ENTER_FULLSCREEN_SHORTCUTS: &str = "F11";
const EXIT_FULLSCREEN_SHORTCUTS: &str = "F11 Escape";

/// Names the device and channel of the tune in progress so progress and
/// failure copy can say which tuner and channel they refer to (ADR-0002).
/// It carries display text only: never a stream URL or credential.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TuneContext {
    device: String,
    channel: String,
}

impl TuneContext {
    /// Describe `key` from the accepted snapshot: the device's friendly name
    /// (or model, or "HDHomeRun") with its address, and the channel number
    /// with its name. Parts missing from the snapshot fall back to the
    /// DeviceID or the bare channel number.
    pub(crate) fn from_snapshot(snapshot: &ApplicationSnapshot, key: &ChannelKey) -> Self {
        let device = snapshot
            .devices()
            .iter()
            .find(|summary| summary.device_id() == key.device_id())
            .map_or_else(
                || format!("HDHomeRun {}", key.device_id()),
                |summary| {
                    let name = summary
                        .friendly_name()
                        .or(summary.model_number())
                        .unwrap_or("HDHomeRun");
                    format!("{name} ({})", summary.preferred_locator().ip())
                },
            );
        let channel = snapshot
            .selected_lineup()
            .channels()
            .iter()
            .find(|summary| summary.key() == key)
            .map_or_else(
                || playback_failure::channel_number(key.guide_number().as_str()).into_owned(),
                |summary| format!("{} {}", key.guide_number(), summary.name()),
            );
        Self { device, channel }
    }

    fn device(&self) -> &str {
        &self.device
    }

    fn channel(&self) -> &str {
        &self.channel
    }
}

impl Default for TuneContext {
    fn default() -> Self {
        let labels = ContextLabels::current();
        Self {
            device: labels.selected_device.into_owned(),
            channel: labels.selected_channel.into_owned(),
        }
    }
}

fn pipeline_failure_copy(
    failure: PlaybackPipelineFailure,
    context: &TuneContext,
) -> PlaybackFailureCopy {
    playback_failure::pipeline(failure, context.channel(), context.device())
}

fn handoff_failure_copy(failure: StreamHandoffError, context: &TuneContext) -> PlaybackFailureCopy {
    let category = match failure {
        StreamHandoffError::Protected => PlaybackPipelineFailure::Protected,
        StreamHandoffError::SelectionChanged
        | StreamHandoffError::DeviceMismatch
        | StreamHandoffError::ChannelUnavailable => PlaybackPipelineFailure::ChannelMissing,
        StreamHandoffError::SelectionNotReady | StreamHandoffError::ControllerStopped => {
            PlaybackPipelineFailure::Offline
        }
        StreamHandoffError::OriginRejected => PlaybackPipelineFailure::ChannelMissing,
        StreamHandoffError::Internal => PlaybackPipelineFailure::Internal,
        _ => PlaybackPipelineFailure::Internal,
    };
    pipeline_failure_copy(category, context)
}

fn session_failure_copy(
    failure: PlaybackSessionFailure,
    context: &TuneContext,
) -> Option<PlaybackFailureCopy> {
    match failure {
        PlaybackSessionFailure::PipelineTeardown => None,
        PlaybackSessionFailure::Pipeline(category) => {
            Some(pipeline_failure_copy(category, context))
        }
        PlaybackSessionFailure::Handoff(failure) => Some(handoff_failure_copy(failure, context)),
        PlaybackSessionFailure::HandoffMismatch => Some(pipeline_failure_copy(
            PlaybackPipelineFailure::ChannelMissing,
            context,
        )),
        PlaybackSessionFailure::ComponentsUnavailable => Some(pipeline_failure_copy(
            PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Unknown),
            context,
        )),
        _ => Some(pipeline_failure_copy(
            PlaybackPipelineFailure::Internal,
            context,
        )),
    }
}

impl PlayerView {
    pub(crate) fn connect_idle_inhibition(&self, window: &impl IsA<gtk::Window>) {
        self.idle_inhibitor
            .replace(Some(PlaybackInhibitor::new(window)));
    }

    fn inhibit_idle(&self, active: bool) {
        if let Some(inhibitor) = self.idle_inhibitor.borrow_mut().as_mut() {
            inhibitor.set_active(active);
        }
    }

    /// Return the widget rooted in the live-TV navigation page.
    pub(crate) const fn root(&self) -> &adw::ToolbarView {
        &self.root
    }

    /// Return the native pointer/Enter/Space fullscreen request control.
    pub(crate) const fn fullscreen_button(&self) -> &gtk::Button {
        &self.fullscreen_button
    }

    /// Return the header-presented playback status copy for smoke
    /// assertions; production code reaches this label only through the
    /// session-state presentation it applies itself.
    #[cfg(test)]
    pub(crate) const fn playback_status(&self) -> &gtk::Label {
        &self.playback_status
    }

    /// Return how often `stop` has been invoked. The bin-side window smoke
    /// cannot host the lib-only fake stream device, so it observes this
    /// counter to prove the stop wiring itself fired; production code has
    /// no such observation.
    #[cfg(test)]
    pub(crate) fn stop_call_count(&self) -> u32 {
        self.stop_calls.get()
    }

    /// Reconcile presentation only after the application window reports its
    /// compositor-confirmed fullscreen state.
    pub(crate) fn apply_fullscreen_presentation(&self, fullscreen: bool) {
        let accessibility = PlayerLabels::current();
        let (icon, label, shortcuts) = if fullscreen {
            (
                "view-restore-symbolic",
                accessibility.exit_fullscreen_label.as_ref(),
                EXIT_FULLSCREEN_SHORTCUTS,
            )
        } else {
            (
                "view-fullscreen-symbolic",
                accessibility.enter_fullscreen_label.as_ref(),
                ENTER_FULLSCREEN_SHORTCUTS,
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

    /// Project every URL-free session transition onto the GTK main context.
    ///
    /// The receiver retains only immutable state. Its task captures this view
    /// weakly, so neither the session nor GTK signal graph can retain the
    /// playback owner after window shutdown.
    pub(crate) fn connect_session_state(self: &Rc<Self>) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let Ok(mut states) = session.subscribe_state() else {
            self.show_playback_failure();
            return;
        };
        let initial = states.borrow_and_update().clone();
        self.apply_session_state(&initial);

        let player_view = Rc::downgrade(self);
        let task = gtk::glib::MainContext::default().spawn_local(async move {
            while states.changed().await.is_ok() {
                let state = states.borrow_and_update().clone();
                let Some(player_view) = player_view.upgrade() else {
                    return;
                };
                player_view.apply_session_state(&state);
            }
        });
        drop(task);
    }

    /// Start one URL-free channel intent and consume the actor-private response
    /// only through the generation-owned playback session.
    pub(crate) fn activate_channel(
        self: &Rc<Self>,
        controller: &ControllerHandle,
        selection: StreamSelection,
        context: TuneContext,
    ) {
        self.inhibit_idle(false);
        self.abort_pending_response();
        self.tune_context.replace(Some(context));

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
            Err(error) => {
                if let Some(session) = self.session.as_ref() {
                    let handoff_failure = match error {
                        ControllerCommandError::Full => StreamHandoffError::Internal,
                        ControllerCommandError::ShuttingDown => {
                            StreamHandoffError::ControllerStopped
                        }
                    };
                    let _ = session.complete_tune(request, Err(handoff_failure));
                    self.stop_button.set_sensitive(false);
                    self.show_session_failure(PlaybackSessionFailure::Handoff(handoff_failure));
                    return;
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
        self.inhibit_idle(false);
        #[cfg(test)]
        self.stop_calls.set(self.stop_calls.get() + 1);
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
        self.inhibit_idle(false);
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
        self.picture.set_visible(has_video);
        self.status.set_visible(!has_video);
        has_video
    }

    fn apply_session_state(&self, state: &PlaybackSessionState) {
        let progress = ProgressLabels::current();
        self.inhibit_idle(PlaybackPresentation::from(state).keeps_awake());
        match PlaybackPresentation::from(state) {
            PlaybackPresentation::Stopped => {
                self.playback_status.set_label(&progress.stopped);
                self.stop_button.set_sensitive(false);
                self.apply_paintable(None);
                self.show_idle();
            }
            PlaybackPresentation::Connecting => {
                self.playback_status.set_label(&progress.connecting);
                self.stop_button.set_sensitive(true);
                self.show_connecting();
            }
            PlaybackPresentation::Playing => {
                self.playback_status.set_label(&progress.playing);
                self.stop_button.set_sensitive(true);
                self.status.set_visible(false);
            }
            PlaybackPresentation::Buffering(percent) => {
                let text = playback_status::buffering(percent);
                self.playback_status.set_label(&text.status);
                self.stop_button.set_sensitive(true);
                self.status.set_title(&progress.buffering);
                self.set_description_text(&text.description);
                self.status.set_visible(true);
            }
            PlaybackPresentation::Failed(failure) => {
                self.playback_status.set_label("Playback failed");
                self.stop_button.set_sensitive(false);
                self.apply_paintable(None);
                self.show_session_failure(failure);
            }
            PlaybackPresentation::ShutDown => {
                self.playback_status.set_label(&progress.stopped);
                self.stop_button.set_sensitive(false);
                self.set_audio_controls_sensitive(false);
                self.apply_paintable(None);
            }
            PlaybackPresentation::Unknown => {
                self.playback_status
                    .set_label(&progress.playback_unavailable);
                self.stop_button.set_sensitive(false);
                self.apply_paintable(None);
                self.show_playback_failure();
            }
        }
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
            Err(failure) => self.handle_audio_failure(failure),
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
            Err(failure) => self.handle_audio_failure(failure),
        }
    }

    fn handle_audio_failure(&self, failure: PlaybackSessionFailure) {
        self.restore_audio_controls();
        if matches!(
            failure,
            PlaybackSessionFailure::PipelineTeardown | PlaybackSessionFailure::ShutDown
        ) {
            self.set_audio_controls_sensitive(false);
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

    fn tune_context(&self) -> TuneContext {
        self.tune_context.borrow().clone().unwrap_or_default()
    }

    fn show_connecting(&self) {
        let context = self.tune_context();
        self.status.set_title(&ProgressLabels::current().connecting);
        self.set_description_text(&playback_status::opening(
            context.channel(),
            context.device(),
        ));
        self.status.set_visible(true);
    }

    fn show_idle(&self) {
        self.status.set_title(&self.idle_title);
        self.set_description_text(&self.idle_description);
        self.status.set_visible(true);
    }

    fn show_playback_failure(&self) {
        let copy = playback_failure::generic();
        self.status.set_title(&copy.title);
        self.set_description_text(&copy.description);
        self.status.set_visible(true);
    }

    fn show_failure_copy(&self, copy: &PlaybackFailureCopy) {
        self.playback_status.set_label(&copy.title);
        self.status.set_title(&copy.title);
        self.set_description_text(&copy.description);
        self.status.set_visible(true);
    }

    fn show_stop_failure(&self) {
        self.set_audio_controls_sensitive(false);
        let copy = playback_failure::stop();
        self.status.set_title(&copy.title);
        self.set_description_text(&copy.description);
        self.status.set_visible(true);
    }

    fn show_session_failure(&self, failure: PlaybackSessionFailure) {
        match session_failure_copy(failure, &self.tune_context()) {
            Some(copy) => self.show_failure_copy(&copy),
            None => self.show_stop_failure(),
        }
    }

    fn set_description_text(&self, text: &str) {
        self.status.set_description(Some(&description_markup(text)));
    }
}

impl Drop for PlayerView {
    fn drop(&mut self) {
        self.abort_pending_response();
    }
}

/// Build the player pane and inert session without creating a media pipeline.
pub(crate) fn build(runtime: Result<PlaybackRuntime, PlaybackInitializationError>) -> PlayerView {
    let accessibility = PlayerLabels::current();
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .accessible_role(gtk::AccessibleRole::Img)
        .visible(false)
        .hexpand(true)
        .vexpand(true)
        .build();
    picture.update_property(&[gtk::accessible::Property::Label(&accessibility.video_label)]);

    let (startup, session) = match runtime {
        Ok(runtime) => {
            let text = playback_startup::capabilities(runtime.capabilities());
            (text, Some(PlaybackSession::new(runtime)))
        }
        Err(error) => (playback_startup::initialization(error), None),
    };
    let empty_state = adw::StatusPage::builder()
        .icon_name("video-display-symbolic")
        .title(startup.title.as_ref())
        .description(description_markup(&startup.description))
        .accessible_role(gtk::AccessibleRole::Status)
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
        .tooltip_text(accessibility.volume_label.as_ref())
        .adjustment(&volume_adjustment)
        .build();
    volume_scale.update_property(&[
        gtk::accessible::Property::Label(accessibility.volume_label.as_ref()),
        gtk::accessible::Property::Orientation(gtk::Orientation::Horizontal),
    ]);

    let mute_button = gtk::ToggleButton::builder()
        .icon_name("audio-volume-high-symbolic")
        .tooltip_text(accessibility.mute_label.as_ref())
        .focusable(true)
        .sensitive(audio_enabled)
        .build();
    mute_button.update_property(&[gtk::accessible::Property::Label(
        accessibility.mute_label.as_ref(),
    )]);

    let stop_button = gtk::Button::builder()
        .icon_name("media-playback-stop-symbolic")
        .tooltip_text(accessibility.stop_label.as_ref())
        .focusable(true)
        .sensitive(false)
        .build();
    stop_button.update_property(&[gtk::accessible::Property::Label(
        accessibility.stop_label.as_ref(),
    )]);
    let fullscreen_button = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text(accessibility.enter_fullscreen_label.as_ref())
        .focusable(true)
        .build();
    fullscreen_button.update_property(&[
        gtk::accessible::Property::Label(accessibility.enter_fullscreen_label.as_ref()),
        gtk::accessible::Property::KeyShortcuts(ENTER_FULLSCREEN_SHORTCUTS),
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
    let progress = ProgressLabels::current();
    let playback_status = gtk::Label::builder()
        .label(if session.is_some() {
            progress.stopped.as_ref()
        } else {
            progress.unavailable.as_ref()
        })
        .accessible_role(gtk::AccessibleRole::Status)
        .tooltip_text(accessibility.status_label.as_ref())
        .build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&playback_status));
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
        playback_status,
        idle_title: startup.title.into_owned(),
        idle_description: startup.description.into_owned(),
        session,
        tune_context: RefCell::new(None),
        updating_audio_controls: Cell::new(false),
        pending_response: RefCell::new(None),
        idle_inhibitor: RefCell::new(None),
        #[cfg(test)]
        stop_calls: Cell::new(0),
    };
    // Exercise the same narrow binding path used after a future tune. The
    // newly constructed session is inert, so this keeps the status visible.
    let _ = view.sync_paintable();
    view
}

fn update_mute_presentation(button: &gtk::ToggleButton, volume: f64, muted: bool) {
    let accessibility = PlayerLabels::current();
    let (icon, tooltip) = if muted {
        (
            "audio-volume-muted-symbolic",
            accessibility.unmute_label.as_ref(),
        )
    } else {
        let icon = if volume > 0.66 {
            "audio-volume-high-symbolic"
        } else if volume > 0.33 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-low-symbolic"
        };
        (icon, accessibility.mute_label.as_ref())
    };
    button.set_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.update_property(&[gtk::accessible::Property::Label(
        accessibility.mute_label.as_ref(),
    )]);
}

fn description_markup(text: &str) -> gtk::glib::GString {
    // AdwStatusPage descriptions always parse markup. Compose ordinary text
    // first, including any device/channel names, then escape once at this
    // rendering boundary. Titles and GtkLabel::set_label remain plain text.
    gtk::glib::markup_escape_text(text)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use balun::domain::{ChannelKey, DeviceId, GuideNumber};
    use balun::playback::TuneGeneration;

    use super::*;

    fn assert_literal_markup(markup: &str, expected: &str) {
        let (attributes, rendered, _) =
            gtk::pango::parse_markup(markup, '\0').expect("description must be valid markup");
        assert_eq!(rendered, expected);
        assert!(attributes.iterator().attrs().is_empty());
    }

    #[test]
    fn status_descriptions_preserve_literal_names_without_formatting() {
        for text in [
            "News & Weather",
            "<span foreground='red' size='50000'>Tuner</span>",
            "<a href='about:blank'>Channel</a>",
            "&lt;b&gt;already an entity, not a tag&lt;/b&gt;",
            "\"Channel\" < > ' & 日本語",
        ] {
            assert_literal_markup(&description_markup(text), text);
        }
    }

    #[test]
    fn every_failure_description_keeps_device_metadata_literal() {
        let context = TuneContext {
            device: "Room & <span weight='bold'>Tuner</span>".into(),
            channel: "7.1 <i>News & Weather</i>".into(),
        };
        for failure in [
            PlaybackPipelineFailure::TunerBusy,
            PlaybackPipelineFailure::ChannelMissing,
            PlaybackPipelineFailure::HttpRejected,
            PlaybackPipelineFailure::Offline,
            PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Unknown),
            PlaybackPipelineFailure::Protected,
            PlaybackPipelineFailure::Internal,
        ] {
            let copy = pipeline_failure_copy(failure, &context);
            assert_literal_markup(&description_markup(&copy.description), &copy.description);
        }
    }

    #[test]
    fn only_playing_and_buffering_hold_idle_inhibition() {
        assert!(PlaybackPresentation::Playing.keeps_awake());
        assert!(PlaybackPresentation::Buffering(0).keeps_awake());
        for presentation in [
            PlaybackPresentation::Stopped,
            PlaybackPresentation::Connecting,
            PlaybackPresentation::Failed(PlaybackSessionFailure::ComponentsUnavailable),
            PlaybackPresentation::ShutDown,
            PlaybackPresentation::Unknown,
        ] {
            assert!(!presentation.keeps_awake());
        }
    }

    struct DropProbe(Rc<Cell<bool>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    fn fixture_context() -> TuneContext {
        TuneContext {
            device: "Living room (192.0.2.44)".to_owned(),
            channel: "7.1 Synthetic News".to_owned(),
        }
    }

    /// Copy may name the device and channel (ADR-0002) but never a stream
    /// URL, port, path, or credential.
    fn assert_failure_copy_names_device_without_secrets(copy: &PlaybackFailureCopy) {
        for secret in [
            "http://",
            "https://",
            "5004",
            "/auto/v7.1",
            "device-auth-secret",
            "DeviceAuth",
        ] {
            assert!(!copy.title.contains(secret), "{secret} in {:?}", copy.title);
            assert!(
                !copy.description.contains(secret),
                "{secret} in {:?}",
                copy.description
            );
        }
        assert!(
            copy.description.contains("Living room (192.0.2.44)"),
            "description must name the device: {:?}",
            copy.description
        );
        assert!(
            copy.description
                .ends_with("Device discovery and lineup inspection remain available."),
            "description must keep the recovery hint: {:?}",
            copy.description
        );
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display-dependent widget contract.
    #[test]
    #[ignore = "requires the isolated display supplied by scripts/test-desktop-lifecycle.sh"]
    fn opaque_paintable_binding_tracks_status_and_shutdown() {
        let locale = balun::localization::initialize().expect("initialize startup locale");
        if let Ok(expected) = std::env::var("BALUN_TEST_EXPECTED_LOCALE") {
            assert_eq!(locale, expected);
        }
        let accessibility = PlayerLabels::current();
        let startup =
            playback_startup::initialization(PlaybackInitializationError::InitializationFailed);
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
        assert_eq!(view.picture.accessible_role(), gtk::AccessibleRole::Img);
        assert!(!view.picture.is_visible());
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), startup.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &startup.description,
        );
        assert_eq!(
            view.status.upcast_ref::<gtk::Widget>().accessible_role(),
            gtk::AccessibleRole::Status
        );
        assert_eq!(
            view.playback_status.label(),
            ProgressLabels::current().unavailable.as_ref()
        );
        assert_eq!(
            view.playback_status.accessible_role(),
            gtk::AccessibleRole::Status
        );
        assert_eq!(
            view.playback_status.tooltip_text().as_deref(),
            Some(accessibility.status_label.as_ref())
        );
        assert_eq!(
            view.stop_button.icon_name().as_deref(),
            Some("media-playback-stop-symbolic")
        );
        assert_eq!(
            view.stop_button.tooltip_text().as_deref(),
            Some(accessibility.stop_label.as_ref())
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
            Some(accessibility.volume_label.as_ref())
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
            Some(accessibility.mute_label.as_ref())
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
            Some(accessibility.enter_fullscreen_label.as_ref())
        );
        assert!(!view.root.is_extend_content_to_top_edge());

        view.apply_fullscreen_presentation(true);
        assert_eq!(
            view.fullscreen_button.icon_name().as_deref(),
            Some("view-restore-symbolic")
        );
        assert_eq!(
            view.fullscreen_button.tooltip_text().as_deref(),
            Some(accessibility.exit_fullscreen_label.as_ref())
        );
        assert!(view.root.is_extend_content_to_top_edge());
        assert!(!view.header.shows_back_button());
        view.apply_fullscreen_presentation(false);
        assert!(!view.root.is_extend_content_to_top_edge());
        assert!(view.header.shows_back_button());

        // Exercise the production setter with admitted metadata characters,
        // not only the pure escaping helper. The rendered name stays literal.
        let context = TuneContext {
            device: "Room & <span weight='bold'>Tuner</span>".into(),
            channel: "7.1 <i>News & Weather</i>".into(),
        };
        view.tune_context.replace(Some(context.clone()));
        view.show_connecting();
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &playback_status::opening(context.channel(), context.device()),
        );
        let failure = pipeline_failure_copy(PlaybackPipelineFailure::Offline, &context);
        view.show_failure_copy(&failure);
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &failure.description,
        );
        view.tune_context.take();
        view.show_idle();

        let bytes = gtk::glib::Bytes::from_static(&[0x18, 0x30, 0x48, 0xff]);
        let paintable =
            gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 4)
                .upcast::<gtk::gdk::Paintable>();
        assert!(view.apply_paintable(Some(&paintable)));
        assert_eq!(view.picture.paintable().as_ref(), Some(&paintable));
        assert!(!view.status.is_visible());

        let generation = TuneGeneration::default();
        let channel_key = ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        );
        view.apply_session_state(&PlaybackSessionState::Connecting {
            generation,
            channel_key: channel_key.clone(),
        });
        assert_eq!(
            view.playback_status.label(),
            ProgressLabels::current().connecting.as_ref()
        );
        assert!(view.stop_button.is_sensitive());
        assert!(view.status.is_visible());
        assert!(view.picture.paintable().is_some());

        view.apply_session_state(&PlaybackSessionState::Buffering {
            generation,
            channel_key: channel_key.clone(),
            percent: 42,
        });
        let buffering = playback_status::buffering(42);
        assert_eq!(view.playback_status.label(), buffering.status.as_ref());
        assert_eq!(
            view.status.title(),
            ProgressLabels::current().buffering.as_ref()
        );
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &buffering.description,
        );
        assert!(view.picture.paintable().is_some());

        view.apply_session_state(&PlaybackSessionState::Playing {
            generation,
            channel_key: channel_key.clone(),
        });
        assert_eq!(
            view.playback_status.label(),
            ProgressLabels::current().playing.as_ref()
        );
        assert!(!view.status.is_visible());
        assert!(view.picture.paintable().is_some());

        view.apply_session_state(&PlaybackSessionState::Failed {
            generation,
            channel_key: channel_key.clone(),
            failure: PlaybackSessionFailure::Pipeline(PlaybackPipelineFailure::Internal),
        });
        let internal =
            pipeline_failure_copy(PlaybackPipelineFailure::Internal, &TuneContext::default());
        assert_eq!(view.playback_status.label(), internal.title.as_ref());
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), internal.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &internal.description,
        );
        if locale == "de" {
            assert_eq!(view.status.title(), "Wiedergabe fehlgeschlagen");
            assert!(
                view.status
                    .description()
                    .as_deref()
                    .unwrap()
                    .contains("ausgewählte")
            );
        }
        assert!(!view.stop_button.is_sensitive());

        view.apply_session_state(&PlaybackSessionState::Failed {
            generation,
            channel_key,
            failure: PlaybackSessionFailure::PipelineTeardown,
        });
        let stop = playback_failure::stop();
        assert_eq!(view.status.title(), stop.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &stop.description,
        );
        view.show_playback_failure();
        let generic = playback_failure::generic();
        assert_eq!(view.status.title(), generic.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &generic.description,
        );

        assert!(view.apply_paintable(Some(&paintable)));
        view.apply_session_state(&PlaybackSessionState::Stopped);
        assert_eq!(
            view.playback_status.label(),
            ProgressLabels::current().stopped.as_ref()
        );
        assert!(view.picture.paintable().is_none());
        assert_eq!(view.status.title(), startup.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &startup.description,
        );

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
        assert!(!view.picture.is_visible());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), startup.title.as_ref());
        assert_literal_markup(
            view.status.description().as_deref().unwrap(),
            &startup.description,
        );
        assert!(!view.stop_button.is_sensitive());
        assert!(task_dropped.get());

        assert!(view.apply_paintable(Some(&paintable)));
        assert!(view.picture.is_visible());
        assert!(!view.status.is_visible());
        view.stop_button.set_sensitive(true);
        view.shut_down().unwrap();
        assert!(!view.picture.is_visible());
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
        let accessibility = PlayerLabels::current();
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
        view.connect_session_state();

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
        assert_eq!(
            view.playback_status.label(),
            ProgressLabels::current().stopped.as_ref()
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
            Some(accessibility.unmute_label.as_ref())
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

        view.handle_audio_failure(PlaybackSessionFailure::PipelineTeardown);
        assert!(!view.volume_scale.is_sensitive());
        assert!(!view.mute_button.is_sensitive());
        view.shut_down().unwrap();
        assert!(!view.volume_scale.is_sensitive());
        assert!(!view.mute_button.is_sensitive());
    }

    #[test]
    fn accessibility_copy_preserves_english_defaults_and_shortcut_syntax() {
        let labels = PlayerLabels::current();
        assert_eq!(labels.volume_label, "Live TV volume");
        assert_eq!(labels.mute_label, "Mute live TV");
        assert_eq!(labels.unmute_label, "Unmute live TV");
        assert_eq!(labels.stop_label, "Stop live TV");
        assert_eq!(labels.enter_fullscreen_label, "Enter fullscreen");
        assert_eq!(labels.exit_fullscreen_label, "Exit fullscreen");
        assert_eq!(ENTER_FULLSCREEN_SHORTCUTS, "F11");
        assert_eq!(EXIT_FULLSCREEN_SHORTCUTS, "F11 Escape");
    }

    #[test]
    fn pipeline_failure_copy_is_exhaustive_stable_and_endpoint_free() {
        let expected = [
            (PlaybackPipelineFailure::TunerBusy, "No tuner available"),
            (
                PlaybackPipelineFailure::ChannelMissing,
                "Channel unavailable",
            ),
            (
                PlaybackPipelineFailure::HttpRejected,
                "Device rejected the stream",
            ),
            (
                PlaybackPipelineFailure::Offline,
                "Device or stream unavailable",
            ),
            (
                PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Unknown),
                "Required playback component or codec unavailable",
            ),
            (
                PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Ac4Audio),
                "Required playback component or codec unavailable",
            ),
            (
                PlaybackPipelineFailure::Protected,
                "Protected channel unsupported",
            ),
            (PlaybackPipelineFailure::Internal, "Playback failed"),
        ];

        let context = fixture_context();
        for (failure, title) in expected {
            let copy = pipeline_failure_copy(failure, &context);
            assert_eq!(copy.title, title);
            assert_failure_copy_names_device_without_secrets(&copy);
            if failure != PlaybackPipelineFailure::TunerBusy {
                assert!(
                    copy.description.contains("7.1 Synthetic News"),
                    "{failure:?} must name the channel: {:?}",
                    copy.description
                );
            }
        }
        assert_eq!(
            pipeline_failure_copy(PlaybackPipelineFailure::TunerBusy, &context).description,
            "All tuners on Living room (192.0.2.44) are busy. Device discovery and lineup inspection remain available."
        );
        assert_eq!(
            pipeline_failure_copy(
                PlaybackPipelineFailure::MissingCodecOrPlugin(MissingMedia::Ac4Audio),
                &context
            )
            .description,
            "This Balun build has no AC-4 audio decoder for 7.1 Synthetic News on Living room (192.0.2.44). Device discovery and lineup inspection remain available."
        );
        assert_eq!(
            pipeline_failure_copy(PlaybackPipelineFailure::Offline, &TuneContext::default())
                .description,
            "the selected device did not deliver the selected channel. Check that the device is powered and reachable. Device discovery and lineup inspection remain available."
        );
    }

    #[test]
    fn non_pipeline_failure_copy_is_stable_endpoint_free_and_keeps_teardown_distinct() {
        let expected = [
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::Protected),
                "Protected channel unsupported",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::SelectionChanged),
                "Channel unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::DeviceMismatch),
                "Channel unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::ChannelUnavailable),
                "Channel unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::SelectionNotReady),
                "Device or stream unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::ControllerStopped),
                "Device or stream unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::OriginRejected),
                "Channel unavailable",
            ),
            (
                PlaybackSessionFailure::Handoff(StreamHandoffError::Internal),
                "Playback failed",
            ),
            (
                PlaybackSessionFailure::HandoffMismatch,
                "Channel unavailable",
            ),
            (
                PlaybackSessionFailure::ComponentsUnavailable,
                "Required playback component or codec unavailable",
            ),
            (
                PlaybackSessionFailure::GenerationExhausted,
                "Playback failed",
            ),
        ];

        let context = fixture_context();
        for (failure, title) in expected {
            let copy =
                session_failure_copy(failure, &context).expect("ordinary playback failure copy");
            assert_eq!(copy.title, title);
            assert_failure_copy_names_device_without_secrets(&copy);
        }
        assert_eq!(
            session_failure_copy(PlaybackSessionFailure::PipelineTeardown, &context),
            None
        );
    }

    #[test]
    fn tune_context_names_the_device_and_channel_from_the_snapshot() {
        use balun::controller::{
            ChannelSummary, DeviceSummary, DiscoveryState, OperationGeneration,
            SelectedLineupState, SnapshotRevision,
        };
        use balun::domain::{DeviceId, GuideNumber};

        let device_id = DeviceId::new(0x105A_1232).unwrap();
        let key = ChannelKey::new(device_id, GuideNumber::new("7.1").unwrap());
        let generation = OperationGeneration::new(3);
        let channels =
            [
                ChannelSummary::new(key.clone(), "Synthetic News".to_owned(), true, false, true)
                    .unwrap(),
            ];
        let device = DeviceSummary::new(
            device_id,
            Some("Living room".to_owned()),
            Some("HDHR5-4K".to_owned()),
            Some(4),
            "192.0.2.44:65001".parse().unwrap(),
            vec!["192.0.2.44:65001".parse().unwrap()],
        )
        .unwrap();
        let snapshot = ApplicationSnapshot::new(
            SnapshotRevision::new(1),
            OperationGeneration::new(1),
            generation,
            DiscoveryState::ready(OperationGeneration::new(1), 0),
            [device],
            Some(device_id),
            SelectedLineupState::ready(device_id, generation, channels).unwrap(),
        )
        .unwrap();

        let context = TuneContext::from_snapshot(&snapshot, &key);
        assert_eq!(context.device(), "Living room (192.0.2.44)");
        assert_eq!(context.channel(), "7.1 Synthetic News");

        let other = ChannelKey::new(device_id, GuideNumber::new("9.9").unwrap());
        let context = TuneContext::from_snapshot(&snapshot, &other);
        assert_eq!(context.channel(), "channel 9.9");

        let stranger = DeviceId::new(0x105A_1243).unwrap();
        let context = TuneContext::from_snapshot(
            &snapshot,
            &ChannelKey::new(stranger, GuideNumber::new("7.1").unwrap()),
        );
        assert_eq!(context.device(), "HDHomeRun 105A1243");
        assert!(!format!("{context:?}").contains("http"));
    }

    #[test]
    fn playback_presentation_groups_only_url_free_session_state() {
        let generation = TuneGeneration::default();
        let channel_key = ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("7.1").unwrap(),
        );

        assert_eq!(
            PlaybackPresentation::from(&PlaybackSessionState::Stopped),
            PlaybackPresentation::Stopped
        );
        for state in [
            PlaybackSessionState::Resolving {
                generation,
                channel_key: channel_key.clone(),
            },
            PlaybackSessionState::Connecting {
                generation,
                channel_key: channel_key.clone(),
            },
        ] {
            assert_eq!(
                PlaybackPresentation::from(&state),
                PlaybackPresentation::Connecting
            );
        }
        assert_eq!(
            PlaybackPresentation::from(&PlaybackSessionState::Playing {
                generation,
                channel_key: channel_key.clone(),
            }),
            PlaybackPresentation::Playing
        );
        assert_eq!(
            PlaybackPresentation::from(&PlaybackSessionState::Buffering {
                generation,
                channel_key: channel_key.clone(),
                percent: u8::MAX,
            }),
            PlaybackPresentation::Buffering(100)
        );
        assert_eq!(
            PlaybackPresentation::from(&PlaybackSessionState::Failed {
                generation,
                channel_key,
                failure: PlaybackSessionFailure::Pipeline(PlaybackPipelineFailure::Internal),
            }),
            PlaybackPresentation::Failed(PlaybackSessionFailure::Pipeline(
                PlaybackPipelineFailure::Internal
            ))
        );
        assert_eq!(
            PlaybackPresentation::from(&PlaybackSessionState::ShutDown),
            PlaybackPresentation::ShutDown
        );
    }
}
