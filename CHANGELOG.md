# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Essential live-TV controls** — Complete M2.8 with native header controls for
  Stop, normalized volume, independent mute, and fullscreen alongside the
  existing exact-generation channel activation. The main-context playback
  session retains process-local audio settings across Stop, replacement, EOS,
  and native error, applies them before each authorized URI enters `playbin3`,
  and maps the UI level to playbin's linear property with a cubic gain curve.
  Active updates target only the current owner; failed mutations preserve the
  last accepted settings, and terminal teardown or shutdown disables the audio
  widgets. This is a property-level contract, not proof of audible output or a
  complete codec/audio-sink package. The focusable slider and toggle expose
  stable accessible roles and labels while retaining native pointer and keyboard
  behavior. A labeled fullscreen button and unmodified F11 toggle the window;
  Escape exits only from confirmed fullscreen. Presentation follows the
  compositor's reported state, forces the player while protecting both nested
  Back paths, focuses the exit control, and restores the exact prior pages,
  pop permissions, and focus on exit. Compact channel activation presents the
  player even for setup failure without retaining a widget/layout cycle.
  Factory-backed, fake-backend, widget, isolated Wayland fullscreen, and
  production-session smokes provide layered coverage without claiming the
  broader M4.6 accessibility audit, native-platform runtime behavior, packaged
  playback, or physical-device acceptance.
- **Initial channel-activation lane** — Make standard double-click and keyboard
  activation on an unprotected channel submit only its exact `ChannelKey` and
  applied selected-lineup generation. The main-context player invalidates the
  predecessor before entering the controller's existing bounded FIFO, aborts
  superseded response tasks, consumes the opaque response through the sole
  playback session, and binds only an applied generation's GDK paintable. A
  missing paintable or setup failure hides the old frame and settles the
  pipeline. Successful admission of a selected-device change stops playback
  immediately; the resulting generation publication repeats that stop before
  replacing the sidebars as a fail-safe, and controller snapshot-channel
  closure also stops the independent playback owner. Connecting and
  activation-setup failure copy is fixed and endpoint-free. An accessible
  header Stop button is enabled during a pending or applied activation; pointer
  click or native Enter/Space activation disables it immediately, aborts the
  private response task, clears presentation, and synchronously stops the exact
  session generation. Detailed state/error projection and live-device
  acceptance remain open.
- **Initial live-video presentation contract** — Configure each private
  `playbin3` with its documented adaptive deinterlace flag, forced source
  aspect-ratio preservation, and the library-owned GTK paintable sink with its
  own aspect preservation enabled before the authorized URI enters native
  storage. Missing properties or flag support fail with fixed
  pipeline-construction copy while the pipeline is still `NULL`; later start
  or bus failures retain the generation-owned cleanup path. Factory-backed and
  display-backed tests verify the configuration without network access. The
  main-context `PlayerView` now owns the session, binds only its URI-opaque
  paintable into a containment-fit `GtkPicture`, clears it during synchronous
  playback shutdown, and remains retained until controller join completes. An
  applied channel activation now invokes this binding boundary. Display-backed
  tests separately prove the real production session's opaque paintable and
  bounded `NULL` shutdown, the production `PlayerView` binding/clearing path,
  and decoded-frame/EOS behavior without adding any URI-forging desktop test
  API. This completes M2.7's deterministic live-video presentation contract;
  physical-device and packaged-runtime acceptance remain later milestones.
- **Generation-owned tune session** — Add one default-main-context playback
  owner which assigns a monotonic tune generation before waiting for the
  actor-private stream response, consumes the opaque URI only inside the core
  library while constructing `playbin3`, and tags every reduced Playing,
  buffering, EOS, and native-error bus event. Replacement invalidates the
  predecessor first, detaches its watch, and requires a bounded transition to
  `NULL` before a successor can be constructed. Stop, terminal bus events,
  shutdown, and Drop likewise settle the exact owner; stale responses and
  events cannot mutate a successor, and successful stale handoffs are dropped
  for zeroization. All bus reductions and stateful public calls use the exact
  default main context, and reentrant access fails with fixed errors instead of
  panicking. Exposed states and failures remain URL-free. The library
  constructs and retains `gtk4paintablesink` itself and exposes only its
  URI-opaque GDK paintable, never a GStreamer element whose parents reveal the
  playbin URI. The completed M2.8 controls submit the URL-free intent and consume
  its response here. A deduplicated URL-free latest-state watch now projects
  connecting, buffering percentage, playing, stopped, EOS, and generic terminal
  failure onto an accessible GTK status and clears terminal paintables;
  category-complete native errors, live-device, and packaged-runtime acceptance
  remain open.
- **Actor-private stream handoff** — Add a URL-free channel intent containing
  the complete ChannelKey and selected-snapshot generation, resolve it in the
  controller's existing bounded FIFO only against the current complete private
  snapshot, and return one opaque one-shot handoff without publishing a new
  GTK-facing state revision. Retain the successful responder authority,
  require the current registry to still authorize it, reject stale
  generations, cross-device or absent channels, protected rows, and any stream
  scheme, host, port, credential, query, fragment, or channel-path mismatch.
  The non-cloneable handoff has no public URI accessor or `Display`, redacts its
  custom `Debug`, and zeroizes its private URI bytes on drop. The later M2.6
  session is its only URI consumer; the handoff itself performs no HTTP or
  GStreamer work.
- **Optional GStreamer playback foundation** — Add a GTK-free `playback`
  feature using the optional Rust `gstreamer` 0.25 binding with its `v1_20` API
  surface, and make the desktop feature include it while leaving the default
  library and diagnostic free of GTK/GStreamer. A non-`Send`, non-`Sync` owner
  initializes on the owned default GLib main context, enforces a native
  GStreamer 1.20 floor, exposes fixed path-free initialization failures, and
  takes one immutable startup snapshot of `playbin3`, `uridecodebin3`,
  `decodebin3`, `souphttpsrc`, `tsdemux`, `deinterlace`, and
  `gtk4paintablesink`. Missing components produce a playback-unavailable player
  state without disabling discovery or lineup inspection. This foundation does
  not yet create a pipeline, hand off a stream URL, choose codecs or an audio
  sink, render application video, or complete the packaged-runtime probes.
- **Synthetic GTK playback acceptance** — Add a checked-in, deterministic,
  video-only MPEG-2 transport-stream fixture and a process-isolated Linux smoke
  which drives it through explicit `playbin3` and `gtk4paintablesink` in a real
  presented `GtkPicture`. Require multiple rendered-frame and paintable-update
  observations, negotiated 160-by-96 raw-video caps, EOS, detached bus handling,
  and a bounded transition to `NULL`. Run the test under its own isolated
  headless-Wayland session-bus process with fixed failure categories, sanitized
  display/GStreamer overrides, and inner/outer deadlines; retain Xvfb only as
  an optional local fallback when Weston is unavailable. The fixture, libav
  development decoder, and fake audio sink open no network source or tuner and
  do not establish the complete codec, cross-platform, or packaging contract;
  existing libdvdcss, optical-disc, DRM, and circumvention exclusions remain
  unchanged.
- **GTK development shell** — Add an opt-in `balun` desktop binary using GTK 4
  and libadwaita at Tributary's proven API floors, with adaptive nested device,
  channel, and live-TV panes, truthful packet-free empty states, exact
  application identity, Linux desktop compile/link/lint coverage, and MSRV
  checking without adding GTK to the default library or diagnostic build. A
  local GTK 4 Broadway smoke verifies display initialization and event-loop
  entry, while an isolated headless-Wayland/D-Bus smoke exercises the ordinary
  window close path and requires a successful joined controller shutdown. The
  helper uses Xvfb only as an optional local fallback.
- **Cross-platform desktop link checks** — Compile and link the feature-gated
  shell against native Homebrew GTK/libadwaita/GStreamer on macOS arm64 and an
  MSYS2 CLANG64 GTK/libadwaita/GStreamer environment on Windows x86_64, without
  claiming a runnable bundle or package.
- **Desktop-default platform build helpers** — Make the same-named Linux,
  macOS, and Windows helpers derived from Tributary build locked release desktop
  executables without launching when invoked with no options. Keep the GTK-
  free diagnostic explicit behind `--diagnostic` on Linux/macOS and
  `-Diagnostic` on Windows, and make compile-oriented quick modes desktop-first
  by default. Preserve Tributary's existing Windows-only `-Run` launch flag
  without inventing a shell `--run`. Linux binds Cargo to the exact validated
  native target and repository target path and applies its ELF gate; macOS does
  the same before its pinned Mach-O component gate; Windows already pins the
  desktop target and now also pins native diagnostic targets. A Windows-only
  `-InspectLocal` mode builds and validates the GTK-free tool before invoking
  exactly `--inspect --local`, avoiding manual executable paths. Unavailable
  packaging modes remain fail-closed. CI exercises all three no-option desktop
  routes, while release-candidate jobs select the diagnostic routes explicitly.
  None of these native executables is yet a portable bundle, installer, or
  application package.
- **Headless discovery foundation** — Add the initial Rust library and
  `balun-discover` diagnostic for ordinary local discovery, exact-address
  probes, and explicitly approved routed discovery.
- **HDHomeRun protocol handling** — Validate tuner discovery frames, TLVs,
  CRCs, DeviceIDs, response limits, and stable device observations without a C
  protocol dependency.
- **Stable device and channel identity** — Retain independently expiring
  locators and discovery origins behind one validated DeviceID while keeping
  every channel key scoped to exactly one device.
- **Bounded device inspection** — Add responder-pinned metadata and lineup
  fetching plus a reusable, cancellation-aware preferred-locator fallback
  service used by `balun-discover --inspect`; it summarizes devices and
  channels without returning stream URLs, opening a stream, or allocating a
  tuner.
- **Selected-device snapshot resolution** — Add a separate identity-checked
  resolver for one registry device, with a frozen preferred-first locator
  order, one monotonic deadline across deterministic fallback, terminal
  cancellation, bounded fixed-category issues, and URL-redacted snapshot and
  lineup debug output. Complete stream-bearing rows remain inside the
  HDHomeRun core boundary for later controller-owned playback.
- **Controller snapshot boundary** — Add bounded, immutable, GTK-free device
  and selected-lineup projections with independent discovery and selection
  generations, URL-free channel rows, deterministic ordering, and validation
  that prevents cross-device lineup merging or stale reducer replacement.
- **Packet-free controller runtime** — Own local and exact-address discovery
  and the device registry on one named current-thread Tokio worker, behind an
  eight-command
  nonblocking ingress and coalesced immutable snapshot receiver. Startup is
  inert; only an explicit discovery command invokes network work,
  supersession cancels and joins its predecessor, successful scans atomically
  replace their authoritative source view,
  failed scans retain last-good devices, and queue-independent shutdown wins
  races before joining the worker. An independent selection lane resolves
  exactly one registered device, cancels and joins superseded work, re-resolves
  after a successful registry refresh, rejects stale completions, retains
  stream URLs only inside the actor, and publishes URL-free metadata and
  channel rows.
- **Desktop exact-address discovery** — Add parser-gated numeric IPv4 and
  unscoped-IPv6 entry for one known routed tuner, a fixed two-attempt/200 ms
  probe budget, one accepted identity, bounded receive work, DeviceID binding
  after first success, and an independent local-plus-exact registry union.
  Keep the entry itself out of snapshots and fixed status/error copy so only a
  validated responder locator reaches the device projection. A never-validated
  target creates no device evidence, while transport or validation failure
  retains any prior last-good evidence. Count at most 32 distinct addresses
  toward session traffic admission even when no valid reply is accepted, and
  weakly capture address-entry widgets so dialog close clears and releases the
  raw text. Expose one Stop action that cancels and joins either discovery kind.
- **Connected device and channel sidebars** — Reduce complete controller
  snapshots on the GLib main context into virtualized device and selected-
  lineup models, restore selection by stable DeviceID or ChannelKey rather
  than list position, strictly reset recycled rows, keep protected channels
  visible but disabled, expose local discovery through explicit Refresh and
  exact discovery through a parser-gated address action, and offer a shared
  Stop action only while discovery is active. Selecting a device never starts
  playback.
- **Route candidate policy** — Add a platform-neutral route snapshot/provider
  boundary, a native Linux rtnetlink provider, and deterministic policy for
  exact or approved private tunnel candidates; native macOS and Windows
  providers remain pending.
- **Approved routed target runner** — Probe a caller-approved, deduplicated set
  of private IPv4 route candidates with separate provenance, bounded packet
  rate and concurrency, immediate cancellation, and a hard overall deadline.
- **Route-derived approval policy** — Bind remembered consent to a keyed,
  topology-redacted fingerprint of the exact targets, tunnel bindings, route
  scopes, and packet policy, then issue only single-use, crash-reserved scan
  authority with deterministic cooldown and backoff transitions.
- **Durable routed authority** — Persist only keyed fingerprints and bounded
  policy state behind a private installation key, strict cross-process lock,
  atomic durability barriers, semantic quarantine, global run sequencing,
  exact revocation, and a topology-free revoke-all path; on Unix, pin all
  operations to one validated directory descriptor and reject persistent
  aliases of permanent entries.
- **Sealed reservation publication** — Keep a confirmed durable reservation's
  raw permit inside a non-cloneable, redacted typestate until a fresh exact
  reread matches the complete approval ledger and immutable key binding; no
  permit escapes uncertain or unsupported publication.
- **Fresh route gate** — Consume a durably active permit only after rebuilding
  the exact proposal from a fresh route snapshot; reject every stable topology
  or policy change, use only a fresh nonzero interface identity, and cap the
  scan budget to the remaining reservation lease.
- **Sealed Linux routed socket** — Construct a nonblocking IPv4 UDP socket only
  after one `SO_BINDTODEVICE` assignment and matching interface-name/index
  readbacks before and after local bind; retain it as an opaque, non-cloneable
  capability with no I/O or raw-descriptor escape until the monitored runner
  exists.
- **Packet-free routed admission** — Register source-bound invalidation before
  durable reserve, obtain and revalidate a fresh route snapshot, map wall-clock
  authority onto one non-extending monotonic deadline, and leave abandoned
  authority under its crash-conservative reservation and cooldown.
- **Linux route-event monitor** — Add an unwired rtnetlink observer foundation
  with subscribe-before-snapshot barriers, strict work bounds, coalesced
  reconciliation, invalidate-before-notify behavior, and a consuming final-
  drain-to-synchronous-activation handoff; a prepared/live bridge carries the
  exact pre-snapshot token and owns cancellation-safe actor shutdown.
- **Linux approval-store observer** — Add an unwired inotify foundation bound
  to the store's exact pinned directory, with subscribe/read/drain baselines,
  bounded event work, and fail-closed permanent-entry invalidation; its bridge
  performs blocking subscription and exact rereads off-executor and owns the
  live actor through joined shutdown.
- **Combined routed observer authority** — Add an unwired, platform-neutral
  coordinator which mints only one non-cloneable route-and-store health epoch;
  either observer event, failure, replacement, or drop synchronously cancels
  every registration, stale callbacks cannot revive a successor, and a
  no-await paired callback rendezvous retains no authority after owner drop.
  Add a Linux whole-pair owner that prepares route observation before the
  store reread, retains both live actors and the activation owner, coalesces
  replacement requests, and retires authority synchronously before concurrent
  joined shutdown; partial-start and activation failures await destruction of
  every actor they started before a replacement may proceed.
- **Cross-platform repository checks** — Add locked formatting, strict Clippy,
  debug and release tests, exact-MSRV validation, macOS and Windows compile
  smoke checks, and immutable-tag release-candidate validation.
- **Reusable packaging foundation** — Port Tributary's pinned Flatpak Cargo
  generator and preparatory Linux/Flatpak artifact-policy validators, adapted
  to Balun's application identity and exercised with synthetic archive, ELF,
  metadata, completed-commit, exact Flatpak-permission, cross-platform
  build-helper, and macOS icon/bundle fixtures.
- **Tributary filename parity** — Retain the exact relative filename for every
  equivalent `scripts/` and `build-aux/` port, and record each genuinely new,
  split, or product-identity-specific filename in the port ledger.
- **Rust floor synchronization** — Coordinate the Cargo, Dependabot proposal,
  exact MSRV CI, and README compiler declarations while leaving stable-channel
  jobs and immutable action-code pins independent; keep the proposal manifest
  under `build-aux` so it is neither a repository-wide rustup override nor a
  privileged `.github` update target.
- **v0.1 product plan** — Define the separate device/channel sidebars,
  unprotected playback and guide scope, neighbor-friendly tunnel discovery,
  supported hardware sites, architecture, test strategy, and release contract.
  Add a countable dependency-aware task ledger that distinguishes completed
  foundations from the work still required for the first alpha.

### Changed

- **Windows local discovery compatibility** — Derive each attached IPv4
  network from the OS-reported prefix length, use the vendor-compatible limited
  broadcast from each Windows interface-bound socket, and continue to accept
  replies only from that interface prefix and discovery port. Other platforms
  retain directed subnet broadcast. Omit link-local IPv6 probes until the HTTP
  layer can preserve the required scope, preventing an unusable-only device
  row from failing selection before a lineup request can start.
- **Rust minimum version** — Deliberately adopt the Dependabot-proposed Rust
  1.98 toolchain across Cargo, exact-MSRV CI, and developer documentation, and
  preserve workflow whitespace during future synchronized promotions.

### Security

- **Playback component boundary** — Treat the seven structural GStreamer
  factories as a startup capability snapshot rather than a bundle allowlist.
  Future self-contained packages must derive and review the actual codec,
  parser, HTTP, MPEG-TS, audio, and video plugin/native closure, while continuing
  to reject libdvdcss, optical-disc copy-control/circumvention components, and
  proprietary DRM modules at staging, native-import, completed-tree, and
  reopened-artifact boundaries. Broad development/runtime packages do not grant
  authority to stage those unused components.
- **Release component input policy** — Add a shared, fail-closed optical-disc
  and proprietary-DRM token policy, a reviewed-policy checksum,
  protected-television module names relevant to HDHomeRun deployments,
  bounded UTF-8 source/build-input validation, and adversarial fixtures in CI
  and immutable-tag release checks. Staging, native-import, completed-tree,
  and reopened-artifact enforcement remains mandatory when the GTK/GStreamer
  packages are introduced.
- **Artifact-policy scaffolding** — Load the same checksum-pinned deny policy in
  preparatory Linux tree/archive and Flatpak-commit validators, fail closed on
  missing or mutated policy data, and derive negative fixtures without adding
  prohibited component names to test source. Linux tree inspection now applies
  bounded, hidden-inclusive before/after content manifests and rejects unsafe
  links; real packages still require archive preflight, source/resource bounds,
  extraction containment, and reopened-artifact gates.
- **Preparatory desktop package boundaries** — Fix a minimal Flatpak permission
  allowlist without host/media filesystem or broad device access, and adapt
  Tributary's macOS package inspector without its copy-any-plugin staging API;
  both remain synthetic gates until real Balun application packages land.
- **Bounded routed discovery** — Require an explicit RFC 1918 range no wider
  than `/24`, cap candidates and concurrency, rate-limit targeted starts, and
  keep point-to-point interfaces out of automatic local enumeration. Native
  Linux route inspection fails closed on policy routing, ambiguous next hops,
  and route-table precedence it cannot model safely.
- **Fail-closed automatic authority** — Reject ambiguous tunnel origins,
  changed topology or traffic policy, stale and expired run completion,
  backward-clock shortcuts, uncertain durability, and revoked or replaced
  active runs. A confirmed store publication also releases no permit until a
  fresh exact full-ledger and key-binding match. The sealed Linux socket,
  packet-free admission, whole-pair observer owner, and combined-epoch
  foundations are present, but automatic route-derived traffic remains
  disconnected until one production controller replaces the pair after each
  store publication, serializes final pre-send revalidation, and lands
  consuming-runner completion.
- **Authenticated route invalidation** — Accept Linux route notifications only
  from the kernel sender address, preflight every untrusted frame and attribute
  length without third-party payload logging, and poison authority on overflow,
  malformed input, observer loss, or budget exhaustion.
- **Bounded lineup parsing** — Enforce the channel-row limit while
  deserializing lineup JSON so oversized responses cannot allocate every raw
  row before rejection.
- **Untrusted packet boundaries** — Reject malformed, oversized, bad-CRC, and
  inconsistent discovery replies and restrict broadcast or multicast replies
  to the directly attached prefix that was probed.
- **Pinned device HTTP** — Rebind advertised hosts to the validated responder,
  require the observed HDHomeRun metadata and stream ports, reject credentials
  and redirects, bypass proxies and DNS, cap response work, and verify
  `/discover.json` identity before accepting a lineup.
- **Bounded long-lived registry** — Cap devices, locators, and origins and
  reject conflicting DeviceID ownership until reassignment is independently
  confirmed.

### Privacy

- **Local and explicit discovery only** — Use local-interface discovery or
  targets and private ranges supplied directly by the user; do not contact a
  cloud discovery service, analytics service, or guide scraper.
- **Credential-safe diagnostics** — Hide advertised URL values, never
  deserialize, persist, or print `DeviceAuth`, and wipe the bounded metadata
  response buffer after use.
- **Topology-redacted approvals** — Keep route summaries ephemeral and redact
  keys, fingerprints, targets, interface names, and prefixes from default
  debug output; the durable store retains only keyed fingerprints and bounded
  policy metadata rather than raw route topology.
