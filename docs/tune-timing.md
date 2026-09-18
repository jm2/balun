# Tune startup measurements

V2.1 / [#63](https://github.com/jm2/balun/issues/63), September 17, 2026. The
desktop session emits bounded startup observations for each admitted tune
generation. This is the first measurement slice; V2.1 remains open for transport,
decoded/rendered media, usable-media deadlines, and packaged hardware budgets.

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
seven phase records and one terminal record per admitted generation, with no
timer, history collection, or per-buffer logging. A stalled native call can
prevent later records, including the terminal one, from being emitted.

## What the phases measure

| Phase | Observation on the session's owning main context |
| --- | --- |
| `requested` | The session has admitted a new generation, before retiring its predecessor. The offset is zero; UI event queuing before this point is excluded. |
| `predecessor_retired` | The previous active pipeline settled at `NULL` and its owned transport joined, or there was no active predecessor. A failed retirement emits no such phase. |
| `handoff_accepted` | The current controller response passed channel and selection-generation checks. Stale responses add no observations. |
| `graph_prepared` | The video sink, playbin, bus watch, constant URI, and private source policy were constructed/configured, before requesting `PAUSED`. This is not decoder readiness. |
| `paused_request_returned` | The synchronous native `set_state(PAUSED)` call returned. Its subsequent result may still fail; this phase does not assert successful state settlement. |
| `stream_notice_received` | The session reduced the transport's first accepted-appsrc-buffer notification. This includes bus dispatch and main-context scheduling delay; it is not the HTTP first-byte timestamp. |
| `playing_notice_received` | The session reduced the owned pipeline's `PLAYING` state notification. It does not establish decoded, displayed, or audible media. |

All offsets are cumulative from `requested`, not phase durations. Subtract two
offsets from the same generation to compare observed intervals. For example,
`predecessor_retired` includes the predecessor's client-side retirement work,
and `graph_prepared - handoff_accepted` includes structural graph construction.
Neither reports device-side tuner release or lock. A first tune with no
predecessor is not a release measurement. Logging and main-context scheduling
contribute to these intervals; compare runs with the same logging configuration.

The first `playing_notice_received` ends this startup recorder with outcome
`playing_notice`. Other closed outcomes are `failed`, `cancelled`, `superseded`,
`stopped`, `shut_down`, and `dropped`. `dropped` records destruction before its
fail-safe native teardown; it does not claim successful release. A failed tune
can end after native cleanup returns, so its final elapsed value can include
cleanup. Playback errors after the startup recorder ended retain the existing
session diagnostics without reopening that recorder.

## Evidence and remaining acceptance

Deterministic recorder tests inject monotonic instants and check exact offsets,
once-only phase emission, closed terminal labels, and the eight-record bound.
Session tests capture the real tracing output while forcing replacement, stale
responses and events, repeated notifications, cancellation, clean/partial start
failure, shutdown, Drop, and failed predecessor retirement. They verify timing
output contains no synthetic endpoint/channel/device values. Existing lifecycle
tests continue to establish joined release and failed-teardown quarantine.

```bash
cargo test --locked --features desktop --lib playback::session::
```

The mock backend deliberately emits no native graph phases. Native CI and the
existing display-backed session probes cover construction and playback on their
supported hosts; local mock results are not measurements of a native decoder,
sink, device, or packaged artifact.

Still required for V2.1: HTTP request/header/first-byte observations at their
actual boundaries, decoded and presented video/audio progress with explicit
semantics, unusable/trickle-stream fixtures, missing-progress behavior, and
fresh packaged first-frame/switch/release measurements on the supported matrix.
`balun-stream-started` only reports an accepted appsrc byte buffer; it does not
prove a program map table, a keyframe, or useful media. No new deadline or
pipeline optimization is established by these records. The accepted
[in-process native-call limits](native-media-failure-boundary.md) still apply.
