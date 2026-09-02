# Playback foundation

Last reviewed: 2026-09-02

This document records Balun's implemented M2.4 GStreamer boundary, M2.5 private
stream handoff, M2.6 generation-owned tune session, completed M2.7 video
presentation path, completed M2.8 essential controls, and the M2.9
application-owned direct stream transport with its endpoint-free failure
classification. The product and milestone scope remains authoritative in
[`plan-v0.1.md`](plan-v0.1.md), while countable completion is tracked in
[`task.md`](task.md).

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
apply both to the active pipeline and successor tunes. This property contract
does not establish audible output; playbin's native stream and sink selection,
the complete codec/audio-sink contract, and package contents remain M0.10 and
later acceptance work.

Process-isolated Linux tests prove the checked-in MPEG-2 fixture renders into a
real GTK paintable, the real production session streams that fixture from a
loopback HTTP listener through the transport and exact `appsrc` feed to
PLAYING, natural EOS, and joined `NULL` settlement while exposing only the
paintable, and `PlayerView` binds/clears an opaque paintable through its
production widgets and Stop control. Physical HDHomeRun and packaged-runtime
acceptance remain M2.11-M2.12 work. Additional isolated widget and Wayland
smokes cover M2.8 audio-control state, exact ListView activation, and a real
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
1.20 floor covers the implemented foundation; M0.10 remains open until real
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
sink. The M0.5 test proves one narrow MPEG-2 fixture path through `playbin3` and
`gtk4paintablesink`; M0.10 must still record the complete tested factory
contract, and M2.11 must turn that contract into fake-device, development, and
packaged-runtime probes.

Registry presence also does not prove that a factory can construct, negotiate,
decode, render, reach EOS, or tear down cleanly. Those behaviors require the
process-isolated synthetic and fake-device tests in the remaining milestones.

## Actor-private stream handoff

M2.5 adds a narrow URL-bearing path which is separate from application
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
crate-private exposure is a consuming higher-ranked closure used by the M2.9
transport when `playbin3` delivers its exact `appsrc`; the borrow cannot escape
that closure, and the parsed URL lives only in that transport's reader thread.
Merely selecting
a channel row remains inert. Double-clicking or pressing Enter on a valid,
unprotected row constructs a URL-free `StreamSelection`; only the controller's
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
complete M2.8; the broader M4.6 accessibility audit and M2.10 teardown
acceptance remain open.

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
categories and controller handoff failures to fixed endpoint-free visible and
accessible status text, while a teardown failure retains its stronger
close-Balun warning.

## Application-owned direct transport

[ADR-0001](architecture/adr-0001-discovery-playback.md) selected this path,
and M2.9 implements it: keep `playbin3`, expose only the fixed
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
reported as EOS or as a failure. A live `appsrc` feed does not post `playbin3`
buffering messages, so the session's buffering state stays reserved for
runtimes that publish it and the connecting state covers preroll.

Network-free loopback tests cover accepted configuration, repeated, foreign,
and retired source rejection, handoff zeroization, worker-thread
`source-setup` delivery, every status category, redirect refusal with an
uncontacted target, refused, stalled, and truncated streams, bounded chunk
splitting, bounded queue growth under a paused sink, cancellation while reads
and pushes are blocked, rapid replacement, joined teardown, and `playbin3`
resolving the constant URI to exact `appsrc` and decoding the checked-in
fixture to EOS. A child-process trap proves that ambient `http_proxy` and
`all_proxy` configuration reaches a default client but never the transport.
The native macOS and Windows CI lanes must run the same source-selection
checks before M2.9 is recorded complete.

M2.7's pipeline-side visual contract is now explicit: Balun validates the
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
completes M2.7 without exposing a desktop test API capable of forging stream
handoffs.

## Synthetic display-backed acceptance

M0.5 adds one ignored desktop integration test which is compiled by ordinary
all-target test runs and invoked explicitly by
`scripts/test-desktop-lifecycle.sh`. The test uses the checked-in
`tests/fixtures/synthetic-mpeg2.ts`: a deterministic, video-only, 160-by-96,
25 fps MPEG-2 transport stream containing 25 generated test-pattern frames.
The 18,424-byte file is exactly 98 188-byte MPEG-TS packets. Its provenance,
generation command, and SHA-256 digest are committed beside it; it contains no
audio, external media, device data, or network-derived content and is not an
application resource.

M2.7 adds two separate ignored display smokes to the same harness. One drives
the real production session with the checked-in fixture served by a loopback
HTTP listener, verifies its opaque paintable, drives the main context through
PLAYING and natural EOS, and proves joined terminal shutdown. The other
exercises the production `PlayerView` paintable and Stop boundary with a
one-pixel in-memory texture. Neither grants the desktop access to the
pipeline, sink, or stream URI; the loopback URL enters only the library's
crate-private `cfg(test)` handoff constructor.

M2.8 extends the layered harness rather than weakening that boundary. A
production `PlayerView` smoke changes volume and mute through the real GTK
widgets and verifies retained session state and terminal disabling. A ListView
smoke proves selection remains inert while double-click/Enter activation carries
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
Those claims remain in M0.6, M0.10, M1.10, and M2.10 through M2.12.

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
rather than fail when the libav decoders are absent because M0.10 has not
frozen the decoder contract. They do not install packages or claim a
relocatable runtime. If a desktop executable built elsewhere starts on a
machine that lacks one or more structural plugins, it continues to support
discovery and lineup inspection and reports playback as unavailable.

The libav package used by the Linux smoke is a development/CI system dependency
only. It is not part of the seven-factory startup snapshot, a package allowlist,
or authority to copy a broad plugin distribution into Balun. Future packages
must derive and inspect a minimal runtime closure after M0.10 and M2.11.

## Packaging and protected-content boundary

The seven registry names are also not a self-contained packaging allowlist.
Autoplugging will require a capability-derived closure based on the formats and
platform audio/video paths actually proven by M0.6, M0.10, and M2.11. A future
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

1. M2.9-M2.10: prove the constant-URI `appsrc` contract on the native macOS
   and Windows CI lanes, then complete deterministic tuner-release acceptance
   around the connected session and transport.
2. M0.10: freeze the complete tested factory and platform package contract,
   including codecs and audio sinks.
3. M2.11-M2.12: run fake-device, development-runtime, packaged-runtime, and
   native live-TV smoke coverage on Linux, macOS, and Windows.
