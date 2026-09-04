//! Opt-in, display-free acceptance proofs against real HDHomeRun tuners.
//!
//! Every test here is `#[ignore]`d and additionally gated on
//! `BALUN_LIVE_HARDWARE=1`, so neither CI nor ordinary local runs ever touch
//! hardware. The proofs drive only public crate APIs plus the crate-internal
//! playback pieces to capture P0 evidence: live-TV machine proof with decoded
//! video and audio, tune/switch/release budgets, codec and factory
//! observations, sanitized metadata captures, and the exact-address probe.
//! Nothing this harness prints or writes may contain addresses, friendly
//! names, real channel names, or URLs; addresses and names stay in memory.
//!
//! Run the proofs serially so they never compete for tuners:
//!
//! ```text
//! BALUN_LIVE_HARDWARE=1 cargo test --features desktop --lib live_hardware -- \
//!     --ignored --nocapture --test-threads=1
//! ```
//!
//! The modern-codec lane also needs `BALUN_LIVE_MODERN_CHANNEL=<guide number>`
//! naming an unprotected ATSC 3.0 channel in the 4K unit's lineup; without it
//! the lane writes the metadata captures and stops. Audio renders into a
//! `fakesink` unless `BALUN_LIVE_AUDIO_SINK` names another sink factory such
//! as `pulsesink`, which proves the desktop audio path at the cost of a few
//! seconds of sound.

mod tests {
    use std::collections::BTreeSet;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use gst::prelude::*;
    use gstreamer as gst;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    use crate::controller::{
        ApplicationSnapshot, ControllerCommand, ControllerHandle, ControllerRuntime,
        DiscoveryStatus, OperationGeneration, SelectedLineupStatus, StreamHandoff, StreamSelection,
    };
    use crate::discovery::{DeviceRegistry, DiscoveryClient, RegistryInstant};
    use crate::domain::{ChannelKey, DeviceId};
    use crate::hdhr::{DeviceSnapshot, DeviceSnapshotResolver, DeviceSnapshotTarget};
    use crate::playback::pipeline_failure::classify_pipeline_message;
    use crate::playback::source_policy::SourcePolicy;
    use crate::playback::test_support::hold_decoder_selection;
    use crate::playback::transport::{PIPELINE_URI, TransportConfig};

    const FIRST_FRAME_BOUND: Duration = Duration::from_secs(25);
    const TEARDOWN_BOUND: Duration = Duration::from_secs(5);
    const SNAPSHOT_WAIT: Duration = Duration::from_secs(30);
    const POLL_INTERVAL: Duration = Duration::from_millis(5);
    /// Production-shaped deadlines tightened for evidence capture: connect,
    /// response headers, and idle body reads against a real tuner.
    const QUICK: TransportConfig = TransportConfig::new(
        Duration::from_secs(2),
        Duration::from_secs(5),
        Duration::from_secs(5),
    );

    /// Holds the shared decoder-selection lock and restores the original rank
    /// of every demoted decoder factory when dropped, on every exit path
    /// including a panicking assertion, so a later pipeline in the same
    /// process autoplugs from the registry it started with.
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

    /// Demote hosted-CI hardware MPEG-2 decoder factories that cannot open a
    /// decoding session without a GPU for the duration of the returned guard,
    /// so `decodebin3` autoplugs the software decoders against real tuners.
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

    /// The audio sink factory the proofs render into: `fakesink` unless
    /// `BALUN_LIVE_AUDIO_SINK` names another base sink such as `pulsesink`.
    fn live_audio_sink_factory() -> String {
        std::env::var("BALUN_LIVE_AUDIO_SINK")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| String::from("fakesink"))
    }

    /// A headless `playbin3` with a fakesink video sink and the configured
    /// audio sink set before any URI, or `None` when the local GStreamer
    /// cannot supply playbin3 or that sink.
    fn live_headless_pipeline() -> Option<(gst::Pipeline, gst::Element, gst::Element)> {
        gst::init().ok()?;
        let playbin = gst::ElementFactory::make("playbin3")
            .build()
            .ok()?
            .downcast::<gst::Pipeline>()
            .ok()?;
        let video_sink = gst::ElementFactory::make("fakesink").build().ok()?;
        let audio_sink = gst::ElementFactory::make(&live_audio_sink_factory())
            .build()
            .ok()?;
        playbin.set_property("video-sink", &video_sink);
        playbin.set_property("audio-sink", &audio_sink);
        Some((playbin, video_sink, audio_sink))
    }

    /// Buffers the sink has rendered so far, mirroring the transport tests'
    /// `stats` extraction.
    fn rendered_count(sink: &gst::Element) -> u64 {
        sink.property::<gst::Structure>("stats")
            .get::<u64>("rendered")
            .unwrap_or(0)
    }

    /// The sink's first sink-pad current caps as a compact string.
    fn sink_caps(sink: &gst::Element) -> Option<String> {
        let Ok(Some(pad)) = sink.iterate_sink_pads().next() else {
            return None;
        };
        Some(pad.current_caps()?.to_string().chars().take(200).collect())
    }

    /// Every element factory name used anywhere inside the live pipeline,
    /// recursing through bins, sorted and deduplicated.
    fn collect_pipeline_factories(pipeline: &gst::Pipeline) -> Vec<String> {
        let mut factories = Vec::new();
        collect_bin_factories(pipeline.upcast_ref::<gst::Bin>(), &mut factories);
        factories.sort();
        factories.dedup();
        factories
    }

    fn collect_bin_factories(bin: &gst::Bin, factories: &mut Vec<String>) {
        for element in bin.iterate_elements() {
            let Ok(element) = element else { continue };
            if let Some(factory) = element.factory() {
                factories.push(factory.name().to_string());
            }
            if let Some(child) = element.dynamic_cast_ref::<gst::Bin>() {
                collect_bin_factories(child, factories);
            }
        }
    }

    /// Wait for the first rendered video buffer and return the elapsed time
    /// since `started`.
    fn wait_for_first_video_frame(
        video_sink: &gst::Element,
        started: Instant,
        bound: Duration,
    ) -> Duration {
        let deadline = started + bound;
        loop {
            if rendered_count(video_sink) > 0 {
                return started.elapsed();
            }
            assert!(
                Instant::now() < deadline,
                "the live tune must decode a first video frame within {bound:?}"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Endpoint-free diagnostics for a failed live run: the native error
    /// domain and code plus the factory name of the reporting element. No
    /// error or debug text is rendered.
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

    /// The media type a `missing-plugin` element message asks for, e.g.
    /// `video/x-h265`, without any of the caps' other fields.
    fn missing_plugin_media_type(message: &gst::MessageRef) -> String {
        let Some(structure) = message.structure() else {
            return String::from("<no structure>");
        };
        structure
            .get::<gst::Caps>("detail")
            .ok()
            .and_then(|caps| {
                caps.structure(0)
                    .map(|structure| structure.name().to_string())
            })
            .or_else(|| structure.get::<String>("name").ok())
            .unwrap_or_else(|| String::from("<unknown>"))
    }

    fn terminal_diagnostic(pipeline: &gst::Pipeline) -> String {
        let Some(bus) = pipeline.bus() else {
            return String::from("no bus");
        };
        let Some(message) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Error, gst::MessageType::Application],
        ) else {
            return String::from("no terminal bus message");
        };
        format!(
            "terminal classified={:?} native={}",
            classify_pipeline_message(&message, pipeline),
            native_error_summary(&message)
        )
    }

    async fn wait_for_snapshot(
        receiver: &mut watch::Receiver<Arc<ApplicationSnapshot>>,
        predicate: impl Fn(&ApplicationSnapshot) -> bool,
    ) -> Arc<ApplicationSnapshot> {
        tokio::time::timeout(SNAPSHOT_WAIT, async {
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

    /// One channel retained in memory only; the real name is never printed
    /// or written by this harness.
    #[allow(dead_code)]
    struct LiveChannel {
        key: ChannelKey,
        name: String,
    }

    /// One discovered, fully resolved live device. Addresses, friendly names,
    /// and real channel names stay in memory only.
    struct LiveDevice {
        device_id: DeviceId,
        model_number: Option<String>,
        #[allow(dead_code)]
        friendly_name: Option<String>,
        tuner_count: Option<u8>,
        source: SocketAddr,
        non_drm_channels: Vec<LiveChannel>,
        snapshot: DeviceSnapshot,
    }

    /// Discover every local HDHomeRun device, resolve each through a fresh
    /// registry and the real snapshot resolver, and print one sanitized
    /// summary line per device.
    async fn discover_live_devices() -> Vec<LiveDevice> {
        let cancellation = CancellationToken::new();
        let report = DiscoveryClient::default()
            .discover_local(&cancellation)
            .await
            .expect("local discovery must reach the local network");
        let mut devices = Vec::new();
        let mut seen = BTreeSet::new();
        for observation in report.observations {
            if !seen.insert(observation.device_id) {
                continue;
            }
            let mut registry = DeviceRegistry::default();
            registry
                .observe(
                    observation.clone(),
                    RegistryInstant::from_duration(Duration::ZERO),
                )
                .expect("the live observation must register");
            let registered = registry
                .get(observation.device_id)
                .expect("the registry retains the observed device");
            let target = DeviceSnapshotTarget::from_registered(registered)
                .expect("the registered device has a resolvable locator");
            let resolved = tokio::time::timeout(
                SNAPSHOT_WAIT,
                DeviceSnapshotResolver::default().resolve(&target, &cancellation),
            )
            .await
            .expect("live snapshot resolution must remain bounded")
            .expect("resolve the complete live device snapshot");
            let snapshot = resolved.into_snapshot();
            let info = snapshot.info();
            let channels = snapshot.lineup().channels();
            let drm = channels.iter().filter(|channel| channel.is_drm()).count();
            let device = LiveDevice {
                device_id: observation.device_id,
                model_number: info.model_number().map(str::to_owned),
                friendly_name: info.friendly_name().map(str::to_owned),
                tuner_count: info.tuner_count(),
                source: observation.source,
                non_drm_channels: channels
                    .iter()
                    .filter(|channel| !channel.is_drm())
                    .map(|channel| LiveChannel {
                        key: channel.key().clone(),
                        name: channel.name().to_owned(),
                    })
                    .collect(),
                snapshot,
            };
            eprintln!(
                "live device: model={} tuners={} channels={} drm={} non-drm={}",
                device.model_number.as_deref().unwrap_or("unknown"),
                device
                    .tuner_count
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "unknown".to_owned()),
                device.snapshot.lineup().channels().len(),
                drm,
                device.non_drm_channels.len()
            );
            devices.push(device);
        }
        assert!(
            !devices.is_empty(),
            "live-hardware evidence requires at least one real HDHomeRun device on the local network"
        );
        devices
    }

    fn pick_model_prefixed<'a>(
        devices: &'a [LiveDevice],
        prefix: &str,
        missing: &str,
    ) -> &'a LiveDevice {
        devices
            .iter()
            .find(|device| {
                device
                    .model_number
                    .as_deref()
                    .is_some_and(|model| model.starts_with(prefix))
            })
            .unwrap_or_else(|| panic!("{missing}"))
    }

    async fn is_guide_number_responsive(ip: std::net::IpAddr, guide_number: &str) -> bool {
        let Ok(client) = reqwest::Client::builder()
            .timeout(Duration::from_millis(1500))
            .no_proxy()
            .build()
        else {
            return false;
        };
        let stream_url = format!("http://{ip}:5004/auto/v{guide_number}");
        match client.get(&stream_url).send().await {
            Ok(mut res) if res.status().is_success() => {
                match tokio::time::timeout(Duration::from_millis(1500), res.chunk()).await {
                    Ok(Ok(Some(chunk))) => {
                        let ok = !chunk.is_empty();
                        drop(res);
                        drop(client);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        ok
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

    async fn first_responsive_non_drm_channel(
        snapshot: &ApplicationSnapshot,
        ip: std::net::IpAddr,
    ) -> ChannelKey {
        if let Some(channel) = std::env::var("BALUN_LIVE_ATSC1_CHANNEL")
            .ok()
            .map(|guide| guide.trim().to_string())
            .filter(|guide| !guide.is_empty())
            .and_then(|guide| {
                snapshot
                    .selected_lineup()
                    .channels()
                    .iter()
                    .find(|channel| {
                        channel.key().guide_number().as_str() == guide && !channel.is_drm()
                    })
                    .map(|channel| channel.key().clone())
            })
        {
            return channel;
        }
        let channels = snapshot.selected_lineup().channels();
        for channel in channels.iter().filter(|c| !c.is_drm() && c.is_favorite()) {
            if is_guide_number_responsive(ip, channel.key().guide_number().as_str()).await {
                eprintln!(
                    "live ATSC 1.0: selected responsive favorite channel {}",
                    channel.key().guide_number()
                );
                return channel.key().clone();
            }
        }
        for channel in channels.iter().filter(|c| !c.is_drm() && !c.is_favorite()) {
            if is_guide_number_responsive(ip, channel.key().guide_number().as_str()).await {
                eprintln!(
                    "live ATSC 1.0: selected responsive non-favorite channel {}",
                    channel.key().guide_number()
                );
                return channel.key().clone();
            }
        }
        channels
            .iter()
            .find(|channel| !channel.is_drm() && channel.is_favorite())
            .or_else(|| channels.iter().find(|channel| !channel.is_drm()))
            .expect("the live device must expose at least one non-DRM channel")
            .key()
            .clone()
    }

    struct SelectedLive {
        runtime: ControllerRuntime,
        handle: ControllerHandle,
        ready: Arc<ApplicationSnapshot>,
        device_ip: std::net::IpAddr,
    }

    /// Start the real production controller services, refresh local
    /// discovery, and select one device through to a ready lineup.
    async fn controller_select_device(
        device: &LiveDevice,
        expected_device_count: usize,
    ) -> SelectedLive {
        let runtime =
            ControllerRuntime::start_default().expect("start the production controller services");
        let handle = runtime.handle();
        let mut snapshots = handle.subscribe();
        handle
            .try_send(ControllerCommand::RefreshLocalDiscovery)
            .expect("admit the local refresh command");
        wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.devices().len() == expected_device_count
                && snapshot.discovery().status() == DiscoveryStatus::Ready
        })
        .await;
        handle
            .try_send(ControllerCommand::SelectDevice(device.device_id))
            .expect("admit the select-device command");
        let ready = wait_for_snapshot(&mut snapshots, |snapshot| {
            snapshot.selected_lineup().status() == SelectedLineupStatus::Ready
        })
        .await;
        SelectedLive {
            runtime,
            handle,
            ready,
            device_ip: device.source.ip(),
        }
    }

    /// Request and receive the controller-authorized live stream handoff,
    /// returning it with the request-to-receive duration.
    async fn request_live_handoff(
        handle: &ControllerHandle,
        key: ChannelKey,
        generation: OperationGeneration,
    ) -> (StreamHandoff, Duration) {
        eprintln!("live tune: requesting the controller stream handoff");
        let started = Instant::now();
        let receiver = handle
            .try_request_stream(StreamSelection::new(key, generation))
            .expect("admit the live stream request");
        let handoff = receiver
            .receive()
            .await
            .expect("the controller authorizes the live stream");
        let elapsed = started.elapsed();
        eprintln!("live tune: controller handoff received in {elapsed:?}");
        (handoff, elapsed)
    }

    /// Retire the policy, settle the pipeline to `NULL`, and join the
    /// transport within the teardown bound; returns the total elapsed time.
    fn retire_live_pipeline(pipeline: &gst::Pipeline, policy: &SourcePolicy) -> Duration {
        let started = Instant::now();
        let mut transport = policy
            .retire()
            .expect("the played live transport is returned once");
        pipeline
            .set_state(gst::State::Null)
            .expect("request teardown");
        let (transition, current, _) = pipeline.state(gst::ClockTime::from_seconds(5));
        assert!(transition.is_ok());
        assert_eq!(current, gst::State::Null);
        assert_eq!(
            transport.join(Instant::now() + TEARDOWN_BOUND),
            Ok(()),
            "the client-side tuner release must join within the bound"
        );
        started.elapsed()
    }

    /// Write one sanitized JSON capture per device under the target tmp
    /// directory: model, firmware, tuner count, counts, and a channel list
    /// whose names are synthesized, never the real ones. File names carry
    /// the device's position and model only, never any DeviceID digits.
    fn write_metadata_captures(devices: &[LiveDevice]) -> PathBuf {
        let directory = std::env::var_os("CARGO_TARGET_TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("target")
                    .join("tmp")
            })
            .join("live-hardware");
        std::fs::create_dir_all(&directory).expect("create the live capture directory");
        for (index, device) in devices.iter().enumerate() {
            let info = device.snapshot.info();
            let channels = device.snapshot.lineup().channels();
            let firmware = match (info.firmware_name(), info.firmware_version()) {
                (Some(name), Some(version)) => Some(format!("{name} {version}")),
                (Some(name), None) => Some(name.to_owned()),
                (None, Some(version)) => Some(version.to_owned()),
                (None, None) => None,
            };
            let capture = serde_json::json!({
                "model": info.model_number(),
                "firmware": firmware,
                "tuner_count": info.tuner_count(),
                "total_channels": channels.len(),
                "drm_channels": channels.iter().filter(|channel| channel.is_drm()).count(),
                "favorite_count": channels.iter().filter(|channel| channel.is_favorite()).count(),
                "hd_count": channels.iter().filter(|channel| channel.is_hd()).count(),
                "channels": channels
                    .iter()
                    .enumerate()
                    .map(|(index, channel)| {
                        serde_json::json!({
                            "guide_number": channel.key().guide_number().as_str(),
                            "guide_name": format!("Channel {}", index + 1),
                            "drm": channel.is_drm(),
                            "favorite": channel.is_favorite(),
                            "hd": channel.is_hd(),
                        })
                    })
                    .collect::<Vec<_>>(),
            });
            let model = device
                .model_number
                .as_deref()
                .unwrap_or("unknown")
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let path = directory.join(format!("device-{}-{model}.json", index + 1));
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&capture).expect("serialize the sanitized capture"),
            )
            .expect("write the sanitized capture");
            eprintln!("live metadata capture: {}", path.display());
        }
        directory
    }

    /// P0 live-TV machine proof against the two-tuner ATSC 1.0 unit: the
    /// real controller must authorize a non-DRM channel, `playbin3` must
    /// decode both video and audio from the real tuner's live stream, and
    /// teardown must release the tuner inside the client-side bound.
    #[tokio::test]
    #[ignore = "requires BALUN_LIVE_HARDWARE=1 and real HDHomeRun tuners on the local network"]
    async fn live_atsc1_channel_plays_with_decoded_audio_and_releases_the_tuner() {
        if std::env::var("BALUN_LIVE_HARDWARE").as_deref() != Ok("1") {
            eprintln!("skipping: set BALUN_LIVE_HARDWARE=1 to allow real-tuner evidence capture");
            return;
        }
        let Some((playbin, video_sink, audio_sink)) = live_headless_pipeline() else {
            return;
        };
        let _ranks = prefer_software_mpeg2_decoders();

        let devices = discover_live_devices().await;
        let atsc1 = pick_model_prefixed(
            &devices,
            "HDHR4",
            "live evidence requires the HDHR4 ATSC 1.0 tuner on the local network",
        );
        let selected = controller_select_device(atsc1, devices.len()).await;
        let generation = selected.ready.selected_lineup().generation();
        let key = first_responsive_non_drm_channel(&selected.ready, selected.device_ip).await;
        let (handoff, handoff_elapsed) =
            request_live_handoff(&selected.handle, key, generation).await;

        let policy = SourcePolicy::install(&playbin, handoff, QUICK)
            .expect("install the appsrc policy for the live ATSC 1.0 stream");
        playbin.set_property("uri", PIPELINE_URI);
        let started = Instant::now();
        playbin
            .set_state(gst::State::Playing)
            .expect("request live ATSC 1.0 playback");

        let deadline = started + FIRST_FRAME_BOUND;
        let mut first_frame = None;
        let mut reached_playing = false;
        loop {
            if first_frame.is_none() && rendered_count(&video_sink) > 0 {
                first_frame = Some(started.elapsed());
            }
            reached_playing |= playbin.current_state() == gst::State::Playing;
            if reached_playing
                && rendered_count(&video_sink) >= 5
                && rendered_count(&audio_sink) >= 3
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the live ATSC 1.0 tune must decode inside the bound: {}",
                terminal_diagnostic(&playbin)
            );
            thread::sleep(POLL_INTERVAL);
        }

        let video_caps = sink_caps(&video_sink).unwrap_or_else(|| "<no caps>".to_string());
        let audio_caps = sink_caps(&audio_sink).unwrap_or_else(|| "<no caps>".to_string());
        eprintln!(
            "live ATSC 1.0 evidence: handoff {handoff_elapsed:?}, first video frame {:?}, stable decode {:?} after PLAYING",
            first_frame.expect("a decoded video frame was observed"),
            started.elapsed()
        );
        eprintln!("live ATSC 1.0 evidence: video caps {video_caps}");
        eprintln!(
            "live ATSC 1.0 evidence: audio caps {audio_caps} (rendered by {})",
            live_audio_sink_factory()
        );
        eprintln!(
            "live ATSC 1.0 evidence: factories {:?}",
            collect_pipeline_factories(&playbin)
        );

        let release = retire_live_pipeline(&playbin, &policy);
        eprintln!("live ATSC 1.0 evidence: tuner release in {release:?}");

        assert!(
            video_caps.contains("video/x-raw"),
            "the live video path must be decoded, not passthrough: {video_caps}"
        );
        assert!(
            audio_caps.contains("audio/x-raw"),
            "the live audio path must be decoded, not passthrough: {audio_caps}"
        );
        selected
            .runtime
            .shutdown()
            .expect("join the controller cleanly after the live ATSC 1.0 tune");
    }

    /// P0 switch and release budgets on the ATSC 1.0 unit: tearing down an
    /// open-ended live stream must join within the 5-second class, and the
    /// successor channel must decode its first frame within the first-frame
    /// bound after a synchronous switch.
    #[tokio::test]
    #[ignore = "requires BALUN_LIVE_HARDWARE=1 and real HDHomeRun tuners on the local network"]
    async fn live_channel_switch_and_release_budgets() {
        if std::env::var("BALUN_LIVE_HARDWARE").as_deref() != Ok("1") {
            eprintln!("skipping: set BALUN_LIVE_HARDWARE=1 to allow real-tuner evidence capture");
            return;
        }
        let Some((pipeline_a, video_sink_a, _audio_sink_a)) = live_headless_pipeline() else {
            return;
        };
        let Some((pipeline_b, video_sink_b, _audio_sink_b)) = live_headless_pipeline() else {
            return;
        };
        let _ranks = prefer_software_mpeg2_decoders();

        let devices = discover_live_devices().await;
        let atsc1 = pick_model_prefixed(
            &devices,
            "HDHR4",
            "live evidence requires the HDHR4 ATSC 1.0 tuner on the local network",
        );
        let selected = controller_select_device(atsc1, devices.len()).await;
        let generation = selected.ready.selected_lineup().generation();
        let mut non_drm = Vec::new();
        if let Some(c) = std::env::var("BALUN_LIVE_ATSC1_CHANNEL")
            .ok()
            .and_then(|guide_a| {
                selected
                    .ready
                    .selected_lineup()
                    .channels()
                    .iter()
                    .find(|c| c.key().guide_number().as_str() == guide_a.trim() && !c.is_drm())
                    .map(|c| c.key().clone())
            })
        {
            non_drm.push(c);
        }
        if let Some(c) = std::env::var("BALUN_LIVE_ATSC1_CHANNEL_B")
            .ok()
            .and_then(|guide_b| {
                selected
                    .ready
                    .selected_lineup()
                    .channels()
                    .iter()
                    .find(|c| c.key().guide_number().as_str() == guide_b.trim() && !c.is_drm())
                    .map(|c| c.key().clone())
            })
        {
            non_drm.push(c);
        }
        if non_drm.is_empty() {
            non_drm
                .push(first_responsive_non_drm_channel(&selected.ready, selected.device_ip).await);
        }
        if non_drm.len() < 2 {
            let channels = selected.ready.selected_lineup().channels();
            let mut found_b = None;
            let ip = selected.device_ip;
            for channel in channels
                .iter()
                .filter(|c| !c.is_drm() && c.is_favorite() && c.key() != &non_drm[0])
            {
                if is_guide_number_responsive(ip, channel.key().guide_number().as_str()).await {
                    found_b = Some(channel.key().clone());
                    break;
                }
            }
            if found_b.is_none() {
                for channel in channels
                    .iter()
                    .filter(|c| !c.is_drm() && !c.is_favorite() && c.key() != &non_drm[0])
                {
                    if is_guide_number_responsive(ip, channel.key().guide_number().as_str()).await {
                        found_b = Some(channel.key().clone());
                        break;
                    }
                }
            }
            if let Some(key_b) = found_b {
                non_drm.push(key_b);
            } else {
                non_drm.push(non_drm[0].clone());
            }
        }
        assert!(
            !non_drm.is_empty(),
            "the live ATSC 1.0 device must expose at least one non-DRM channel"
        );
        let channel_a = non_drm[0].clone();
        let channel_b = non_drm
            .get(1)
            .cloned()
            .unwrap_or_else(|| non_drm[0].clone());
        eprintln!(
            "live switch budget: channel A is {}, channel B is {}",
            channel_a.guide_number(),
            channel_b.guide_number()
        );

        let handoff_a_started = Instant::now();
        let (handoff_a, handoff_a_elapsed) =
            request_live_handoff(&selected.handle, channel_a, generation).await;
        let policy_a = SourcePolicy::install(&pipeline_a, handoff_a, QUICK)
            .expect("install the appsrc policy for the first live channel");
        pipeline_a.set_property("uri", PIPELINE_URI);
        let playing_a = Instant::now();
        pipeline_a
            .set_state(gst::State::Playing)
            .expect("request the first live channel");
        let first_frame_a = wait_for_first_video_frame(&video_sink_a, playing_a, FIRST_FRAME_BOUND);
        let total_a = handoff_a_started.elapsed();
        eprintln!(
            "live switch budget: handoff A {handoff_a_elapsed:?}, channel A first frame {first_frame_a:?} after PLAYING, {total_a:?} from handoff request"
        );

        let switch_started = Instant::now();
        let release_a = retire_live_pipeline(&pipeline_a, &policy_a);
        eprintln!("live switch budget: channel A release in {release_a:?}");

        let (handoff_b, handoff_b_elapsed) =
            request_live_handoff(&selected.handle, channel_b, generation).await;
        let policy_b = SourcePolicy::install(&pipeline_b, handoff_b, QUICK)
            .expect("install the appsrc policy for the second live channel");
        pipeline_b.set_property("uri", PIPELINE_URI);
        let playing_b = Instant::now();
        pipeline_b
            .set_state(gst::State::Playing)
            .expect("request the second live channel");
        let first_frame_b = wait_for_first_video_frame(&video_sink_b, playing_b, FIRST_FRAME_BOUND);
        let switch_total = switch_started.elapsed();
        eprintln!(
            "live switch budget: handoff B {handoff_b_elapsed:?}, channel B first frame {first_frame_b:?} after PLAYING, switch total {switch_total:?}"
        );
        assert!(
            switch_total <= FIRST_FRAME_BOUND + TEARDOWN_BOUND,
            "the complete switch must stay inside its class bounds"
        );

        let release_b = retire_live_pipeline(&pipeline_b, &policy_b);
        eprintln!("live switch budget: channel B release in {release_b:?}");
        selected
            .runtime
            .shutdown()
            .expect("join the controller cleanly after the live switch budgets");
    }

    /// P0 metadata capture plus the modern-codec observation: sanitized JSON
    /// captures for every discovered device, then the unprotected ATSC 3.0
    /// channel named by `BALUN_LIVE_MODERN_CHANNEL` on the 4K unit must
    /// either decode video and audio or fail closed with an honestly
    /// classified terminal; neither outcome fails, only a timeout does. The
    /// lane stops after the captures when no channel is named, because the
    /// 4K unit's lineup starts with ATSC 1.0 channels that observe nothing
    /// modern.
    #[tokio::test]
    #[ignore = "requires BALUN_LIVE_HARDWARE=1 and real HDHomeRun tuners on the local network"]
    async fn live_metadata_capture_and_modern_codec_observation() {
        if std::env::var("BALUN_LIVE_HARDWARE").as_deref() != Ok("1") {
            eprintln!("skipping: set BALUN_LIVE_HARDWARE=1 to allow real-tuner evidence capture");
            return;
        }
        let devices = discover_live_devices().await;
        let directory = write_metadata_captures(&devices);
        eprintln!(
            "live metadata captures written under {}",
            directory.display()
        );

        let Some(guide_number) = std::env::var("BALUN_LIVE_MODERN_CHANNEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            eprintln!(
                "modern-codec lane: set BALUN_LIVE_MODERN_CHANNEL=<guide number> to an unprotected ATSC 3.0 channel on the 4K unit; captures written, lane skipped"
            );
            return;
        };
        let Some((playbin, video_sink, audio_sink)) = live_headless_pipeline() else {
            return;
        };
        let _ranks = prefer_software_mpeg2_decoders();
        let modern = pick_model_prefixed(
            &devices,
            "HDHR5",
            "the modern-codec lane requires the HDHR5 4K tuner on the local network",
        );
        let selected = controller_select_device(modern, devices.len()).await;
        let generation = selected.ready.selected_lineup().generation();
        let channel = selected
            .ready
            .selected_lineup()
            .channels()
            .iter()
            .find(|channel| channel.key().guide_number().as_str() == guide_number.trim())
            .expect("BALUN_LIVE_MODERN_CHANNEL must name a channel in the 4K unit's lineup");
        assert!(
            !channel.is_drm(),
            "BALUN_LIVE_MODERN_CHANNEL must name an unprotected channel"
        );
        let key = channel.key().clone();
        let (handoff, handoff_elapsed) =
            request_live_handoff(&selected.handle, key, generation).await;

        let policy = SourcePolicy::install(&playbin, handoff, QUICK)
            .expect("install the appsrc policy for the modern-codec lane");
        playbin.set_property("uri", PIPELINE_URI);
        let started = Instant::now();
        playbin
            .set_state(gst::State::Playing)
            .expect("request modern-codec playback");

        let bus = playbin.bus().expect("playbin3 provides a bus");
        let deadline = started + FIRST_FRAME_BOUND;
        let mut decoded = false;
        let mut terminal = false;
        while !decoded && !terminal {
            if rendered_count(&video_sink) >= 3 && rendered_count(&audio_sink) >= 1 {
                decoded = true;
            } else if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
                match message.view() {
                    gst::MessageView::Eos(_) => {
                        eprintln!("modern-codec lane: live stream posted EOS");
                        terminal = true;
                    }
                    gst::MessageView::Element(element)
                        if element
                            .structure()
                            .is_some_and(|structure| structure.name() == "missing-plugin") =>
                    {
                        let media_type = missing_plugin_media_type(&message);
                        eprintln!("modern-codec lane: missing plugin for {media_type}");
                        terminal = true;
                    }
                    _ => {
                        if let Some(failure) = classify_pipeline_message(&message, &playbin) {
                            eprintln!(
                                "modern-codec lane: fail-closed terminal classified={failure} native={}",
                                native_error_summary(&message)
                            );
                            terminal = true;
                        }
                    }
                }
            }
            assert!(
                Instant::now() < deadline,
                "the modern-codec lane must decode or fail closed within the bound"
            );
        }

        if decoded {
            eprintln!(
                "modern-codec lane: decoded live video and audio (handoff {handoff_elapsed:?}, decode {:?} after PLAYING)",
                started.elapsed()
            );
            eprintln!(
                "modern-codec lane: video caps {}",
                sink_caps(&video_sink).unwrap_or_else(|| "<no caps>".to_string())
            );
            eprintln!(
                "modern-codec lane: audio caps {}",
                sink_caps(&audio_sink).unwrap_or_else(|| "<no caps>".to_string())
            );
            eprintln!(
                "modern-codec lane: factories {:?}",
                collect_pipeline_factories(&playbin)
            );
        } else {
            eprintln!(
                "modern-codec lane: verified fail-closed observation (terminal reached in {:?})",
                started.elapsed()
            );
        }

        let release = retire_live_pipeline(&playbin, &policy);
        eprintln!("modern-codec lane: tuner release in {release:?}");
        selected
            .runtime
            .shutdown()
            .expect("join the controller cleanly after the modern-codec lane");
    }

    /// P0.7 exact-address probe: for every discovered device, a targeted
    /// discovery at its exact source address with the expected DeviceID must
    /// return exactly one observation matching that identity.
    #[tokio::test]
    #[ignore = "requires BALUN_LIVE_HARDWARE=1 and real HDHomeRun tuners on the local network"]
    async fn live_exact_address_probe_matches_every_discovered_device() {
        if std::env::var("BALUN_LIVE_HARDWARE").as_deref() != Ok("1") {
            eprintln!("skipping: set BALUN_LIVE_HARDWARE=1 to allow real-tuner evidence capture");
            return;
        }
        let devices = discover_live_devices().await;
        let cancellation = CancellationToken::new();
        for device in &devices {
            let report = DiscoveryClient::default()
                .discover_target(device.source, Some(device.device_id), &cancellation)
                .await
                .expect("the exact-address probe must reach the discovered device");
            assert_eq!(
                report.observations.len(),
                1,
                "the exact probe must retain exactly its targeted device"
            );
            assert_eq!(report.observations[0].device_id, device.device_id);
            eprintln!(
                "exact probe ok for {}",
                device.model_number.as_deref().unwrap_or("unknown")
            );
        }
    }
}
