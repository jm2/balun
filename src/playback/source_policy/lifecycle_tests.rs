//! Forced source-callback schedules using real appsrc/transport workers and
//! owned loopback listeners. No physical tuner or display is involved.

use std::cell::Cell;
use std::net::TcpListener;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::controller::OperationGeneration;
use crate::domain::{ChannelKey, DeviceId, GuideNumber};
use crate::playback::test_support::{
    FixtureStreamServer, StreamBehavior, open_ended_response_head,
};
use crate::playback::transport::{TransportStartError, with_spawn_hook};

const BOUND: Duration = Duration::from_secs(5);

fn fixture(url: &str) -> (gst::Pipeline, Arc<SourcePolicy>, gst::Element) {
    gst::init().unwrap();
    let pipeline = gst::ElementFactory::make("playbin3")
        .build()
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
    let handoff = StreamHandoff::test_fixture(
        ChannelKey::new(
            DeviceId::new(0x105A_1232).unwrap(),
            GuideNumber::new("5.1").unwrap(),
        ),
        OperationGeneration::new(9),
        url,
    );
    let policy =
        SourcePolicy::install(&pipeline, handoff, TransportConfig::PRODUCTION, None).unwrap();
    let source = gst::ElementFactory::make("appsrc").build().unwrap();
    (pipeline, Arc::new(policy), source)
}

type Pause = Arc<dyn Fn() + Send + Sync>;

fn pause() -> (Pause, Receiver<()>, SyncSender<()>) {
    let (entered, observed) = mpsc::sync_channel(1);
    let (resume, resumed) = mpsc::sync_channel(1);
    let resumed = Mutex::new(resumed);
    let hook = Arc::new(move || {
        let _ = entered.send(());
        // Keep a failed test from leaving a native callback blocked forever.
        let _ = resumed.lock().unwrap().recv_timeout(BOUND);
    });
    (hook, observed, resume)
}

#[test]
fn retirement_before_final_admission_prevents_all_transport_work() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (pipeline, policy, source) = fixture(&format!(
        "http://{}/auto/v5.1",
        listener.local_addr().unwrap()
    ));
    let (pause, entered, resume) = pause();
    *policy.state.startup_hook.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == StartupStage::BeforeAdmission {
            pause();
        }
    }));
    let callback =
        thread::spawn(move || pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]));
    entered.recv_timeout(BOUND).unwrap();
    assert!(policy.retire().is_none());
    resume.send(()).unwrap();
    callback.join().unwrap();
    assert!(policy.is_rejected());
    let state = policy.state.lifecycle.lock().unwrap();
    assert!(state.retired && state.pending.is_none() && state.transport.is_none());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn retirement_joins_admission_during_startup_and_before_publication() {
    // None pauses second-thread creation, after the first worker exists.
    for point in [
        Some(StartupStage::HandoffTaken),
        None,
        Some(StartupStage::TransportStarted),
    ] {
        let server = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let (pipeline, policy, source) = fixture(&server.stream_url());
        let (pause, entered, resume) = pause();
        if let Some(point) = point {
            let pause = Arc::clone(&pause);
            *policy.state.startup_hook.lock().unwrap() = Some(Arc::new(move |stage| {
                if stage == point {
                    pause();
                }
            }));
        }
        let callback = thread::spawn(move || {
            let spawns = Cell::new(0);
            with_spawn_hook(
                Box::new(move |_| {
                    spawns.set(spawns.get() + 1);
                    if point.is_none() && spawns.get() == 2 {
                        pause();
                    }
                    Ok(())
                }),
                || pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]),
            );
        });
        entered.recv_timeout(BOUND).unwrap();
        assert!(matches!(
            policy.state.lifecycle.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let requested = point == Some(StartupStage::TransportStarted);
        if requested {
            assert!(server.request(BOUND).is_some());
        }
        let (retiring, retirement_started) = mpsc::channel();
        let (retired, retirement_finished) = mpsc::channel();
        let retiring_policy = Arc::clone(&policy);
        let retirement = thread::spawn(move || {
            retiring.send(()).unwrap();
            retired.send(retiring_policy.retire()).unwrap();
        });
        retirement_started.recv_timeout(BOUND).unwrap();
        assert!(matches!(
            retirement_finished.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        resume.send(()).unwrap();
        callback.join().unwrap();
        let mut transport = retirement_finished
            .recv_timeout(BOUND)
            .unwrap()
            .expect("every admitted worker is returned by the first retirement");
        retirement.join().unwrap();
        transport.join(Instant::now() + BOUND).unwrap();
        assert!(policy.retire().is_none());
        if requested {
            assert!(server.client_disconnected_within(BOUND));
        }

        // The successor starts only after the exact predecessor join above.
        let next_server =
            FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let (next_pipeline, next_policy, next_source) = fixture(&next_server.stream_url());
        next_pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&next_source]);
        assert!(next_server.request(BOUND).is_some());
        let mut next_transport = next_policy.retire().unwrap();
        next_transport.join(Instant::now() + BOUND).unwrap();
        assert!(next_server.client_disconnected_within(BOUND));
    }
}

#[test]
fn failed_thread_creation_keeps_every_created_worker_for_teardown() {
    for failed_spawn in [1, 2] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let (pipeline, policy, source) = fixture(&format!(
            "http://{}/auto/v5.1",
            listener.local_addr().unwrap()
        ));
        let bus = pipeline.bus().unwrap();
        bus.set_flushing(false);
        let spawns = Cell::new(0);
        with_spawn_hook(
            Box::new(move |_| {
                spawns.set(spawns.get() + 1);
                if spawns.get() == failed_spawn {
                    Err(TransportStartError::Thread)
                } else {
                    Ok(())
                }
            }),
            || pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]),
        );
        assert!(policy.is_rejected());
        let marker = bus
            .timed_pop_filtered(
                gst::ClockTime::from_seconds(1),
                &[gst::MessageType::Application],
            )
            .unwrap();
        assert!(is_rejection_marker(marker.structure().unwrap()));
        let transport = policy.retire();
        assert_eq!(transport.is_some(), failed_spawn == 2);
        if let Some(mut transport) = transport {
            transport.join(Instant::now() + BOUND).unwrap();
        }
        assert!(policy.retire().is_none());
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn poisoned_lifecycle_still_returns_its_transport_for_join() {
    let server = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
    let (pipeline, policy, source) = fixture(&server.stream_url());
    pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
    assert!(server.request(BOUND).is_some());
    let state = Arc::clone(&policy.state);
    assert!(
        thread::spawn(move || {
            let _guard = state.lifecycle.lock().unwrap();
            panic!("injected lifecycle poison");
        })
        .join()
        .is_err()
    );
    let mut transport = policy
        .retire()
        .expect("poison must not discard worker ownership");
    transport.join(Instant::now() + BOUND).unwrap();
    assert!(server.client_disconnected_within(BOUND));
}

#[test]
fn rejection_during_publication_cancels_transport_but_retains_its_join() {
    for point in [StartupStage::HandoffTaken, StartupStage::TransportStarted] {
        let server = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
        let (pipeline, policy, source) = fixture(&server.stream_url());
        let (pause, entered, resume) = pause();
        let (request_seen, wait_for_request) = mpsc::sync_channel(0);
        let wait_for_request = Mutex::new(wait_for_request);
        *policy.state.startup_hook.lock().unwrap() = Some(Arc::new(move |stage| {
            if stage == point {
                pause();
            }
            if stage == StartupStage::TransportStarted {
                // Keep publication's lock until the reader really connected,
                // including the earlier HandoffTaken schedule. Rejection may
                // already be waiting for this lock; it must cancel that reader.
                wait_for_request
                    .lock()
                    .unwrap()
                    .recv_timeout(BOUND)
                    .unwrap();
            }
        }));
        let publishing_pipeline = pipeline.clone();
        let callback = thread::spawn(move || {
            publishing_pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
        });
        entered.recv_timeout(BOUND).unwrap();
        assert!(matches!(
            policy.state.lifecycle.try_lock(),
            Err(std::sync::TryLockError::WouldBlock)
        ));
        let requested = point == StartupStage::TransportStarted;
        if requested {
            assert!(server.request(BOUND).is_some());
        }
        let foreign = gst::ElementFactory::make("fakesrc").build().unwrap();
        let (started, starting) = mpsc::channel();
        let rejection = thread::spawn(move || {
            started.send(()).unwrap();
            pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&foreign]);
        });
        starting.recv_timeout(BOUND).unwrap();
        resume.send(()).unwrap();
        if !requested {
            assert!(server.request(BOUND).is_some());
        }
        request_seen.send(()).unwrap();
        callback.join().unwrap();
        rejection.join().unwrap();
        assert!(policy.is_rejected());
        {
            let lifecycle = policy.state.lifecycle.lock().unwrap();
            assert!(lifecycle.retired && lifecycle.pending.is_none());
            assert!(
                lifecycle.transport.is_some(),
                "rejection must retain join ownership"
            );
        }
        // This observation precedes retire/join, so their cancellation cannot
        // accidentally make a missing rejection cancellation pass in either schedule.
        assert!(server.client_disconnected_within(BOUND));
        let mut transport = policy.retire().expect("retained transport");
        transport.join(Instant::now() + BOUND).unwrap();
        assert!(policy.retire().is_none());
    }
}

#[test]
fn poisoned_admission_rejects_without_deadlocking_or_discarding_workers() {
    let server = FixtureStreamServer::start(open_ended_response_head(), StreamBehavior::Hold);
    let (pipeline, policy, source) = fixture(&server.stream_url());
    pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
    assert!(server.request(BOUND).is_some());
    let state = Arc::clone(&policy.state);
    assert!(
        thread::spawn(move || {
            let _guard = state.lifecycle.lock().unwrap();
            panic!("injected lifecycle poison");
        })
        .join()
        .is_err()
    );
    let (finished, observed) = mpsc::channel();
    let callback = thread::spawn(move || {
        pipeline.emit_by_name::<()>(SOURCE_SETUP_SIGNAL, &[&source]);
        finished.send(()).unwrap();
    });
    observed
        .recv_timeout(BOUND)
        .expect("poisoned admission must not deadlock in rejection");
    callback.join().unwrap();
    assert!(policy.is_rejected());
    assert!(server.client_disconnected_within(BOUND));
    let mut transport = policy.retire().expect("poison must retain the transport");
    transport.join(Instant::now() + BOUND).unwrap();
    assert!(policy.retire().is_none());
}
