# Balun v0.1 Implementation Plan

- Status: Active
- Target: v0.1.0-alpha.1
- Last updated: 2026-09-01

## 1. Product direction

Balun is a lightweight cross-platform HDHomeRun live TV viewer built
with Rust, GTK 4, libadwaita, and GStreamer.

The first release will concentrate on one job: discover one or more HDHomeRun
tuners, keep their lineups separate, and begin reliable unprotected live
playback with minimal delay. It will not attempt to be a DVR, tuner
configuration utility, or universal television platform.

Balun takes structural and release-engineering lessons from Tributary while
remaining a much narrower application. In particular, it will reuse
Tributary's proven GTK/Tokio separation, stable-identity model, GStreamer
packaging knowledge, and release validation patterns. It will not copy
Tributary's music-specific models, database stack, source-registry
complexity, or large UI and engine coordinators.

### 1.1 Primary goals

- Discover HDHomeRun tuners on ordinary local networks.
- Find tuners across routed networks such as WireGuard without depending on
  broadcast or multicast forwarding.
- Keep every device's lineup visibly and structurally separate.
- Start, switch, and stop live TV streams predictably without leaking tuner
  allocations.
- Show useful channel and program information from sources that are legal,
  privacy-respecting, and freely available to the user.
- Support Linux, macOS, and Windows as real tested targets.
- Establish documentation, CI, packaging, and release discipline before the
  codebase becomes difficult to reshape.

### 1.2 Product success criteria

The compatibility spike will establish measured targets, but v0.1 should
demonstrate:

- A responsive warm launch with no network work on the GTK main thread.
- Near-zero idle CPU when no stream or discovery operation is active.
- A fixed and inspectable packet budget for every discovery run.
- Cancellation of an in-progress discovery or tune request.
- Complete release of the old tuner before a new channel is opened.
- No merged channel identity or lineup state across devices.
- Actionable errors for unreachable devices, busy tuners, stale channels,
  protected channels, and missing codecs.
- Reproducible behavior on all three desktop platforms.

"Lightweight" refers primarily to launch latency, idle CPU and memory,
channel-change latency, discovery traffic, and responsiveness. Self-contained
GTK and GStreamer application bundles will not necessarily be small.

## 2. v0.1 scope

### 2.1 Included

- Standard IPv4 and IPv6 HDHomeRun tuner discovery.
- Manual device address or hostname entry.
- Cached-address targeted rediscovery.
- Bounded, user-approved routed/tunnel discovery.
- Multiple devices with a dedicated device sidebar.
- A separate channel list for the selected device.
- Lineup number, name, favorite status, DRM status, and device information.
- Playback of unprotected streams supported by the installed or bundled
  GStreamer runtime.
- Volume, mute, fullscreen, buffering state, and useful playback errors.
- Opportunistic guide information from the stream already being watched
  where the device and broadcast preserve it.
- User-configured XMLTV file or URL import after the basic playback slice.
- Versioned settings and bounded lineup/guide caches.
- Linux, macOS, and Windows CI coverage.
- Initial Flatpak, Windows x86_64, and macOS arm64 release packages.

### 2.2 Explicitly deferred

- Recording, DVR scheduling, timeshift, and trick-play.
- Protected or DRM channel playback.
- Channel scanning, antenna setup, firmware updates, or tuner administration.
- A merged "all devices" channel list.
- Transcoding or relaying streams to other clients.
- General remote-internet streaming.
- Background EPG harvesting that consumes an otherwise unused tuner.
- Bundled third-party guide scrapers.
- Guaranteed ATSC 3.0, AC-4, or protected-content support.
- Windows arm64 and native distribution packages until they are exercised
  regularly.

## 3. User experience

### 3.1 Main window

The desktop layout has two left sidebars and one player:

~~~text
┌──────────────┬──────────────────────────┬────────────────────────────┐
│ HDHR devices │ Channels for one device  │ Live video                 │
│              │                          │                            │
│ Living room  │ 5.1  WXYZ   Now...       │ Player overlay             │
│ Basement     │ 7.1  WABC   News...      │ channel / errors / volume  │
│ VPN device   │ 11.2 WXQZ   No guide     │                            │
└──────────────┴──────────────────────────┴────────────────────────────┘
~~~

The outer device sidebar should be narrow and show:

- Friendly device name, with a stable DeviceID suffix where useful.
- Reachability and last-seen state.
- Tuner count and current address.
- Refresh, add-device, and routed-search actions.

The channel sidebar should show only the selected device's lineup:

- Natural-sorted virtual channel number and channel name.
- Favorite and unavailable/DRM indicators.
- Optional now-playing title, time, and progress when guide data exists.
- Search and filtering without changing device identity.
- A compact now/next or channel-information area.

The player area should contain:

- Video rendered as a GdkPaintable in GtkPicture.
- A minimal overlay for channel identity, buffering, errors, mute, volume,
  and fullscreen.
- A clear empty state when no channel is selected.
- A clear offline state without automatically tuning the last channel.

Nested AdwNavigationSplitView containers should keep all three panes visible
at normal desktop widths and collapse them into sensible navigation at smaller
widths. Lists should use Gio ListStore, Gtk SingleSelection, and
SignalListItemFactory with strict recycled-row reset and unbind behavior.

### 3.2 Interaction rules

- Selecting a device never starts playback.
- Selecting a device atomically replaces the channel model.
- Activating a channel starts a generation-scoped tune request.
- A second activation cancels and tears down the first request.
- Device or channel disappearance does not silently switch the user to a
  different device or channel.
- Protected channels remain visible but are disabled with an explanation.
- Discovery and routed scans expose progress and cancellation.

## 4. Architecture

### 4.1 Design principles

- Library-first: protocol, discovery, domain, guide, and controller logic must
  be usable without GTK.
- Stable identity is separate from network location.
- GTK objects do not cross into core services.
- Network data is untrusted and bounded at every parser boundary.
- Long-running work is cancellable and has a deadline.
- Async results are generation-scoped so stale work cannot update the UI.
- Platform-specific behavior lives behind narrow interfaces.
- One stream is owned at a time in v0.1.
- Settings and caches are replaceable details, not domain identity.

### 4.2 Initial module shape

~~~text
src/
  main.rs
  lib.rs
  app.rs
  domain/
    identity.rs
    device.rs
    channel.rs
    guide.rs
  hdhr/
    protocol.rs
    client.rs
    lineup.rs
    stream.rs
  discovery/
    standard.rs
    routed.rs
    manual.rs
    registry.rs
    routes.rs
  guide/
    provider.rs
    broadcast.rs
    xmltv.rs
    cache.rs
  controller/
    commands.rs
    events.rs
    state.rs
  playback/
    gstreamer.rs
    runtime_probe.rs
  ui/
    window.rs
    device_sidebar.rs
    channel_sidebar.rs
    player_view.rs
    preferences.rs
  platform/
    mod.rs
    linux.rs
    macos.rs
    windows.rs
    bundle.rs
~~~

This is a module boundary, not a requirement to create empty files before
they have behavior.

### 4.3 Runtime and concurrency

- GTK and GStreamer pipeline ownership stay on the GLib main thread.
- A Tokio runtime handles UDP, HTTP, route inspection, timers, XMLTV, and
  cache I/O.
- Typed bounded channels carry commands, immutable snapshots, and events.
- Bursty status changes should be coalesced rather than creating unbounded
  queues.
- Application shutdown cancels discovery and HTTP work, moves the pipeline
  to NULL, waits for bounded cleanup, and exits normally.
- Device selection, lineup refresh, guide refresh, and tune requests each use
  monotonically increasing generations.

### 4.4 Domain identity

- DeviceKey is the validated HDHomeRun DeviceID.
- DeviceLocator contains an address, base URL, discovery origin, and
  observation time.
- ChannelKey is DeviceKey plus the device-native GuideNumber.
- Stream URLs are resolved at tune time and are never used as identity.
- Guide mappings are scoped to ChannelKey, not just a channel number or call
  sign.

The compact DeviceRegistry aggregates observations from broadcast, cached,
manual, neighbor-assisted, and routed discovery. Loss of one locator does not
remove a device while another valid observation remains.

## 5. HDHomeRun discovery

### 5.1 Standard discovery

Normal discovery will:

1. Probe previously successful and manually configured targets.
2. Send the documented tuner-only IPv4 broadcast request on eligible
   interfaces.
3. Perform supported IPv6 local discovery.
4. Collect replies for the documented bounded discovery window.
5. Validate framing, CRC, packet type, DeviceID, string lengths, and reply
   origin.
6. Deduplicate by DeviceID while retaining every valid locator and discovery
   origin.
7. Fetch bounded device and lineup metadata only from accepted responders.

Discovery sockets should be reused during a run, and repeated create/destroy
cycles should be avoided.

### 5.2 Routed and WireGuard discovery

A routed layer-3 tunnel supplies routes but no remote service directory.
Balun therefore uses the following order:

1. Exact targeted probes for cached, manual, DNS, and high-confidence neighbor
   candidates.
2. Inspection of active tunnel routes through a platform RouteProvider.
3. A remembered, user-approved bounded scan of an eligible route.
4. Manual address or smaller-range entry for routes that cannot be scanned
   safely.

The route policy is:

- Consider only private IPv4 or IPv6 ULA space associated with a tunnel, or a
  range the user explicitly selected.
- Exclude public, default, loopback, link-local, multicast, and already
  covered directly connected LAN routes.
- Automatically enumerate no more than one /24 and 256 total IPv4 candidates.
- Never enumerate an IPv6 prefix. IPv6 requires exact cached, manual, DNS, or
  neighbor-derived targets.
- Send only HDHomeRun targeted UDP discovery packets to candidates.
- Do not TCP-scan, HTTP-probe, or send directed broadcast to nonresponders.
- Begin with a 64-datagram-per-second token bucket, small jitter, bounded
  concurrency, and an overall deadline. Tune these values only from measured
  hardware behavior.
- Show progress and allow immediate cancellation.
- Apply a 15-to-30-minute cooldown and exponential backoff after empty
  automatic runs.
- Re-run on a debounced network change or explicit refresh, not on a permanent
  timer.
- Run approved tunnel discovery even if a local tuner was found, because
  local and remote devices may coexist.
- Invalidate remembered route approval when the network fingerprint changes
  materially.

Initial route providers:

- Linux: netlink interface and route data, including WireGuard link kind.
- macOS: routing APIs and getifaddrs, including utun interfaces.
- Windows: GetIpForwardTable2 and GetAdaptersAddresses, including tunnel
  interface types.

Implementation status: the platform-neutral candidate policy, bounded routed
target runner, conservative Linux rtnetlink provider, and pure remembered
approval policy are present. The strict durable store retains only keyed
fingerprints and bounded policy metadata, with private key storage,
cross-process locking, atomic durability barriers, global run sequencing,
quarantine, and topology-free revocation. Unix store operations are bound to
one validated directory descriptor, while a separate Linux inotify observer
uses that same identity and brackets exact store rereads with complete bounded
drains. The observer remains an unwired foundation and cannot yet authorize
traffic. The store-owned fresh-snapshot gate
requires the exact active fingerprint and run, rebuilds the complete proposal,
permits only a transient interface-ID replacement, and caps work to the
remaining lease. The Linux socket factory now performs one interface pin plus
matching name/index readbacks before and after local bind, then seals the
nonblocking socket behind a non-cloneable capability with no I/O escape. The
packet-free admission boundary registers invalidation before reserve, obtains
the fresh snapshot, rejects clock rollback or store-time clamping, and maps the
authority onto one non-extending monotonic deadline. Post-reserve failure or
abandonment deliberately retains the crash-conservative durable reservation.

Route-derived execution remains disconnected from the diagnostic. Production
wiring still requires one controller to own non-cloneable route and approval-
store observer sessions, activate only a baseline proven by both observers,
retain both healthy epochs through the run, perform final
socket/route/deadline validation immediately before each send, and use a
consuming runner which stops all packet work before completing the exact durable
run with a fresh paired clock sample. No partial implementation may fall back
to an unpinned or unmonitored socket.

Native macOS and Windows automatic providers are intentionally unavailable,
not merely stubbed. macOS needs a separately audited safe wrapper for bounded
raw route data plus a conservative answer for scoped and per-application route
policy. Windows needs a safe owned route-table wrapper, a supported way to
prove the socket routing compartment, and LUID-bound tunnel identity; a
mutable adapter name or generic virtual-adapter type is insufficient. Exact
address and explicitly supplied private-range discovery remain available on
those platforms. Neither provider may broaden or silently fall back when these
proof obligations are unmet.

For wider routed sites, the supported solutions are an exact address,
operator-provided DNS record, explicit smaller range, or a future
administrator-operated discovery relay.

### 5.3 Discovery implementation decision

ADR-0001 will compare:

- SiliconDust's libhdhomerun behind a narrow Rust FFI boundary.
- A small Rust implementation of the documented discovery packet, TLV, and
  CRC protocol.

The official library offers mature device compatibility but adds a C build
and LGPL distribution obligations. A Rust implementation simplifies packet
budgeting and cross-platform packaging but requires stronger fixture,
hardware, property, and fuzz validation.

The application will depend on a narrow discovery trait so this choice does
not leak into the registry, controller, or UI.

### 5.4 Network security

- Treat discovery replies, JSON, channel names, and returned URLs as hostile.
- Validate DeviceID checksums and reply-source consistency.
- Apply strict packet, response, object-count, row-count, and string limits.
- Apply connect, headers, body, and wall-clock deadlines.
- Accept device HTTP only from an approved locator.
- Reject URL credentials and unexpected schemes.
- Bind or normalize advertised hosts to the accepted responder.
- Require the observed device metadata and stream ports; do not follow
  responder-supplied arbitrary ports.
- Reject cross-host redirects; use no redirects unless a same-origin case is
  explicitly validated.
- Send no Referer and redact URLs or query values from logs.
- Never persist or log DeviceAuth in v0.1.
- Strip unsafe control characters from display strings.
- Discovery is read-only and never initiates channel scans, configuration
  changes, firmware operations, or tuner locks.
- Do not use undocumented cloud discovery by default.

## 6. Lineups and playback

### 6.1 Lineup handling

Balun will fetch the device-provided lineup URL and use:

- GuideNumber.
- UTF-8 GuideName.
- Tags such as favorite and drm, including current firmware's equivalent
  dedicated `Favorite`, `DRM`, and `HD` sentinel fields.
- The device-supplied stream path and port, with its host pinned to the
  validated responder.

Unknown JSON fields are ignored, while malformed known fields, oversized
responses, unsafe URLs, and excessive rows are rejected. Lineups remain
partitioned by DeviceID. A cached lineup may be shown as stale when a device
is offline, but it must not imply that the device is currently playable.

### 6.2 GStreamer MVP

The first player implementation uses:

- playbin3.
- gtk4paintablesink exposed through GtkPicture.
- The validated, responder-pinned device stream URI.
- GStreamer bus handling for error, EOS, state, buffering, stream collection,
  and missing-plugin messages.
- Deinterlacing support suitable for normal 1080i broadcast content.
- Generation-scoped bus watches and state changes.

On every channel change:

1. Invalidate the previous tune generation.
2. Set the previous pipeline to NULL.
3. Wait for bounded teardown and detach stale bus work.
4. Create or reconfigure the pipeline for the new stream.
5. Expose buffering or actionable failure state.

Expected device HTTP behavior:

- Closing the stream releases the allocated tuner.
- HTTP 404 indicates an unknown or stale virtual channel and should prompt a
  lineup refresh.
- HTTP 503 can indicate that all tuners are busy or authorization/tuning
  failed. It should not trigger aggressive retries.

If measurements show long-running drift or unstable live buffering, the next
step is a controlled pipeline based on souphttpsrc, mpegtslivesrc, tsdemux,
and decodebin3. That complexity is not part of the MVP unless the spike proves
playbin3 inadequate.

### 6.3 Codec policy

The compatibility spike must cover representative:

- MPEG-2 and H.264 video.
- Interlaced and progressive content.
- AC-3 and AAC audio.
- MPEG-TS demultiplexing and clock behavior.

HEVC, E-AC-3, ATSC 3.0, AC-4, captions, and alternate audio tracks should be
reported through runtime capabilities. They are not promised until real
cross-platform probes pass.

DRM-tagged channels remain visible but unavailable. Balun's shared
[release component policy](release-component-policy.md) excludes dedicated
optical-disc copy-control and proprietary-DRM components that this application
does not use. The current repository/input gate is in CI; it is not a completed
package claim and does not broadly deny ordinary codecs, containers, TLS, or
general-purpose cryptography.

## 7. Guide data

GuideService uses ordered providers while preserving source and freshness:

1. Lineup metadata is always available.
2. In-band ATSC PSIP or DVB EIT is read opportunistically from the stream
   already being watched.
3. A user-configured XMLTV file or URL can supply broader listings.
4. The official subscription-backed HDHomeRun XMLTV API may be added later.

### 7.1 In-band guide rules

- Never allocate a second tuner solely to harvest guide data by default.
- Verify during the spike whether the device's virtual-channel HTTP PID
  filter preserves guide tables.
- Accept that completeness and horizon vary by broadcaster.
- Store events against the multiplex and mapped ChannelKey with explicit
  freshness.
- Do not present best-effort data as a complete schedule.
- Treat GStreamer's MPEG-TS section API as a compatibility risk until it is
  exercised with captured streams.

### 7.2 XMLTV rules

- Support a user-owned local file or explicitly configured URL.
- Do not bundle website scrapers or questionable listings providers.
- Require explicit per-device channel mapping.
- Preserve source attribution and retrieval time.
- Bound download size, decompressed size, document depth, event count, and
  time range.
- Handle time zones and daylight-saving transitions through fixtures.
- Refresh according to the source's terms, with backoff and conditional HTTP
  requests where available.

The official HDHomeRun 14-day XMLTV API requires a tuner and paid DVR guide
subscription. If later supported, DeviceAuth must be fetched fresh, never
logged, and handled according to SiliconDust's randomized refresh guidance.

## 8. State, privacy, and diagnostics

Initial persisted state uses atomic, versioned JSON:

- Friendly device names.
- Manual endpoints and last successful locators.
- Approved routed discovery ranges and their network fingerprints.
- Discovery budgets and preferences.
- Guide provider configuration and channel mappings.
- Window geometry and UI preferences.

Lineup and guide caches are stored separately with schema versions, bounded
size, and expiration. SQLite should be introduced only if guide query volume
demonstrates that flat bounded caches are inadequate.

Diagnostics should report:

- DeviceID suffix, discovery origin, address family, and reachability.
- Discovery strategy, candidate count, packet budget, elapsed time, and
  cancellation.
- Lineup and guide freshness.
- Required and missing GStreamer capabilities.
- High-level playback state and sanitized error details.

Diagnostics must not include DeviceAuth, full sensitive URLs, or unrelated
network inventory.

## 9. Cross-platform policy

Development may be Linux-led, but architectural changes are not complete
until macOS and Windows compile and smoke tests pass.

The first compatibility spike will choose and record:

- Rust MSRV.
- GTK and libadwaita API floors.
- GStreamer runtime floor and required plugin set.
- Linux native-versus-Flatpak support expectations.
- macOS local-network privacy declarations and any entitlement requirements.
- Windows firewall and local-network diagnostics.

Tributary's currently proven GTK 4.16/libadwaita 1.6 and platform bundle
setup are the initial reference. Balun may lower that floor if its smaller UI
can do so without fragmenting the bundle and CI matrix.

Every completed package must run an exact runtime probe for:

- GTK and libadwaita initialization.
- playbin3.
- HTTP source.
- MPEG-TS demuxer.
- Expected decoders and parsers.
- Platform audio sink.
- gtk4paintablesink and a tiny synthetic video path.

## 10. Delivery milestones

### Milestone 0: Compatibility and protocol spike

Deliverables:

- Sanitized discover, device, lineup, error, and stream fixtures from
  representative hardware.
- Discovery implementation comparison and ADR-0001.
- Proven local and targeted discovery.
- A minimal playbin3 plus gtk4paintablesink playback experiment.
- MPEG-2, H.264, interlace, AC-3, and AAC observations.
- Rapid tune/teardown measurements.
- A WireGuard or equivalent routed test where broadcast fails and targeted
  discovery succeeds.
- An in-band guide-table availability report.
- Recorded Rust, GTK, libadwaita, and GStreamer floors.

Exit criteria:

- The core discovery and playback approach is demonstrated on real hardware.
- Major codec or EPG gaps are reflected honestly in v0.1 scope.
- Platform and licensing decisions needed for scaffolding are recorded.

### Milestone 1: Repository foundation

Deliverables:

- Cargo package with a thin binary and GTK-free library.
- Application ID io.github.jm2.Balun used consistently.
- Minimal adaptive three-pane window.
- Tokio/GLib bridge with bounded channels and clean shutdown.
- Domain identity types and immutable controller state.
- Versioned settings foundation.
- README, changelog, contributing, security, issue/PR templates, and release
  documentation.
- Checksum-pinned shared release-component policy plus deterministic, bounded
  repository and packaging-input validation.
- Fast Linux CI and macOS/Windows compile smoke jobs.

Exit criteria:

- The application opens and shuts down cleanly on all targets.
- Core modules can be unit-tested without GTK.
- Formatting, strict linting, locked tests, metadata checks, and audit pass.

### Milestone 2: Playable vertical slice

Deliverables:

- Standard discovery and manual address entry.
- Device registry and one selected-device lineup.
- Device and channel sidebars with virtualized models.
- Unprotected live playback.
- Volume, mute, fullscreen, buffering, and errors.
- Deterministic pipeline teardown and cancellation.
- Exact packaged-runtime capability probe.

Exit criteria:

- A user can launch Balun, find a local tuner, choose a device and channel,
  watch it, switch channels, and exit without a leaked tuner allocation.
- 404, 503, protected channel, missing codec, and offline-device paths are
  covered by tests or fixtures.
- Linux, macOS, and Windows smoke validation passes.

### Milestone 3: Multi-device and routed discovery

Deliverables:

- Multiple locator claims per stable DeviceID.
- Fully separate lineups and ChannelKeys for every device.
- Cached exact-address rediscovery.
- Safe native RouteProvider implementations where the platform proof
  obligations can be met, with an explicit unavailable reason and no broader
  fallback everywhere else.
- Approved, bounded tunnel scans with progress, cancel, cooldown, and backoff.
- Debounced network-change handling.
- Useful device and discovery diagnostics.

Exit criteria:

- Local and remote tuners coexist without merged channel state.
- The same tuner discovered through multiple paths appears once.
- Loss of one locator does not remove a device with another valid claim.
- A routed test on every enabled provider demonstrates discovery within the
  documented traffic budget; Linux is required for v0.1.
- Route exclusions, candidate caps, and cancellation have deterministic tests.

### Milestone 4: Guide and usability

Deliverables:

- Now/next presentation where data is available.
- Proven active-stream ATSC/DVB guide extraction, or a documented reason it
  is unavailable for a device class.
- XMLTV file and URL support with explicit mappings.
- Search, favorites, keyboard navigation, accessibility, and polished empty
  and error states.
- Bounded guide cache with source and freshness.

Exit criteria:

- Missing guide data degrades to clear lineup information.
- Guide data cannot merge channels across devices accidentally.
- XMLTV limits, mappings, time zones, and refresh behavior are tested.
- No background guide behavior consumes an unexpected tuner.

### Milestone 5: Packaging and release candidate

Deliverables:

- Flatpak x86_64 and aarch64.
- Windows x86_64 portable ZIP and installer.
- macOS arm64 DMG.
- Runtime dependency closure and packaged probes.
- Capability-derived GStreamer staging for self-contained bundles: include
  only the reviewed HTTP, MPEG-TS, parser/decoder, and platform sink plugin
  closure instead of copying a whole plugin distribution. Distribution-owned
  and shared runtimes remain documented external boundaries, not an allowlist
  claim.
- Per-platform denied-component checks before staging, throughout native-import
  dependency closure, over each completed app tree, and after reopening every
  final artifact.
- Exact artifact inventory, checksums, SBOM, and provenance.
- Draft-release automation and release-check command.
- User-focused release notes and known limitations.

Exit criteria:

- Every artifact is reopened and tested after packaging.
- Every platform loads the shared denied-component policy and fails closed if
  its staging, import, tree, or reopened-artifact inspector cannot complete.
- Tag, Cargo, lockfile, changelog, AppStream, and package versions agree.
- No release is publicly visible until every required artifact validates.
- Only the final publication job has release-write permission.

### Milestone 6: v0.1.0-alpha.1 validation

Deliverables:

- Real-hardware compatibility matrix.
- Wayland, X11, macOS, and Windows smoke results.
- Startup, idle resource, discovery traffic, tune latency, and teardown
  measurements.
- Security/privacy review of discovery and guide behavior.
- Signed annotated prerelease tag and final artifact publication.

Exit criteria:

- All declared v0.1 behavior is supported by tests and release evidence.
- Unsupported hardware, codecs, guide sources, and platforms are documented.
- No known issue can allocate unexpected tuners, scan unapproved networks,
  disclose secrets, or mix device identities.

## 11. Verification strategy

### 11.1 Unit and property tests

- Discovery framing, TLV parsing, CRC, unknown tags, truncated packets, and
  oversized fields.
- DeviceID validation and duplicate replies.
- Address classification, tunnel route filtering, range caps, token-bucket
  accounting, cooldowns, and cancellation.
- Lineup missing fields, tags, natural channel sorting, row limits, and
  hostile URLs.
- Redirect, same-origin, credential, scheme, and response-body policies.
- Registry claims and locator expiry.
- Generation handling for device, lineup, guide, and tune races.
- XMLTV mappings, decompression limits, time zones, and daylight-saving
  boundaries.
- Versioned state migration and atomic persistence.

### 11.2 Integration tests

- A fake UDP and HTTP HDHomeRun server.
- Discovery plus device and lineup metadata.
- Duplicate multi-interface replies.
- Slow, chunked, truncated, malformed, oversized, and redirected responses.
- HTTP 404, 503, disconnect, and tune cancellation.
- A local synthetic MPEG-TS fixture through playbin3 or fakesink.
- Rapid channel switching and application shutdown.
- A fake RouteProvider for every platform.
- A routed-network fixture where broadcast cannot cross the boundary but a
  bounded targeted probe can.

### 11.3 Fuzzing

- Unauthenticated HDHomeRun discovery packet parser.
- Lineup and device JSON boundary.
- XMLTV parser or event-normalization boundary.
- MPEG-TS parsing remains GStreamer's responsibility, but Balun-owned section
  conversion should be fuzzed if custom parsing is added.

### 11.4 Real hardware

The initial owner-provided hardware lab is:

| Site | Quantity | Owner description / expected model ID | Initial validation role |
| --- | ---: | --- | --- |
| Primary | 1 | CONNECT Duo, HDHR4-US / likely HDHR4-2US | Two-tuner ATSC 1.0 and clear-QAM compatibility, including interlaced MPEG-TS |
| Primary | 1 | Non-FLEX CONNECT 4K / HDHR5-4K | Local ATSC 1.0/3.0, modern codec, lineup, and playback validation |
| Secondary | 1 | HDHR3-PRIME / HDHR3-CC | Generation-3 PRIME, three-tuner CableCARD/QAM, clear-channel, tuner-busy, and protected-channel error behavior |
| Secondary | 1 | Non-FLEX CONNECT 4K / HDHR5-4K | Routed ATSC 1.0/3.0 discovery and same-model cross-site identity validation |
| Deferred, Australia | 2 | CONNECT QUATRO / likely HDHR5-4DT | Four-tuner DVB-T/T2 and DVB-C regional compatibility when accessible |
| Deferred | Several | Older unspecified units | Opportunistic regression coverage only |

The expected model IDs and capabilities above come from SiliconDust's current
model documentation. Milestone 0 must confirm the exact ModelNumber,
DeviceID, firmware, tuner count, and capabilities reported by each physical
unit before naming fixtures. In particular:

- HDHR3-CC is an older hardware generation but remains in the current
  channel-management and HTTP-capable family; it should not be treated as a
  legacy-protocol device without observed evidence.
- HDHR5-4K tuners 0 and 1 support ATSC 3.0 or ATSC 1.0, while tuners 2 and 3
  are ATSC 1.0-only.
- HDHR5-4DT DVB-C support can depend on firmware, making a firmware capture
  part of the later Australian validation.
- The published HTTP development guide predates ATSC 3.0, so the actual
  HDHR5-4K payload, container, codecs, guide sections, and failure behavior
  remain hardware observations rather than documentation assumptions.

The units are split across two sites that are expected to be joined with
UniFi Site Magic. Balun will treat this as a generic routed multi-site
network, not a product-specific discovery path. Once the mesh is available,
the hardware matrix will capture:

- A client at the primary site discovering the local CONNECT and 4K through
  normal discovery and the remote PRIME and 4K through routed discovery.
- A client at the secondary site discovering the local PRIME and 4K through
  normal discovery and the remote CONNECT and 4K through routed discovery.
- The two HDHR5-4K units remaining distinct by DeviceID despite sharing a
  model and appearing through different discovery paths.
- Client operating system and site.
- Device site, address, subnet, and whether the address or DNS name is stable.
- Routes visible to the client before and after the mesh is enabled.
- Whether ordinary HDHomeRun broadcast or IPv6 discovery crosses the mesh.
- Exact-address targeted discovery behavior.
- Neighbor and route-derived candidate quality.
- Bounded routed-scan packet count, completion time, cooldown, and
  cancellation.
- Simultaneous discovery of local and remote devices without lineup merging.
- Route-change, site-disconnect, device-restart, and stale-cache behavior.

Until the mesh exists, local networks and a controlled routed fixture can
develop and verify the same strategy interfaces. No implementation should
assume that Site Magic forwards broadcast, exposes a particular tunnel type,
or uses one fixed route layout.

Maintain sanitized fixtures and a manual matrix covering:

- Device model and firmware.
- Broadcast standard and region.
- Number of tuners.
- Video and audio codecs.
- Interlaced content.
- Local and routed/WireGuard access.
- Multiple simultaneous devices.
- Tuner-busy and device-restart behavior.

Hardware tests complement rather than replace deterministic fake-device
tests. Sanitized observations from completed rows are maintained in
[`compatibility-v0.1.md`](compatibility-v0.1.md).

## 12. CI, dependency, and release policy

### 12.1 Initial CI

- Cargo formatting check.
- Strict Clippy with warnings denied.
- Locked checks and tests for all targets and relevant features.
- Exact MSRV job.
- Dependency audit and policy checks.
- Shared release-component policy integrity and repository/packaging-input
  validation. Every new input family extends the classifier and its negative
  fixtures in the same change. This is the current pre-package enforcement
  boundary.
- Desktop entry and AppStream validation.
- Markdown, TOML, YAML, and GitHub Actions linting.
- Linux debug and release tests.
- macOS and Windows compile smoke tests.
- Concurrency cancellation for superseded branch runs.

Add full packaging jobs, coverage ratchets, weekly fuzzing, and expensive GUI
smoke tests after the playable vertical slice makes them meaningful.
Dependabot should begin with grouped weekly pull requests and manual merging.
Automatic merging waits until required checks and branch protection are
deployed and tested.

### 12.2 Release contract

- Accept only a v-prefixed Semantic Version tag.
- Use prerelease versions such as v0.1.0-alpha.1 while the release is alpha.
- Require a signed annotated tag on an approved main-branch commit.
- Resolve the tag once and build every package from that immutable SHA.
- Use locked Rust dependencies and pinned build tools/actions where practical.
- Generate a draft release first.
- Require an exact expected artifact inventory with no missing, extra, or
  duplicate files.
- Reopen and validate every completed package.
- Apply the shared component policy before staging, during native-import
  traversal, to the completed app-owned tree, and again to the reopened final
  package. These gates become mandatory in the same change that adds each
  platform package.
- Generate checksums, SBOM, and build provenance.
- Grant write permission only to a final publication job that checks out no
  project source.
- Publish useful release notes rather than only a comparison link.
- Add Apple notarization and Windows code signing as distribution broadens.

An xtask release-check command should semantically compare Cargo, lockfile,
changelog, AppStream, packaging metadata, and the proposed tag.

### 12.3 Tributary build-infrastructure port

Treat Tributary's `scripts/` and `build-aux/` trees as Balun's release-
engineering baseline and maintain a file-by-file port ledger. Generic,
identity-neutral helpers should stay as close to upstream as practical;
application-facing helpers must replace Tributary's identity and music-library
assumptions with `balun`, `Balun`, `io.github.jm2.Balun`, network/video runtime
needs, and the exact product tagline.

An equivalent port keeps Tributary's filename. Balun uses a different filename
only when a helper has a materially new or split responsibility, with that
decision recorded in the port ledger. Product-named recipes replace only the
Tributary identity portion of their filenames.

The port lands in dependency order:

1. Vendored tool provenance, source-generation helpers, and synthetic policy
   validators that are useful before a desktop package exists.
2. Cross-platform developer build/check helpers, dependency-update policy, and
   fuzz-lock coordination once their referenced workspaces exist.
3. Flatpak, native Linux, Windows, and macOS recipes together with the GTK
   binary, desktop metadata, icons, runtime probes, and real package tests they
   describe.

Generated dependency snapshots are always rebuilt from Balun's lockfiles.
Music-library permissions, MPRIS behavior, Rhythmbox compatibility, audio-file
associations, and Tributary artifact names are not portable. Package recipes
must never land in a state that claims a desktop executable or asset Balun does
not yet produce. Every real package adds bounded traversal/extraction,
containment and link checks, stable completed-tree inspection, and reopened
final-artifact enforcement in the same change.
The maintained file-by-file status is in
[`tributary-build-infrastructure.md`](tributary-build-infrastructure.md).

## 13. Documentation plan

The README should remain user-focused and substantially shorter than
Tributary's current README:

- Product description and screenshot.
- Supported devices, platforms, codecs, and limitations.
- Installation.
- Short architecture diagram.
- Build and test commands.
- Discovery, network, and privacy summary.
- Guide-data behavior.
- License and third-party notices.

Detailed contracts belong in:

- docs/architecture.md
- docs/discovery.md
- docs/playback.md
- docs/epg.md
- docs/security.md
- docs/packaging.md
- docs/releasing.md

CHANGELOG.md will follow Keep a Changelog and Semantic Versioning with an
Unreleased section and the standard Added, Changed, Deprecated, Removed,
Fixed, and Security headings. Privacy is added when network or guide behavior
changes. Entries describe user-visible outcomes; implementation detail stays
in pull requests and design documents.

The repository should also gain:

- CONTRIBUTING.md
- SECURITY.md
- CODE_OF_CONDUCT.md
- SUPPORT.md
- Issue forms.
- Pull-request template.
- Third-party provenance and license notices.

## 14. Decisions and owner input

### 14.1 Confirmed decisions and test inventory

- Application ID: io.github.jm2.Balun.
- License expression: GPL-3.0-or-later.
- Primary-site hardware: one HDHR4-US CONNECT Duo and one non-FLEX HDHR5-4K.
- Secondary-site hardware: one HDHR3-PRIME and one non-FLEX HDHR5-4K.
- Deferred Australian hardware: two HDHR5-DT Quatro units that are not
  currently accessible.
- The initial real deployment spans two sites that are expected to be joined
  with UniFi Site Magic.

The following working defaults allow implementation to begin without waiting:

- Primary development environment: Linux, with macOS and Windows kept green.
- Initial release packages: Flatpak x86_64/aarch64, Windows x86_64, and
  macOS arm64.
- Guide baseline: lineup metadata plus opportunistic in-band data, followed
  by user-provided XMLTV.
- No cloud discovery, website scraping, DRM, or background tuner consumption.
- Routed discovery requires remembered user approval before enumerating a
  prefix.
- No merged lineup view.

Milestone 1 must apply GPL-3.0-or-later consistently in Cargo, AppStream,
package metadata, source notices, and documentation, while preserving license
and provenance notices for adapted Tributary or third-party code.

### 14.2 Remaining non-blocking input

1. Firmware versions and available channel/codec types on each accessible
   unit.
2. Site subnets, visible route sizes, client platforms, and whether tuner
   addresses or DNS names will be stable.
3. Whether Linux should remain the primary user experience or all three
   platforms should carry equal release priority from the first alpha.
4. Any XMLTV source already used by the intended testers.
5. When the Australian HDHR5-DT units become accessible for regional
   validation.

These inputs refine the spike and support matrix; they do not block the
repository foundation.

## 15. Primary risks

- Real device generations may differ from the current documented discovery
  and HTTP behavior.
- Cross-platform C FFI packaging may outweigh libhdhomerun's compatibility
  benefit.
- Virtual-channel streams may omit the guide tables needed for free in-band
  EPG.
- GStreamer codec availability differs significantly among system and bundled
  runtimes.
- AC-4 and protected ATSC 3.0 channels may not have a generally distributable
  playback path.
- macOS and Windows local-network permissions may make discovery behavior
  less transparent than on Linux.
- A broad tunnel route contains too many hosts for silent, friendly
  enumeration.
- Rapid channel changes can consume multiple tuners if teardown ownership is
  not strict.
- Packaging breadth can outpace the small application's feature development.

Each risk is attached to an early spike, an explicit scope boundary, or a
release exit criterion above.

## 16. References

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
