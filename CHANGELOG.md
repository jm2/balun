# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-09-05

### Added

- **HDHomeRun discovery** — Find tuners on every attached interface with IPv4 broadcast and IPv6
  multicast, probe one exact address or hostname (resolved to at most four unicast addresses) for
  a tuner behind WireGuard or another routed link, and enumerate one approved private range no
  wider than `/24` from the diagnostic.
- **Remembered devices** — A tuner found by address or hostname is remembered once it answers and
  probed again at the next launch, up to 32 entries, once the launch discovery settles;
  right-click a listed device to forget its entry. Hostnames stay remembered by name when multiple
  addresses answer and across startup rediscovery, without persisting their resolved addresses.
- **Stable device and channel identity** — One validated DeviceID per tuner even when it answers
  at several addresses, stale addresses that expire independently, and lineups that are never
  merged across devices.
- **Device inspection and lineups** — Identity-checked `discover.json` and `lineup.json` from the
  responder that answered, with strict size, time, redirect, and credential limits. **Reload
  channels** retries failures or fetches lineup changes directly from the selected device.
- **GTK 4 / libadwaita desktop** — An adaptive window with a device sidebar, a per-device channel
  sidebar with favorite, HD, and protected badges, channel search, a favorites-only filter, and a
  live-TV pane; one local discovery runs at launch, and window size and maximized state are
  remembered.
- **Live channel playback** — Click or press Enter on an unprotected channel. A generation-owned
  `playbin3` session renders through `gtk4paintablesink`, starts its live clock only once the
  first stream bytes arrive, settles the previous tune before the next, and tears down in bounded
  time.
- **Application-owned stream transport** — GStreamer receives only the constant `appsrc://balun`
  URI; Balun's own HTTP client, with proxies, redirects, Referer, and DNS disabled, fetches the
  MPEG-TS stream (ADR-0001).
- **Playback controls** — Stop, a volume slider, an independent mute toggle, and fullscreen with
  `F11` and `Escape`; volume and mute carry across channel changes.
- **Software deinterlacing** — Adaptive YADIF at full field rate for interlaced SD and HD video,
  with automatic field order and progressive passthrough; diagnostics report the method and
  negotiated output frame rate. GPU deinterlacing and inverse telecine remain deferred.
- **Playback idle inhibition** — Request that the display and computer stay awake while playing
  or buffering, and release the request on Stop, failure, replacement, or close.
- **Playback errors that name the device and channel** — Failures reduce to fixed messages (no
  tuner available, channel unavailable, stream rejected, device unavailable, missing codec,
  protected channel, internal error) that name the tuner and channel but never a stream URL or
  `DeviceAuth`; a missing decoder is named from a closed list such as AC-4 audio or HEVC video.
- **Discovery feedback** — Every address search shows a searching notice and then a result notice
  for a reply, no reply, failure, stop, or replacement, and routed-discovery failures name their
  topology-safe reason.
- **Keyboard navigation and accessibility** — Tab order across the three panes, arrow-key item
  navigation, Enter and Space activation, `Ctrl+F` for channel search, and accessible roles and
  names for assistive technologies.
- **Network-change handling (Linux)** — Adapter, address, and route changes cancel any routed
  scan, expire addresses seen only through a lost interface, keep devices with another valid
  address, and show a brief notice; nothing rescans on its own.
- **Route-table-derived tunnel discovery (Linux)** — **Search routes behind your tunnel** proposes
  a bounded private-address scan from the active tunnel route, asks for approval once per route
  set with the address count and packet budget, and **Forget routed approvals** revokes it.
- **Windows local discovery** — Each network is derived from the OS-reported prefix length and
  probed with the vendor-compatible limited broadcast from an interface-bound socket.
- **`balun-discover` diagnostic** — A GTK-free command-line tool for local discovery,
  exact-address probes, approved-range enumeration, route-provider reports, and `--inspect`
  summaries that never open a stream or allocate a tuner.
- **Diagnostic logging** — Discovery, lineup, tune, and playback outcomes go to standard error at
  `info` by default (`RUST_LOG=balun=debug` for detail), including the native GStreamer error
  behind a playback failure; stream URLs, credentials, and query values are never logged. On
  Windows, `.\scripts\build-windows.ps1 -Run` keeps the console attached for developers.
- **Versioned settings** — An atomic, schema-versioned `settings.json` under the platform
  configuration directory holds window state and remembered addresses; a malformed or newer file
  is reported and left untouched.
- **Packages** — Flatpak x86_64 and aarch64 on the GNOME 50 runtime, Debian amd64 and arm64, RPM
  x86_64 and aarch64, an Arch x86_64 package, an Apple Silicon DMG, and Windows x86_64 and ARM64
  ZIPs and installers. Every package is reopened and validated after it is built, and the macOS
  and Windows packages carry a reviewed, capability-derived GStreamer closure that is probed
  before upload.
- **Release automation** — A manual workflow builds every package from one annotated tag, checks
  the tag against every version declaration, requires exactly 12 binary artifacts plus
  `SHA256SUMS.txt`, and creates a draft release from a job that checks out no source.
- **Build helpers and CI** — Tributary-derived Linux, macOS, and Windows helpers with the same
  filenames and flags, a `--run` (`-Run`) build-and-launch route, runtime plugin gates,
  installed-runtime playback probes, strict Clippy on every native lane, an exact-MSRV job
  (Rust 1.98), a dependency audit, and repository linting.
- **Verified on real hardware** — Live TV with audio on Linux, macOS, and Windows against CONNECT,
  CONNECT 4K, and PRIME tuners across two sites, including Clear QAM, DRM refusal, tuner-busy
  handling, and routed tunnel discovery within a 64 packet/s budget with no idle traffic.
- **Hardware and synthetic proofs** — Opt-in live-hardware tests, a loopback fake HDHomeRun that
  drives the real controller end to end, teardown-release proofs, and a checked-in MPEG-2 fixture
  rendered under headless Wayland in CI.
- **Desktop metadata and assets** — The `io.github.jm2.Balun` desktop entry, AppStream metainfo,
  icon, and a sanitized 1280×720 screenshot.
- **Documentation** — The v0.1 plan and ledger, sanitized compatibility notes, an evidence-backed
  support matrix, `CONTRIBUTING.md`, and `SECURITY.md`.
- **Live audio timing** — The MPEG-TS feed carries arrival timestamps, so audio stays in sync
  when usable media arrives after the first transport bytes instead of turning choppy or silent.
- **Routed discovery through route-event bursts** — On Linux a kernel or NetworkManager change
  arrives as several datagrams; the route observers wait for the burst to settle, retry a
  rejected baseline a bounded number of times, and are re-established by the next request.
- **Reply banner** — A successful exact reply is announced in the sidebar for three seconds;
  failures stay until the next search.
- **Channel list position** — The channel list keeps its scroll position and selection across
  snapshots that leave the lineup unchanged, such as tuning.

### Security

- **Private vulnerability reporting** — Enabled GitHub's private report form linked by the security
  policy, which now covers v0.1 alpha versions.
- **Security and privacy review** — Network admission, persisted state, logs and diagnostics,
  package contents and CI, and every tuner-allocation path are audited in
  `docs/security-review-v0.1.md`; `settings.json` is owner-only on Unix.
- **Pinned device HTTP** — Metadata and lineup requests go only to the responder that answered
  discovery, on the observed ports, without credentials, redirects, proxies, or DNS, and only
  after the device identity matches.
- **Bounded untrusted input** — Malformed, oversized, or out-of-prefix discovery replies are
  rejected, lineup rows are capped while parsing, and the registry caps devices, locators, and
  origins and refuses conflicting DeviceID ownership.
- **Bounded routed discovery** — Range enumeration needs an explicit RFC 1918 range no wider than
  `/24` with capped candidates, rate, and concurrency; route-table-derived authority fails closed
  on ambiguous topology, changed policy, clock rollback, or uncertain durability, and the Linux
  runner rechecks authority and its pinned interface before every datagram.
- **Release component policy** — A shared, fail-closed policy rejects dedicated optical-disc
  decryption and proprietary DRM components across repository inputs, build helpers, and every
  completed package, while leaving ordinary codecs, TLS, and general-purpose cryptography alone.
- **Pinned workflow actions** — Every GitHub action in CI and the release workflow runs from an
  immutable commit.

### Privacy

- **Local and explicit discovery only** — Balun contacts tuners on attached networks or at
  addresses and ranges you supply or approve; it never calls a cloud discovery service, analytics
  endpoint, or guide scraper.
- **Credential-safe diagnostics** — Advertised URLs are hidden, `DeviceAuth` is never read,
  stored, or printed, and metadata buffers are wiped after parsing.
- **Topology-redacted approvals** — Remembered routed-discovery consent stores only keyed
  fingerprints and bounded policy state, never raw routes, interface names, or prefixes.
- **Sanitized hardware evidence** — The opt-in packaged-hardware validator redacts addresses and
  device identifiers from its displayed and saved output; production logging is unchanged.

### Known limitations

- **Packaged live-tuner acceptance** — The recorded Linux and Windows physical-tuner trials used
  development builds; package gates cover the artifacts, but no cross-platform packaged tuner
  result is recorded yet (P4.1).
- **Architecture evidence differs** — ARM64 Linux and Windows packages pass native CI build and
  package gates, but the physical-tuner trials do not separately cover every CPU architecture.
- **macOS Gatekeeper** — The DMG is ad-hoc signed but not notarized, so the first launch needs the
  one-time `xattr -cr` step from the README.
- **Windows plugin cache** — The Windows package uses GStreamer's default per-user registry cache.
- **No program guide** — Guide data is a v0.2 candidate; the tested CONNECT's per-channel streams
  carry no PSIP tables, so an in-band guide needs a full-multiplex crawl or XMLTV.
- **ATSC 3.0** — HEVC video needs gst-libav or a platform decoder, and AC-4 audio has no open
  decoder, so those channels fail closed and cannot be transcoded.

[Unreleased]: https://github.com/jm2/balun/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jm2/balun/releases/tag/v0.1.0
