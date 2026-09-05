# Balun v0.1 implementation backlog

Last audited: 2026-09-04

This is the executable work ledger for `v0.1.0`. Scope, architecture,
and delivery order are authoritative in [`plan-v0.1.md`](plan-v0.1.md);
sanitized real-device evidence belongs in
[`compatibility-v0.1.md`](compatibility-v0.1.md); merged user-visible outcomes
belong in [`../CHANGELOG.md`](../CHANGELOG.md). The original 64-record ledger is
archived at 24/64 in [`task-foundation-2026-09.md`](task-foundation-2026-09.md),
and the decisions behind this restart are in
[ADR-0002](architecture/adr-0002-scope-and-diagnostics.md).

## How to use this file

- Work from the earliest unchecked record whose prerequisites are satisfied.
- A top-level checkbox is one countable outcome. Check it only when its code,
  deterministic tests, relevant documentation, and changelog entry have
  landed on `main`.
- Keep partially implemented records unchecked; do not treat scaffolding as
  completion.
- Split work into reviewable pull requests, but do not weaken the network,
  identity, tuner-release, privacy, or package-inspection contracts to make a
  slice fit.
- Record physical-device results without device IDs, addresses, channel
  names, credentials, or raw network topology.
- Records are one to three lines. Evidence, measurements, and status prose go
  in the compatibility notes, the changelog, or a design document, not here.
- Recount the literal top-level checkboxes whenever a record is added, split,
  completed, or removed.

Current status: **28/30 (93.3%)** records complete. This is a dependency ledger,
not an effort estimate; packaging and routed-discovery records are larger than
most evidence records.

## Current focus

The remaining records are P4.1 and P4.5. P4.1 still needs packaged live-tuner
acceptance across Linux, macOS, and Windows; the recorded Linux and Windows
trials used development builds, and the macOS validation path does not supply
the missing cross-platform evidence. P4.5 has an Alpha draft with the exact
12-artifact inventory and checksums from the release-candidate workflow.
The maintainer approved an unsigned annotated `v0.1.0` tag for this release;
signed annotated tags remain the procedure for subsequent releases. Publication
is still pending. An unpublished draft does not count as a release.

## P0 — Evidence and contract

- [x] **P0.1 — Record the Windows live-TV result.** Add the sanitized owner
  trial to the compatibility notes: ATSC 1.0 plays with audio, ATSC 3.0 fails
  closed on AC-4, and discovery, switching, Stop, and close behave as expected.

- [x] **P0.2 — Linux live-TV acceptance on real hardware.** Same checklist as
  P0.1 on the Linux development build, including audio.

- [x] **P0.3 — macOS live-TV acceptance on real hardware.** Same checklist as
  P0.1 on the macOS development build, including audio.

- [x] **P0.4 — Measure tune and teardown budgets.** Record first-frame time,
  channel-switch time, and tuner-release time on one device, and confirm the
  tuner is released on switch, Stop, device change, and window close.

- [x] **P0.5 — Freeze the per-platform plugin and codec contract.** From P0.1
  to P0.3, record the exact GStreamer factories, decoders, and audio sinks each
  platform uses; this is the input to the packaged runtime closure.

- [x] **P0.6 — Land sanitized fixtures from real devices.** Add representative
  discover replies, device JSON, lineup JSON, and HTTP failures without
  topology, authentication, or channel data.

- [x] **P0.7 — Prove the exact-address probe on real hardware.** Exercise and
  document one exact-address probe against an accessible tuner.

- [x] **P0.8 — Run the in-band guide spike.** In one day, observe whether
  PSIP/EIT tables survive the device PID filter on an active stream and record
  the result; it gates the v0.2 guide candidate.

## P1 — Viewer completion

- [x] **P1.1 — Add versioned settings.** Persist remembered targets, friendly
  names, and window state as atomic, migration-tested JSON; never credentials,
  stream URLs, or incidental topology.

- [x] **P1.2 — Remember targets and admit hostnames.** Rediscover persisted
  exact targets at startup and accept a hostname resolved to a bounded set of
  unicast addresses; neither becomes prefix-scan authority.

- [x] **P1.3 — Let errors and diagnostics name the device.** Per ADR-0002,
  failure copy and `--inspect` output may show the device name, address, and
  DeviceID suffix; `DeviceAuth` and credentials stay redacted.

- [x] **P1.4 — Name the missing codec.** The failure copy names the stream
  type from a closed list (AC-4 audio, HEVC video, and the ATSC 1.0 set).
  AC-4 channels keep failing closed in v0.1; video-only playback is declined.

- [x] **P1.5 — Add channel search and a favorites filter.** Filter the selected
  device's lineup without changing device or channel identity.

- [x] **P1.6 — Complete keyboard navigation and accessibility.** Review both
  sidebars and the player for focus order, labels, and keyboard operation.

## P2 — Route-table-derived discovery (Linux)

- [x] **P2.1 — Connect the monitored routed runner.** Replace and rebaseline
  the observer pair after store publication, serialize the final pre-send
  check, consume the sealed socket, and settle reservation completion.

- [x] **P2.2 — Add routed-discovery UX.** Preview candidates and packet budget,
  require explicit approval, and expose progress, cancel, cooldown, backoff,
  and revocation.

- [x] **P2.3 — Reconcile network changes.** Debounce adapter and route changes,
  expire stale evidence, cancel invalid authority synchronously, and keep
  devices that retain another valid locator.

- [x] **P2.4 — Complete discovery diagnostics.** Report probe counts, accepted
  and rejected replies, provider availability, and failure classes without
  persisting unrelated topology.

- [x] **P2.5 — Pass routed and multi-site validation.** Prove one routed case on
  the owner's tunnel where broadcast does not cross, keep local and remote
  devices separate across both sites, and measure the traffic budget.

## P3 — Packages

- [x] **P3.1 — Add desktop metadata and assets.** Land the icon, desktop entry,
  AppStream metadata, and a screenshot with exact Balun identity data.

- [x] **P3.2 — Build the Linux package set.** Validate Flatpak x86_64/aarch64,
  Debian amd64/arm64, RPM x86_64/aarch64, and Arch x86_64 from locked inputs;
  reopen every artifact through its format-specific gates.

- [x] **P3.3 — Build the Windows ZIPs and installers.** Stage strict x86_64 and
  ARM64 GTK/GStreamer closures, validate PE architecture, imports, and the
  completed tree, and reopen all four artifacts.

- [x] **P3.4 — Build the macOS arm64 app and DMG.** Complete the app tree,
  runtime closure, Mach-O inspection, signing policy, and reopened DMG check.

- [x] **P3.5 — Complete release automation.** Build from one annotated tag
  (signed after v0.1.0), require 12 public binaries and `SHA256SUMS.txt`, create a draft,
  and confine release-write authority to the final no-source job.

- [x] **P3.6 — Harden CI for packages.** Add the dependency audit and
  Markdown, TOML, YAML, and GitHub Actions linting.

## P4 — v0.1.0

- [ ] **P4.1 — Validate packaged artifacts on every platform.** Record
  launch, discover, tune, switch, and close on Linux (Wayland and X11), macOS,
  and Windows candidates, with startup, idle, and switch budgets.

- [x] **P4.2 — Complete the sanitized hardware matrix.** Cover the accessible
  primary-site and secondary-site devices; defer the Australian units without
  claiming regional support.

- [x] **P4.3 — Complete the security and privacy review.** Re-audit network
  admission, persisted state, logs, package contents, and unexpected
  tuner-allocation paths.

- [x] **P4.4 — Publish the support matrix and minimal governance docs.** Name
  supported devices, platforms, codecs, and limitations from evidence; add
  CONTRIBUTING and SECURITY.

- [ ] **P4.5 — Cut and publish v0.1.0.** Pass every gate, have the maintainer
  create an annotated `v0.1.0` tag (unsigned by explicit approval), validate the artifact set, and publish
  release notes.

## Explicitly outside v0.1

- Guide data: XMLTV, the HDHomeRun XMLTV API, and an in-band crawl of each
  full multiplex are v0.2 candidates; P0.8 ruled out now/next from the playing
  stream.
- Native macOS and Windows route-table providers and observers.
- SBOM, build provenance, scheduled fuzzing, and a coverage ratchet: beta.
- Code of conduct, support policy, and issue forms until there are
  contributors.
- Decryption, DRM bypass, CableCARD protected-channel playback, or any
  optical-disc copy-control component.
- Recording, timeshift, tuner configuration, cloud guide scraping, telemetry,
  or automatic broad subnet scanning.
- A guarantee of ATSC 3.0 AC-4 playback on any platform.

## Archived foundation

The following records were completed under the original ledger and are
preserved verbatim in [`task-foundation-2026-09.md`](task-foundation-2026-09.md):
M0.2, M0.3, M0.5, M1.1, M1.2, M1.3, M1.4, M1.7, M1.8, M1.9, M2.1, M2.2, M2.3,
M2.4, M2.5, M2.6, M2.7, M2.8, M2.9, M3.1, M3.2, M3.3, M3.8, and M5.1. The
fake-device teardown-release proofs that closed the test-side half of M2.10
are recorded there and in the changelog; P0.4 carries its live-device
remainder.
