# Tune startup measurements

V2.1 / [#63](https://github.com/jm2/balun/issues/63), September 19, 2026. The
desktop session emits bounded startup observations for each admitted tune
generation, including timestamps captured by the private transport workers and
raw-media sink probes. V2.1 remains open for decoder-output/presentation timing,
continuing media progress, usable-media deadlines, and packaged hardware budgets.

## Collecting observations

Enable the dedicated debug target in a development console:

```bash
RUST_LOG=off,balun::playback::timing=debug cargo run --locked --features desktop --bin balun
```

Use the Windows helper's `-Run` console build to collect the same target there.
The distributed Windows application does not attach a console. No file, network
telemetry, or new diagnostics UI is added by the timing recorder.

Each record contains a process-local `generation`, a fixed `phase` or `outcome`,
and `elapsed_us` from a monotonic clock. The recorder accepts no endpoint,
channel, device, plugin text, caps, or stream identifier. The dedicated filter
above excludes other Balun log targets; broader filters retain their existing
logging behavior. GStreamer's separate `GST_DEBUG` facility is not filtered by
this recorder and can contain stream-derived text.

The session retains at most one fixed-size startup recorder. A phase appears
at most once; one terminal outcome consumes the recorder. There are at most
fifteen phase records and one terminal record per admitted generation, with no
timer, history collection, or per-buffer logging. The two transport workers share
four first-observation atomic slots with the recorder. They do not log from those
slots or post timing data to GStreamer. The session emits captured offsets at its
next phase or terminal reduction. A stalled native call can prevent later
records, including captured worker offsets and the terminal one, from being emitted.

Media hooks share four additional first-observation slots. Their callbacks post
at most four fixed, field-free wakeups to the owned pipeline's bus; the session
checks the generation before emitting the captured offsets. Hooks retain no
buffer, caps, device name, or strong pipeline reference. There are at most nine
sink probes total, including the explicit video sink's probe when available,
plus one element-added and one paintable callback. After a sink's first observation,
its probe only checks atomic flags. Hooks detach before native teardown. The
fixed `media_observer_incomplete` phase records a missing sink pad, exhausted
probe budget, or failed probe installation; playback behavior is unchanged.

## What the phases measure

| Phase | Observation point |
| --- | --- |
| `requested` | The session has admitted a new generation, before retiring its predecessor. The offset is zero; UI event queuing before this point is excluded. |
| `predecessor_retired` | The previous active pipeline settled at `NULL` and its owned transport joined, or there was no active predecessor. A failed retirement emits no such phase. |
| `handoff_accepted` | The current controller response passed channel and selection-generation checks. Stale responses add no observations. |
| `graph_prepared` | The video sink, playbin, bus watch, constant URI, and private source policy were constructed/configured, before requesting `PAUSED`. This is not decoder readiness. |
| `paused_request_returned` | The synchronous native `set_state(PAUSED)` call returned. Its subsequent result may still fail; this phase does not assert successful state settlement. |
| `http_request_polled` | The private reader first polls the request future after the biased cancellation check. This precedes connection/request work; it does not prove a packet was sent or accepted by the device. |
| `http_response_received` | The private reader receives the HTTP response headers, before interpreting status. Rejected responses can have this phase without any body phase. |
| `http_body_received` | The reader receives its first nonempty body chunk from reqwest for an accepted HTTP 200 response. This is the application's body observation, not a kernel TCP-arrival timestamp or useful-media proof. |
| `appsrc_buffer_accepted` | The feeder's first appsrc push returns `FlowReturn::Ok`. This establishes acceptance into the source queue, not decoding or rendering. |
| `stream_notice_received` | The session reduced the transport's first accepted-appsrc-buffer notification. This includes bus dispatch and main-context scheduling delay; it is not the HTTP first-byte timestamp. |
| `playing_notice_received` | The session reduced the owned pipeline's `PLAYING` state notification. It does not establish decoded, displayed, or audible media. |
| `video_sink_buffer` | The explicit GTK video sink's input pad sees its first nonempty buffer with negotiated `video/x-raw` caps, excluding GAP, CORRUPTED, and DECODE_ONLY buffers. This is raw-buffer ingress before sink processing, not decoder-output time or successful display. |
| `audio_sink_buffer` | An owned concrete audio sink's input pad sees its first such `audio/x-raw` buffer. Automatic-output bins and generic fallback fakesinks are excluded. Mute/volume do not suppress this observation. This precedes sink processing, clock scheduling, and device output; it is not proof of audible sound. |
| `video_paintable_invalidated` | The owned paintable first emits `invalidate-contents` with positive intrinsic dimensions after raw video ingress was observed. This means contents were invalidated, not that a window painted, a compositor presented, or a user saw the frame. |
| `media_observer_incomplete` | Some intended sink observation could not be installed within the fixed hook contract or budget. Missing phases must not be interpreted as proof that no media existed. |

All offsets are cumulative from `requested`, not phase durations. Subtract two
offsets from the same generation to compare observed intervals. For example,
`predecessor_retired` includes the predecessor's client-side retirement work,
and `graph_prepared - handoff_accepted` includes structural graph construction.
Neither reports device-side tuner release or lock. A first tune with no
predecessor is not a release measurement. Logging and main-context scheduling
contribute to these intervals; compare runs with the same logging configuration.
Worker offsets use the same monotonic origin and retain their capture time when
the main context emits them later. Output line order is therefore not necessarily
timestamp order. A terminal reduction samples the slots once; observations that
race after that sample are omitted, never relabeled as a successor generation.
Successful transport startup captures all four phases before its stream notice.

The recorder remains alive after `playing_notice_received` so late first media
can be observed, including on audio-only streams. It ends with one of `failed`,
`cancelled`, `superseded`, `stopped`, `shut_down`, or `dropped`. The terminal
elapsed value can therefore include the entire playback session; it is not a
time-to-first-media measurement. `dropped` records destruction before its
fail-safe native teardown; it does not claim successful release. A failed tune
can end after native cleanup returns, so its final elapsed value can include
cleanup. Native callback observations racing retirement can be omitted. Nothing
reopens a finished recorder or relabels its data as the next generation.

Raw ingress is stronger evidence than network bytes, but it does not prove
semantic quality, increasing media timestamps, successful rendering, continued
progress, or useful audiovisual content. Compressed audio passthrough, plugins
without the expected concrete audio-sink class/static pad, and unavailable
outputs may have no audio phase. No missing phase alone establishes a failure.

## Evidence and remaining acceptance

Deterministic recorder tests inject monotonic instants and check exact offsets,
once-only phase emission, closed terminal labels, and the sixteen-record bound.
Session tests capture the real tracing output while forcing replacement, stale
responses and events, repeated notifications, cancellation, clean/partial start
failure, shutdown, Drop, and failed predecessor retirement. They verify timing
output contains no synthetic endpoint/channel/device values. Worker tests retain
original offsets across deferred reduction and discard late observations after
the recorder ended. Real loopback HTTP/appsrc fixtures distinguish complete bodies,
empty HTTP 200 responses, rejected statuses, and already-cancelled requests;
the actual admitted playbin source publishes into the supplied timing object.
Existing lifecycle tests continue to establish joined release and failed-teardown
quarantine.

Real GStreamer pad fixtures cover nonmedia caps, empty/flagged buffers, buffer
lists, duplicate probes, retirement, bounded hook counts, and concurrent wakeups.
They accept harmless synthetic buffers without decoding them or opening an
audio device. The existing display-backed production session fixture also
requires raw video ingress and paintable invalidation before natural EOS; it
does not claim physical screen presentation or audible output.

```bash
cargo test --locked --features desktop --lib playback::session::
```

The mock backend deliberately emits no native graph phases. Native CI and the
existing display-backed session probes cover construction and playback on their
supported hosts; local mock results are not measurements of a native decoder,
sink, device, or packaged artifact.

Still required for V2.1: decoded and presented video/audio progress with explicit
semantics, unusable/trickle-stream media fixtures, missing-progress behavior, and
fresh packaged first-frame/switch/release measurements on the supported matrix.
`balun-stream-started` only reports an accepted appsrc byte buffer; it does not
prove a program map table, a keyframe, or useful media. No new deadline or
pipeline optimization is established by these records. The accepted
[in-process native-call limits](native-media-failure-boundary.md) still apply.
