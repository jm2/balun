//! GTK application lifecycle.

use adw::prelude::*;
use balun::controller::ControllerRuntime;
use balun::playback::{PlaybackRuntime, ToolkitPreparation, configure_before_toolkit};
use balun::settings::SettingsStore;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::ui;
use crate::ui::settings_session::SettingsSession;

/// Reverse-DNS application identifier shared by Balun desktop integrations.
pub(crate) const APPLICATION_ID: &str = "io.github.jm2.Balun";

/// Start the desktop application and run the GLib main loop.
pub(crate) fn run() -> gtk::glib::ExitCode {
    // The hidden packaging probe must run before GTK, GStreamer, or the
    // controller start; it exits the process without a window.
    match configure_before_toolkit() {
        Ok(ToolkitPreparation::Continue) => {}
        Ok(ToolkitPreparation::ProbeCompleted) => return gtk::glib::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Balun could not prepare the platform runtime: {error}");
            return gtk::glib::ExitCode::FAILURE;
        }
    }
    let controller = match ControllerRuntime::start_default() {
        Ok(controller) => controller,
        Err(error) => {
            eprintln!("Could not start the Balun controller: {error}");
            return gtk::glib::ExitCode::FAILURE;
        }
    };
    let (application, shutdown_failed) = application_with_controller(controller, |_| {});
    finish_run(application.run(), &shutdown_failed)
}

fn application_with_controller<F>(
    controller: ControllerRuntime,
    window_ready: F,
) -> (adw::Application, Rc<Cell<bool>>)
where
    F: Fn(&adw::ApplicationWindow) + 'static,
{
    gtk::glib::set_prgname(Some("Balun"));
    gtk::glib::set_application_name("Balun");

    let application = adw::Application::builder()
        .application_id(APPLICATION_ID)
        .build();
    application.add_action_entries([
        gtk::gio::ActionEntry::builder("quit")
            .activate(|application: &adw::Application, _, _| {
                // Closing the window joins playback/controller teardown and
                // drains settings writes before the application exits.
                if let Some(window) = existing_window(application) {
                    window.close();
                } else {
                    application.quit();
                }
            })
            .build(),
        gtk::gio::ActionEntry::builder("about")
            .activate(|application: &adw::Application, _, _| {
                if let Some(window) = existing_window(application) {
                    about_dialog().present(Some(&window));
                }
            })
            .build(),
    ]);
    application.set_accels_for_action("app.quit", &["<primary>q"]);
    application.connect_startup(|_| ui::icons::initialize());

    let controller = Rc::new(RefCell::new(Some(controller)));
    let shutdown_failed = Rc::new(Cell::new(false));
    let window_shutdown_failed = Rc::clone(&shutdown_failed);

    application.connect_activate(move |application| {
        if let Some(window) = existing_window(application) {
            window.present();
            return;
        }

        let Some(controller) = controller.borrow_mut().take() else {
            eprintln!("Balun cannot create another window after controller shutdown");
            application.quit();
            return;
        };
        // `activate` runs while the default GLib main context is owned. A
        // fixed initialization failure remains a player-pane state so device
        // discovery and lineup inspection stay available.
        let playback = PlaybackRuntime::initialize();
        let settings = SettingsSession::open(SettingsStore::at_default_location());
        let window = ui::window::build(
            application,
            controller,
            playback,
            settings,
            Rc::clone(&window_shutdown_failed),
        );
        window_ready(&window);
        window.present();
    });

    (application, shutdown_failed)
}

fn existing_window(application: &adw::Application) -> Option<gtk::Window> {
    application
        .active_window()
        .or_else(|| application.windows().into_iter().next())
}

/// Keep the About implementation shared across platforms, as in Tributary.
fn about_dialog() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name("Balun")
        .application_icon(APPLICATION_ID)
        .developer_name("John-Michael Mulesa")
        .version(env!("CARGO_PKG_VERSION"))
        .website(env!("CARGO_PKG_REPOSITORY"))
        .issue_url(concat!(env!("CARGO_PKG_REPOSITORY"), "/issues"))
        .copyright("© 2026 John-Michael Mulesa")
        .license_type(gtk::License::Gpl30)
        .build()
}

fn finish_run(exit_code: gtk::glib::ExitCode, shutdown_failed: &Cell<bool>) -> gtk::glib::ExitCode {
    if shutdown_failed.get() {
        gtk::glib::ExitCode::FAILURE
    } else {
        exit_code
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use balun::controller::{DiscoveryFailure, DiscoveryFuture, DiscoveryService, DiscoveryStatus};
    use balun::discovery::{DiscoveryReport, ExactDiscoveryTarget};
    use balun::domain::DeviceId;
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[derive(Clone)]
    struct CountingDiscovery {
        local: Arc<AtomicUsize>,
        exact: Arc<AtomicUsize>,
    }

    impl DiscoveryService for CountingDiscovery {
        fn discover_local(&self, _cancellation: CancellationToken) -> DiscoveryFuture {
            self.local.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok::<_, DiscoveryFailure>(DiscoveryReport::default()) })
        }

        fn discover_exact(
            &self,
            _target: ExactDiscoveryTarget,
            _expected_device: Option<DeviceId>,
            _cancellation: CancellationToken,
        ) -> DiscoveryFuture {
            self.exact.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok::<_, DiscoveryFailure>(DiscoveryReport::default()) })
        }
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; the ordinary unit-test
    /// jobs deliberately compile but skip this display-dependent smoke.
    ///
    /// The window queues one local discovery as it is built, so the close
    /// waits for that lane to settle: closing from an idle callback would
    /// race the controller for the queued command and make the count depend
    /// on timing. The script's isolated settings root holds no remembered
    /// target, so nothing may follow the launch discovery.
    #[test]
    #[ignore = "requires the isolated display and D-Bus session supplied by scripts/test-desktop-lifecycle.sh"]
    fn headless_window_close_joins_controller_after_launch_discovery() {
        run_lifecycle_smoke(false);
    }

    #[test]
    #[ignore = "requires the isolated display and D-Bus session supplied by scripts/test-desktop-lifecycle.sh"]
    fn headless_about_and_quit_join_controller_after_launch_discovery() {
        run_lifecycle_smoke(true);
    }

    fn run_lifecycle_smoke(quit_action: bool) {
        let local = Arc::new(AtomicUsize::new(0));
        let exact = Arc::new(AtomicUsize::new(0));
        let controller = ControllerRuntime::start(CountingDiscovery {
            local: Arc::clone(&local),
            exact: Arc::clone(&exact),
        })
        .expect("start packet-free smoke controller");
        let mut snapshots = controller.handle().subscribe();
        let before_launch = snapshots.borrow_and_update().discovery().generation();
        let (application, shutdown_failed) =
            application_with_controller(controller, move |window| {
                let window = window.downgrade();
                let mut snapshots = snapshots.clone();
                gtk::glib::MainContext::default().spawn_local(async move {
                    loop {
                        let discovery = snapshots.borrow_and_update().discovery();
                        if discovery.generation() > before_launch
                            && discovery.status() != DiscoveryStatus::Refreshing
                        {
                            break;
                        }
                        if snapshots.changed().await.is_err() {
                            break;
                        }
                    }
                    let window = window.upgrade().expect(
                        "smoke window should remain alive until the launch discovery settles",
                    );
                    if quit_action {
                        let application = window.application().expect("window application");
                        gtk::gio::prelude::ActionGroupExt::activate_action(
                            &application,
                            "about",
                            None,
                        );
                        let dialog = window
                            .visible_dialog()
                            .expect("About dialog presented")
                            .downcast::<adw::AboutDialog>()
                            .expect("Tributary-style About dialog");
                        assert_eq!(dialog.application_name(), "Balun");
                        assert_eq!(dialog.application_icon(), APPLICATION_ID);
                        assert_eq!(dialog.version(), env!("CARGO_PKG_VERSION"));
                        assert_eq!(dialog.developer_name(), "John-Michael Mulesa");
                        assert_eq!(dialog.website(), env!("CARGO_PKG_REPOSITORY"));
                        assert_eq!(dialog.issue_url(), "https://github.com/jm2/balun/issues");
                        assert_eq!(dialog.license_type(), gtk::License::Gpl30);
                        // Quit while About is still open, then request it again
                        // while the asynchronous joined shutdown is in flight.
                        gtk::gio::prelude::ActionGroupExt::activate_action(
                            &application,
                            "quit",
                            None,
                        );
                        gtk::gio::prelude::ActionGroupExt::activate_action(
                            &application,
                            "quit",
                            None,
                        );
                    } else {
                        window.close();
                    }
                });
            });

        let exit_code = application.run_with_args(&["balun-desktop-lifecycle-smoke"]);
        let exit_code = finish_run(exit_code, &shutdown_failed);

        assert_eq!(exit_code, gtk::glib::ExitCode::SUCCESS);
        assert!(!shutdown_failed.get(), "controller join must succeed");
        assert_eq!(
            local.load(Ordering::SeqCst),
            1,
            "window activation must run exactly one launch discovery"
        );
        assert_eq!(
            exact.load(Ordering::SeqCst),
            0,
            "no remembered target may be probed without settings"
        );
    }
}
