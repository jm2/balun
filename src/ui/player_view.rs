//! Live-TV picture shell and generation-owned playback-session owner.

use adw::prelude::*;
use balun::playback::{
    PlaybackCapabilities, PlaybackInitializationError, PlaybackRuntime, PlaybackSession,
    PlaybackSessionFailure,
};

/// Main-context-owned player pane.
///
/// The native pipeline and GTK sink remain private to `PlaybackSession`; this
/// pane retains only that owner and the URI-opaque GDK paintable it publishes.
pub(crate) struct PlayerView {
    root: adw::ToolbarView,
    picture: gtk::Picture,
    status: adw::StatusPage,
    session: Option<PlaybackSession>,
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

    /// Clear presentation and terminally settle the playback owner.
    pub(crate) fn shut_down(&self) -> Result<(), PlaybackSessionFailure> {
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
        .description(description)
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
        session,
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
                "The GStreamer {} playback foundation is available; live TV tuning is not connected in this development build.",
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

        let bytes = gtk::glib::Bytes::from_static(&[0x18, 0x30, 0x48, 0xff]);
        let paintable =
            gtk::gdk::MemoryTexture::new(1, 1, gtk::gdk::MemoryFormat::R8g8b8a8, &bytes, 4)
                .upcast::<gtk::gdk::Paintable>();
        assert!(view.apply_paintable(Some(&paintable)));
        assert_eq!(view.picture.paintable().as_ref(), Some(&paintable));
        assert!(!view.status.is_visible());

        view.shut_down().unwrap();
        assert!(view.picture.paintable().is_none());
        assert!(view.status.is_visible());
    }
}
