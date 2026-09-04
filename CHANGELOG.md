# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] — 2026-09-04

### Added

- **macOS runtime probe and packaging** — Deliver P3.4 runtime probe, cache isolation, and
  packaging for macOS. `scripts/build-macos.sh` supports `--app` and `--dmg`, blinding the launcher
  environment to Homebrew (`PATH`, `DYLD_LIBRARY_PATH`, and empty `GST_PLUGIN_SYSTEM_PATH`),
  staging a 21-element frozen GStreamer plugin closure, resolving transitive dynamic libraries via
  BFS into `Contents/Frameworks`, patching library rpaths with `install_name_tool`, verifying the
  closure against pinned component policies, performing ad-hoc codesigning, running an isolated
  read-only runtime probe loopback test from a relocated path with spaces, and generating a
  drag-to-Applications disk image `dist/Balun.dmg` via `create-dmg`.
- **Release verification and checksums** — Add the immutable release verification check
  (`scripts/release_check.py`) ensuring Semantic Version agreement across Cargo manifests, the
  lockfile, AppStream metainfo, and changelog release links, and enforce exact release candidate
  inventories and SHA-256 digests (`SHA256SUMS.txt`).

- **Multi-site hardware matrix** — Document the secondary tuner and tunnel compatibility matrix in
  `docs/compatibility-v0.1.md`, covering the secondary-site HDHR3-PRIME (CableCARD/QAM, 3 tuners)
  and secondary HDHR5-4K (ATSC 1.0/3.0, 4 tuners), completing P4.2. Unprotected Clear QAM channels
  play over the tunnel, DRM channels are identified and refused without leaking allocations, and
  tuner-busy HTTP 503 responses classify cleanly without retry loops.
- **Routed tunnel validation and traffic budgets** — Validate cross-site tuner discovery and live
  playback over a routed tunnel (UniFi Site Magic / WireGuard), completing P2.5. Broadcast
  discovery remains confined to local segments while approved routed scans discover remote tuners
  within a 64 packet/second budget (< 6.5 KB/s peak) with zero idle traffic, maintaining distinct
  device identities and lineups across sites.

- **macOS live-TV acceptance** — Exercise the live-hardware acceptance suite on macOS against
  physical HDHomeRun tuners, completing P0.3. Unprotected ATSC 1.0 channels play with progressive
  video and decoded audio rendered through `osxaudiosink`, channel switches settle within 780 ms,
  and client-side tuner release finishes in under 18 ms.
- **macOS codec and audio sink contract** — Record the macOS decoder and sink inventory from
  `scripts/build-macos.sh --probe-playback`, completing P0.5. VideoToolbox decoders outrank software
  libav decoders for H.264 and HEVC video; libav decodes MPEG-2 video; AudioToolbox, libav, and
  mpg123 provide MPEG-1/2, AAC, and AC-3 audio; `osxaudiosink` is the verified platform audio sink;
  and AC-4 audio fails closed as on other platforms.
- **HDHomeRun discovery** — Discover tuners on every attached interface with IPv4 broadcast and
  IPv6 multicast, probe one exact address for a tuner behind WireGuard or another routed link, or
  enumerate one explicitly approved private range no wider than `/24` from the diagnostic.
- **Stable device and channel identity** — Keep each tuner behind one validated DeviceID even
  when it is reachable at several addresses, expire stale locators independently, and scope every
  channel to exactly one device so lineups are never merged.
- **Device inspection and lineups** — Fetch identity-checked `discover.json` and `lineup.json`
  from the responder that answered, with strict size, time, redirect, and credential limits, and
  accept both the documented `Tags` field and the current `Favorite`, `DRM`, and `HD` fields.
- **`balun-discover` diagnostic** — A GTK-free command-line tool for local discovery,
  exact-address probes, approved-range enumeration, and `--inspect` metadata and lineup summaries
  that never open a stream or allocate a tuner.
- **Controller runtime** — Own discovery, the device registry, and the selected lineup on one
  background thread that publishes immutable, URL-free snapshots to the user interface. Startup
  sends no packets, newer requests cancel older ones, and shutdown joins all work.
- **GTK 4 / libadwaita desktop** — An adaptive window with a device sidebar, a per-device channel
  sidebar, and a live-TV pane. **Refresh devices** runs local discovery, **Find device by
  address** probes one numeric IPv4 or unscoped IPv6 address, and **Stop device discovery**
  cancels either.
- **Live channel playback** — Click or press Enter on an unprotected channel to tune it. A
  generation-owned `playbin3` session renders through `gtk4paintablesink`, settles the previous
  tune before starting the next, and tears down within a bounded time.
- **Application-owned stream transport** — GStreamer receives only the constant `appsrc://balun`
  URI; Balun's own HTTP client, with proxies, redirects, Referer, and DNS disabled, fetches the
  MPEG-TS stream and feeds the built-in `appsrc`. Adopted in ADR-0001.
- **Playback controls** — Stop, a volume slider, an independent mute toggle, and fullscreen with
  `F11` and `Escape`. Volume and mute carry across channel changes for the running session, and
  fullscreen presentation follows the compositor's confirmation.
- **Network-change reconciliation** — On Linux, Balun watches for adapter, address, and route
  changes, coalesces each burst, cancels any routed scan or proposal at once, and drops device
  addresses that were only ever seen through a lost interface or tunnel. Devices with another
  valid address stay, remembered addresses are never removed, and a brief notice says what changed.
- **Dependency audit and repository linting** — CI now runs `cargo audit` and lints Markdown,
  TOML, YAML, and GitHub Actions workflows with pinned tools and root-level configs.
- **Strict Clippy on every native lane** — The macOS and Windows jobs now lint the desktop and
  diagnostic targets with warnings denied in both profiles, as Tributary does, so platform-only
  dead code fails CI instead of surfacing in a developer's build log.
- **Keyboard navigation and accessibility** — Full keyboard navigation and accessible roles across
  the device sidebar, channel sidebar, and player pane. Tab and Shift+Tab traverse controls across
  panes, arrow keys navigate items, Enter and Space activate them, and `Ctrl+F` focuses the channel
  search field. Dedicated accessible roles, names, and shortcuts describe controls for assistive
  technologies.
- **Desktop metadata and assets** — Add the `io.github.jm2.Balun` desktop entry, AppStream
  metainfo, an application icon, and a 1280×720 (16:9) AppStream screenshot depicting tuner
  discovery, channel lineup, and live playback. The screenshot strictly adheres to Balun's
  privacy and identity contracts with sanitized RFC 5737 addresses, synthetic test device IDs,
  and simulated station names. CI validates icon formats and screenshot dimensions.
- **Flatpak bundle** — Add the `io.github.jm2.Balun` manifest on the GNOME 50 runtime with the
  reviewed six-entry permission policy, the Freedesktop ffmpeg-full codec extension, an offline
  cargo build from generated locked sources, build-time and installed-bundle checks that the
  runtime supplies every structural playback factory, and app-payload validation. CI builds
  and reopens an x86_64 bundle; the release-candidate workflow builds x86_64 and aarch64
  bundles as artifacts.
- **Native Linux packages** — Add Debian metadata for amd64 and arm64, RPM metadata for x86_64
  and aarch64, and an x86_64 Arch recipe. `scripts/build-linux.sh --deb`, `--rpm`, and
  `--arch-pkg` reuse the locked desktop build, require an explicitly preinstalled pinned
  packager, validate the completed payload, and reopen the final package without installing tools
  or dependencies.
- **Windows ZIP and installer** — `scripts\build-windows.ps1 -Zip` stages a self-contained
  `dist\balun-windows` tree in the MSYS2 prefix shape (`bin\balun.exe` beside its DLLs,
  `lib\gstreamer-1.0`, `libexec\gstreamer-1.0`, `share`) from a reviewed, capability-derived
  closure of 27 GStreamer plugins and only the DLLs those binaries import, embeds the icon and
  version resource in `balun.exe`, runs a hidden packaged-runtime probe inside the staged tree
  with a sanitized environment (the bundled plugin scanner, a fresh registry, and the synthetic
  MPEG-2 fixture decoded through the production stream transport), reopens the ZIP against the
  staged tree, and `-InnoSetup` compiles `balun-setup.exe` from a new Inno Setup recipe with a
  deterministic application GUID. The release component policy is applied at every copy, during
  import traversal, over the completed tree, and inside the reopened archive. Strict x86_64
  CLANG64 and ARM64 CLANGARM64 profiles bind the Rust target, MSYS2 prefix, every PE machine type,
  the probe receipt, and the Inno Setup architecture as one tuple. CI builds and reopens both ZIPs;
  the release-candidate workflow adds both ZIPs and both installers to the exact inventory.
- **Release automation** — The release-candidate workflow now checks that the tag agrees with
  `Cargo.toml`, `Cargo.lock`, the Arch recipe, the changelog section and compare link, and the
  AppStream release (`scripts/release_check.py`). It requires exactly 12 binary artifacts—two
  Flatpaks, five native Linux packages, one Apple Silicon DMG, and four Windows packages—plus
  `SHA256SUMS.txt`, then creates a draft GitHub release from a job that checks out no source.
  Every action the workflow runs is pinned to an immutable commit. Publishing stays manual.
- **Channel search and favorites filter** — A search field above the channel list matches a
  channel number prefix or part of a name, and a star toggle shows favorites only. Filtering
  hides rows without changing device or channel identity, keeps the highlighted channel when it
  is still visible, clears the search when another device is selected, and explains an empty
  result.
- **Versioned settings** — Balun remembers its window size and maximized state across launches
  in an atomic, schema-versioned `settings.json` under the platform configuration directory.
  The same file reserves bounded, credential-free storage for remembered device addresses and
  user-assigned device names, and a malformed or newer file is reported and left untouched.
- **Remembered devices** — A tuner found through **Find device by address** is remembered once
  it answers, and Balun probes those addresses again at the next launch so routed tuners
  reappear without retyping. Up to 32 addresses are kept; local discovery still waits for
  Refresh.
- **Hostname entry** — **Find device by address** also accepts a hostname. It is resolved once
  on the controller, bounded to five seconds and four usable unicast addresses that are probed
  one at a time, and remembered by name so it is resolved again at the next launch. A name that
  cannot be resolved shows a brief notice. The settings schema moves to version 2; version 1
  files are read and rewritten in place.
- **Endpoint-free playback errors** — Failures reduce to fixed messages for no tuner available,
  channel unavailable, stream rejected, device or stream unavailable, missing codec or plugin,
  protected channel, and internal error. A missing decoder is named from a closed list, such as
  AC-4 audio or HEVC video. No native error text, header, or address is retained in the
  application state; the native error goes to the log.
- **Fake HDHomeRun end-to-end tests** — A loopback fake device with UDP discovery, metadata, and
  an MPEG-TS stream server drives the real controller, transport, and session from discovery
  through lineup, DRM refusal, tuning, channel switching, and observed tuner release.
- **Teardown-release proofs** — The fake device can also present a deliberately missing channel
  and a second synthetic device: a failed tune must classify as its fixed category and release
  its own connection, and the real window's device-change, discovery-mutation, and close paths
  must stop playback through the production wiring.
- **Synthetic playback acceptance** — A checked-in MPEG-2 transport-stream fixture and an isolated
  headless-Wayland harness prove decoding, rendering, EOS, and teardown on Linux CI.
- **Diagnostic logging** — Balun logs discovery, lineup, tune, and playback outcomes to standard
  error, `info` by default and `RUST_LOG=balun=debug` for detail, including the native GStreamer
  error, HTTP status, or source-policy reason behind a playback failure. Stream URLs, credentials,
  and query values are never logged.
- **Installed-runtime playback probes** — `--probe-playback` (`-ProbePlayback` on Windows) checks
  the seven required GStreamer factories and the constant-URI `appsrc` contract, then prints the
  installed decoders for each broadcast stream type and the audio sinks, so each platform's codec
  contract can be recorded from the same command; every native CI lane runs it.
- **Routed discovery foundation** — Native Linux route inspection, a keyed and topology-redacted
  approval policy, a durable private approval store, and fail-closed route and store observers
  provide the authority boundary used by the monitored runner and tunnel-search interface.
- **Build helpers and CI** — Tributary-derived Linux, macOS, and Windows helpers with the same
  filenames and flags, locked formatting, strict Clippy, debug and release tests, an exact-MSRV
  job, native macOS, Windows x86_64, and Windows ARM64 checks, and an immutable-tag
  release-candidate workflow.
- **Package policy gates** — The pinned Flatpak Cargo-source generator, reviewed Flatpak
  permission contract, and Linux, Flatpak, macOS, and Windows artifact validators now gate the
  corresponding completed and reopened packages as well as their adversarial fixtures.
- **Rust floor synchronization** — One helper keeps `Cargo.toml`, the Dependabot proposal
  manifest, the MSRV job, and the README compiler declarations aligned.
- **v0.1 plan and task ledger** — The product plan, architecture, safety constraints, and a
  countable dependency-aware ledger of what is done and what remains for the first alpha.
- **Monitored routed runner (Linux)** — The approval store, the route and store observers, and the
  interface-pinned socket are connected into one library runner. It reserves the approved scan,
  rebaselines the observers around the exact post-publication reread, probes each target through a
  socket that re-checks authority and its pin before every datagram, and settles completion
  durably on every exit path. The controller offers it as a lane of its own (propose, approve,
  run, revoke) with topology-free snapshot state and copy for every decision.
- **Routed tunnel discovery (Linux)** — A tunnel-search button in the device sidebar runs the
  approved routed scan. The first search shows the exact routes, address count, and packet budget
  and asks for approval once per route set; approval is remembered on disk, cooldown and busy
  decisions are shown in place, Stop cancels the scan, and **Forget routed approvals** revokes
  them.
- **Live-hardware proofs** — Opt-in, display-free tests that discover the real tuners, tune an
  unprotected ATSC 1.0 channel with decoded video and audio, switch channels, observe the ATSC 3.0
  fail-closed path, and probe each device at its own address. They run only with
  `BALUN_LIVE_HARDWARE=1`, never in CI, and print no address, name, or channel.
- **Sanitized device fixtures** — Real `discover.json`, `lineup.json`, and HTTP failure responses
  from a CONNECT and a CONNECT 4K, with identities, addresses, credentials, and channel names
  replaced, now back the parser tests.
- **Support matrix and governance docs** — `docs/support-v0.1.md` names the verified platforms,
  devices, discovery methods, codecs, and v0.1 limitations from the compatibility notes;
  `CONTRIBUTING.md` and `SECURITY.md` say how to contribute and how to report a vulnerability.
- **Discovery diagnostics** — `balun-discover --providers` reports route-provider availability and
  bounded tunnel candidate counts without sending a packet or printing a route; every run prints
  its fixed probe budget, and each probe issue carries a fixed failure class beside its message.

### Changed

- **Parallel release builds** — The release profile now keeps Cargo's default codegen units with
  thin LTO, matching Tributary, so release and playback-probe builds spread across cores instead
  of optimizing the main crate on one thread.
- **Playback errors name the device and channel** — Failure and connecting messages now say
  which tuner (friendly name and address) and which channel they refer to, and the diagnostic's
  inspection line prints the device address. Stream URLs and `DeviceAuth` stay out of every
  message (ADR-0002).
- **Build helpers require the playback runtime** — Before a desktop build, every helper checks
  that the GStreamer plugin files behind `playbin3`, `appsrc`, `tsdemux`, `deinterlace`, and
  `gtk4paintablesink` are installed, names the package for each missing one, and warns when the
  gst-libav decoders are absent.
- **Windows helper packaging modes** — `-Bundle`, `-Zip`, and `-InnoSetup` (with `-SkipBundle`
  for an installer-only run and `-NoCargoBuild` to package an existing build) replace the
  placeholder switches; the default run is still build-only, and `-Package` and `-Installer`
  are gone.
- **Helper parity with Tributary** — Restore the release-profile Clippy pass, a read-only check
  that the selected Windows GNU-LLVM target is installed, install hints for missing tools and
  development packages, and the formatting pre-commit hook.
- **Windows local discovery** — Derive each network from the OS-reported prefix length and send
  the vendor-compatible limited broadcast from each interface-bound socket. Link-local IPv6 probes
  are omitted until lineup HTTP can preserve their scope.
- **Rust minimum version** — Rust 1.98 is the declared and CI-verified minimum.

### Fixed

- **Smooth audio from the first second** — The pipeline now waits for the first stream bytes
  before it starts the live clock, so a slow tuner lock no longer leaves every audio buffer late
  and stuttering until the next channel change.

### Security

- **Security and privacy review** — Re-audited network admission, persisted state, logs and
  diagnostics, package contents and CI, and every tuner-allocation path against the v0.1 plan
  and ADRs; the write-up is `docs/security-review-v0.1.md`. `settings.json` is now explicitly
  owner-only on Unix, discovery observations redact advertised URLs in debug output, and the
  remaining GitHub actions are pinned by commit.
- **Pinned device HTTP** — Metadata and lineup requests go only to the responder that answered
  discovery, on the observed ports, without credentials, redirects, proxies, or DNS, with bounded
  response sizes and times, and only after the device identity matches.
- **Bounded untrusted input** — Discovery replies are rejected when malformed, oversized, or from
  outside the probed prefix; lineup rows are capped while parsing; and the long-lived registry
  caps devices, locators, and origins and refuses conflicting DeviceID ownership.
- **Bounded routed discovery** — Range enumeration requires an explicit RFC 1918 range no wider
  than `/24`, caps candidates, rate, and concurrency, and keeps point-to-point interfaces out of
  ordinary local discovery. Automatic route-derived authority fails closed on ambiguous topology,
  changed policy, clock rollback, or uncertain durability; the Linux production runner rechecks
  that authority and its pinned interface before every datagram.
- **Playback component boundary** — The required GStreamer factories are a startup capability
  check, not a bundling allowlist; each self-contained package derives and reviews its real plugin
  closure separately.
- **Release component policy** — A shared, fail-closed policy rejects dedicated optical-disc
  decryption and proprietary DRM components across repository inputs, build helpers, and the
  completed Linux, Flatpak, macOS, and Windows artifact validators, while leaving ordinary codecs,
  TLS, and general-purpose cryptography untouched.

### Privacy

- **Local and explicit discovery only** — Balun contacts tuners on attached networks or at
  addresses and ranges you supply or explicitly approve; it never calls a cloud discovery service,
  analytics endpoint, or guide scraper.
- **Credential-safe diagnostics** — Advertised URLs are hidden, `DeviceAuth` is never read,
  stored, or printed, and metadata buffers are wiped after parsing.
- **Topology-redacted approvals** — Remembered routed-discovery consent stores only keyed
  fingerprints and bounded policy state, never raw routes, interface names, or prefixes.
- **Sanitized packaged-hardware evidence** — The opt-in hardware validation wrapper sanitizes the
  displayed and saved validator stream, including complete IPv4 and IPv6 addresses and contextual
  device identifiers, while retaining unrelated module paths, firmware values, and measurements.
  The sanitizer exists only on that test path; production live logging is unchanged.

### Known limitations

- **Binary publication is manual** — The complete package inventory can remain a draft after every
  build, payload, runtime, checksum, and inventory gate passes. It is official only when the
  `v0.1.0` release is visible on GitHub Releases.
- **Architecture evidence differs** — ARM64 Linux and Windows candidates run native CI build and
  package gates, but the recorded physical-tuner trials are platform-level evidence and do not
  separately cover every CPU architecture and package format.
- **Windows plugin cache** — The Windows package uses GStreamer's default per-user registry cache;
  the macOS launcher instead confines its cache to Balun's private user cache directory.
- **No program guide** — Guide data is a v0.2 candidate. The tested HDHomeRun CONNECT's
  per-channel streams carry no PSIP tables, so an in-band guide needs a full-multiplex crawl or
  XMLTV.
- **ATSC 3.0** — HEVC video needs gst-libav or a platform decoder, and AC-4 audio has no open
  decoder, so those channels fail closed and cannot be transcoded.

[Unreleased]: https://github.com/jm2/balun/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/jm2/balun/releases/tag/v0.1.0
