# Playback foundation

Last reviewed: 2026-09-01

This document records Balun's implemented M2.4 GStreamer boundary, M2.5 private
stream handoff, M2.6 generation-owned tune session, and the remaining work
before a live-TV stream can be opened from the desktop. The product and
milestone scope remains authoritative in [`plan-v0.1.md`](plan-v0.1.md), while
countable completion is tracked in [`task.md`](task.md).

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
library-owned `gtk4paintablesink`.
The desktop cannot inspect the URI and does not yet invoke that session, so the
application still does not open an HDHomeRun HTTP stream, allocate a tuner,
select an audio sink, or render a channel. M0.5 is
demonstrated separately by a process-isolated Linux test
which decodes a pinned local fixture into a real GTK paintable. That bounded
test does not complete the full runtime/plugin contract in M0.10 or the
GTK activation, paintable, and live-source work in M2.7 and later.

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
| `souphttpsrc` | HTTP source for an eventual responder-pinned device stream |
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
crate-private exposure is a consuming higher-ranked closure used by the M2.6
pipeline constructor; the borrow cannot escape that closure. Channel rows
remain inert, so no stream is opened yet.

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
Rust-owned URI storage is zeroized. The URI is copied directly into the native
pipeline property inside the core library and is never returned to the desktop
crate. The same library constructs and retains `gtk4paintablesink`; it exposes
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

The desktop pane has not yet presented the paintable or connected channel
activation. M2.7 will bind it into the `GtkPicture` and complete deinterlacing;
M2.8-M2.10 will connect activation, controls, user-visible state, and full
teardown paths.

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
macOS or Windows rendering, audio, physical MPEG-TS variants, live-source
behavior, channel switching, tuner release, or packaged-runtime relocation.
Those claims remain in M0.6, M0.10, M1.10, and M2.7 through M2.12.

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

Balun's developer helpers check installed development-library floors; they do
not install packages or claim a relocatable runtime. If a development machine
has the core library but lacks one or more structural plugins, the desktop
continues to support discovery and lineup inspection and reports playback as
unavailable.

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

1. M2.7-M2.10: connect the generation-scoped tune session and own its
   paintable, controls, errors, and deterministic tuner release.
2. M0.10: freeze the complete tested factory and platform package contract,
   including codecs and audio sinks.
3. M2.11-M2.12: run fake-device, development-runtime, packaged-runtime, and
   native live-TV smoke coverage on Linux, macOS, and Windows.
