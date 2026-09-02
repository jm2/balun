# Balun v0.1 implementation backlog

Last audited: 2026-09-01

This is the executable work ledger for `v0.1.0-alpha.1`. The product scope,
architecture, safety constraints, and acceptance rationale remain authoritative
in [`plan-v0.1.md`](plan-v0.1.md); sanitized real-device evidence belongs in
[`compatibility-v0.1.md`](compatibility-v0.1.md), and merged user-visible
outcomes belong in [`../CHANGELOG.md`](../CHANGELOG.md).

## How to use this file

- Work from the earliest unchecked record whose prerequisites are satisfied.
- A top-level checkbox is one countable outcome. Check it only when its code,
  deterministic tests, relevant documentation, and changelog entry have
  landed on `main`.
- Keep partially implemented records unchecked and summarize the usable
  foundation beneath them instead of treating scaffolding as completion.
- Split work into reviewable commits, but do not weaken the network, identity,
  tuner-release, privacy, or package-inspection contracts to make a slice fit.
- Record physical-device results without device IDs, addresses, channel names,
  credentials, or other raw network topology.
- Recount the literal top-level checkboxes whenever a record is added, split,
  completed, or removed.

Current status: **21/64 (32.8%)** implementation records complete. This is a
dependency ledger, not an effort estimate; packaging and cross-platform
playback records are substantially larger than most completed foundation work.

## Current focus and critical path

The discovery, identity, selected-lineup, and two-sidebar path is implemented.
The current Windows correction uses per-interface limited IPv4 broadcast,
omits unusable scoped-link-local-only results, and provides a fixed
`-InspectLocal` diagnostic. It still needs the two physical Windows hosts to
confirm the repair. The optional GStreamer foundation is also implemented: the
desktop now owns one main-context runtime/capability snapshot. A separate,
process-isolated Linux smoke proves a pinned local MPEG-2 fixture can decode,
render multiple frames, update its GTK paintable, reach EOS, and tear down to
`NULL`. The controller can now authorize one generation-bound, URL-redacted
stream handoff from its current complete selected snapshot. The playback
library owns the sole consumer: a separate tune generation serializes actor
responses, pipeline replacement, bus events, terminal settlement, and bounded
teardown. The desktop pane owns that session and its URI-opaque paintable
binding/clearing path. Double-click/Enter activation of an unprotected row now
submits its exact applied lineup generation, consumes the private response, and
binds an applied paintable. Superseding activation, device-selection change,
and window shutdown cancel the appropriate pending or active owner. A device
change stops immediately after command admission and again on its accepted
generation publication, so lineup-worker cancellation cannot extend the old
tuner lifetime.

The shortest path to the first live-TV test is:

1. Re-run Windows local discovery and selected-device lineup loading.
2. Run the desktop dev build and activate one unprotected ATSC 1.0 channel.
3. Prove picture/audio compatibility plus channel switch, explicit Stop,
   device loss, and window-close tuner release.

The first playback target is a clear ATSC 1.0 channel on an accessible CONNECT
or CONNECT 4K. ATSC 3.0 codec/audio support and protected PRIME channels are
compatibility/error-path work, not prerequisites for that first picture.

## M0 — Compatibility and protocol spike

- [ ] **M0.1 — Prove bounded HDHomeRun discovery.** The framed protocol,
  DeviceID validation, local/exact numeric probe paths, and real-hardware local
  discovery are implemented. Complete this record by exercising and documenting
  an exact-address probe against accessible real hardware.

- [x] **M0.2 — Prove responder-pinned metadata and lineup loading.** Fetch
  bounded `discover.json` and `lineup.json` data from validated responders,
  reject hostile origins/redirects/credentials, and record sanitized observed
  JSON compatibility.

- [ ] **M0.3 — Land ADR-0001 for discovery and playback choices.** Extract the
  implemented discovery decision and the selected GStreamer pipeline approach
  into a stable decision record, including rejected alternatives.

- [ ] **M0.4 — Land sanitized compatibility fixtures.** Add representative
  discover replies, device JSON, lineup JSON, HTTP failures, and stream
  characteristics without retaining topology, authentication, or real channel
  data.

- [x] **M0.5 — Complete a minimal playback experiment.** Feed a checked-in,
  video-only MPEG-2 transport-stream fixture through explicit `playbin3` and
  `gtk4paintablesink` in an isolated Linux display process; require multiple
  rendered frames and paintable updates, negotiated raw-video dimensions, EOS,
  and a bounded transition to `NULL`, without discovery, HTTP, or tuner work.

- [ ] **M0.6 — Record codec/container observations.** Cover MPEG-2, H.264,
  MPEG-TS, interlace, AC-3, and AAC on accessible ATSC 1.0 channels; record
  ATSC 3.0 HEVC/AC-4 behavior separately and honestly.

- [ ] **M0.7 — Measure tune and teardown behavior.** Capture first-frame time,
  rapid channel-switch behavior, cancellation, device loss, and evidence that
  teardown releases a tuner promptly.

- [ ] **M0.8 — Prove one routed-network case.** On an owned WireGuard/Site
  Magic or equivalent route where broadcast does not cross, show that one
  approved exact target succeeds without neighbor enumeration.

- [ ] **M0.9 — Report in-band guide availability.** Observe ATSC/DVB guide
  tables during an already active stream, without allocating a background
  tuner, and document device/region gaps.

- [ ] **M0.10 — Freeze the GStreamer floor and plugin contract.** The selected
  GStreamer 1.20 foundation floor and seven structural factories are recorded.
  Complete this record by proving and freezing the exact required
  HTTP, MPEG-TS, parser/decoder, platform-audio, and GTK-video factory/package
  contract across the supported platforms.

## M1 — Repository foundation

- [x] **M1.1 — Establish package identity and license.** Keep the GTK-free Rust
  library, thin feature-gated desktop binary, `io.github.jm2.Balun`, exact
  product description, Rust floor, and GPL-3.0-or-later declarations aligned.

- [x] **M1.2 — Build the adaptive three-pane shell.** Provide separate device
  and channel sidebars plus a live-video pane that adapts without merging the
  two navigation levels.

- [x] **M1.3 — Own asynchronous work outside GTK.** Use bounded commands,
  immutable/coalesced snapshots, generation checks, cancellation, and joined
  shutdown across the Tokio/GLib boundary.

- [x] **M1.4 — Establish stable domain identity.** Scope channels to DeviceID,
  preserve devices across locator changes, natural-sort lineup numbers, and
  reject stale or cross-device state.

- [ ] **M1.5 — Add versioned settings.** Persist only reviewed preferences and
  manual-device configuration through a migration-tested schema; do not store
  credentials, stream URLs, or incidental discovery topology.

- [ ] **M1.6 — Complete project-governance documentation.** Add contribution,
  security, conduct, support, issue/PR templates, and the focused architecture,
  discovery, playback, EPG, packaging, and release documents named in the
  implementation plan.

- [x] **M1.7 — Enforce forbidden release components.** Pin one shared policy
  that rejects optical-disc copy-control/circumvention and proprietary DRM
  components from reviewed source and packaging inputs, with negative fixtures.

- [x] **M1.8 — Establish cross-platform CI.** Run locked formatting, strict
  linting, debug/release tests, exact MSRV checks, Linux desktop checks, and
  native macOS/Windows compile/link smoke lanes.

- [x] **M1.9 — Align developer build helpers.** Keep Tributary-compatible
  script names, make all three no-option routes build the desktop without
  launching, retain explicit diagnostics, validate native-target-qualified
  outputs, and keep incomplete package modes fail-closed.

- [ ] **M1.10 — Prove native desktop lifecycle on every target.** Retain the
  isolated Linux close/join smoke and add Windows and macOS runtime activation,
  ordinary close, and no-implicit-network evidence on native hosts.

- [ ] **M1.11 — Complete verification and CI hardening.** Add dependency audit
  plus Markdown, TOML, YAML, and GitHub Actions linting. After the playback
  slice makes them meaningful, add a coverage ratchet and scheduled fuzzing for
  discovery packets, device/lineup JSON, XMLTV/event normalization, and any
  Balun-owned guide-section parser.

## M2 — Playable vertical slice

- [x] **M2.1 — Connect explicit discovery to the desktop.** Refresh performs
  bounded local discovery; the add action accepts one numeric unicast address;
  Stop cancels either operation; no action starts automatically.

- [x] **M2.2 — Load one selected device lineup.** Resolve preferred locators
  with identity checks and bounded fallback while keeping complete stream URLs
  inside the controller actor.

- [x] **M2.3 — Populate both virtualized sidebars.** Keep device and channel
  selection stable by identity, reset recycled rows, expose protected channels
  honestly, and never tune merely because a device was selected.

- [x] **M2.4 — Add an optional GStreamer feature boundary.** Keep `gstreamer`
  0.25/`v1_20` behind a GTK-free `playback` feature included by `desktop`, while
  the default library and diagnostic acquire neither GTK nor GStreamer. Own
  initialization on the default GLib main context, enforce the native 1.20
  floor, expose fixed path-free failures, and snapshot exactly `playbin3`,
  `uridecodebin3`, `decodebin3`, `souphttpsrc`, `tsdemux`, `deinterlace`, and
  `gtk4paintablesink`. Missing components disable playback readiness without
  disabling discovery or lineup inspection; no pipeline or stream URL exists in
  this slice.

- [x] **M2.5 — Define the actor-private stream handoff.** Select a ChannelKey
  only from the current complete selected snapshot, revalidate its device and
  URL origin, and never publish or log the URL through GTK-facing state.

- [x] **M2.6 — Implement one generation-owned tune session.** Serialize start,
  replacement, cancellation, bus messages, and terminal settlement so stale
  pipeline events cannot affect a successor channel.

- [x] **M2.7 — Render live video.** Connect `playbin3` to
  `gtk4paintablesink`, own the paintable on the main context, preserve aspect
  ratio, provide deinterlacing suitable for normal 1080i content, and handle
  sink/deinterlacer setup failure without leaking the stream. The pipeline side
  now validates and sets the private sink, adaptive deinterlace flag, and forced
  aspect preservation on both playbin and its GTK paintable before assigning a
  URI. The main-context `PlayerView` now owns the session, binds only the opaque
  paintable, and clears it before joined window shutdown. A display-backed
  smoke covers this production binding boundary, and the initial activation
  lane invokes it for an applied tune. Separate display-backed processes prove
  the real production session's opaque paintable plus bounded `NULL` shutdown,
  production `PlayerView` binding/clearing, and decoded-frame/EOS behavior.
  Keeping those proofs layered avoids exposing a URI-forging desktop test API;
  physical-device and packaged-runtime acceptance remain M2.11-M2.12 work.

- [ ] **M2.8 — Add essential controls.** Implement channel activation, Stop,
  volume, mute, and fullscreen with accessible keyboard and pointer behavior.
  Double-click and Enter activation now carry only the non-protected row's
  exact applied lineup generation into the bounded actor/session path; rapid
  successors abort the prior wait. An accessible header Stop button supports
  pointer and native Enter/Space activation, disables before aborting the
  response task, clears the paintable, and synchronously stops the exact
  generation. Volume, mute, fullscreen, and their complete accessibility
  coverage remain open.

- [ ] **M2.9 — Add truthful playback state and errors.** Surface connecting,
  buffering, playing, stopped, tuner-busy/503, missing/404, protected, missing
  codec/plugin, offline, and internal pipeline failures without endpoint data.
  Activation now has fixed endpoint-free connecting and setup-failure copy,
  but bus Error/EOS state is not yet projected back into `PlayerView`, so a
  terminal session can leave its last paintable/status presentation stale.

- [ ] **M2.10 — Prove deterministic teardown.** Rapid switch, device
  reselection, discovery mutation, pipeline error, window close, and process
  shutdown must cancel/join the exact owner and release the tuner. Device
  change admission, accepted generation change, controller snapshot-channel
  closure, and window shutdown now invoke stop or terminal shutdown; complete
  fake/live-tuner proof remains open.

- [ ] **M2.11 — Add playback integration coverage and runtime probes.** Use a
  fake HDHomeRun HTTP server and synthetic MPEG-TS path, then verify the exact
  GStreamer factory set in development and packaged runtimes.

- [ ] **M2.12 — Pass the first cross-platform live-TV smoke.** Watch and switch
  an unprotected channel on Linux, macOS, and Windows dev builds and record
  sanitized results; unsupported codecs remain explicit rather than silently
  falling back to unreviewed components.

## M3 — Multi-device and routed discovery

- [x] **M3.1 — Preserve multi-locator device identity.** Retain independently
  expiring locator/origin claims under DeviceID and keep every lineup and
  ChannelKey device-scoped.

- [x] **M3.2 — Union local and exact desktop evidence safely.** Replace local
  and per-target batches atomically, bind successful exact retries to DeviceID,
  cap distinct session targets, and avoid automatic selection or HTTP work.

- [x] **M3.3 — Build fail-closed route authority foundations.** Provide bounded
  candidate policy, durable topology-redacted approval, exact fresh-route
  revalidation, and Linux route/store observer ownership without connecting an
  automatic packet sender prematurely.

- [ ] **M3.4 — Persist approved manual targets and admit hostnames safely.** Add
  versioned, user-revocable, bounded configuration and startup rediscovery.
  Keep numeric addresses and hostnames distinct, cap DNS answers/work, admit
  only usable unicast results as exact probes, and never turn historical
  observations or name resolution into prefix-scan authority.

- [ ] **M3.5 — Connect the monitored Linux routed runner.** Replace/rebaseline
  the whole observer pair after store publication, serialize the final
  pre-send check, consume the sealed socket, and settle reservation completion.

- [ ] **M3.6 — Add routed-discovery UX.** Preview exact candidates and packet
  budget, require explicit approval, and expose bounded progress, cancel,
  cooldown, backoff, and revocation.

- [ ] **M3.7 — Reconcile network changes.** Debounce adapter/route changes,
  expire stale evidence, cancel invalid authority synchronously, and preserve
  devices that retain another valid locator.

- [x] **M3.8 — Fail closed on unsupported native providers.** Keep automatic
  macOS and Windows route providers unavailable with a fixed reason until safe
  route-domain APIs and proof obligations are implemented.

- [ ] **M3.9 — Complete privacy-safe diagnostics.** Explain probe counts,
  accepted/rejected replies, provider availability, and coarse failure classes
  without persisting or rendering unrelated topology.

- [ ] **M3.10 — Pass real multi-device/routed validation.** Keep local and
  remote CONNECT/4K/PRIME devices separate across both sites, deduplicate the
  same DeviceID across paths, and measure the documented traffic budget.

## M4 — Guide and usability

- [ ] **M4.1 — Define guide source precedence.** Specify lineup text, in-band
  now/next, and optional XMLTV ownership, freshness, conflict, and fallback
  rules per device-scoped channel.

- [ ] **M4.2 — Implement in-band guide extraction or a proved limitation.** Do
  work only on an already active stream, bound section parsing, and never
  allocate a background tuner merely to populate the guide.

- [ ] **M4.3 — Add bounded XMLTV support.** Accept explicit file/URL sources,
  cap download/decompression/document/event work, require channel mappings,
  handle time zones/DST, and disable implicit scraping.

- [ ] **M4.4 — Add bounded lineup and guide caches.** Store them separately with
  explicit schema versions, size/age limits, and expiration. Keep lineup rows
  and normalized guide events isolated by device/channel identity; an offline
  cached lineup must be visibly stale and must not imply that playback is
  currently reachable.

- [ ] **M4.5 — Present channel information and now/next.** Degrade cleanly to
  lineup data when guide data is absent and never imply that stale data is live.

- [ ] **M4.6 — Finish core usability.** Add search, favorites, keyboard
  navigation, accessibility review, and polished empty/error states without
  weakening the double-sidebar model.

## M5 — Packaging and release candidate

- [x] **M5.1 — Port reusable Tributary release-engineering foundations.** Keep
  equivalent script names, provenance checks, dependency/toolchain policy, and
  preparatory Linux/Flatpak/macOS component gates while removing music-only and
  optical-disc/circumvention assumptions.

- [ ] **M5.2 — Add desktop metadata and assets.** Land icons, desktop entry,
  AppStream metadata, screenshots, MIME/protocol policy, and validation with
  exact Balun identity/version data.

- [ ] **M5.3 — Build Flatpak x86_64 and aarch64.** Generate locked sources,
  retain the narrow permission policy, stage only the required runtime, and
  validate/reopen the finished bundle.

- [ ] **M5.4 — Build Windows x86_64 ZIP and installer.** Stage a complete GTK
  and GStreamer runtime closure, validate PE imports and app identity, and
  reopen both artifacts before upload.

- [ ] **M5.5 — Build the macOS arm64 app and DMG.** Complete the app tree,
  icons/plist, runtime closure, Mach-O dependency inspection, signing policy,
  and reopened DMG checks.

- [ ] **M5.6 — Derive the GStreamer runtime closure from capability.** Include
  only reviewed HTTP, MPEG-TS, codec/parser, core, and platform sink components;
  never copy an entire plugin distribution.

- [ ] **M5.7 — Complete every denied-component artifact gate.** Apply the
  shared policy before staging, during native dependency traversal, to each
  completed tree, and again after reopening every final package.

- [ ] **M5.8 — Produce supply-chain evidence.** Require an exact artifact
  inventory, checksums, SBOM, build provenance, and third-party license/source
  notices for the immutable tagged commit.

- [ ] **M5.9 — Complete release automation.** Add semantic release-check
  tooling, build all packages from one signed annotated tag, create a draft,
  and grant release-write authority only to the final no-source publication
  job after every artifact passes.

## M6 — v0.1.0-alpha.1 validation

- [ ] **M6.1 — Complete the sanitized hardware matrix.** Cover accessible
  primary-site CONNECT/4K and secondary-site PRIME/4K devices; defer the
  inaccessible Australian units without claiming regional support.

- [ ] **M6.2 — Complete desktop smoke coverage.** Record Wayland, X11, macOS,
  and Windows launch/discover/tune/switch/close results using the actual
  candidate artifacts where available.

- [ ] **M6.3 — Measure operational budgets.** Record startup, idle resources,
  discovery traffic, first-frame latency, rapid-switch latency, and tuner
  release/teardown time.

- [ ] **M6.4 — Complete the security and privacy review.** Re-audit network
  admission, URL/credential handling, guide sources, persisted state, logs,
  package contents, and unexpected tuner-allocation paths.

- [ ] **M6.5 — Publish an honest support/limitations matrix.** Name supported
  device families, platforms, codecs, broadcast standards, guide sources, and
  known protected/ATSC 3.0/regional limitations from evidence only.

- [ ] **M6.6 — Cut and publish the prerelease.** Pass every required gate,
  create the signed annotated `v0.1.0-alpha.1` tag, validate the immutable
  artifact set, and publish release notes only after final approval.

## Explicitly outside v0.1

- Decryption, DRM bypass, CableCARD protected-channel playback, libdvdcss, or
  any optical-disc copy-control/circumvention component.
- Recording, DVR scheduling, timeshift, pause-live-TV buffering, or tuner
  configuration/firmware management.
- Cloud guide scraping, account-bound commercial guide services, telemetry,
  analytics, or automatic broad subnet scanning.
- A guarantee that ATSC 3.0 HEVC/AC-4 works on every platform before the
  required legal codec/runtime path is observed and packaged safely.
- Required Australian DVB validation before those devices are accessible;
  support remains evidence-driven and may land after the first alpha.
