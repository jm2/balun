# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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
- **Live channel playback** — Double-click or press Enter on an unprotected channel to tune it. A
  generation-owned `playbin3` session renders through `gtk4paintablesink`, settles the previous
  tune before starting the next, and tears down within a bounded time.
- **Application-owned stream transport** — GStreamer receives only the constant `appsrc://balun`
  URI; Balun's own HTTP client, with proxies, redirects, Referer, and DNS disabled, fetches the
  MPEG-TS stream and feeds the built-in `appsrc`. Adopted in ADR-0001.
- **Playback controls** — Stop, a volume slider, an independent mute toggle, and fullscreen with
  `F11` and `Escape`. Volume and mute carry across channel changes for the running session, and
  fullscreen presentation follows the compositor's confirmation.
- **Desktop metadata and icon** — Add the `io.github.jm2.Balun` desktop entry, AppStream
  metainfo, and a Tango-style application icon: a CRT showing colour bars, rabbit ears joined by
  a balun, and the name on the bezel, as scalable and symbolic SVG, hicolor PNGs from 16 to
  512 px, a macOS iconset, and a Windows `.ico`, validated by a new CI job. Packages are still to
  come.
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
  AC-4 audio or HEVC video. No native error text, header, or address is retained.
- **Fake HDHomeRun end-to-end tests** — A loopback fake device with UDP discovery, metadata, and
  an MPEG-TS stream server drives the real controller, transport, and session from discovery
  through lineup, DRM refusal, tuning, channel switching, and observed tuner release.
- **Teardown-release proofs** — The fake device can also present a deliberately missing channel
  and a second synthetic device: a failed tune must classify as its fixed category and release
  its own connection, and the real window's device-change, discovery-mutation, and close paths
  must stop playback through the production wiring.
- **Synthetic playback acceptance** — A checked-in MPEG-2 transport-stream fixture and an isolated
  headless-Wayland harness prove decoding, rendering, EOS, and teardown on Linux CI.
- **Installed-runtime playback probes** — `--probe-playback` (`-ProbePlayback` on Windows) checks
  the seven required GStreamer factories and the constant-URI `appsrc` contract; every native CI
  lane runs it.
- **Routed discovery foundation** — Native Linux route inspection, a keyed and topology-redacted
  approval policy, a durable private approval store, and fail-closed route and store observers.
  None of it is connected to the user interface yet, so no automatic route-derived scan runs.
- **Build helpers and CI** — Tributary-derived Linux, macOS, and Windows helpers with the same
  filenames and flags, locked formatting, strict Clippy, debug and release tests, an exact-MSRV
  job, native macOS and Windows checks, and an immutable-tag release-candidate workflow.
- **Packaging scaffolding** — The pinned Flatpak Cargo-source generator, a reviewed Flatpak
  permission contract, and Linux, Flatpak, and macOS artifact validators with synthetic fixtures,
  ready for the first real packages.
- **Rust floor synchronization** — One helper keeps `Cargo.toml`, the Dependabot proposal
  manifest, the MSRV job, and the README compiler declarations aligned.
- **v0.1 plan and task ledger** — The product plan, architecture, safety constraints, and a
  countable dependency-aware ledger of what is done and what remains for the first alpha.
- **Live-hardware proofs** — Opt-in, display-free tests that discover the real tuners, tune an
  unprotected ATSC 1.0 channel with decoded video and audio, switch channels, observe the ATSC 3.0
  fail-closed path, and probe each device at its own address. They run only with
  `BALUN_LIVE_HARDWARE=1`, never in CI, and print no address, name, or channel.
- **Sanitized device fixtures** — Real `discover.json`, `lineup.json`, and HTTP failure responses
  from a CONNECT and a CONNECT 4K, with identities, addresses, credentials, and channel names
  replaced, now back the parser tests.

### Changed

- **Playback errors name the device and channel** — Failure and connecting messages now say
  which tuner (friendly name and address) and which channel they refer to, and the diagnostic's
  inspection line prints the device address. Stream URLs and `DeviceAuth` stay out of every
  message (ADR-0002).
- **Build helpers require the playback runtime** — Before a desktop build, every helper checks
  that the GStreamer plugin files behind `playbin3`, `appsrc`, `tsdemux`, `deinterlace`, and
  `gtk4paintablesink` are installed, names the package for each missing one, and warns when the
  gst-libav decoders are absent.
- **Helper parity with Tributary** — Restore the release-profile Clippy pass, a read-only check
  that the Windows `x86_64-pc-windows-gnullvm` target is installed, install hints for missing
  tools and development packages, and the formatting pre-commit hook.
- **Windows local discovery** — Derive each network from the OS-reported prefix length and send
  the vendor-compatible limited broadcast from each interface-bound socket. Link-local IPv6 probes
  are omitted until lineup HTTP can preserve their scope.
- **Rust minimum version** — Rust 1.98 is the declared and CI-verified minimum.

### Security

- **Pinned device HTTP** — Metadata and lineup requests go only to the responder that answered
  discovery, on the observed ports, without credentials, redirects, proxies, or DNS, with bounded
  response sizes and times, and only after the device identity matches.
- **Bounded untrusted input** — Discovery replies are rejected when malformed, oversized, or from
  outside the probed prefix; lineup rows are capped while parsing; and the long-lived registry
  caps devices, locators, and origins and refuses conflicting DeviceID ownership.
- **Bounded routed discovery** — Range enumeration requires an explicit RFC 1918 range no wider
  than `/24`, caps candidates, rate, and concurrency, and keeps point-to-point interfaces out of
  ordinary local discovery. Automatic route-derived authority fails closed on ambiguous topology,
  changed policy, clock rollback, or uncertain durability, and stays disconnected until a
  production runner exists.
- **Playback component boundary** — The required GStreamer factories are a startup capability
  check, not a bundling allowlist; future packages must derive and review their real plugin
  closure.
- **Release component policy** — A shared, fail-closed policy rejects dedicated optical-disc
  decryption and proprietary DRM components across repository inputs, build helpers, and the
  preparatory Linux, Flatpak, and macOS artifact validators, while leaving ordinary codecs, TLS,
  and general-purpose cryptography untouched.

### Privacy

- **Local and explicit discovery only** — Balun contacts tuners on attached networks or at
  addresses and ranges you supply; it never calls a cloud discovery service, analytics endpoint,
  or guide scraper.
- **Credential-safe diagnostics** — Advertised URLs are hidden, `DeviceAuth` is never read,
  stored, or printed, and metadata buffers are wiped after parsing.
- **Topology-redacted approvals** — Remembered routed-discovery consent stores only keyed
  fingerprints and bounded policy state, never raw routes, interface names, or prefixes.

### Known limitations

- **macOS live-device acceptance pending** — Live TV with audio is verified on Windows and Linux
  development builds against real tuners; macOS has not yet been exercised against real hardware.
- **No packaged-runtime acceptance yet** — Playback is proven on development runtimes and the
  loopback fake tuner; the packaged codec closure is not yet frozen.
- **No packages** — Flatpak, deb, rpm, DMG, and winget packaging are planned; the release workflow
  builds the diagnostic only.
- **No program guide or hostname entry** — Guide data is a v0.2 candidate; hostname entry is
  planned for v0.1.
- **ATSC 3.0** — HEVC video needs gst-libav or a platform decoder, and AC-4 audio has no open
  decoder, so those channels fail closed and cannot be transcoded.

[Unreleased]: https://github.com/jm2/balun/commits/main
