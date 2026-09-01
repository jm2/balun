# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **GTK development shell** — Add an opt-in `balun` desktop binary using GTK 4
  and libadwaita at Tributary's proven API floors, with adaptive nested device,
  channel, and live-TV panes, truthful packet-free empty states, exact
  application identity, Linux desktop compile/link/lint coverage, and MSRV
  checking without adding GTK to the default library or diagnostic build. A
  local GTK 4 Broadway smoke verifies display initialization and event-loop
  entry, while an isolated Xvfb/D-Bus smoke exercises the ordinary window
  close path and requires a successful joined controller shutdown.
- **Cross-platform desktop link checks** — Compile and link the feature-gated
  shell against native Homebrew GTK/libadwaita on macOS arm64 and an MSYS2
  CLANG64 GTK/libadwaita environment on Windows x86_64, without claiming a
  runnable bundle or package.
- **Windows desktop build helper** — Make the same-named Tributary-derived
  `build-windows.ps1` route a no-flag invocation to a build-only locked release
  of `balun.exe`, auto-detect and configure an MSYS2 CLANG64 environment, keep
  launch explicit behind `-Run`, and preserve the GTK-free
  `balun-discover.exe` route behind `-Diagnostic`. Bundle, ZIP, Inno Setup, and
  dependency-update modes remain fail-closed; the output is not yet a portable
  bundle or installer.
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
- **Packet-free controller runtime** — Own local discovery and the device
  registry on one named current-thread Tokio worker, behind an eight-command
  nonblocking ingress and coalesced immutable snapshot receiver. Startup is
  inert; only an explicit refresh invokes discovery, supersession cancels and
  joins its predecessor, successful scans atomically replace the local view,
  failed scans retain last-good devices, and queue-independent shutdown wins
  races before joining the worker. An independent selection lane resolves
  exactly one registered device, cancels and joins superseded work, re-resolves
  after a successful registry refresh, rejects stale completions, retains
  stream URLs only inside the actor, and publishes URL-free metadata and
  channel rows.
- **Connected device and channel sidebars** — Reduce complete controller
  snapshots on the GLib main context into virtualized device and selected-
  lineup models, restore selection by stable DeviceID or ChannelKey rather
  than list position, strictly reset recycled rows, keep protected channels
  visible but disabled, and expose discovery only through an explicit Refresh
  action. Selecting a device never starts playback.
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

### Security

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
