//! Device sidebar shell.

/// Build the device pane without starting discovery or other network work.
pub(crate) fn build() -> adw::ToolbarView {
    let empty_state = adw::StatusPage::builder()
        .icon_name("network-wired-symbolic")
        .title("No HDHomeRun devices")
        .description("Device discovery is not connected in this development shell.")
        .vexpand(true)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&empty_state));
    toolbar
}
