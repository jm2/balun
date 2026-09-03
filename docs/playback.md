# Playback foundation

Last reviewed: 2026-09-02

This document records Balun's implemented GStreamer boundary, private stream
handoff, generation-owned tune session, video presentation path, essential
controls, and application-owned direct stream transport with its failure
classification. Those landed as archived ledger records M2.4-M2.9 in
[`task-foundation-2026-09.md`](task-foundation-2026-09.md). The product scope
remains authoritative in [`plan-v0.1.md`](plan-v0.1.md), and the active
countable ledger is [`task.md`](task.md).

## Current status

Balun has an optional, GTK-free GStreamer initialization and capability layer.
The desktop owns that layer on the default GLib main context and can report
whether the structural factories needed for the first playback experiment are
present. An initialization failure, an old loadable runtime, or missing
factories disables playback only; device discovery, selected-device lineup
loading, and the two sidebars remain usable. The native core library is still a
dynamic dependency of a desktop build and must be present for that executable
to start.

The desktop-enabled playback library now has one main-context session capable
of consuming an opaque actor handoff and constructing `playbin3` with a
library-owned `gtk4paintablesink`. Standard activation of an unprotected row
now submits only its exact applied selection generation, and an applied private
response opens the HDHomeRun HTTP stream through Balun's own direct transport,
allocates a tuner, feeds GStreamer's built-in `appsrc` behind the constant
`appsrc://balun` URI, and binds the session's URI-opaque paintable into the
production pane. Neither the desktop nor the GStreamer graph ever holds the
device endpoint. The session and native player controls now
retain a normalized process-local volume plus independent mute setting and
apply both to the active pipeline and successor tunes. Audible live-TV output
is verified on Windows against real tuners
([compatibility notes](compatibility-v0.1.md)); Linux and macOS acceptance and
the frozen per-platform codec/audio-sink contract remain P0 work.

Process-isolated Linux tests prove the checked-in MPEG-2 fixture renders into a
real GTK paintable, the real production session streams that fixture from a
loopback HTTP listener through the transport and exact `appsrc` feed to
PLAYING, natural EOS, and joined `NULL` settlement while exposing only the
paintable, and `PlayerView` binds/clears an opaque paintable through its
production widgets and Stop control. The Linux live-device result and budgets
are in [`compatibility-v0.1.md`](compatibility-v0.1.md); macOS live-device
acceptance (P0.3) and packaged-runtime acceptance (P3) remain open. Additional
isolated widget and Wayland smokes cover the audio-control state, exact ListView activation, and a real
compositor-confirmed fullscreen round trip without adding a URI-forging test
surface.

## Feature and version boundary

The default Cargo feature set remains free of GTK and GStreamer. The features
have these roles:

| Feature | Adds | Intended use |
| --- | --- | --- |
| default | Neither GTK nor GStreamer | Core library, tests, and `balun-discover` |
| `playback` | Optional `gstreamer` Rust binding only | GTK-free playback capability code and tests |
| `desktop` | GTK 4, libadwaita, `playback`, and the tune session | Balun desktop application |

The optional dependency is the Rust `gstreamer` 0.25 series with default Cargo
features disabled and its `v1_20` API feature enabled. Balun also checks the
loaded native runtime and rejects versions older than GStreamer 1.20.0. This
1.20 floor covers the implemented foundation; P0.5 remains open until real
cross-platform testing freezes the complete parser, decoder, platform-audio,
and packaging contract.

Useful boundary checks are:

```bash
cargo check --locked
cargo check --locked --features playback
cargo check --locked --features desktop --bin balun
```

Only the latter two commands require a `pkg-config`-visible GStreamer 1.20 or
newer development library. The default library and diagnostic do not acquire
that native build dependency.

## Main-context ownership and errors

`PlaybackRuntime::initialize()` must run while the caller owns the default GLib
main context. The owner is intentionally neither `Send` nor `Sync`, takes one
immutable startup capability snapshot, and is retained by the player pane until
the joined window-close transaction completes. GStreamer process-global state
is not deinitialized when this owner drops; future pipelines must still perform
their own bounded transition to `NULL` before shutdown.

Initialization exposes only fixed, path-free error copy:

- the default main context is not owned;
- GStreamer initialization failed; or
- the loaded numeric runtime version is below the numeric 1.20.0 floor.

Native initialization text, plugin filenames, registry paths, and future media
URLs are not retained in these errors. Missing factories are not initialization
errors: they produce a stable capability list and a playback-unavailable player
state while the controller remains operational.

## Structural factory snapshot

The startup snapshot checks these seven exact registry names in stable order:

| Factory | Foundation role |
| --- | --- |
| `playbin3` | Modern top-level URI player |
| `uridecodebin3` | URI source and decode orchestration used by `playbin3` |
| `decodebin3` | Stream parser/decoder orchestration |
| `appsrc` | Application-fed source filled by Balun's private HTTP transport |
| `tsdemux` | MPEG transport-stream demultiplexing |
| `deinterlace` | Interlaced-video conversion for ordinary 1080i content |
| `gtk4paintablesink` | GTK 4 paintable video output |

These are structural readiness checks, not a promise that a channel is
decodable or audible. They deliberately do not yet name MPEG-2, H.264, HEVC,
AC-3, AAC, E-AC-3, or AC-4 parsers/decoders, nor a Linux, macOS, or Windows audio
sink. The synthetic acceptance test proves one narrow MPEG-2 fixture path
through `playbin3` and `gtk4paintablesink`; P0.5 must still record the
complete tested factory contract. The helpers' `--probe-playback` mode now provides the
development-runtime probe of this snapshot and of the constant-URI `appsrc`
contract on all three platforms and prints the decoder and audio-sink
inventory that P0.5 records per platform; the fake-device probes exist and P3 adds
packaged-runtime probes.

Registry presence also does not prove that a factory can construct, negotiate,
decode, render, reach EOS, or tear down cleanly. Those behaviors require the
process-isolated synthetic and fake-device tests in the remaining milestones.

## Actor-private stream handoff

The stream handoff is a narrow URL-bearing path which is separate from application
snapshots and GStreamer ownership. A `StreamSelection` contains only a complete
ChannelKey and the selected-lineup generation copied from one immutable
snapshot. `ControllerHandle::try_request_stream` admits it into the same
bounded FIFO as selection and discovery commands, preserving their order, and
returns one single-consumer response. Resolving it is synchronous and performs
no HTTP, DNS, discovery, tuner, or media work.

The actor fails closed unless the generation is still current, the selected
lineup is `Ready`, its retained complete snapshot exactly matches the URL-free
projection, and every DeviceID agrees. It finds the exact ChannelKey only in
that private snapshot, refuses protected rows, requires the successful
responder address to remain a locator for the selected device, and revalidates
HTTP, numeric host, port 5004, absent credentials/query/fragment, and the exact
`/auto/v<GuideNumber>` path before constructing a handoff.

`StreamHandoff` is non-cloneable, has no public URI accessor or `Display`, uses
a custom URL-redacted `Debug`, and zeroizes its private URI bytes on drop. The
desktop can hold and move the opaque type but cannot inspect the URI. Its sole
crate-private exposure is a consuming higher-ranked closure used by the
transport when `playbin3` delivers its exact `appsrc`; the borrow cannot escape
that closure, and the parsed URL lives only in that transport's reader thread.
Merely highlighting
a channel row with the keyboard remains inert. Clicking or pressing Enter on a
valid, unprotected row constructs a URL-free `StreamSelection`; only the controller's
validated opaque handoff can then open the stream through `PlaybackSession`.

## Generation-owned tune session

`PlaybackSession` owns the GStreamer runtime and exactly one serialized tune
lane on the default GLib main context. Its stateful public accessors and
mutations fail with fixed URL-free errors when that context is not owned or a
native callback reenters an in-progress borrow; they do not panic. The immutable
capability-ready bit remains directly readable. `begin_tune` first assigns the
successor's `TuneGeneration`, making every predecessor callback stale, then
detaches the predecessor's bus watch and requires it to reach `NULL` within
five seconds before any controller wait. A teardown failure quarantines the
owner and permanently blocks construction of a successor. Terminal shutdown
can retry the retained owner, while the failure remains visible. The returned
`TuneRequest` contains only that generation and the URL-free `StreamSelection`.

Only a response matching the exact pending tune can construct `playbin3`.
Successful responses from superseded attempts are dropped immediately so their
Rust-owned URI storage is zeroized. The URI never enters a native property: the
pipeline receives only the constant `appsrc://balun`, and the handoff moves
into the source policy's private state until the transport consumes it. The
same library constructs and retains `gtk4paintablesink`; it exposes
only the GDK paintable, not a GStreamer element whose parent graph can reach
`playbin3`. Exposed request, state, failure, and session debug output is
URL-free.

Each installed bus watch closes over its tune generation and is attached to
the session's exact default-main-context handle. Playing, buffering, EOS, and
native error messages are deferred out of the native callback and reduced on
that same context only when that generation still owns the active pipeline; a
nested-loop reentry retries there instead of losing a terminal event. Native
error/debug text is ignored. Terminal events, explicit Stop, replacement,
terminal shutdown, and Drop all retire only the exact owner. Tests use an
injected pipeline backend and custom main context to prove late resolution,
late predecessor EOS/error, replacement ordering, clean and quarantined start
failure, current and stale cancellation, synchronous native callbacks,
generation-scoped buffering, teardown poison, explicit Stop, shutdown and
active-owner Drop, handoff mismatch, and generation exhaustion without opening
a network source.

Audio state belongs to that same serialized owner. Its normalized finite range
is `0.0..=1.0`, defaults to full volume and unmuted, and survives Stop,
replacement, EOS, and native-error retirement so every successor inherits the
last accepted values. Mute remains independent, allowing the unmuted level to
change while muted. Before assigning a URI, and on every active update, Balun
validates playbin's readable/writable `volume` and `mute` properties, converts
the UI level to playbin's linear gain with `level³`, and reads both properties
back. Invalid or rejected changes leave the retained state unchanged. Teardown
force-mutes the exact pipeline before its bounded `NULL` wait without changing
the successor preference; teardown poison and terminal shutdown disable the UI
controls. Fake-backend and factory-backed tests prove ownership, inheritance,
failure, and property semantics. The checked-in transport-stream fixture is
video-only, so none of these tests claims decoded or audible audio.

The main-context desktop pane now owns the session and a narrow presentation
boundary which can retrieve only its URI-opaque GDK paintable, bind it into the
`GtkPicture`, and clear it before joined window shutdown. Standard activation
of a non-protected row now carries its exact applied lineup generation into the
bounded controller FIFO, aborts superseded response tasks, and invokes this
binding only after the session applies that generation. Selected-device
changes stop the session immediately after successful command admission; the
published generation repeats that stop before replacing the sidebars as a
fail-safe. The accessible header Stop control disables immediately, aborts the
pending private response, clears the paintable, and invokes the same exact-
generation stop path without yielding. A focusable native slider and toggle
provide volume and mute through weak, main-context signal closures with stable
accessible roles and action labels. The fullscreen button supports pointer and
native Enter/Space activation; unmodified F11 toggles and Escape exits only
while fullscreen. The desktop does not infer success from a request: it updates
icons, labels, top-edge presentation, navigation protection, and focus only from
the window's compositor-confirmed fullscreen property. Both nested split views
are forced to the player and their Back/pop paths are disabled while fullscreen,
then their exact pages, pop permissions, and prior focus are restored. Native
Back choices outside fullscreen remain synchronized, and a valid compact-width
channel activation presents the player even when setup fails. These controls
are complete; the accessibility pass (P1.6) remains open, and the live-device
teardown numbers (P0.4) are in [`compatibility-v0.1.md`](compatibility-v0.1.md).

The session publishes every owned transition through a deduplicated,
URL-free latest-state watch. `PlayerView` consumes it on the GTK main context
through a weak capture and exposes connecting, buffering percentage, playing,
stopped, and failed state in an accessible header status. Native Error and EOS
therefore clear the exact terminal paintable instead of leaving a stale frame.

The reducer never formats, logs, or stores native error text, debug text,
source names, structure fields, extraction errors, headers, bodies, or
endpoint values. Adversarial tests place URI and credential-like secrets in
those ignored fields and prove the public category, session failure, and
failed session-state debug output remain fixed. `PlayerView` maps all seven
categories and controller handoff failures to fixed titles whose descriptions
name the device and channel from the accepted snapshot (ADR-0002) and never a
URL or credential, while a teardown failure retains its stronger
close-Balun warning.

## Application-owned direct transport

[ADR-0001](architecture/adr-0001-discovery-playback.md) selected this path,
and the transport implements it: keep `playbin3`, expose only the fixed
`appsrc://balun` URI to GStreamer, and feed its exact built-in `appsrc` from a
bounded Balun-owned HTTP worker. Production playback therefore never gives the
media framework a device endpoint, and no libsoup/GIO proxy resolver can be
consulted because no GStreamer element ever performs the request.

The source policy validates `playbin3`'s `source-setup` signal schema before
playback starts. Its worker-thread-safe handler accepts only the exact
`appsrc` factory, validates native property types and mutability, and applies
and reads back `video/mpegts, systemstream=true` caps, byte format, stream
type, live and blocking behavior, disabled signal emission and timestamping,
and a 4 MiB queued-byte limit. It then consumes the one authorized handoff and
starts the transport. Any other, repeated, retired, or unconfigurable source is
locked and requested to `NULL`; a single field-free application marker feeds
the generation-owned error and bounded teardown path without native or
endpoint text. The generic GObject property and action-signal interface is
schema-validated and used deliberately, so no `gstreamer-app` native
development dependency is required on any platform.

The transport owns two threads per tune. A reader thread runs a private
current-thread Tokio runtime and a `reqwest` client with `no_proxy`, redirects
and Referer disabled, a fixed Balun user agent, HTTP/1.1 only, no connection
pooling, a five-second connect deadline, a ten-second response-header
deadline, and a ten-second idle-read deadline. The URL must already be a
credential-free, query-free, numeric-host HTTP URL, so the request never
resolves a name. Only the numeric status is interpreted: 200 streams, 503 is
tuner busy, 404 is channel missing, and every other status, including
redirects that are never followed, is HTTP rejection. Connect, header, and
read failures, including a stalled or truncated body, are offline. Body
chunks are split to at most 64 KiB buffers and sent through a bounded
eight-slot channel to a dedicated blocking feeder thread, which pushes them
through `appsrc`'s validated `push-buffer` action signal and emits
`end-of-stream` only after a natural end of body. Neither the GTK main context
nor the controller runtime ever blocks on GStreamer backpressure: when
`appsrc` is full the feeder blocks, the channel fills, and TCP flow control
holds the device.

Failures are posted to the pipeline bus as one application message from the
exact pipeline carrying a single bounded numeric category code. The
generation-scoped bus watch reduces that marker, native missing-plugin,
codec-not-found, and decryption errors, and the source-policy rejection marker
into the seven fixed `PlaybackPipelineFailure` categories; malformed markers,
foreign pipelines, and every other native condition close to internal.

Teardown cancels the request first, so the device connection begins closing
while the pipeline moves to `NULL`; the transport is then joined inside the
same five-second bound, because a flushing `appsrc` is what unblocks a feeder
waiting on the byte limit. `NULL` alone is no longer sufficient proof: a
teardown that cannot join both workers fails, quarantines the owner, and
retains the unjoined transport for the shutdown retry. Cancellation is never
reported as EOS or as a failure. Every bus error, missing-plugin notice,
transport rejection, and source-policy rejection is logged to standard error
with its native detail before it is reduced to a category (`RUST_LOG=balun=debug`
shows the full event trace); the category is all the state retains. A live
`appsrc` feed does not post `playbin3`
buffering messages, so the session's buffering state stays reserved for
runtimes that publish it and the connecting state covers preroll. The
session first holds the pipeline at `PAUSED`, which a live source reaches
without preroll while the transport starts fetching, and requests `PLAYING`
only when the feeder's first accepted push posts a stream-started notice on
the bus. The running clock's base time therefore starts with stream bytes
rather than with the tuner request, so the demuxer's live latency budget
covers decoding instead of tuner lock; without the hold, a slow lock left
every later buffer late and the audio sink clipped it into stutter until the
next tune.

Network-free loopback tests cover accepted configuration, repeated, foreign,
and retired source rejection, handoff zeroization, worker-thread
`source-setup` delivery, every status category, redirect refusal with an
uncontacted target, refused, stalled, and truncated streams, bounded chunk
splitting, bounded queue growth under a paused sink, cancellation while reads
and pushes are blocked, rapid replacement, joined teardown, and `playbin3`
resolving the constant URI to exact `appsrc` and decoding the checked-in
fixture to EOS. A child-process trap proves that ambient `http_proxy` and
`all_proxy` configuration reaches a default client but never the transport.
The macOS CI lane runs that same loopback suite, and the Linux, macOS, and
Windows lanes run the helpers' `--probe-playback`/`-ProbePlayback` mode, which
proves the exact factory snapshot and the constant-URI `appsrc` contract on
each development runtime. That evidence completed the transport record;
packaged-runtime probes are P3 work.

The pipeline-side visual contract is explicit: Balun validates the
`playbin3` flags, aspect-ratio, URI, and video-sink properties while the
pipeline is still `NULL`; enables playbin's adaptive `deinterlace` flag;
forces source aspect-ratio preservation; and installs the private GTK paintable
sink with its own aspect preservation enabled plus the bus watch before copying
the authorized URI into native storage. A factory-backed unit test proves the
URI-free playbin configuration, while the display-backed synthetic acceptance
checks the native paintable property and `GtkPicture::ContentFit::Contain`
without network access. A second display-backed smoke exercises the production
`PlayerView` binding, empty-state transition, clearing, and shutdown boundary.
The activation lane now joins an actor-authorized response to that boundary.
A third display-backed smoke constructs the real production session around the
checked-in fixture, verifies its URI-opaque paintable in a containment-fit
picture, clears presentation, and proves bounded terminal `NULL` shutdown.
Together with the decoded-frame/EOS and production-`PlayerView` smokes, this
completes the presentation contract without exposing a desktop test API capable of forging stream
handoffs.

## Synthetic display-backed acceptance

The synthetic acceptance is one ignored desktop integration test which is compiled by ordinary
all-target test runs and invoked explicitly by
`scripts/test-desktop-lifecycle.sh`. The test uses the checked-in
`tests/fixtures/synthetic-mpeg2.ts`: a deterministic, video-only, 160-by-96,
25 fps MPEG-2 transport stream containing 25 generated test-pattern frames.
The 18,424-byte file is exactly 98 188-byte MPEG-TS packets. Its provenance,
generation command, and SHA-256 digest are committed beside it; it contains no
audio, external media, device data, or network-derived content and is not an
application resource.

Two separate ignored display smokes share the same harness. One drives
the real production session with the checked-in fixture served by a loopback
HTTP listener, verifies its opaque paintable, drives the main context through
PLAYING and natural EOS, and proves joined terminal shutdown. The other
exercises the production `PlayerView` paintable and Stop boundary with a
one-pixel in-memory texture. Neither grants the desktop access to the
pipeline, sink, or stream URI; the loopback URL enters only the library's
crate-private `cfg(test)` handoff constructor.

The controls work extends the layered harness rather than weakening that boundary. A
production `PlayerView` smoke changes volume and mute through the real GTK
widgets and verifies retained session state and terminal disabling. A ListView
smoke proves selection remains inert while click/Enter activation carries
the exact applied generation and protected rows fail closed. A separate fresh
headless Wayland desktop-shell process requests fullscreen through the real
window, waits for compositor entry and exit notifications, proves nested
navigation protection plus focus transfer, and verifies exact restoration.
Pure state and key-filter tests cover responsive layout decisions, F11, Escape,
and rejected modified shortcuts. The video-only fixture and isolated controls
still make no audible-output, native macOS/Windows runtime, or live-device
claim.

Within an isolated Linux headless Wayland compositor and session bus, the test:

- initializes GTK and the existing GStreamer owner on the default main
  context;
- feeds only the fixture's absolute local `file:` URI to explicit `playbin3`;
- attaches explicit `gtk4paintablesink` output to a presented `GtkPicture` and
  uses a non-output fake audio sink;
- requires the window and top-level pipeline to reach their active states,
  multiple decoded/rendered-frame observations and paintable invalidations,
  negotiated 160-by-96 raw-video caps, and EOS; and
- removes the bus watch and proves a bounded transition to `NULL` before
  releasing ownership.

Both an in-process watchdog and an outer process timeout bound native plugin
work. The harness prefers a software-rendered headless Weston session and uses
an isolated Xvfb server only as a local fallback when Weston is unavailable;
X11 is not required by Balun or by the CI acceptance route. The harness removes
inherited display selection plus GStreamer registry, plugin-path, window,
debug, and tracer overrides, and the Rust test maps native failures to fixed
categories rather than exposing plugin paths, debug strings, or the fixture
URI. The test performs no discovery, DNS, HTTP, proxy, tuner, or other network
work.

The helper's `auto` mode selects a Weston installation with `wayland-info` and
headless fake-seat support before considering Xvfb. If that selected compositor
cannot start or pass its bounded protocol probe, `auto` reports the problem and
uses an installed Xvfb fallback. The explicit `wayland` mode instead fails
closed; CI uses that mode and does not install Xvfb. Explicit `x11` exists only
to exercise the fallback on a developer host.

This is a Linux development/CI acceptance record only. It does not prove native
macOS or Windows rendering, audio, physical MPEG-TS variants, live-device
behavior, channel switching, tuner release, or packaged-runtime relocation.
Those claims belong to the P0 evidence records and the P3 packaging records.

## Fake-device end-to-end acceptance

The fake-server path is covered by a complete loopback fake HDHomeRun
device: a UDP discovery responder on the fixed discovery port advertising a
checksum-valid identity and the fake's metadata origin, identity-checked
`discover.json`/`lineup.json` responses on an ephemeral loopback port, and an
MPEG-TS stream server on the fixed device stream port that records per-path
connection open and close instants. Because an unprivileged test process
cannot bind port 80, a test-only exemption accepts exactly the installed
loopback fake metadata port — and nothing else — in place of the production
port-80 policy for the device's lifetime.

A headless test drives the real controller through `RefreshLocalDiscovery`
and `SelectDevice` against this fake, asserts the identity-checked lineup and
honest DRM presentation, proves a protected request is refused before any
tuner contact, and then feeds the real controller-authorized handoff through
the production `appsrc` source policy and transport to natural EOS with
bounded joined `NULL` teardown and an observed device-side connection close.
It also verifies the metadata fetch order and that neither application
snapshots nor the handoff debug output contain endpoint text.

A separate display-backed lifecycle smoke runs the real production
`PlaybackSession` through three full tune lifecycles against the fake:
an open-ended channel reaches PLAYING, switching to a finite channel is
admitted only after the predecessor's transport is joined and the fake
observes its connection close, the successor's open is observed strictly
after that release, natural EOS settles to Stopped with a cleared paintable,
and an explicit Stop on a second live tune releases the observed connection.
A second session-level smoke configures one lineup row whose stream path
answers `404 Not Found`: the tune must fail as the exact channel-missing
category, and the 404 connection itself must be observed closed before the
session and controller settle, so a failing tune never strands a tuner.

One bin-side Wayland window smoke drives the production window wiring
against the real controller and a real targeted discovery probe: a loopback
UDP responder plus one hand-built second-device observation populate two
device rows through a stateful discovery lane, the sidebar signal's
stop-on-admission fires for a user device change, a mutation refresh that
empties the batch clears the vanished selection through a new generation and
stops playback through the snapshot reducer, and the joined window close
settles the controller and playback session. The loopback stream device
itself is a library test module — the bin target enforces the production
metadata-port policy whose loopback exemption compiles only into library
test builds — so this proof exercises the window stop wiring, while the
live-tuner release evidence for those same paths stays with the library
end-to-end smokes above. These proofs cover fake-tuner release ordering; the
live-device release numbers (P0.4) are recorded, and packaged-runtime (P3)
acceptance remains open.

## Development runtime examples

Package names and plugin grouping vary by platform and release. The exact
factory snapshot is authoritative; these are current examples, not a portable
bundle manifest:

- Fedora uses `gstreamer1-devel` for the core build dependency. The seven
  structural factories are commonly supplied across `gstreamer1-plugins-base`,
  `gstreamer1-plugins-good`, `gstreamer1-plugins-bad-free`, and
  `gstreamer1-plugin-gtk4`. The Linux synthetic acceptance runner additionally
  installs `gstreamer1-plugin-libav` for its MPEG-2 decoder.
- Homebrew supplies the native development/runtime stack through its
  `gstreamer` formula.
- MSYS2 CLANG64 uses `mingw-w64-clang-x86_64-gstreamer` for the core build
  dependency, with structural runtime plugins commonly supplied by the matching
  `gst-plugins-base`, `gst-plugins-good`, `gst-plugins-bad`, and
  `gst-plugins-rs` packages.

Balun's developer helpers check installed development-library floors and,
before a desktop build, the plugin files behind the seven structural
factories, naming the providing package for each missing file; they warn
rather than fail when the libav decoders are absent because P0.5 has not
frozen the decoder contract. They do not install packages or claim a
relocatable runtime. If a desktop executable built elsewhere starts on a
machine that lacks one or more structural plugins, it continues to support
discovery and lineup inspection and reports playback as unavailable.

The libav package used by the Linux smoke is a development/CI system dependency
only. It is not part of the seven-factory startup snapshot, a package allowlist,
or authority to copy a broad plugin distribution into Balun. Future packages
must derive and inspect a minimal runtime closure after P0.5 and the P3
packaging records.

## Packaging and protected-content boundary

The seven registry names are also not a self-contained packaging allowlist.
Autoplugging will require a capability-derived closure based on the formats and
platform audio/video paths actually proven by the P0 evidence records. A future
self-contained package must stage only that reviewed plugin and native-library
closure, traverse native imports, inspect the completed application tree, and
reopen and validate its final artifact. Copying an entire GStreamer plugin
distribution is not acceptable.

The shared [release component policy](release-component-policy.md) remains
mandatory at every package boundary. Balun does not implement protected-channel
decryption, DVD or Blu-ray playback, or proprietary content-decryption modules.
No libdvdcss, optical-disc copy-control/circumvention, AACS/BD+ bridge, Widevine,
PlayReady, FairPlay, DTCP, OpenCDM, or equivalent DRM/circumvention component may
be staged merely because a broad media package makes it available. Ordinary
codecs and containers still require their own compatibility, licensing, patent,
provenance, and distribution review.

## Next acceptance steps

1. P0.3: repeat the Windows and Linux live-TV result on macOS.
2. P0.5: record the Windows and macOS factory sets and freeze the per-platform
   factory, codec, and audio-sink contract; the Linux set is recorded.
3. P3: stage the derived runtime closure into each package and run the
   packaged-runtime probes on Linux, macOS, and Windows.
