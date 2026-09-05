//! Playback-owned idle inhibition, released as soon as playback settles.

use gtk::prelude::*;

struct Inhibition<G> {
    requested: bool,
    guard: Option<G>,
}

impl<G> Inhibition<G> {
    fn update(&mut self, active: bool, acquire: impl FnOnce() -> Option<G>) {
        if active == self.requested {
            return;
        }
        self.requested = active;
        self.guard = if active { acquire() } else { None };
    }
}

pub(crate) struct PlaybackInhibitor {
    window: gtk::glib::WeakRef<gtk::Window>,
    inhibition: Inhibition<NativeGuard>,
}

impl PlaybackInhibitor {
    pub(crate) fn new(window: &impl IsA<gtk::Window>) -> Self {
        Self {
            window: window.as_ref().downgrade(),
            inhibition: Inhibition {
                requested: false,
                guard: None,
            },
        }
    }

    pub(crate) fn set_active(&mut self, active: bool) {
        let window = &self.window;
        self.inhibition
            .update(active, || NativeGuard::acquire(window));
    }
}

#[cfg(not(target_os = "macos"))]
struct NativeGuard {
    application: gtk::Application,
    cookie: u32,
}

#[cfg(not(target_os = "macos"))]
impl NativeGuard {
    fn acquire(window: &gtk::glib::WeakRef<gtk::Window>) -> Option<Self> {
        let window = window.upgrade()?;
        let application = window.application()?;
        let cookie = application.inhibit(
            Some(&window),
            gtk::ApplicationInhibitFlags::IDLE | gtk::ApplicationInhibitFlags::SUSPEND,
            Some("Watching live TV"),
        );
        if cookie == 0 {
            tracing::debug!("The desktop did not grant playback idle inhibition");
            return None;
        }
        Some(Self {
            application,
            cookie,
        })
    }
}

#[cfg(not(target_os = "macos"))]
impl Drop for NativeGuard {
    fn drop(&mut self) {
        self.application.uninhibit(self.cookie);
    }
}

// GTK's Quartz backend records IDLE/SUSPEND cookies without creating a power
// assertion. Use the OS helper instead; -w also releases it if Balun crashes.
#[cfg(target_os = "macos")]
struct NativeGuard(std::process::Child);

#[cfg(target_os = "macos")]
impl NativeGuard {
    fn acquire(window: &gtk::glib::WeakRef<gtk::Window>) -> Option<Self> {
        window.upgrade()?;
        use std::process::{Command, Stdio};
        match Command::new("/usr/bin/caffeinate")
            .args(["-d", "-i", "-w", &std::process::id().to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => Some(Self(child)),
            Err(_) => {
                tracing::warn!("Could not request macOS playback idle inhibition");
                None
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for NativeGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    struct Guard(Rc<Cell<u32>>);

    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn repeated_playback_updates_share_one_guard_and_every_stop_releases_it() {
        let released = Rc::new(Cell::new(0));
        let acquired = Cell::new(0);
        let acquire = || {
            acquired.set(acquired.get() + 1);
            Some(Guard(Rc::clone(&released)))
        };
        let mut inhibition = Inhibition {
            requested: false,
            guard: None,
        };
        inhibition.update(false, acquire);
        inhibition.update(true, acquire);
        inhibition.update(true, acquire);
        assert_eq!(acquired.get(), 1);
        assert_eq!(released.get(), 0);
        inhibition.update(false, acquire);
        inhibition.update(false, acquire);
        assert_eq!(released.get(), 1);
        inhibition.update(true, acquire);
        drop(inhibition);
        assert_eq!(acquired.get(), 2);
        assert_eq!(
            released.get(),
            2,
            "dropping the player also releases inhibition"
        );
    }

    #[test]
    fn denied_inhibition_retries_only_after_playback_restarts() {
        let attempts = Cell::new(0);
        let acquire = || {
            attempts.set(attempts.get() + 1);
            None::<Guard>
        };
        let mut inhibition = Inhibition {
            requested: false,
            guard: None,
        };
        inhibition.update(true, acquire);
        inhibition.update(true, acquire);
        assert_eq!(attempts.get(), 1);
        inhibition.update(false, acquire);
        inhibition.update(true, acquire);
        assert_eq!(attempts.get(), 2);
    }
}
