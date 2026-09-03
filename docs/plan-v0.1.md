# Balun v0.1 implementation plan

- Status: Active
- Target: v0.1.0-alpha.1
- Last updated: 2026-09-02

This is the scope, architecture, and delivery-order contract for the first
alpha. The countable ledger is [`task.md`](task.md); sanitized hardware
evidence is in [`compatibility-v0.1.md`](compatibility-v0.1.md); user-visible
outcomes are in [`../CHANGELOG.md`](../CHANGELOG.md); decisions are in
[ADR-0001](architecture/adr-0001-discovery-playback.md) and
[ADR-0002](architecture/adr-0002-scope-and-diagnostics.md). The original
milestone plan and its ledger are archived in
[`task-foundation-2026-09.md`](task-foundation-2026-09.md).

## 1. Product direction

Balun is a lightweight, cross-platform HDHomeRun live TV viewer built with
Rust, GTK 4, libadwaita, and GStreamer. v0.1 does one job: find HDHomeRun
tuners, keep each device's lineup separate, and play unprotected channels
reliably. It is not a DVR, a tuner administration tool, or a guide service.

The end goal for v0.1.0-alpha.1: a user on Linux, macOS, or Windows installs a
package, sees local tuners and any remembered remote tuner, picks a device and a
channel, watches it with sound, switches channels, and quits without leaving a
tuner allocated. On Linux, an explicitly approved routed scan can also find
tuners across a tunnel.

Primary goals:

- Discover HDHomeRun tuners on ordinary local networks with a fixed packet
  budget and no traffic while idle.
- Reach tuners across routed networks such as WireGuard by exact address or
  hostname, and on Linux by a bounded, user-approved routed scan.
- Keep every device's lineup visibly and structurally separate.
- Start, switch, and stop live TV predictably without leaking tuner
  allocations.
- Report unreachable devices, busy tuners, stale channels, protected channels,
  and missing codecs in plain language that names the device.
- Ship Linux, macOS, and Windows as tested targets with real packages.

"Lightweight" means launch latency, idle CPU and memory, channel-change
latency, discovery traffic, and responsiveness. Self-contained GTK and GStreamer
bundles will not be small.

## 2. v0.1 scope

Included:

- IPv4 broadcast and non-link-local IPv6 multicast tuner discovery.
- Exact IP address entry, hostname entry, and remembered targets rediscovered
  at startup.
- Bounded, user-approved routed discovery on Linux; exact and hostname targets
  on macOS and Windows.
- Multiple devices in a device sidebar, one selected device's lineup in a
  channel sidebar, with favorite, HD, and protected badges.
- Playback of unprotected channels with the installed or bundled GStreamer
  runtime, including audio.
- Stop, volume, mute, fullscreen, connecting and failure states.
- Channel search, a favorites filter, and keyboard navigation.
- Versioned settings for remembered devices and window state.
- Linux, macOS, and Windows CI plus Flatpak, Windows x86_64, and macOS arm64
  packages.

Explicitly deferred:

- Program guide data of any kind (in-band PSIP/EIT, XMLTV, the HDHomeRun XMLTV
  API): v0.2 candidate. The P0.8 spike showed per-channel streams carry no
  PSIP; an in-band guide would crawl full multiplexes.
- Recording, DVR scheduling, timeshift, and trick-play.
- Protected or DRM channel playback.
- AC-4 audio; ATSC 3.0 channels that carry it fail closed with a clear message.
- Channel scanning, antenna setup, firmware updates, or tuner administration.
- A merged "all devices" channel list.
- Transcoding, relaying, or general remote-internet streaming.
- Background EPG harvesting that consumes an otherwise unused tuner.
- Windows arm64 and native Linux distribution packages until they are
  exercised regularly.
- SBOM, build provenance, fuzzing, and a coverage ratchet: beta.

## 3. Current baseline (2026-09-02)

Built and tested:

- Safe-Rust HDHomeRun discovery framing, TLV, CRC, and DeviceID validation;
  per-interface local discovery; exact-address probes; an approved-range
  runner used by the diagnostic.
- A bounded DeviceID registry that keeps every locator and origin without
  merging devices or channels.
- Responder-pinned `discover.json` and `lineup.json` fetching, lineup parsing,
  and a cancellation-aware inspection service behind `balun-discover`.
- A controller on one Tokio worker publishing immutable, URL-free snapshots;
  device selection resolves one lineup without tuning.
- The adaptive three-pane GTK 4 / libadwaita window with virtualized device and
  channel sidebars and the live-TV pane.
- A generation-owned `playbin3` session behind a library-private
  `gtk4paintablesink`, fed through the constant `appsrc://balun` URI by Balun's
  own no-proxy HTTP transport, with Stop, volume, mute, fullscreen, and fixed
  failure categories.
- A loopback fake HDHomeRun device driving the real controller, transport, and
  session end to end, plus a synthetic MPEG-2 acceptance test under headless
  Wayland.
- Tributary-derived build helpers with a runtime plugin gate, installed-runtime
  playback probes, five CI lanes, a release-candidate workflow, and preparatory
  packaging validators.
- Linux route inspection, a keyed approval policy, a durable approval store,
  and route/store observers, present but not connected to any sender.
- Live TV verified by the owner on the Windows development build against real
  tuners: ATSC 1.0 channels play with audio; ATSC 3.0 channels fail closed on
  AC-4 ([compatibility notes](compatibility-v0.1.md)).

- Versioned, atomic settings that remember the window size and maximized
  state and the exact addresses that answered, which are probed again at
  launch; storage for user-assigned names is reserved.

- Hostname entry resolved on the controller to a bounded set of unicast
  addresses, probed one at a time and remembered by name.

Not yet done: macOS live-device acceptance, the Windows and macOS halves of
the per-platform codec contract, the routed sender and its UX, packages, and
the alpha release.

## 4. Architecture

Principles:

- Library first: protocol, discovery, domain, and controller logic are usable
  without GTK, and the default Cargo feature set carries neither GTK nor
  GStreamer.
- Stable identity is separate from network location.
- GTK objects never cross into core services; the controller publishes
  immutable snapshots.
- Network data is untrusted and bounded at every parser boundary.
- Long-running work is cancellable and has a deadline.
- Async results are generation-scoped so stale work cannot update the UI.
- One stream is owned at a time.
- Platform-specific behavior lives behind narrow interfaces and fails closed
  where its proof obligations are unmet.
- Settings and caches are replaceable details, not identity.

Module map as built:

~~~text
src/
  lib.rs, main.rs, app.rs      GTK-free core; thin desktop entry; app lifecycle
  bin/balun-discover.rs        GTK-free diagnostic
  domain/                      DeviceID and device-scoped ChannelKey
  hdhr/                        protocol, device HTTP, lineup, inspection, resolver, fake device
  discovery/                   client, local, manual, registry, routed, routes, approval
  controller/                  runtime actor, snapshots, stream handoff
  settings/                    versioned atomic settings.json store
  playback/                    GStreamer runtime, session, source policy, transport, failures
  ui/                          window, device sidebar, channel sidebar, address dialog, player
~~~

Runtime and concurrency:

- GTK and the GStreamer pipeline live on the GLib main thread.
- One named Tokio worker owns UDP, HTTP, route inspection, and timers.
- Bounded typed channels carry commands and coalesced immutable snapshots.
- Shutdown cancels network work, moves the pipeline to `NULL`, joins the
  transport and controller within a fixed bound, and exits.
- Device selection and tune requests use monotonically increasing generations.

Identity:

- DeviceKey is the validated HDHomeRun DeviceID.
- A DeviceLocator is an address, origin, and observation time; a device keeps
  several and loses none while another is valid.
- ChannelKey is DeviceKey plus the device-native GuideNumber.
- Stream URLs are resolved at tune time and are never identity.

## 5. Discovery policy

Order of authority, least to most expansive: remembered and explicit targets,
local broadcast and multicast, then a user-approved bounded routed scan.

Local discovery:

- Send the documented tuner-only request from each eligible interface-bound
  socket: limited broadcast on Windows, directed subnet broadcast elsewhere,
  scoped site-local multicast for supported IPv6.
- Accept replies only from the probed prefix and discovery port; validate
  framing, CRC, packet type, DeviceID, and string lengths.
- Deduplicate by DeviceID while retaining each locator and origin.
- Fetch bounded device and lineup metadata only from accepted responders.
- Link-local IPv6 stays excluded until lineup HTTP can preserve its scope.

Exact and hostname targets:

- One numeric IPv4 or unscoped IPv6 address, or one hostname resolved to a
  bounded number of usable unicast addresses; never a URL, port, or range.
- At most two request datagrams with 200 ms windows, 16 received datagrams,
  and one accepted identity per operation; at most 32 distinct addresses per
  session.
- A successful target binds to its first DeviceID; remembered targets are
  probed again at startup and never become scan authority.

Approved routed discovery:

- Consider only private IPv4 space behind an active tunnel route or a range
  the user typed; exclude public, default, loopback, link-local, multicast, and
  directly connected LAN routes.
- Enumerate at most one `/24` and 256 candidates; never enumerate IPv6.
- Send only HDHomeRun UDP discovery frames at 64 datagrams per second with
  bounded concurrency, jitter, a 15-second default deadline, progress, and
  immediate cancellation.
- Require remembered approval bound to a keyed, topology-redacted fingerprint
  of the targets, tunnel, routes, and budget; revoke it when that fingerprint,
  the route table, or the durable store changes.
- Apply cooldown and exponential backoff after empty runs; rerun on a debounced
  network change or explicit refresh, never on a timer.
- Run approved tunnel discovery even when a local tuner exists.

Native providers: Linux uses rtnetlink and recognizes WireGuard and other
unambiguous tunnel links. macOS and Windows providers stay unavailable with a
fixed reason until a safe route-table wrapper, a provable routing domain, and a
stable tunnel identity exist; exact and hostname targets are the supported path
there.

Network rules:

- Treat discovery replies, JSON, channel names, and returned URLs as hostile.
- Apply strict packet, response, row-count, and string limits, plus connect,
  header, body, and wall-clock deadlines.
- Accept device HTTP only from an accepted locator on the observed metadata and
  stream ports; rebind advertised hosts to the responder.
- Reject URL credentials, unexpected schemes, and every redirect; send no
  Referer; use no proxy and no DNS for device HTTP.
- Never deserialize, persist, or print `DeviceAuth`; redact query values.
- Device addresses and names may appear in errors and diagnostics (ADR-0002);
  stream URLs stay out of GTK-facing snapshots.
- Discovery is read-only: no channel scans, configuration changes, firmware
  operations, or tuner locks.
- No cloud discovery, telemetry, or analytics.

## 6. Playback

Implemented path:

- `playbin3` with a library-private `gtk4paintablesink` shown through
  `GtkPicture`, adaptive deinterlacing, and forced aspect preservation.
- The constant `appsrc://balun` URI; a schema-validated `source-setup` handler
  accepts exactly one built-in `appsrc` and configures a live MPEG-TS byte feed
  with a 4 MiB queue.
- Balun's own `reqwest` transport: no proxy, no redirects, no Referer, HTTP/1.1,
  connect, header, and idle-read deadlines, bounded 64 KiB buffers through an
  eight-slot channel to a blocking feeder.
- Failures reduce to seven fixed categories: tuner busy (503), channel missing
  (404), rejected (other status), offline (connect, stall, truncation), missing
  codec or plugin, protected, internal.

Every channel change:

1. Invalidate the previous tune generation.
2. Cancel the previous transport, then move its pipeline to `NULL`.
3. Join the transport workers and pipeline within five seconds; a failure
   quarantines the owner instead of starting a successor.
4. Construct the new pipeline for the authorized handoff.
5. Publish connecting, playing, or a failure category.

Device HTTP semantics: closing the stream releases the tuner; 404 means an
unknown or stale channel and should prompt a lineup refresh; 503 means busy
tuners or a tuning failure and must not trigger aggressive retries.

Codec policy:

- MPEG-2, H.264, AC-3, and AAC over MPEG-TS are the v0.1 contract; Windows has
  demonstrated it against real ATSC 1.0 channels, and P0 records the exact
  per-platform plugin set for packaging.
- HEVC decodes where gst-libav or a platform decoder is installed and is
  reported as a capability, not promised.
- AC-4 has no open decoder; those channels fail closed with a message that names
  the missing codec (P1.4 decides whether video-only playback is offered).
- DRM-tagged channels stay visible but disabled.
- The shared [release component policy](release-component-policy.md) keeps
  optical-disc decryption and proprietary DRM components out of every package;
  ordinary codecs need their own compatibility and licensing review.

If measured live streams show drift or unstable buffering, the fallback is a
controlled `appsrc ! tsdemux ! decodebin3` graph. That is not part of v0.1
unless the evidence demands it.

## 7. State and diagnostics

Persisted state is atomic, versioned JSON:

- Friendly device names and remembered exact or hostname targets.
- Approved routed ranges and their fingerprints (Linux).
- Window geometry and UI preferences.

No credentials, stream URLs, or incidental topology are persisted. Lineup
caches are added only when an offline device needs a visibly stale lineup, and
SQLite only if a guide feature later proves flat caches inadequate.

Diagnostics report device name and address, DeviceID suffix, discovery origin,
reachability, probe counts and packet budget, required and missing GStreamer
capabilities, and the playback failure category. They never include
`DeviceAuth`.

## 8. Cross-platform, CI, and release

Floors: Rust 1.98, GTK 4.16, libadwaita 1.6, GStreamer 1.20 with the base,
good, bad, and gst-plugins-rs (gtk4) plugins. Development is Linux-led, but a
change is not complete until macOS and Windows pass.

CI on every push and pull request: Linux quality (policy tests, fmt, strict
Clippy, debug and release tests), MSRV, Linux desktop (build, installed-runtime
probes, Wayland lifecycle smokes), and macOS and Windows compile smoke lanes
that build the desktop through the same helpers developers use. A manual
release-candidate workflow builds an immutable tag on all three platforms.

Release contract:

- Accept only a signed, annotated, v-prefixed Semantic Version tag on `main`.
- Build every package from that one SHA with locked dependencies and pinned
  actions.
- Stage only a capability-derived GStreamer closure into self-contained
  bundles; apply the component policy before staging, during native-import
  traversal, over the completed tree, and after reopening the final artifact.
- Require an exact artifact inventory and checksums; create a draft release;
  give write permission only to a final publication job that checks out no
  source.
- Keep tag, Cargo, changelog, AppStream, and package versions in agreement.

The Tributary port ledger for `scripts/` and `build-aux/` is
[`tributary-build-infrastructure.md`](tributary-build-infrastructure.md).

## 9. Implementation order

Phases are dependency order, not a release promise. Hardware evidence comes
first because it decides the codec contract that packaging depends on.

**P0 — Evidence and contract.** Record the Windows result, repeat live TV on
Linux and macOS, measure first-frame, switch, and tuner-release times, freeze
the per-platform plugin contract, land sanitized fixtures, and run a one-day
in-band guide spike. Exit: every supported desktop has played a real channel
with audio, the numbers are written down, and the guide decision is made.

**P1 — Viewer completion.** Versioned settings, remembered targets and
hostname entry, errors that name the device, the missing-codec message,
search and favorites, keyboard navigation. Exit: a user can set up two sites'
tuners once and use the viewer without the diagnostic.

**P2 — Routed discovery (Linux).** Connect the monitored runner to the existing
approval store and observers, add the approval and progress UX, reconcile
network changes, expose diagnostics, and prove one routed case plus two-site
multi-device validation. Exit: local and remote tuners coexist within the
documented traffic budget with deterministic cancellation and revocation.

**P3 — Packages.** Desktop metadata and assets, then Flatpak, Windows ZIP and
installer, and macOS DMG with the capability-derived closure and all four
component gates, release automation, and the CI hardening packages make
meaningful. Exit: every artifact is reopened, probed, and validated before
upload.

**P4 — v0.1.0-alpha.1.** Validate the packaged artifacts on every platform,
complete the hardware matrix and budgets, run the security and privacy review,
publish the support and limitations matrix, and cut the prerelease.

v0.2 candidates: an in-band guide crawled from full-multiplex streams, since
per-channel streams carry no PSIP (P0.8); XMLTV file or URL with explicit
mappings; native macOS and Windows route providers; SBOM, provenance, fuzzing,
and a coverage ratchet; conduct, support, and issue-form governance once
contributors exist.

## 10. Verification strategy

Unit and property tests: discovery framing and hostile packets; DeviceID
validation and duplicates; address classification, route filtering, budgets,
cooldowns, and cancellation; lineup fields, tags, sorting, and hostile URLs;
device HTTP redirect, credential, scheme, and body policy; registry claims and
expiry; generation races; settings migration.

Integration tests: the loopback fake HDHomeRun device through discovery,
lineup, DRM refusal, tuning, switching, 404 and 503, cancellation, and
shutdown; the synthetic MPEG-2 fixture through `playbin3` and
`gtk4paintablesink` under headless Wayland; fake route providers; installed-
and packaged-runtime probes.

Real hardware complements rather than replaces those tests. Results are
recorded in [`compatibility-v0.1.md`](compatibility-v0.1.md) without device
IDs, addresses, channel names, or credentials.

| Site | Qty | Device | Validation role |
| --- | ---: | --- | --- |
| Primary | 1 | HDHR4-2US CONNECT Duo | Two-tuner ATSC 1.0, interlaced MPEG-TS |
| Primary | 1 | HDHR5-4K (non-FLEX) | Local ATSC 1.0/3.0, modern codecs |
| Secondary | 1 | HDHR3-PRIME | CableCARD/QAM, tuner-busy and protected-channel paths |
| Secondary | 1 | HDHR5-4K (non-FLEX) | Routed ATSC 1.0/3.0, cross-site identity |
| Deferred | 2 | HDHR5-4DT (Australia) | DVB-T/T2/C when accessible |

The two sites are expected to be joined with UniFi Site Magic; Balun treats
that as a generic routed network. The matrix records which discovery path
found each device, that the two HDHR5-4K units stay distinct, routed packet
counts and timings, and route-change, site-disconnect, and device-restart
behavior.

## 11. Primary risks

- Real devices may differ from the documented discovery and HTTP behavior.
- GStreamer codec availability differs between system and bundled runtimes.
- AC-4 and protected ATSC 3.0 channels have no distributable playback path.
- Packaging breadth can outpace feature work; the phase order guards this.
- macOS and Windows local-network permissions may hide discovery failures.
- A broad tunnel route contains too many hosts for friendly enumeration.
- Rapid channel changes can consume multiple tuners if teardown ownership
  slips.
- In-band guide tables may not survive the device's PID filter.

## 12. References

- [HDHomeRun Device Discovery API](https://info.hdhomerun.com/info/discovery_api)
- [HDHomeRun HTTP Development Guide](https://www.silicondust.com/hdhomerun/hdhomerun_http_development.pdf)
- [HDHomeRun model and software downloads](https://info.hdhomerun.com/info/downloads)
- [HDHomeRun PRIME specifications](https://info.hdhomerun.com/info/prime)
- [HDHomeRun CONNECT specifications](https://info.hdhomerun.com/info/connect)
- [HDHomeRun ATSC 3.0 models](https://info.hdhomerun.com/info/atsc_3.0)
- [HDHomeRun CableCARD DRM behavior](https://info.hdhomerun.com/info/drm)
- [HDHomeRun DVB-C update information](https://info.hdhomerun.com/info/expand)
- [SiliconDust libhdhomerun](https://github.com/Silicondust/libhdhomerun)
- [HDHomeRun XMLTV API](https://info.hdhomerun.com/info/dvr%3Axmltv)
- [WireGuard](https://www.wireguard.com/)
- [GStreamer playbin3](https://gstreamer.freedesktop.org/documentation/playback/playbin3.html)
- [GStreamer GTK4 paintable sink](https://gstreamer.freedesktop.org/documentation/gtk4/index.html)
- [GStreamer MPEG-TS support](https://gstreamer.freedesktop.org/documentation/mpegts/index.html)
- [Libadwaita NavigationSplitView](https://gnome.pages.gitlab.gnome.org/libadwaita/doc/main/class.NavigationSplitView.html)
- [ATSC A/65](https://www.atsc.org/wp-content/uploads/2021/04/A65_2013.pdf)
- [DVB EN 300 468](https://dvb.org/wp-content/uploads/2022/11/A038r16_Specification-for-Service-Information-SI-in-DVB-Systems_Interim-draft_EN_300-468-v1-18-1_Apr-2023.pdf)
- [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
- [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
