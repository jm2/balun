//! Top-level adaptive three-pane window.

use adw::prelude::*;

use super::{channel_sidebar, device_sidebar, player_view};

const DEFAULT_WIDTH: i32 = 1_200;
const DEFAULT_HEIGHT: i32 = 720;
const DEVICE_SIDEBAR_MIN_WIDTH: f64 = 160.0;
const DEVICE_SIDEBAR_MAX_WIDTH: f64 = 220.0;
const CHANNEL_SIDEBAR_MIN_WIDTH: f64 = 240.0;
const CHANNEL_SIDEBAR_MAX_WIDTH: f64 = 360.0;
const COLLAPSE_DEVICE_SIDEBAR_AT: f64 = 1_000.0;
const COLLAPSE_CHANNEL_SIDEBAR_AT: f64 = 700.0;

/// Build Balun's single application window.
pub(crate) fn build(application: &adw::Application) -> adw::ApplicationWindow {
    let device_sidebar = device_sidebar::build();
    let channel_sidebar = channel_sidebar::build();
    let player_view = player_view::build();

    let device_page = adw::NavigationPage::new(&device_sidebar, "Devices");
    let channel_page = adw::NavigationPage::new(&channel_sidebar, "Channels");
    let player_page = adw::NavigationPage::new(&player_view, "Live TV");

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

    let medium_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_DEVICE_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    medium_breakpoint.add_setters(&[(&device_and_content, "collapsed", true)]);
    window.add_breakpoint(medium_breakpoint);

    let compact_breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        COLLAPSE_CHANNEL_SIDEBAR_AT,
        adw::LengthUnit::Sp,
    ));
    compact_breakpoint.add_setters(&[(&channel_and_player, "collapsed", true)]);
    window.add_breakpoint(compact_breakpoint);

    // Both split views intentionally keep `show-content = false` while this
    // shell has no selectable rows. Device and channel activation will advance
    // the corresponding split explicitly when their models are connected.

    window
}
