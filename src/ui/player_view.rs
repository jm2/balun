//! Live-TV picture shell and generation-owned playback-session owner.

use std::cell::RefCell;
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
    picture: gtk::Picture,
    status: adw::StatusPage,
    idle_title: String,
    idle_description: String,
    session: Option<PlaybackSession>,
    pending_response: RefCell<Option<gtk::glib::JoinHandle<()>>>,
}

impl PlayerView {
    /// Return the widget rooted in the live-TV navigation page.
    pub(crate) const fn root(&self) -> &adw::ToolbarView {
        &self.root
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
        let Ok(request) = request else {
            self.show_playback_failure();
            return;
        };
        self.show_connecting();

        let receiver = match controller.try_request_stream(request.selection().clone()) {
            Ok(receiver) => receiver,
            Err(_) => {
                if let Some(session) = self.session.as_ref() {
                    let _ = session.cancel_tune(request);
                }
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
        self.abort_pending_response();
        self.apply_paintable(None);
        let result = match self.session.as_ref() {
            Some(session) => session.stop(),
            None => Ok(()),
        };
        if result.is_ok() {
            self.show_idle();
        } else {
            self.show_playback_failure();
        }
        result
    }

    /// Clear presentation and terminally settle the playback owner.
    pub(crate) fn shut_down(&self) -> Result<(), PlaybackSessionFailure> {
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
            self.show_playback_failure();
            return;
        };
        match session.complete_tune(request, response) {
            Ok(TuneCompletion::Applied) => match self.sync_paintable() {
                Ok(true) => {}
                Ok(false) | Err(_) => {
                    self.apply_paintable(None);
                    let _ = session.stop();
                    self.show_playback_failure();
                }
            },
            Ok(TuneCompletion::Stale) => {
                // Reflect the current generation, which may already own a
                // successor paintable or may deliberately be idle.
                let _ = self.sync_paintable();
            }
            Err(_) => {
                self.apply_paintable(None);
                self.show_playback_failure();
            }
        }
    }

    fn abort_pending_response(&self) {
        if let Some(task) = self.pending_response.borrow_mut().take() {
            task.abort();
        }
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

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&player));
    let view = PlayerView {
        root: toolbar,
        picture,
        status: empty_state,
        idle_title: title.to_owned(),
        idle_description: description,
        session,
        pending_response: RefCell::new(None),
    };
    // Exercise the same narrow binding path used after a future tune. The
    // newly constructed session is inert, so this keeps the status visible.
    let _ = view.sync_paintable();
    view
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
    use super::*;

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
        let view = build(Err(PlaybackInitializationError::InitializationFailed));

        assert_eq!(view.picture.content_fit(), gtk::ContentFit::Contain);
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), "Playback initialization unavailable");

        let bytes = gtk::glib::Bytes::from_static(&[0x18, 0x30, 0x48, 0xff]);
        let paintable =
            gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 4)
                .upcast::<gtk::gdk::Paintable>();
        assert!(view.apply_paintable(Some(&paintable)));
        assert_eq!(view.picture.paintable().as_ref(), Some(&paintable));
        assert!(!view.status.is_visible());

        view.stop().unwrap();
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
        assert_eq!(view.status.title(), "Playback initialization unavailable");

        assert!(view.apply_paintable(Some(&paintable)));
        view.shut_down().unwrap();
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
    }
}
