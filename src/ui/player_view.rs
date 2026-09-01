//! Empty live-TV player shell and playback-runtime owner.

use balun::playback::{PlaybackCapabilities, PlaybackInitializationError, PlaybackRuntime};

/// Main-context-owned player pane.
///
/// Retaining the runtime here gives the next slice one explicit owner for the
/// generation-scoped pipeline without creating a pipeline prematurely.
pub(crate) struct PlayerView {
    root: adw::ToolbarView,
    _runtime: Option<PlaybackRuntime>,
}

impl PlayerView {
    /// Return the widget rooted in the live-TV navigation page.
    pub(crate) const fn root(&self) -> &adw::ToolbarView {
        &self.root
    }
}

/// Build the player pane without creating a media pipeline.
pub(crate) fn build(runtime: Result<PlaybackRuntime, PlaybackInitializationError>) -> PlayerView {
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();

    let (title, description) = match runtime.as_ref() {
        Ok(runtime) => empty_state_copy(runtime.capabilities()),
        Err(error) => (
            "Playback initialization unavailable",
            format!("{error}. Device discovery and lineup inspection remain available."),
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
    PlayerView {
        root: toolbar,
        _runtime: runtime.ok(),
    }
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
