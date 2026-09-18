//! Isolated evidence for the limit of a timeout after a synchronous native call.
//! The deliberately stuck GStreamer callback only runs in an owned child process.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use gst::glib;
use gst::prelude::*;
use gst::subclass::prelude::*;
use gstreamer as gst;

const CHILD_TEST: &str = "playback::session::native_failure_study::blocking_native_child";
const PHASE_ENV: &str = "BALUN_TEST_NATIVE_STALL_PHASE";
const MARKER_ENV: &str = "BALUN_TEST_NATIVE_STALL_MARKER";

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct StalledElement;

    #[glib::object_subclass]
    impl ObjectSubclass for StalledElement {
        const NAME: &'static str = "BalunIsolatedStalledElement";
        type Type = super::StalledElement;
        type ParentType = gst::Element;
    }

    impl ObjectImpl for StalledElement {}
    impl GstObjectImpl for StalledElement {}

    impl ElementImpl for StalledElement {
        fn change_state(
            &self,
            transition: gst::StateChange,
        ) -> Result<gst::StateChangeSuccess, gst::StateChangeError> {
            let phase = std::env::var(PHASE_ENV).expect("isolated fixture phase");
            if (phase == "startup" && transition == gst::StateChange::NullToReady)
                || (phase == "teardown" && transition == gst::StateChange::ReadyToNull)
            {
                let marker = std::env::var_os(MARKER_ENV).expect("isolated fixture marker");
                std::fs::write(marker, b"entered native state callback").unwrap();
                // There is deliberately no in-process recovery. The parent owns
                // and terminates this entire child after the observation bound.
                loop {
                    std::thread::park();
                }
            }
            self.parent_change_state(transition)
        }
    }
}

glib::wrapper! {
    pub struct StalledElement(ObjectSubclass<imp::StalledElement>)
        @extends gst::Element, gst::Object;
}

#[test]
#[ignore = "child fixture; the ordinary parent test owns its lifetime"]
fn blocking_native_child() {
    let Ok(phase) = std::env::var(PHASE_ENV) else {
        // Running the ignored suite directly must not accidentally hang it.
        return;
    };
    assert!(matches!(phase.as_str(), "startup" | "teardown"));
    gst::init().expect("GStreamer initialization");
    let pipeline = gst::Pipeline::new();
    let element: StalledElement = glib::Object::new();
    pipeline.add(&element).unwrap();
    if phase == "teardown" {
        pipeline.set_state(gst::State::Ready).unwrap();
    }
    let deadline = Instant::now() + super::PIPELINE_TEARDOWN_TIMEOUT;
    let target = if phase == "startup" {
        gst::State::Ready
    } else {
        gst::State::Null
    };
    let _ = pipeline.set_state(target);
    // This mirrors the production call order. A timeout on this later wait
    // cannot be reached while the preceding set_state call is still blocked.
    let _ = pipeline.state(super::clock_time_until(deadline));
    panic!("the deliberately blocked native call unexpectedly returned");
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn synchronous_native_stalls_outlive_the_later_state_wait_deadline() {
    for phase in ["startup", "teardown"] {
        let temporary = tempfile::tempdir().expect("isolated marker directory");
        let marker = temporary.path().join("entered");
        let mut child = OwnedChild(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", CHILD_TEST, "--ignored", "--nocapture"])
                .env(PHASE_ENV, phase)
                .env(MARKER_ENV, &marker)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("isolated native fixture"),
        );
        let ready_deadline = Instant::now() + Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "native fixture exited before entering"
            );
            assert!(
                Instant::now() < ready_deadline,
                "native fixture did not enter its callback"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let entered = Instant::now();
        let observation = super::PIPELINE_TEARDOWN_TIMEOUT + Duration::from_millis(200);
        while entered.elapsed() < observation {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "native stall unexpectedly ended"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(child.0.try_wait().unwrap().is_none());
        child
            .0
            .kill()
            .expect("terminate only the owned native fixture");
        assert!(!child.0.wait().expect("reap the owned fixture").success());
    }
}
