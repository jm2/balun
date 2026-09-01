# Playback foundation

Last reviewed: 2026-09-01

This document records Balun's implemented M2.4 GStreamer boundary and the
remaining work before a live-TV stream can be opened. The product and milestone
scope remains authoritative in [`plan-v0.1.md`](plan-v0.1.md), while countable
completion is tracked in [`task.md`](task.md).

## Current status

Balun has an optional, GTK-free GStreamer initialization and capability layer.
The desktop owns that layer on the default GLib main context and can report
whether the structural factories needed for the first playback experiment are
present. An initialization failure, an old loadable runtime, or missing
factories disables playback only; device discovery, selected-device lineup
loading, and the two sidebars remain usable. The native core library is still a
dynamic dependency of a desktop build and must be present for that executable
to start.

This foundation does not create a pipeline, accept a stream URL, open an
HDHomeRun HTTP stream, allocate a tuner, decode media, select an audio sink, or
render a frame. In particular, it does not complete the synthetic playback
experiment in M0.5, the full runtime/plugin contract in M0.10, or the stream
handoff and player work in M2.5 and later.

## Feature and version boundary

The default Cargo feature set remains free of GTK and GStreamer. The features
have these roles:

| Feature | Adds | Intended use |
| --- | --- | --- |
| default | Neither GTK nor GStreamer | Core library, tests, and `balun-discover` |
| `playback` | Optional `gstreamer` Rust binding only | GTK-free playback capability code and tests |
| `desktop` | GTK 4, libadwaita, and `playback` | Balun desktop application |

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
sink. M0.5 must still prove `playbin3` plus `gtk4paintablesink` with a bounded
synthetic stream, and M0.10 must record the complete tested factory contract.
M2.11 will turn that contract into development and packaged-runtime probes.

Registry presence also does not prove that a factory can construct, negotiate,
decode, render, reach EOS, or tear down cleanly. Those behaviors require the
process-isolated synthetic and fake-device tests in the remaining milestones.

## Development runtime examples

Package names and plugin grouping vary by platform and release. The exact
factory snapshot is authoritative; these are current examples, not a portable
bundle manifest:

- Fedora uses `gstreamer1-devel` for the core build dependency. The seven
  structural factories are commonly supplied across `gstreamer1-plugins-base`,
  `gstreamer1-plugins-good`, `gstreamer1-plugins-bad-free`, and
  `gstreamer1-plugin-gtk4`.
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

1. M0.5: run a bounded, display-backed synthetic MPEG-TS experiment through
   explicit `playbin3` and `gtk4paintablesink`, observe real video progress, and
   prove bounded teardown to `NULL`.
2. M0.10: freeze the complete tested factory and platform package contract,
   including codecs and audio sinks.
3. M2.5: pass one revalidated stream URL privately from the controller actor
   without publishing or logging it through GTK-facing state.
4. M2.6-M2.10: own one generation-scoped tune session, paintable, controls,
   errors, and deterministic tuner release.
5. M2.11-M2.12: run fake-device, development-runtime, packaged-runtime, and
   native live-TV smoke coverage on Linux, macOS, and Windows.
