# Balun v0.1 security and privacy review

Reviewed: 2026-09-03 at main `8c0df0ed94b3f0db49d9baa57ab8820256f7c736` (ledger
record P4.3). The package section also covers the Flatpak package as merged
into main in `946a2ee`.

Contracts audited: [`plan-v0.1.md`](plan-v0.1.md) §5-§8,
[ADR-0001](architecture/adr-0001-discovery-playback.md),
[ADR-0002](architecture/adr-0002-scope-and-diagnostics.md),
[`release-component-policy.md`](release-component-policy.md), and
[`playback.md`](playback.md). Every claim names the file and function it was
checked in. The default and `desktop` test suites, strict Clippy, and
`cargo audit` pass with the fixes applied; the live-hardware tests were not run.

## 2026-09-17 playback description correction

The interface review found a rendering-boundary gap in
`src/ui/player_view.rs`. Device and channel names admit printable punctuation,
but [AdwStatusPage descriptions](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/class.StatusPage.html)
interpret markup. Unescaped names such as `News & Weather` could make a status
description invalid; tag-like names could alter its formatting. This affects
the connecting message and device-specific playback failures.

Playback descriptions now compose ordinary text and escape it exactly once at
the widget boundary, including initial/idle/failure copy. Titles and ordinary
labels keep their plain-text API. Parsed names and diagnostic text are unchanged.
Pango regressions cover ampersands, formatting tags, link-like text, existing
entities, quotes, and Unicode across every pipeline-failure category. They
require the original rendered text and no formatting attributes. The native
player smoke also exercises the production connecting and failure setters.

This targeted fix does not complete H3.4's broader refreshed audit or H4.3's
platform accessibility review.

## 2026-09-17 routed-send correction (H0.1)

The historical pre-send proof below did not cover an asynchronous send waiting
for write readiness. [Issue #85](https://github.com/jm2/balun/issues/85) identified
that gap. The pinned sender now waits for readiness, rechecks current authority
(including cancellation/epoch invalidation and the deadline) and both kernel
pin views, then attempts a synchronous nonblocking send without another await.
Every `WouldBlock` retry repeats the same checks. This closes the pending-send
gap without relying on cancellation-select ordering or changing packet budgets.

Deterministic Linux regressions in `src/discovery/routed/linux.rs` cover a
pending request cancelled, invalidated, or expired before readiness; a failed
kernel pin readback; revocation after `WouldBlock`; and a valid retry sending
exactly one datagram. The receiver reads the kernel directly to verify that
refused attempts produce no packet. Existing monitored-runner tests retain
reservation completion, invalidation, and cancellation coverage.

This is a targeted correction, not H3.4's refreshed audit. The historical
summary and findings below describe their reviewed revisions; the other open
findings in [the adopted review](review-and-backlog-proposal-2026-09.md) remain
tracked in [the active ledger](task.md).

## Summary

### 2026-09-17 hostname worker lifetime (H0.3)

[Issue #89](https://github.com/jm2/balun/issues/89) showed that the five-second
async timeout did not stop a system resolver job in Tokio's blocking pool;
dropping the controller runtime could therefore wait indefinitely. System
lookups now use separately owned threads under one process-wide four-slot
semaphore. Admission never queues, and the worker holds its slot until it
actually exits, including after timeout, caller cancellation, or controller
shutdown. All controller instances and the public resolver share this limit.

The OS resolver remains non-cancellable. Up to four native threads can remain
for an unbounded duration, until their system calls return or the process
exits. This is a bounded worker-count policy, not a claim that the OS lookup
itself finishes within five seconds. At saturation, further names return the
fixed `Busy` category; direct IP entry remains available. No controller Tokio
runtime owns these jobs, so they cannot delay its destruction or window close.

Workers only send to a private one-shot result channel. Timeout, dropped
receivers, and shutdown discard late results; workers never initiate device
probes. `hostname::resolver::tests` uses a blocked resolver and virtual time to
prove timeout/cancellation retain admission, repeated submissions add no work,
and capacity returns on actual exit. The controller regression keeps a resolver
blocked past timeout, checks 100 rejected submissions, closes the controller,
and starts a replacement that still observes the occupied slot, without a
discovery service call. Normal address filtering and the four-address cap remain.

### 2026-09-17 macOS native closure (H0.4)

[Issue #86](https://github.com/jm2/balun/issues/86) reproduced a dependency under
an arbitrary external prefix that the old final check silently accepted.
`scripts/macos_native_closure.py` now reads bounded Mach-O headers and every
architecture's load commands. It enumerates all regular native members, including
scanner/query helpers, frameworks, and dynamically loaded GStreamer/pixbuf modules.
The completed-tree component gate brackets closure inspection with its existing
content manifests, so changes during inspection invalidate the result.

Every non-system dependency must resolve to a compatible native slice inside the
canonical app root. Loader and executable tokens use their owning contexts;
run paths include the importing chain, and an external run-path candidate rejects
the package even if a later bundled candidate exists. Dynamic modules must work
under every compatible packaged executable context. Absolute non-system install
names, bare relative names, missing dependencies, symlink escapes, malformed
headers, and dyld environment overrides fail closed. Weak dependencies receive
the same closure requirement. Only normalized `/usr/lib/`,
`/System/Library/Frameworks/`, and `/System/Library/PrivateFrameworks/` install
names may remain external: Apple supplies these from the OS, including its shared
cache. This policy does not assert their availability on every older OS version.

The packager extracts complete import strings, including spaces, and stops on
failed copies/rewrites or an exhausted traversal. It repeats completed-tree and
closure validation after ad-hoc signing, after the runtime probe, and on the
read-only reopened DMG. The existing relocated playback probe now uses a checked-in
`sandbox-exec` profile denying reads from the actual Homebrew prefix and both
standard Homebrew roots. The app, home, and caches are relocated to a canonical
scratch directory outside those roots; an unsuitable `TMPDIR` fails with an
explicit diagnostic. A native fixture launches a relocated copy from a checkout
inside the denied prefix. This is packaging evidence, not a decoder sandbox or new
signing/provenance policy.

Portable fixtures cover malformed headers, hidden fat slices, loader/run-path
resolution, pixbuf imports, path escapes, architecture mismatch, and valid
transitive closure. The native CI fixture compiles real Mach-O files, reproduces
external/missing/pixbuf dependencies, proves the probe profile denies the vendor
library, and launches the valid bundled case after deleting that external library.
Physical packaged-tuner acceptance remains P4.1; archive containment remains H2.4.

### 2026-09-17 Windows complete-tree probe receipt (H0.5)

[Issue #87](https://github.com/jm2/balun/issues/87) reproduced a changed or deleted
non-anchor DLL retaining the four-file runtime-probe receipt. Version 3 binds the
whole staged tree: ordinal, case-collision-checked relative paths, member types,
file sizes, and SHA-256 content hashes, including hidden files and empty directories.
It also binds the selected profile and local packaging helper, Cargo manifest/lock,
component-policy file, and Inno recipe. Reparse points, hard-link aliases, unsafe
paths, and Windows alternate data streams are refused.

Manifest budgets are 65,536 members, 64 path components, 1,024 path characters,
1 GiB per file, 4 GiB total file bytes, 16 MiB manifest text, and five minutes.
Files are hashed in bounded chunks with a post-read size/write-time check; Windows
opens exclude concurrent writes/deletes. The receipt itself is at most 4 KiB and
strictly decoded as UTF-8. A pre-probe manifest must match before a receipt is
written; final gates and the last step before invoking Inno revalidate it.

The portable PowerShell regressions exercise both profiles, every former non-anchor
class, same-size/write-time modifications, missing/extra members, aliases, policy
changes, malformed/legacy receipts, and mutation while the probe runs. Native
Windows CI changes only the COFF timestamp of the already-probed GStreamer core
DLL, preserving its code/imports/exports and PE structure, verifies receipt rejection,
restores it, and verifies acceptance again.

This receipt is not an authenticity boundary against someone able to rewrite both
payload and receipt. The separate H1.1 gate below inspects the completed installer.
No signing identities, release attestations, or new provenance work are introduced.

### 2026-09-17 Windows policy snapshot (H1.2)

[Issue #92](https://github.com/jm2/balun/issues/92) identified that Windows accepted
syntactically valid replacement policy tokens without checking the reviewed digest.
The native Windows loader now opens the leaf with `FILE_FLAG_OPEN_REPARSE_POINT`,
checks the opened disk-file handle's attributes, size, and single-link count, and
shares it for reads only. A bounded byte snapshot is strictly decoded without NUL,
hashed against the shared checksum, then parsed using the shared token/line limits.
The digest and parser never reread a pathname. No source-directory immutability or
parent-path trust stronger than the existing local-checkout boundary is claimed.

Portable and native PowerShell regressions cover replaced/deleted tokens, invalid
digest, malformed UTF-8, NUL, oversized bytes, aliases/reparse inputs, case-folding,
duplicates, and each syntax limit. Routing fixtures verify refusal before downstream
packaging tools or Cargo. The [component-policy document](release-component-policy.md)
records the historical overclaim and the exercised replacement guarantee.

### 2026-09-17 completed Windows installer payload (H1.1)

[Issue #90](https://github.com/jm2/balun/issues/90) is covered by the
[installer inspection gate](windows-installer-inspection.md). After compilation,
a pinned non-executing tool supplies raw paths, sizes, and SHA-256 hashes for
bounded preflight and exact comparison with staging. Only a matching manifest
can trigger extraction into a fresh scratch directory. The extracted tree must
match again, pass the native/resource gates and relocated runtime probe, and
remain identical through the final check. The installer is held read-only on
Windows and its content hash is checked before and after inspection.

Portable regressions prove changed, missing, extra, escaping, conflicting,
oversized, and unsupported member declarations cannot start extraction. They
also exercise wrong PE architecture and mutation during the final probe. The
document records the separate static comparison of both published v0.1.0
installers with their ZIPs. Both native Windows CI lanes must validate the new
implementation before merge. P4.1 installed playback and H2.4 containment for
arbitrary untrusted native archives remain separate outcomes.

### 2026-09-17 value-free JSON diagnostics (H3.1)

[Issue #93](https://github.com/jm2/balun/issues/93) demonstrated that a mistyped
JSON field could echo a secret-shaped URL through serde's error text. The earlier
claim that these errors could never include a URL was incorrect; ignoring the
`DeviceAuth` field did not prevent a value in another field from being echoed.

Both device metadata and lineup parsing now convert serde errors immediately into
`JsonParseError`: a fixed I/O/syntax/data/end-of-input category and numeric line/column
positions. The original error is discarded, including its source chain. `Display`,
`Debug`, and nested `DeviceSnapshotError`/`LineupFetchError` sources therefore cannot
recover a device-chosen JSON value. The public category/position accessors preserve
useful classification without changing response bounds or identity validation.

Regressions prove the raw serde error would expose a synthetic credential/query URL,
then verify its absence from parser errors, both debug formats, every nested source,
inspection issue messages/report debug, and the actual CLI stderr writer. Cases cover
wrong field types, arbitrary strings, malformed/truncated JSON, trailing data, ignored
authorization fields, and an I/O source carrying the marker. No real credential is used.
This closes the prior JSON diagnostic exception; it is not the broader H3.4 audit.

### 2026-09-17 accepted native media failure boundary (H3.5)

The maintainer accepted [in-process native decoding with explicit limits](native-media-failure-boundary.md).
A native call can block the UI and close path before any timed wait is reached;
the five-second teardown deadline is not a universal release guarantee. Native
code shares application memory and is not confined by Rust's unsafe-code ban.
Network progress and first body bytes do not establish useful-media progress.
No independent useful-media deadline is currently enforced.

Owner: `jm2`; review before beta and after any reproduced native hang. The
isolated startup/teardown stall fixture records why later timed waits cannot
supply recovery. This accepted boundary supersedes unconditional teardown-bound
wording in the historical review below. H3.4's consolidated review remains open.

### Historical review summary

H3.3 corrections and maintainer-approved bounded acceptances are recorded in the
[September 17 finding dispositions](discovery-low-findings-2026-09.md).
Routed positive jitter, stricter CLI admission/reply budgets, and a distinct
newer-schema quarantine supersede the corresponding historical findings below.
The maintainer accepted the library-loopback and sibling-key boundaries on
September 17, 2026, with `jm2` as owner and the linked review triggers. H3.3
completes when these corrections and dispositions land on `main`.

H0.2 / [#88](https://github.com/jm2/balun/issues/88) also has a targeted
[startup/retirement correction](playback.md#source-startup-and-retirement-correction-2026-09-17):
admission and worker ownership now share one lifecycle lock, including partial
thread-creation failures. Its deterministic overlap evidence is described there.
The following table remains the historical September 3 review summary.

| Area | Result | Section |
| --- | --- | --- |
| 1. Network admission | Pass; Low findings open | [§1](#1-network-admission) |
| 2. Persisted state | Pass with fixes | [§2](#2-persisted-state) |
| 3. Logs and diagnostics | Pass with fixes | [§3](#3-logs-and-diagnostics) |
| 4. Package contents and CI | Pass with fixes | [§4](#4-package-contents-and-ci) |
| 5. Tuner-allocation paths | Pass | [§5](#5-unexpected-tuner-allocation-paths) |

No High or Medium finding is open. "Fixed here" means the fix landed in the
review's pull request.

## 1. Network admission

### Verified

- The controller sends nothing on its own: `ControllerActor::new` opens no socket and
  only `RefreshLocalDiscovery` and `DiscoverExact` reach `start_discovery`
  (`src/controller/runtime.rs`); the window queues one local discovery at
  launch and seeds `RediscoveryQueue` from remembered targets alone, one per
  settled lane (`src/ui/window.rs`
  `advance_rediscovery`, `src/controller/remembered.rs`). Remembered hostnames
  are resolved once before their probe. Test `construction_is_inert`.
- Local discovery is bounded per interface: `ProbeConfig::default` is 2 requests
  with 200 ms windows, 256 datagrams, and 64 devices per socket; one socket per
  eligible interface address (`src/discovery/local.rs` `ipv4_endpoint`,
  `ipv6_endpoint`); replies accepted only from the probed prefix on port 65001
  (`src/discovery/client.rs` `source_matches`, `validate_endpoint`).
- Exact probes are unicast-only: `exact_probe_config` is 2 requests, 200 ms, 16
  datagrams, one identity; `MAX_EXACT_DISCOVERY_TARGETS_PER_SESSION` (32) is
  checked before I/O in `start_discovery`; `validate_address`
  (`src/discovery/manual.rs`) refuses URLs, ports, ranges, unspecified,
  loopback, multicast, limited broadcast, IPv4-mapped, and scoped IPv6 input.
  `resolve_hostname` (`src/discovery/hostname.rs`) is bounded to 5 s and four
  addresses, each revalidated through `ExactDiscoveryTarget::from_ip`.
- Approved ranges: `ApprovedIpv4Range::new` (`src/discovery/routed.rs`) requires
  `/24` or narrower inside RFC 1918 (`routes.rs` `wholly_rfc1918`); 256
  candidates, 64 datagrams/s, 16 in flight, 15 s default deadline, paced in
  `scan_approved_targets_until` with cancellation; IPv6 is never enumerated;
  default, loopback, link-local, multicast, public, and directly connected
  networks are excluded (`InterfacePolicy::from_snapshot`, `direct_networks`).
- Routed runner: `MonitoredRoutedDiscovery::run_now`
  (`src/discovery/approval/controller/runner.rs`) registers in the current
  route and store epoch, reserves, rebaselines after its own publication,
  re-registers in the fresh epoch, and only then builds `AdmittedRoutedScan`;
  every datagram passes `PinnedProbeSocket::verify_before_send`
  (`src/discovery/routed/linux.rs`), proven by
  `pinned_probe_socket_reaches_a_loopback_responder_only_while_authority_holds`.
- Nothing scans automatically: no timer exists, `await_replacement` only
  rebaselines, and approval comes only from `from_user_approval` with 15 to 30
  minute cooldowns (`src/discovery/approval.rs`). The runner has no production
  caller yet (`allow(dead_code)`).
- Device HTTP: `normalize_url` (`src/hdhr/http.rs`) pins every advertised host
  to the responder with `set_ip_host` and rejects other schemes, credentials,
  query, fragment, port 0, host mismatch, cross-origin lineup URLs, and ports
  other than 80 and 5004; `DeviceHttpClient::new` sets `redirect::Policy::none()`,
  `referer(false)`, `no_proxy()`, and deadlines; `get_json` caps bodies at
  64 KiB and 4 MiB, `RawLineupVisitor` at 4096 rows. Numeric hosts mean no DNS.
  Test `rejects_redirects_without_contacting_the_location`.
- `balun-discover` (`main`): only the default local run, `--target`, and
  `--approved-range` send; `--inspect` fetches `discover.json` and `lineup.json`.

### Findings

- Low, open: plan §5 and ADR-0001 promise jitter on the routed sender;
  `target_start_spacing` is a fixed interval. The rate cap holds.
- Low, open: `DiscoveryClient` `invalid_target` does not refuse loopback; only
  the desktop parser does, so `balun-discover --target` can probe loopback.
- Low, accepted: the diagnostic's exact probe uses the library default budget
  (256 datagrams, 64 identities), and its `--approved-range` sends from an
  unpinned socket because it is an explicit user command.
- Low, accepted: IPv4 link-local and directed-broadcast addresses pass the
  exact-target parser; `SO_BROADCAST` is unset, so a send the local OS classifies
  as a broadcast fails. Clarified 2026-09-18: this does not establish downstream
  delivery behavior. V2.4's [accepted outbound-request boundary](typed-subnet-discovery-proposal.md#approved-delivery-boundary)
  records its downstream broadcast-blocking requirement and review before beta.

### Not covered

- macOS and Windows route providers (fail closed by design) and the routed
  approval UX, still in open pull requests.

## 2. Persisted state

### Verified

- `settings.json` (`src/settings/mod.rs`, `src/settings/store.rs`): pinned private
  profile and cooperative transaction lock, no-follow opens, regular single-link
  files, checked owner/modes on Unix and trusted inherited DACL on Windows;
  every save preserves a newer or malformed current document. The accepted
  [settings boundary](settings-file-trust.md) excludes shared/network profiles
  and hostile same-account writers; one worker and two-second load/close waits
  bound UI waiting without claiming cancellation of OS I/O. The schema uses
  `SCHEMA_VERSION` 2, `deny_unknown_fields` on every stored struct;
  `StoredSettingsV2` can hold only window state, remembered addresses or
  hostnames, and DeviceID-to-name pairs; `load` refuses symlinks, non-regular
  files, files over 64 KiB, newer schemas, and malformed documents with fixed
  errors and leaves the file untouched; `save` is temp file, fsync, rename,
  directory fsync. Test
  `serialized_document_is_versioned_and_carries_no_endpoints_or_secrets`.
- Routed approval store (`src/discovery/approval/store.rs`, Linux, library
  only): the state holds counters, run ids, and keyed BLAKE3 fingerprints
  (`StoredEnvelopeV1`; `fingerprint` in `approval.rs` hashes addresses,
  prefixes, and interface names into the digest); directory 0700, files 0600,
  owner and link-count checks on every read (`unix_entry_metadata`);
  `publish_state_bytes` is temp, fsync, rename, directory fsync;
  `read_state_locked` quarantines unknown fields, oversize, symlinks, and
  invalid content without rewriting. Test `strict_round_trip_persists_no_topology`.
- Nothing else is written by production code (grep of `fs::write`,
  `File::create`, `OpenOptions`, `tempfile`). No type carries `DeviceAuth`
  (`RawDeviceInfo` has no such field) and `LineupChannel` is not serializable.
- Live-hardware captures: `write_metadata_captures`
  (`src/playback/live_hardware.rs`) writes under `target/tmp/live-hardware/`
  (ignored through `/target/`) model, firmware, counts, and per-channel guide
  number, flags, and synthesized names; no address, DeviceID, name, or URL.
- Fixtures in `tests/fixtures/hdhr/`: RFC 5737 documentation hosts, the test
  DeviceIDs used by the unit tests, a placeholder `DeviceAuth`, synthesized
  guide names, and header-only 404/503 captures. No private address appears
  outside README examples and synthetic test data.

### Findings

- Low, fixed here: `settings.json` relied on the temporary file's default mode;
  `save` now requests 0600 explicitly and a Unix test asserts it.
- Low, open: the approval store keeps its fingerprint key beside the state it
  protects, so the key is an anti-correlation salt for a copied state file, not
  a secret held apart; document that threat model when P2.2 wires the store.
- Low, open: the approval store reports a newer schema as `InvalidState`,
  indistinguishable from corruption; add an unsupported-version reason.
- Low, accepted: live-hardware captures keep real guide numbers, model, and
  firmware under the ignored target tree; the fixture provenance records the
  renumbering applied before anything is committed.

### Not covered

- Non-Unix store permissions; no permit is ever released there.

## 3. Logs and diagnostics

### 2026-09-17 native diagnostic correction (H3.4 slice)

The earlier claim that a constant pipeline URI made native error text safe was
incorrect. A plugin can put stream-derived values into errors, debug strings,
caps fields, structure names, or stream identifiers. No actual credential leak
was observed; synthetic secret-shaped values demonstrate the logging path.

Balun's native playback reports now discard error/debug text and details, map
domains and factory names to closed labels, and retain only known GStreamer error
codes and typed counters. Unknown domains or out-of-table codes omit the code
field, including codes the Rust bindings would otherwise map to `Failed`.
Caps reports accept a closed media/format vocabulary and bounded
integer dimensions/rates; familiar field names do not authorize arbitrary text,
lists, or nested values. Stream collection summaries include at most 16 entries.
Application markers report only known categories. Unknown labels become fixed
`unknown`/`other` values. Deinterlacer reports expose the configured YADIF label
or `other`, never an arbitrary native enum nickname.
Their negotiated output rate uses a fixed vocabulary of standard frame-rate
fractions; other fractions become `other`, without printing either integer.
This changes diagnostics only, not which frame rates the pipeline can play.

`emitted_native_logs_discard_plugin_text_and_stream_values` captures the actual
tracing output for errors, warnings, missing plugins, stream collections,
selection, application markers, and pipeline diagnostics. Fixtures poison the
error domain and numeric code, source name, error/debug/details, caps name and fields, stream ID,
and collection ID; known event categories remain visible and none of the markers
appear. `diagnostic_caps_require_typed_bounded_fields_and_known_labels` rejects
mistyped/list/oversized fields and retains known audio/video formats.

This applies to Balun's tracing subscriber. Separately enabled `GST_DEBUG` and
other native libraries' own output bypass it; it is not a native-code sandbox.
It does not finish H3.4's consolidated review or decide H3.5's recovery policy.

### Verified

- Logging arrived on 2026-09-03: `tracing` with a standard-error subscriber
  (`src/logging.rs`, `RUST_LOG`, default `balun=info`). Log lines carry closed
  categories, the corrected native labels and numeric fields described above,
  HTTP status codes, the `Debug` of value-free
  error enums, and the device identity ADR-0002 allows; no logged type carries
  `DeviceAuth`, a query value, or a stream URL, and the redacted `Debug`
  implementations above were re-checked when the sites were added. The 17
  production print sites
  (`src/bin/balun-discover.rs`, `src/app.rs`, `src/ui/window.rs`,
  `src/ui/settings_session.rs`) interpolate fixed text, counters, `SocketAddr`,
  `DeviceId`, or value-free error enums; the opt-in `live_hardware.rs` harness
  prints model, counts, durations, caps, and factory names only.
- `DeviceAuth` is never deserialized: `RawDeviceInfo` (`src/hdhr/http.rs`) has
  no such field and `fetch_device_info` zeroizes the body. Test
  `fetches_metadata_without_referer_and_discards_device_auth`.
- Redacting `Debug` impls: `LineupChannel`, `DeviceLineup`, `DeviceSnapshot`,
  `DeviceSnapshotTarget`, `ResolvedDeviceSnapshot`, `StreamHandoff` (URI
  zeroized on drop, `with_uri` crate-private), `StreamHandoffReceiver`,
  `DeviceHttpClient`, `StorePaths`, `RouteFingerprintKey`, and now
  `DiscoveryObservation` and `LocatorClaim`.
- `DeviceHttpError::Transport` strips the URL with `without_url()` at all three
  construction sites; `EndpointError` renders roles, addresses, and ports only.
  New test `transport_failures_render_without_the_endpoint`.
- Snapshots (`src/controller/state.rs`): `DeviceSummary` carries a locator only,
  `ChannelSummary` no URL, and the status enums fail by category, never text.
  `balun-discover` prints DeviceID, address, method, interface, counts, name,
  model, and firmware; `advertised_url_summary` hides advertised URLs.
- Playback copy: `classify_pipeline_message` (`src/playback/pipeline_failure.rs`)
  reads only the error domain and code, the missing-plugin caps name, and the
  transport marker's one integer; `pipeline_failure_copy`
  (`src/ui/player_view.rs`) interpolates device and channel text only. Test
  `pipeline_failure_copy_is_exhaustive_stable_and_endpoint_free`.
- GStreamer receives only `PIPELINE_URI`: `GstreamerBackend::start`
  (`src/playback/session.rs`) sets and reads back `appsrc://balun` after
  `SourcePolicy::install` has taken the handoff; `StreamTransport::start`
  (`src/playback/transport.rs`) parses the URL inside the reader thread and
  posts failures as one integer category (`FailureSink::post`).

### Findings

- Low, fixed here: the derived `Debug` of `DiscoveryObservation` and
  `LocatorClaim` would print advertised URLs exactly as received, before the
  HTTP layer rejects credentials and query values; both now redact, with tests.
  No production path rendered them.
- Low, fixed here (test only): `DeviceHttpError::Transport` depends on
  `without_url()` at each construction site; the new test guards it.
- Low, resolved by H3.1 on 2026-09-17: `DeviceHttpError::Json` and
  `LineupError::Json` previously rendered serde's value-bearing message.
  Contrary to the earlier review text, a mistyped field could include a URL
  or secret-shaped value. Both errors now retain only category and position.
- Accepted under ADR-0002: `DeviceEndpoint`'s derived `Debug` shows
  responder-pinned URLs, and the lineup body is not zeroized.

### Not covered

- New log sites; re-audit any type first logged after this review against the
  `Debug` list above.

## 4. Package contents and CI

### Verified

- `ci.yml` and `release.yml`: top-level `permissions: contents: read`, no
  GitHub token permission elevation in any job, `persist-credentials: false`
  on every checkout, no secrets, no `pull_request_target`; `dtolnay/rust-toolchain` and `msys2/setup-msys2`
  pinned by SHA; actionlint pinned by SHA-256; markdownlint, taplo, and
  yamllint pinned by version; a `cargo audit` job; `--locked` on every cargo
  step; `Cargo.lock` committed; Dependabot for cargo, rust-toolchain, and
  actions; an exact-MSRV job.
- The Flatpak job is the one privilege exception: its container runs with
  `options: --privileged` because `flatpak-builder` needs to create its own
  bubblewrap sandbox inside the runner's container. That privilege is
  container-local; the job's token stays at the read-only default, it reads
  only the checked-out tree and the pinned `gnome-50` image, and its only
  output is the bundle uploaded as a seven-day workflow artifact.
- `release.yml`: `workflow_dispatch` with a regex-validated tag that must be
  annotated and agree with `Cargo.toml` and the changelog, builds from the
  resolved SHA, and uploads the diagnostic only.
- Component policy: `forbidden-bundled-components.txt` is pinned by SHA-256 in
  `validate-release-components.sh`; its fixture suite and the Linux, Flatpak,
  and macOS validators run in both workflows.
- Flatpak package (main `946a2ee`): `finish-args` are exactly `wayland`,
  `fallback-x11`, `ipc`, `pulseaudio`, `network`, and `dri`, enforced in
  canonical form and count by `validate-permissions.sh`; no filesystem, D-Bus,
  or `--device=all` grant; decoders come from the `ffmpeg-full` extension
  outside the app payload; the build is offline and `--locked` from the
  checksum-pinned generator's sources; `validate-bundle-compliance.sh` reopens
  the bundle in an isolated OSTree repository, requires one app ref, and runs
  the metadata and tree validators; `validate-bundle-runtime.sh` probes the
  installed bundle for the factories; the Flatpak jobs keep `contents: read`.
- Windows package (P3.3, re-audited 2026-09-03): `build-windows.ps1` stages
  only the plugin closure named in its `$GStreamerPluginClosure` table and the
  DLLs those binaries import, applies the pinned deny policy at every copy,
  during import traversal (a denied import fails the run), over the completed
  tree, and inside the reopened ZIP, and prunes stale plugins and unreachable
  DLLs from incremental trees. The packaged runtime probe runs the staged
  `balun.exe` with every `GST_*`, GIO, and proxy variable removed, `PATH` set to
  `System32` only, and `GST_REGISTRY` in a fresh temporary cache that is deleted
  afterwards; the Rust side (`src/playback/platform_runtime.rs`) rejects any
  other inherited policy key, requires the cache root to be absolute, fresh, and
  outside the package, and writes its sentinel last. Its loopback fixture server
  binds `127.0.0.1:0`, serves one connection, and the transport's request is
  checked for the exact path, host, agent, and the absence of Referer and proxy
  headers. The probe's `StreamHandoff` constructor is crate-private and Windows
  only. The package sets no environment variable at launch; GStreamer derives
  every path from its own DLL. The Windows CI and release jobs keep
  `contents: read`, and the release inventory now lists the ZIP and installer.

### Findings

- Low, fixed here: `actions/checkout@v7` and `actions/upload-artifact@v7` were
  floating tags; every use in both workflows, including the Flatpak jobs, is
  pinned to the v7.0.1 commit.
- Low, subsequently fixed: the Flatpak builder action is pinned by commit
  (see the September 4 delta below). The `gnome-50` container and all other
  job containers now use [reviewed image-index digests](build-input-pins.md);
  packages installed afterward remain mutable H2.2 inputs.
- Low, accepted: `cargo install cargo-audit --locked` takes the latest release;
  the advisory database is fetched live in any case.
- Low, accepted: the release workflow installs Inno Setup with an unpinned
  `choco install innosetup`, as Tributary does; the installer payload is the
  tree validated immediately before compilation, and the compiled installer's
  version resource is reopened.
- Low, accepted: the packaged Windows application uses GStreamer's default
  per-user registry cache under `%LOCALAPPDATA%\gstreamer-1.0`, shared with any
  other GStreamer on the machine, because safe Rust cannot set `GST_REGISTRY`.
  The registry holds plugin metadata only, and GStreamer drops entries whose
  files are not found on the next scan.
- Low, accepted: `avformat` imports the generic `libbluray`, which the
  component policy deliberately allows; no decryption component is present.

### Not covered

- The release publication job with write permission (P3.5, PR #18); the macOS
  package does not exist yet; the privileged flathub builder container was
  reviewed only through its inputs; the Windows installer, compiled locally and
  by the release workflow's Inno Setup, is inspected only through its version
  resource.

## 5. Unexpected tuner-allocation paths

### Verified

- The only stream fetch is `stream_body` in `StreamTransport::start`
  (`src/playback/transport.rs`), reached from `SourcePolicy::install` on the
  pipeline that `PlaybackSession::begin_tune` builds for one `StreamSelection`,
  which the channel sidebar creates only on row activation
  (`src/ui/channel_sidebar.rs`) and `resolve_stream_handoff` authorizes without
  I/O. One reader thread and one connection per tune (`pool_max_idle_per_host(0)`).
- Release: `SourcePolicy::retire` cancels the transport; `begin_tune` retires the
  predecessor first; `PlaybackSession::stop` is called by the Stop control,
  `connect_device_selection`, and `connect_close_request` (`src/ui/window.rs`);
  `retire_active` joins reader, feeder, and pipeline within 5 s or quarantines.
  Test `fake_device_window_releases_tuners_on_device_change_mutation_and_close`
  observes the stream close on switch, device change, mutation, and close.
- Device selection resolves `discover.json` and `lineup.json` only
  (`DeviceSnapshotResolver`); inspection and `--inspect` call
  `fetch_device_snapshot`, and `LineupChannel::stream_url` is crate-private. Test
  `snapshot_verifies_identity_before_requesting_the_lineup` asserts exactly two
  requests. No preload, thumbnail, guide, or PSIP path exists.
- Grep for port 5004, `/auto/`, `/tuner`, `stream_url`, `reqwest::Client`, and
  `TcpStream::connect`: production hits are `normalize_stream_url`,
  `stream_url_matches`, `parse_private_stream_url`, and the two clients above;
  every other hit is test code against loopback fixtures.
- Live-hardware tests are `#[cfg(all(test, feature = "desktop"))]`, `#[ignore]`,
  and gated on `BALUN_LIVE_HARDWARE=1`; CI and the helpers pass `--ignored` only
  with `--exact` names of runtime probes and Wayland smokes.

### Findings

- None.

### Not covered

- macOS live-device acceptance (P0.3) exercises the same path.

## Delta check (2026-09-04)

Scope: the 142 commits between the audited `8c0df0e` and `main` at `2e1a8c0`
plus the v0.1 pull requests #55 to #59. This is a contract re-check of the
five areas above against the current code, not a second full audit.

Re-verified in place:

- Device HTTP still disables redirects, Referer, and proxies
  (`src/hdhr/http.rs` `build_client`), and the stream transport does the same
  (`src/playback/transport.rs`); transport errors are rendered through
  `reqwest::Error::without_url`, which closes the URL-stripping follow-up.
- `DeviceAuth` appears only in fixtures and the settings forbidden-string test;
  no production path reads it. Loopback targets are refused by the desktop
  parser (`src/discovery/manual.rs`) and skipped in interface enumeration
  (`local.rs`); `DiscoveryClient::invalid_target` still accepts them, so the
  diagnostic's `--target` follow-up stays open.
- `settings.json` is created with mode `0o600` (`src/settings/store.rs`).
- The routed runner re-checks authority, the deadline, and the interface pin
  before every datagram (`src/discovery/approval/controller/runner.rs`).
- The Windows console feature exists only for the developer `-Run` build;
  `src/main.rs` keeps the GUI subsystem for every other release build, and #56
  refuses to stage a console-subsystem `balun.exe` into a package.
- The packaged macOS launcher accepts the install-key helper's output only
  when it matches `^[0-9a-f]{16}/$` before using it in a cache path, creates
  the cache with `umask 077`, and no longer executes Perl (#58).
- The Linux package validator applies the forbidden-component policy to the
  reopened deb, RPM, and Arch payloads; those validators only ever receive
  artifacts the workflow itself just built.
- #55 to #58 keep the hidden helper flags (`--balun-platform-runtime-probe`,
  `--balun-macos-install-key`) argument-free and fail-closed.
- #59 (`.github/workflows/release.yml` and `ci.yml`): every `uses:` in both
  workflows names a full commit, the Flatpak builder in CI included; the only
  job with a write token still checks out no source, lists the tag's releases
  and refuses to proceed unless none exists or exactly one unpublished draft
  does, treating a failed lookup as a refusal; the draft body comes from the
  tagged `CHANGELOG.md` section, not workflow input; and every package job
  keeps `contents: read`. `actionlint` 1.7.12 and `yamllint` 1.37.1 pass.

Still open:

- Routed pacing has no jitter (plan §5, ADR-0001).
- `DiscoveryClient::invalid_target` does not refuse loopback (§1).
- At that revision, `DeviceHttpError::Json` and `LineupError::Json` still
  rendered serde's message (§3). H3.1 resolved this on 2026-09-17 by discarding
  the raw error at both parsing boundaries.
- The approval store has no unsupported-version quarantine reason.
- The hostname resolver, network-change reconciliation, and Windows ARM64
  profile were reviewed for bounds and fail-closed defaults only.

## Follow-ups

- Add jitter to the routed pacing or amend plan §5 and ADR-0001 (P2).
- Refuse loopback in `DiscoveryClient` `invalid_target` so `balun-discover
  --target` cannot probe it.
- JSON diagnostic follow-up completed by H3.1 on 2026-09-17: fixed categories
  and positions replace value-bearing serde errors across diagnostic paths.
- Approval store: document the key threat model and add an unsupported-version
  quarantine reason.
- The CI Flatpak image now has an enforced digest pin; complete the remaining
  [H2.2 input inventory](build-input-pins.md#remaining-h22-scope).
- Re-audit new log sites against the `Debug` list at the next review.

Closed by the 2026-09-04 delta check: the URL-stripped
`DeviceHttpError::Transport`, the pinned flatpak-builder action (#59), and the
repeat review owed once the routed sender (P2) and the macOS package (P3.4)
landed.
