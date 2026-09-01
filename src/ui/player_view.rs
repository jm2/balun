//! Empty live-TV player shell.

/// Build the player pane without creating a media pipeline.
pub(crate) fn build() -> adw::ToolbarView {
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .hexpand(true)
        .vexpand(true)
        .build();

    let empty_state = adw::StatusPage::builder()
        .icon_name("video-display-symbolic")
        .title("Select a channel")
        .description("Live TV playback is not connected in this development shell.")
        .vexpand(true)
        .build();

    let player = gtk::Overlay::new();
    player.set_child(Some(&picture));
    player.add_overlay(&empty_state);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&player));
    toolbar
}
