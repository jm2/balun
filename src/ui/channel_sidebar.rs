//! Channel sidebar shell.

/// Build the channel pane without inventing lineup or guide data.
pub(crate) fn build() -> adw::ToolbarView {
    let empty_state = adw::StatusPage::builder()
        .icon_name("view-list-symbolic")
        .title("Select a device")
        .description("Channels for the selected HDHomeRun device will appear here.")
        .vexpand(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&empty_state));
    toolbar
}
