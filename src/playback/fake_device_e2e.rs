//! End-to-end proofs that drive the real controller and the real playback
//! transport against a loopback fake HDHomeRun device.

mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use gst::prelude::*;
    use gstreamer as gst;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use crate::controller::{
        ApplicationSnapshot, ControllerCommand, ControllerHandle, ControllerRuntime,
        DiscoveryFailure, DiscoveryFuture, DiscoveryService, DiscoveryStatus, OperationGeneration,
        SelectedLineupStatus, StreamHandoffError, StreamSelection,
    };
    use crate::discovery::{DiscoveryClient, DiscoveryError, ExactDiscoveryTarget, ProbeConfig};
    use crate::domain::{ChannelKey, DeviceId, GuideNumber};
    use crate::hdhr::fake_device::{
        FakeChannelSpec, FakeHdhrDevice, FakeStreamBody, StreamEventKind,
    };
    use crate::playback::source_policy::SourcePolicy;
    use crate::playback::test_support::hold_decoder_selection;
    use crate::playback::transport::{PIPELINE_URI, TransportConfig};
    use crate::playback::{
        PlaybackRuntime, PlaybackSession, PlaybackSessionState, TuneCompletion, TuneGeneration,
    };

    const WAIT: Duration = Duration::from_secs(5);
    const QUICK: TransportConfig = TransportConfig::new(
        Duration::from_millis(500),
        Duration::from_millis(1_500),
        Duration::from_millis(300),
    );

    struct FakeDeviceDiscovery {
        target: SocketAddr,
    }

    fn fake_probe_config() -> ProbeConfig {
        ProbeConfig::new(1, Duration::from_millis(200), 16, 4)
            .expect("fixed fake-device probe budget must be valid")
    }

    impl DiscoveryService for FakeDeviceDiscovery {
        fn discover_local(&self, cancellation: CancellationToken) -> DiscoveryFuture {
            let client = DiscoveryClient::new(fake_probe_config());
            let target = self.target;
            Box::pin(async move {
                client
                    .discover_target(target, None, &cancellation)
                    .await
                    .map_err(discovery_failure)
            })
        }

        fn discover_exact(
            &self,
            target: ExactDiscoveryTarget,
            expected_device: Option<DeviceId>,
            cancellation: CancellationToken,
        ) -> DiscoveryFuture {
            let client = DiscoveryClient::new(fake_probe_config());
            Box::pin(async move {
                client
                    .discover_target(target.socket_addr(), expected_device, &cancellation)
                    .await
                    .map_err(discovery_failure)
            })
        }
    }

    fn discovery_failure(error: DiscoveryError) -> DiscoveryFailure {
        match error {
            DiscoveryError::Interfaces(_) => DiscoveryFailure::InterfaceEnumeration,
            DiscoveryError::Io { .. } | DiscoveryError::ShortSend { .. } => {
                DiscoveryFailure::Network
            }
            DiscoveryError::InvalidEndpoint { .. }
            | DiscoveryError::Task(_)
            | DiscoveryError::RoutedScanDeadline { .. }
            | DiscoveryError::Cancelled
            | DiscoveryError::Protocol(_) => DiscoveryFailure::Internal,
        }
    }

    async fn wait_for_snapshot(
        receiver: &mut watch::Receiver<Arc<ApplicationSnapshot>>,
        predicate: impl Fn(&ApplicationSnapshot) -> bool,
    ) -> Arc<ApplicationSnapshot> {
        tokio::time::timeout(WAIT, async {
            loop {
                let snapshot = Arc::clone(&receiver.borrow_and_update());
                if predicate(&snapshot) {
                    return snapshot;
                }
                receiver
                    .changed()
                    .await
                    .expect("controller should remain alive while waiting");
            }
        })
        .await
        .expect("controller snapshot wait should remain bounded")
    }

    fn mpeg2_decoder_available() -> bool {
        ["avdec_mpeg2video", "mpeg2dec"]
            .into_iter()
            .any(|factory| gst::ElementFactory::find(factory).is_some())
    }

    fn headless_pipeline_available() -> bool {
        gst::init().is_ok()
            && mpeg2_decoder_available()
            && gst::ElementFactory::find("tsdemux").is_some()
    }

    struct DecoderRankGuard {
        original: Vec<(gst::PluginFeature, gst::Rank)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for DecoderRankGuard {
        fn drop(&mut self) {
            for (feature, rank) in self.original.drain(..) {
                feature.set_rank(rank);
            }
        }
    }

    fn prefer_software_mpeg2_decoders() -> DecoderRankGuard {
        let lock = hold_decoder_selection();
        let registry = gst::Registry::get();
        let mut original = Vec::new();
        for name in [
            "vtdec_hw",
            "vtdec",
            "d3d11mpeg2dec",
            "d3d12mpeg2dec",
            "nvmpeg2videodec",
            "nvmpeg2dec",
            "qsvmpeg2dec",
            "msdkmpeg2dec",
            "amfmpeg2dec",
            "vampeg2dec",
            "vaapimpeg2dec",
            "v4l2slmpeg2dec",
        ] {
            if let Some(feature) = registry.lookup_feature(name) {
                original.push((feature.clone(), feature.rank()));
                feature.set_rank(gst::Rank::NONE);
            }
        }
        DecoderRankGuard {
            original,
            _lock: lock,
        }
    }

    fn native_error_summary(message: &gst::MessageRef) -> String {
        let gst::MessageView::Error(error) = message.view() else {
            return String::from("non-error message");
        };
        let native = error.error();
        let factory = message
            .src()
            .and_then(|source| source.downcast_ref::<gst::Element>().cloned())
            .and_then(|element| element.factory())
            .map_or_else(
                || String::from("<none>"),
                |factory| factory.name().to_string(),
            );
        format!(
            "domain={} code={} source_factory={factory}",
            native.domain().as_str(),
            native.code()
        )
    }

    fn tune_through_controller(
        rt: &tokio::runtime::Runtime,
        handle: &ControllerHandle,
        session: &PlaybackSession,
        guide_number: &'static str,
        device_id: DeviceId,
        generation: OperationGeneration,
    ) -> TuneGeneration {
        let request = session
            .begin_tune(StreamSelection::new(
                ChannelKey::new(
                    device_id,
                    GuideNumber::new(guide_number).expect("valid fake-device guide number"),
                ),
                generation,
            ))
            .expect("begin the generation-owned fake-device tune");
        let receiver = handle
            .try_request_stream(request.selection().clone())
            .expect("admit the private fake-device stream request");
        let handoff = rt
            .block_on(receiver.receive())
            .expect("the controller authorizes the fake-device stream");
        let request_generation = request.generation();
        assert_eq!(
            session.complete_tune(request, Ok(handoff)),
            Ok(TuneCompletion::Applied)
        );
        request_generation
    }

    fn pump_context(main_context: &gst::glib::MainContext) {
        while main_context.pending() {
            main_context.iteration(false);
        }
    }

    fn pump_until_playing(
        main_context: &gst::glib::MainContext,
        states: &mut watch::Receiver<PlaybackSessionState>,
        wanted: TuneGeneration,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        let mut observed_playing = false;
        loop {
            pump_context(main_context);
            if states.has_changed().unwrap_or(false) {
                match states.borrow_and_update().clone() {
                    PlaybackSessionState::Playing { generation, .. } if generation == wanted => {
                        observed_playing = true;
                    }
                    PlaybackSessionState::Failed { failure, .. } => {
                        panic!("the fake-device tune failed: {failure}");
                    }
                    _ => {}
                }
            }
            if observed_playing {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the fake-device tune must reach PLAYING within the bound"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn pump_until_stopped(
        main_context: &gst::glib::MainContext,
        states: &mut watch::Receiver<PlaybackSessionState>,
        timeout: Duration,
    ) {
        let deadline = Instant::now() + timeout;
        loop {
            pump_context(main_context);
            if states.has_changed().unwrap_or(false) {
                match states.borrow_and_update().clone() {
                    PlaybackSessionState::Stopped => return,
                    PlaybackSessionState::Failed { failure, .. } => {
                        panic!("the finite fake-device tune failed: {failure}");
                    }
                    _ => {}
                }
            }
            assert!(
                Instant::now() < deadline,
                "the finite fake-device tune must settle to Stopped within the bound"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[tokio::test]
    async fn fake_device_end_to_end_reaches_the_controller_handoff_and_transport_eos() {
        if !headless_pipeline_available() {
            return;
        }

        let device = FakeHdhrDevice::start(
            2,
            &[
                FakeChannelSpec {
                    guide_number: "5.1",
                    guide_name: "Sample Five One",
                    drm: false,
                    body: FakeStreamBody::FixtureOnce,
                },
                FakeChannelSpec {
                    guide_number: "7.1",
                    guide_name: "Protected Seven",
                    drm: true,
                    body: FakeStreamBody::FixtureOnce,
                },
            ],
        );
        let device_id = device.device_id();

        let runtime = ControllerRuntime::start(FakeDeviceDiscovery {
            target: device.discovery_target(),
        })
        .expect("start the controller against the loopback fake device");
        let handle = runtime.handle();
        let mut snapshots = handle.subscribe();

        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .expect("admit the local refresh command");
        let discovered = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.devices().len() == 1 && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        assert_eq!(discovered.devices()[0].device_id(), device_id);

        handle
            .try_send(ControllerCommand::SelectDevice(device_id))
            .expect("admit the select-device command");
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;
        let generation = ready.selected_lineup().generation();
        assert_eq!(
            ready
                .selected_lineup()
                .channels()
                .iter()
                .map(|row| row.key().guide_number().as_str())
                .collect::<Vec<_>>(),
            ["5.1", "7.1"]
        );
        assert!(!ready.selected_lineup().channels()[0].is_drm());
        assert!(ready.selected_lineup().channels()[1].is_drm());

        let protected = handle
            .try_request_stream(StreamSelection::new(
                ChannelKey::new(
                    device_id,
                    GuideNumber::new("7.1").expect("valid guide number"),
                ),
                generation,
            ))
            .expect("admit the protected stream request");
        assert!(matches!(
            protected.receive().await,
            Err(StreamHandoffError::Protected)
        ));
        assert!(
            device.stream_events().is_empty(),
            "a refused protected channel must never contact a tuner"
        );

        let receiver = handle
            .try_request_stream(StreamSelection::new(
                ChannelKey::new(
                    device_id,
                    GuideNumber::new("5.1").expect("valid guide number"),
                ),
                generation,
            ))
            .expect("admit the unprotected stream request");
        let handoff = receiver
            .receive()
            .await
            .expect("the real controller authorizes the fake-device stream");
        let handoff_debug = format!("{handoff:?}");

        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .expect("the headless foundation provides playbin3")
            .downcast::<gst::Pipeline>()
            .expect("playbin3 is a pipeline");
        let video_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        let audio_sink = gst::ElementFactory::make("fakesink").build().unwrap();
        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-sink", &audio_sink);
        let _software_decoders = prefer_software_mpeg2_decoders();
        let policy = SourcePolicy::install(&playbin, handoff, QUICK)
            .expect("install the appsrc policy with the real handoff");
        playbin.set_property("uri", PIPELINE_URI);
        assert_eq!(
            playbin.property::<Option<String>>("uri").as_deref(),
            Some(PIPELINE_URI)
        );
        let bus = playbin.bus().expect("playbin3 provides a bus");

        playbin
            .set_state(gst::State::Playing)
            .expect("request playback");

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut reached_playing = false;
        let mut terminal = None;
        let mut diagnostic = String::from("no terminal message within the deadline");
        while terminal.is_none() && Instant::now() < deadline {
            let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
                continue;
            };
            match message.view() {
                gst::MessageView::Eos(_) => terminal = Some("eos"),
                gst::MessageView::Error(_) => {
                    diagnostic = native_error_summary(&message);
                    terminal = Some("error");
                }
                gst::MessageView::Application(application) => {
                    diagnostic = format!(
                        "application marker {:?}",
                        application
                            .structure()
                            .map(|structure| structure.name().to_string())
                    );
                    terminal = Some("application");
                }
                gst::MessageView::StateChanged(changed)
                    if message.src() == Some(playbin.upcast_ref::<gst::Object>())
                        && changed.current() == gst::State::Playing =>
                {
                    reached_playing = true;
                }
                _ => {}
            }
        }
        assert_eq!(terminal, Some("eos"), "{diagnostic}");
        assert!(
            reached_playing,
            "playbin3 must reach PLAYING from the fake-device feed"
        );
        assert!(!policy.is_rejected());

        let mut transport = policy
            .retire()
            .expect("the played transport is returned once");
        playbin.set_state(gst::State::Null).unwrap();
        let (transition, current, _) = playbin.state(gst::ClockTime::from_seconds(5));
        assert!(transition.is_ok());
        assert_eq!(current, gst::State::Null);
        assert_eq!(
            transport.join(Instant::now() + Duration::from_secs(5)),
            Ok(())
        );

        assert_eq!(device.metadata_paths(), ["/discover.json", "/lineup.json"]);
        let released = device.wait_for_stream_events(Duration::from_secs(5), |events| {
            events.len() == 2
                && events[0].path == "/auto/v5.1"
                && matches!(events[0].kind, StreamEventKind::Connected)
                && events[1].path == "/auto/v5.1"
                && matches!(events[1].kind, StreamEventKind::Closed)
        });
        assert!(
            released,
            "the finite fake-device tune must connect once and be released by teardown"
        );

        let rendered_snapshot = format!("{:?}", snapshots.borrow_and_update());
        for secret in ["/auto/", "5004", "http://"] {
            assert!(!rendered_snapshot.contains(secret), "{rendered_snapshot}");
        }
        for secret in ["/auto/", "5004", "65001", "127.0.0.1", "http://"] {
            assert!(!handoff_debug.contains(secret), "{handoff_debug}");
        }

        runtime
            .shutdown()
            .expect("join the controller cleanly after the fake-device tune");
    }

    /// Run through `scripts/test-desktop-lifecycle.sh`; ordinary unit jobs
    /// compile but skip this display- and tuner-dependent production proof.
    ///
    /// The fake loopback device supplies discovery, metadata, and stream
    /// endpoints, so the real controller, the real generation-owned session,
    /// and the real transport exercise tuning, synchronous channel switching
    /// with tuner release ordering, natural EOS, and explicit Stop.
    #[test]
    #[ignore = "requires the isolated display and complete playback runtime supplied by scripts/test-desktop-lifecycle.sh"]
    fn fake_device_production_session_tunes_switches_and_releases_the_tuner() {
        let _decoder_selection = hold_decoder_selection();
        adw::init().expect("initialize libadwaita for the fake-device production session");
        let main_context = gst::glib::MainContext::default();
        let _owner = main_context
            .acquire()
            .expect("acquire the default main context for the fake-device production session");
        let playback = PlaybackRuntime::initialize()
            .expect("initialize the complete production playback runtime");
        assert!(
            playback.capabilities().is_foundation_ready(),
            "the lifecycle harness must install Balun's complete playback foundation"
        );
        let session = PlaybackSession::new(playback);

        let device = FakeHdhrDevice::start(
            2,
            &[
                FakeChannelSpec {
                    guide_number: "5.1",
                    guide_name: "Sample Five One",
                    drm: false,
                    body: FakeStreamBody::FixtureRepeat,
                },
                FakeChannelSpec {
                    guide_number: "5.2",
                    guide_name: "Sample Five Two",
                    drm: false,
                    body: FakeStreamBody::FixtureOnce,
                },
                FakeChannelSpec {
                    guide_number: "7.1",
                    guide_name: "Protected Seven",
                    drm: true,
                    body: FakeStreamBody::FixtureOnce,
                },
            ],
        );
        let device_id = device.device_id();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build the controller-await runtime");
        let (controller, generation) = rt.block_on(async {
            let runtime = ControllerRuntime::start(FakeDeviceDiscovery {
                target: device.discovery_target(),
            })
            .expect("start the controller against the loopback fake device");
            let handle = runtime.handle();
            let mut snapshots = handle.subscribe();
            handle
                .try_send(ControllerCommand::RefreshLocalDiscovery)
                .expect("admit the local refresh command");
            let discovered = wait_for_snapshot(&mut snapshots, |snapshot| {
                snapshot.devices().len() == 1
                    && snapshot.discovery().status() == DiscoveryStatus::Ready
            })
            .await;
            assert_eq!(discovered.devices()[0].device_id(), device_id);
            handle
                .try_send(ControllerCommand::SelectDevice(device_id))
                .expect("admit the select-device command");
            let selected = wait_for_snapshot(&mut snapshots, |snapshot| {
                snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
            })
            .await;
            assert_eq!(
                selected
                    .selected_lineup()
                    .channels()
                    .iter()
                    .map(|row| row.key().guide_number().as_str())
                    .collect::<Vec<_>>(),
                ["5.1", "5.2", "7.1"]
            );
            (runtime, selected.selected_lineup().generation())
        });
        let handle = controller.handle();
        let mut states = session
            .subscribe_state()
            .expect("subscribe to the URL-free session state");

        let first_generation =
            tune_through_controller(&rt, &handle, &session, "5.1", device_id, generation);
        pump_until_playing(
            &main_context,
            &mut states,
            first_generation,
            Duration::from_secs(10),
        );

        let second_request = session
            .begin_tune(StreamSelection::new(
                ChannelKey::new(
                    device_id,
                    GuideNumber::new("5.2").expect("valid fake-device guide number"),
                ),
                generation,
            ))
            .expect("begin the switching fake-device tune");
        let second_generation = second_request.generation();
        let second_receiver = handle
            .try_request_stream(second_request.selection().clone())
            .expect("admit the switching fake-device stream request");
        let predecessor_released =
            device.wait_for_stream_events(Duration::from_secs(5), |events| {
                events.iter().any(|event| {
                    event.path == "/auto/v5.1" && matches!(event.kind, StreamEventKind::Closed)
                })
            });
        assert!(
            predecessor_released,
            "the predecessor tuner must be released before the successor tune is completed"
        );
        let second_handoff = rt
            .block_on(second_receiver.receive())
            .expect("the controller authorizes the switching fake-device stream");
        assert_eq!(
            session.complete_tune(second_request, Ok(second_handoff)),
            Ok(TuneCompletion::Applied)
        );
        let successor_opened = device.wait_for_stream_events(Duration::from_secs(5), |events| {
            events.iter().any(|event| {
                event.path == "/auto/v5.2" && matches!(event.kind, StreamEventKind::Connected)
            })
        });
        assert!(
            successor_opened,
            "the successor fake-device stream must open after the predecessor release"
        );
        pump_until_playing(
            &main_context,
            &mut states,
            second_generation,
            Duration::from_secs(10),
        );
        pump_until_stopped(&main_context, &mut states, Duration::from_secs(10));
        assert_eq!(
            session.state().expect("read the session state"),
            PlaybackSessionState::Stopped
        );
        assert!(
            session
                .paintable()
                .expect("read the URI-opaque paintable")
                .is_none(),
            "EOS retirement settles the exact pipeline and its paintable"
        );

        let third_generation =
            tune_through_controller(&rt, &handle, &session, "5.1", device_id, generation);
        pump_until_playing(
            &main_context,
            &mut states,
            third_generation,
            Duration::from_secs(10),
        );
        session.stop().expect("stop the active fake-device tune");
        let released_by_stop = device.wait_for_stream_events(Duration::from_secs(5), |events| {
            let mut connections = events.iter().filter(|event| {
                event.path == "/auto/v5.1" && matches!(event.kind, StreamEventKind::Connected)
            });
            connections.nth(1).is_some_and(|second_connection| {
                events.iter().any(|event| {
                    event.path == "/auto/v5.1"
                        && matches!(event.kind, StreamEventKind::Closed)
                        && event.at > second_connection.at
                })
            })
        });
        assert!(
            released_by_stop,
            "an explicit Stop must release the active tuner after its connection"
        );
        assert_eq!(
            session.state().expect("read the session state"),
            PlaybackSessionState::Stopped
        );

        session
            .shut_down()
            .expect("settle the production session after the fake-device lifecycle");
        assert_eq!(
            session.state().expect("read the session state"),
            PlaybackSessionState::ShutDown
        );
        drop(_owner);
        drop(rt);
        controller
            .shutdown()
            .expect("join the controller cleanly after the fake-device lifecycle");
    }
}
