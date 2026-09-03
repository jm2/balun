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

## Summary

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

- Startup sends nothing on its own: `ControllerActor::new` opens no socket and
  only `RefreshLocalDiscovery` and `DiscoverExact` reach `start_discovery`
  (`src/controller/runtime.rs`); the window seeds `RediscoveryQueue` from
  remembered targets alone, one per settled lane (`src/ui/window.rs`
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
  exact-target parser; `SO_BROADCAST` is unset, so a directed broadcast fails.

### Not covered

- macOS and Windows route providers (fail closed by design) and the routed
  approval UX, still in open pull requests.

## 2. Persisted state

### Verified

- `settings.json` (`src/settings/mod.rs`): platform configuration directory,
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

### Verified

- No logging framework is linked. The 17 production print sites
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
- Low, open: `DeviceHttpError::Json` and `LineupError::Json` render serde's
  message, which can echo a mistyped device-chosen value into the diagnostic's
  stderr; bounded, escaped, never `DeviceAuth` or a URL.
- Accepted under ADR-0002: `DeviceEndpoint`'s derived `Debug` shows
  responder-pinned URLs, and the lineup body is not zeroized.

### Not covered

- A future logging framework; re-audit every `Debug` above if one is added.

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
- Low, open: `flatpak/flatpak-github-actions/flatpak-builder@v6` and the
  `gnome-50` builder image are tag-pinned; it is the only action in either
  workflow not pinned by commit.
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
  reviewed only through its inputs; the Windows installer is compiled only by
  the release workflow's Inno Setup and inspected only through its version
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

## Follow-ups

- Add jitter to the routed pacing or amend plan §5 and ADR-0001 (P2).
- Refuse loopback in `DiscoveryClient` `invalid_target`.
- Wrap `DeviceHttpError::Transport` in a URL-stripping newtype and render serde
  positions only.
- Approval store: document the key threat model and add an unsupported-version
  quarantine reason when P2.2 wires it.
- Pin the flatpak-builder action and its `gnome-50` image; review the
  publication job with #18 (P3.5).
- Repeat this review when P2.2 connects the routed sender and when the macOS
  package lands (P3.4); the Windows package (P3.3) was re-audited above. Add a
  CI guard so a logging framework cannot arrive without a `Debug` re-audit.
