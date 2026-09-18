# Balun implementation backlog

Last audited: 2026-09-17; see the [reassessment](backlog-review-2026-09-17.md).

This is the executable work ledger for v0.1.x hardening and the v0.2 roadmap,
including the historical v0.1.0 records. Existing architecture and safety
contracts remain authoritative in [`plan-v0.1.md`](plan-v0.1.md);
sanitized real-device evidence belongs in
[`compatibility-v0.1.md`](compatibility-v0.1.md); merged user-visible outcomes
belong in [`../CHANGELOG.md`](../CHANGELOG.md). The original 64-record ledger is
archived at 24/64 in [`task-foundation-2026-09.md`](task-foundation-2026-09.md),
and the decisions behind this restart are in
[ADR-0002](architecture/adr-0002-scope-and-diagnostics.md).

The [adopted September review](review-and-backlog-proposal-2026-09.md) supplies
the evidence and acceptance details for H0–H4 and V2 below. Its implementation
order supersedes the original delivery order for post-alpha work. New records
remain unchecked; adopting work does not complete its implementation.
The September 17 reassessment retains these outcomes and prerequisites, records the
completed CI repair, and identifies items awaiting maintainer
decisions or physical-platform evidence without counting those as completed.

## How to use this file

- Start with H0, then eligible H1/H3 work. H2/H4 are beta assurance tracks;
  V2 work follows its stated prerequisites. P4.1 remains the single packaged
  live-tuner acceptance record. Work the earliest eligible record in a track.
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
- The maintainer owns triage and release decisions; record the implementer in
  the linked issue or pull request when work is claimed. Track names are target
  milestones, not release dates or permission to weaken an existing contract.

Current status: **39/58 (67.2%)** records complete: historical P0–P4 **29/30**,
hardening and assurance H0–H4 **10/21**, and roadmap V2 **0/7**. This is a
dependency ledger, not an effort estimate. P4.1 is carried forward once.

## Current focus

The CI baseline repair landed in [PR #96](https://github.com/jm2/balun/pull/96) with all
ten jobs passing. H0's five corrections are complete. Establish the remaining package
guarantees in H1 before claiming them for a new release. H3 maintains privacy and the
review record. H2/H4 may proceed independently and do not delay a corrective patch
solely to complete beta infrastructure. P4.1 follows the relevant fixes and candidate
builds; development-build evidence does not complete it.

P4.5 records publication of "Balun v0.1.0 Alpha" on 2026-09-05 with the
12-artifact inventory and `SHA256SUMS.txt`. The initial tag was unsigned by
maintainer approval; packaged live-tuner acceptance was still incomplete.
Signed annotated tags remain the procedure for later releases; H2.1 adds
machine enforcement of the chosen source policy.

## H0 — Confirmed defects for v0.1.x

- [x] **H0.1 — Revalidate routed sends after readiness ([#85]).** Recheck
  authority, deadline, and pin before every nonblocking attempt; pending-send
  and retry regressions are [recorded here](security-review-v0.1.md#2026-09-17-routed-send-correction-h01).

- [x] **H0.2 — Serialize source retirement and startup ([#88]).** One lifecycle
  lock accounts for all admitted and partially started workers in teardown;
  forced overlap, failed-spawn, poison, and successor tests prove joined release.

- [x] **H0.3 — Bound resolver shutdown ([#89]).** Cap actual system lookups at
  four process-wide, with no queue; timed-out or cancelled jobs retain their
  slots until exit, while controller close and stale-result rejection stay bounded.

- [x] **H0.4 — Prove the full macOS native closure ([#86]).** Resolve every
  non-system dependency inside the final app, including pixbuf loaders, and
  reject external/unresolved references in the signed app and reopened DMG.

- [x] **H0.5 — Bind Windows reuse to the whole probed tree ([#87]).** Require
  exact payload identity or a fresh runtime probe for installer-only reuse;
  reject changed, missing, extra, and aliased non-anchor inputs.

## H1 — Package and native dependency assurance

- [x] **H1.1 — Inspect the completed Windows installer ([#90]).** After H0.5,
  safely extract and compare both architecture payloads with the validated
  tree, repeating native/component checks before upload.

- [x] **H1.2 — Pin the Windows component policy ([#92]).** Apply the shared
  digest, bounded strict text parsing, and regular non-reparse-file checks;
  prove invalid policy inputs fail before build, copy, or probe.

- [ ] **H1.3 — Inventory each shipped native runtime ([#91]).** Bind component
  versions, source identities, licenses, and hashes to final artifact members;
  emit an SBOM and distinguish bundled code from externally managed runtimes.

- [ ] **H1.4 — Establish native advisory response ([#91]).** After H1.3,
  record applicability, exception owners/expiry, and rebuild expectations;
  exercise identification and rebuilding of an affected-version fixture.

## H2 — Beta release assurance

Maintainer direction on 2026-09-17: hold all new signing and provenance work.
No trusted signing identities are designated; H2.1 and H2.3 remain unchecked
and paused. Independent correctness and package-inspection fixes may continue.

- [ ] **H2.1 — Enforce release source identity.** Define trusted signers and
  reviewed-source ancestry, enforce the chosen tag policy before building,
  and test rejection paths while retaining the initial alpha exception.

- [ ] **H2.2 — Pin release build inputs.** Inventory and pin builder images,
  native package inputs, tools, and transitive installer dependencies; record
  reviewed exceptions and prove unapproved input drift is detected.

- [ ] **H2.3 — Attach build provenance.** After H2.1/H2.2, bind source and
  builder/input identity to final artifact digests, publish verifiable
  attestations, and test mismatched source or payload rejection.

- [ ] **H2.4 — Contain native archive inspection.** Preflight member paths,
  types, links, sizes, and extraction budgets before processing untrusted
  artifacts; retain the current trusted-local-output boundary until proven.

## H3 — Security and privacy maintenance

- [x] **H3.1 — Make JSON diagnostics value-free ([#93]).** Replace raw serde
  value echoes with safe categories across metadata, lineup, inspection, and
  CLI errors; prove secret-shaped markers cannot reach diagnostic output.

- [ ] **H3.2 — Define and enforce settings file trust.** Specify parent-path,
  no-follow, replacement, permission, and newer-schema guarantees; test them
  under concurrent replacement and slow I/O on each supported platform.

- [x] **H3.3 — Disposition the older low findings.** Pacing/jitter, CLI admission,
  and schema fixes are [recorded](discovery-low-findings-2026-09.md), together with
  maintainer-approved loopback and fingerprint-key limits, owners, and review triggers.

- [ ] **H3.4 — Refresh the security evidence.** After relevant H0/H1/H3 fixes,
  consolidate current guarantees, threat boundaries, exceptions, and test links
  at one commit; reconcile stale security, playback, release, and support prose.

- [ ] **H3.5 — Decide the native media failure boundary.** Document decoder
  trust, synchronous native-call and useful-media progress limits; use isolated
  stall fixtures to choose enforced recovery or explicit bounded claims.

## H4 — Beta regression and acceptance coverage

- [x] **H4.1 — Add sustained adversarial regression testing.** Replayable mutation
  and property suites cover packets, JSON/URLs, route/approval transitions, and
  package manifests in bounded PR and [scheduled corpora](adversarial-regressions.md).

- [ ] **H4.2 — Measure critical-path test coverage.** Establish a coverage
  baseline for admission, cancellation, identity, privacy, and package gates;
  ratchet meaningful missing branches without treating percentages as proof.

- [ ] **H4.3 — Validate packaged accessibility.** Record keyboard and screen
  reader behavior, dynamic status, focus recovery, high contrast, and large
  text on the platform matrix; coordinate evidence with P4.1 and V2.6.

## V2 — Feature roadmap with explicit prerequisites

- [ ] **V2.1 — Instrument tune and media progress ([#63]).** After H0.2/H0.3,
  measure each tune phase and usable-media deadlines, then retake package
  first-frame, switching, and release budgets before optimization.

- [ ] **V2.2 — Improve rapid switching and large lineups ([#63]).** After
  V2.1, coalesce activations, preserve a safe last frame, and diff lineup rows;
  prove one-stream ownership and measured responsiveness at 400-plus rows.

- [ ] **V2.3 — Decide and validate pipeline reuse ([#63]).** After V2.1 and
  H3.5, compare a reuse prototype with the baseline and retain teardown proofs;
  speculative extra-tuner allocation requires a separate approved design.

- [ ] **V2.4 — Add explicitly approved subnet discovery ([#71]).** After
  H0.1, approve one consistent candidate/datagram/deadline contract before
  implementation; preserve explicit consent and cross-platform cancellation.

- [ ] **V2.5 — Complete deinterlacing quality evidence ([#78]).** Retain the
  implemented YADIF policy and diagnostics; measure mixed fields and CPU use,
  decide remaining GPU/film work from evidence rather than an obsolete default.

- [ ] **V2.6 — Localize the interface ([#74]).** Add catalogs, pluralization,
  locale fallback, and translated accessibility copy; verify missing keys and
  long-string layouts across the supported locales.

- [ ] **V2.7 — Prove mobile/TV prerequisites ([#64]).** After H0 and V2.1,
  validate GTK-free playback, sink/network boundaries, and each platform's
  feasibility; split proven platform milestones before implementing shells.

## Historical v0.1.0 records

Completed outcomes below remain historical evidence; the H records track
newly discovered defects. P4.1 remains open and is not counted again above.

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

- [x] **P4.5 — Cut and publish v0.1.0.** Validate the release artifact set,
  publish notes and the annotated tag (unsigned by explicit approval), and
  record that packaged live-tuner acceptance in P4.1 remains outstanding.

## Outside the adopted implementation scope

- Guide data: XMLTV, the HDHomeRun XMLTV API, and an in-band crawl of each
  full multiplex are v0.2 candidates; P0.8 ruled out now/next from the playing
  stream.
- Native macOS and Windows route-table providers and observers.
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

[#63]: https://github.com/jm2/balun/issues/63
[#64]: https://github.com/jm2/balun/issues/64
[#71]: https://github.com/jm2/balun/issues/71
[#74]: https://github.com/jm2/balun/issues/74
[#78]: https://github.com/jm2/balun/issues/78
[#85]: https://github.com/jm2/balun/issues/85
[#86]: https://github.com/jm2/balun/issues/86
[#87]: https://github.com/jm2/balun/issues/87
[#88]: https://github.com/jm2/balun/issues/88
[#89]: https://github.com/jm2/balun/issues/89
[#90]: https://github.com/jm2/balun/issues/90
[#91]: https://github.com/jm2/balun/issues/91
[#92]: https://github.com/jm2/balun/issues/92
[#93]: https://github.com/jm2/balun/issues/93
