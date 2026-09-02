# Balun

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![CI](https://github.com/jm2/balun/actions/workflows/ci.yml/badge.svg)](https://github.com/jm2/balun/actions/workflows/ci.yml)

A lightweight cross-platform HDHomeRun live TV viewer

Balun is a greenfield Rust, GTK 4, libadwaita, and GStreamer application for
watching unprotected live television from one or more HDHomeRun devices. Each
device will keep its own channel lineup: the main window is structured with a
narrow device sidebar, a second sidebar for that device's channels and
available program information, and a live-video area.

> **Pre-alpha status:** the repository contains a runnable GTK development
> shell plus the GTK-free discovery, device-registry, and lineup foundation.
> The window can explicitly discover local tuners, probe one known numeric
> device address, load the selected device's lineup, and report whether the
> optional GStreamer playback foundation is available. A process-isolated
> Linux smoke also decodes and renders a checked-in synthetic MPEG-2 transport
> stream. The controller also provides a generation-bound, URL-redacted
> tuner-stream handoff, and the playback library now owns its generation-safe
> `playbin3` lifecycle. Double-clicking or pressing Enter on an unprotected
> channel now enters that path and attempts live-device playback without
> exposing the stream URI to the UI. The header now provides Stop, process-local
> volume and mute, and compositor-confirmed fullscreen controls, with native
> pointer/keyboard behavior and fixed accessible labels. Detailed playback-error
> projection, hostname entry, EPG, audible-output proof, and live-device
> acceptance remain unimplemented.

## Current foundation

- Validated HDHomeRun DeviceID and discovery packet framing, TLV, and CRC
  handling.
- Per-interface IPv4 discovery using a Windows-compatible limited broadcast
  from each bound interface and the narrower directed broadcast elsewhere,
  with replies restricted to the interface's reported prefix. Supported
  non-link-local IPv6 interfaces use scoped site-local multicast; link-local
  IPv6 remains excluded until lineup HTTP can preserve its required scope.
- Exact-address unicast discovery for routed networks, including tunnels where
  broadcast and multicast are unavailable. The desktop admits only one
  validated numeric address at a time, applies a fixed small probe budget, and
  caps distinct exact targets across the application session.
- Explicit, bounded enumeration of an approved RFC 1918 IPv4 range no wider
  than `/24`, with a hard overall scan deadline.
- Cancellation, response and device limits, duplicate accounting, and
  diagnostic packet statistics.
- A bounded DeviceID registry that keeps multiple locators without merging
  devices or channels, expires discovery origins independently, and rejects
  unconfirmed address/identity conflicts.
- Responder-pinned, identity-checked `/discover.json` and `/lineup.json`
  fetching with strict time, body, row, string, origin, port, redirect, proxy,
  and credential-handling policy.
- A reusable, cancellation-aware inspection service that tries the preferred
  locator first, falls back deterministically within one report deadline, and
  publishes only bounded metadata and counts—never lineup rows or stream URLs.
- A separate selected-device resolver that freezes the registry's
  preferred-first locator order, applies one overall deadline across fallback,
  verifies identity before accepting a complete lineup, and keeps every
  address and stream URL out of its status, error, and debug output.
- Device-scoped natural channel identities and compatibility with both the
  documented lineup `Tags` field and current `Favorite`, `DRM`, and `HD`
  fields.
- Bounded immutable controller snapshots for device and selected-lineup state,
  with independent operation generations, strict selected-device scoping, and
  no URL-bearing values at the GTK boundary.
- A packet-free controller runtime on one named current-thread Tokio worker,
  with a bounded nonblocking command ingress, coalesced immutable snapshots,
  explicit-only local and exact-address discovery, supersession cancellation,
  an atomic union of independently replaceable local and exact source batches,
  a 32-distinct-address session traffic ledger, last-good retention on errors,
  and queue-independent joined shutdown. An independent cancellable selection
  lane retains the complete URL-bearing device snapshot inside the actor while
  publishing only device-scoped, URL-free channel rows.
- A cross-platform route snapshot and candidate policy with deterministic fake
  providers, plus a native Linux rtnetlink provider that recognizes WireGuard
  and other unambiguous tunnel links. Native macOS and Windows automatic
  providers remain intentionally unavailable until their route-domain and
  safe-API requirements can be proved.
- A GTK-free route-approval policy that fingerprints the exact private
  targets, tunnel identity, route scopes, and traffic budget with an
  installation key, fails closed on ambiguous paths or clock rollback, and
  reserves single-use scan authority before it can be released.
- A private, bounded approval store with cross-process locking, atomic durable
  reservations, strict quarantine, global run sequencing, exact and revoke-all
  controls, and no persisted raw topology. Unix processes pin one validated
  directory descriptor for all store operations and reject persistent aliases
  of permanent files. Even a confirmed reservation retains its permit in a
  non-cloneable, redacted value until an exact locked reread matches the
  complete ledger and immutable key binding; production wiring will perform
  that match inside the newly subscribed store observer's drain sandwich.
- A packet-free fresh-route gate that consumes the stored authority, permits
  only a transient interface-ID replacement, and caps work to the remaining
  reservation lease. No automatic socket runner is connected yet.
- A fail-closed Linux rtnetlink event-monitor building block that subscribes
  before a route snapshot, authenticates kernel senders, applies strict
  drain/barrier budgets, coalesces reconciliation, and invalidates authority
  before notification. Its consuming handoff performs a final drain and
  synchronous activation without an await gap. A prepared/live bridge carries
  the exact pre-snapshot token and owns the observer through cancellation-safe
  joined shutdown.
- A separate fail-closed Linux approval-store observer building block that
  watches the exact directory descriptor used by the store, sandwiches each
  reread between complete inotify drains, and invalidates on permanent-entry
  mutation or observation loss. Its prepared/live bridge performs blocking
  subscription and exact rereads off-executor and likewise owns joined
  shutdown.
- A platform-neutral combined observer coordinator that exposes only one
  route-and-store health epoch, accepts each exact baseline through a no-await
  paired callback rendezvous, and synchronously cancels authority when either
  source changes, fails, or its owner drops. A Linux whole-pair owner prepares
  route observation before the exact store reread, retains the activation and
  both actors, coalesces replacement events, and retires synchronously before
  joining both actors. No production controller replaces that pair yet.
- GTK-free library boundaries and deterministic fake-device tests.
- An optional GTK-free GStreamer 0.25/`v1_20` playback foundation with a native
  GStreamer 1.20 runtime floor, one default-main-context owner, fixed path-free
  initialization errors, and an immutable startup snapshot of the exact seven
  structural factories needed for the first playback experiment. Missing
  components disable playback readiness without disabling device discovery or
  lineup inspection.
- An actor-private stream handoff which accepts only a URL-free ChannelKey and
  selected-snapshot generation, revalidates the current DeviceID, responder
  address, port, channel path, and protection state, and returns one opaque
  zeroizing value through a private one-shot response. URLs never enter the
  immutable GTK-facing snapshots or public debug/error text.
- A default-main-context playback session which assigns a separate monotonic
  tune generation before awaiting that private response, consumes the URI only
  inside the desktop-enabled library while constructing `playbin3` and its
  private `gtk4paintablesink`, and exposes only the URI-opaque GDK paintable.
  Every reduced Playing, buffering, EOS, and native-error bus event carries the
  tune generation, and late successful responses are dropped for immediate
  zeroization. Replacement, Stop, EOS, native failure, shutdown, and owner drop
  detach the predecessor's watch and request a bounded transition to `NULL`; a
  teardown failure quarantines that owner and blocks every successor. Bus
  reductions and stateful public calls share the exact default main context,
  with fixed errors instead of reentrant borrow panics. The desktop pane owns
  this session and a URI-opaque paintable binding/clearing path. Exact-
  generation activation of an unprotected channel now requests the private
  handoff, binds an applied paintable, aborts superseded waits, and stops when
  a selected-device change is admitted, with a defensive repeat when its new
  generation is published. A normalized process-local volume and independent
  mute preference apply before a URI enters each pipeline, update its active
  owner, and carry into every successor; the UI level is converted to playbin's
  linear gain with a cubic curve. This property contract does not yet prove
  audible output or a packaged platform audio sink.
- A bounded, display-backed Linux acceptance test which feeds a deterministic,
  video-only MPEG-2 transport-stream fixture through explicit `playbin3` and
  `gtk4paintablesink`, requires multiple rendered frames and paintable updates,
  reaches EOS, and confirms teardown to `NULL`. The test fixture and its libav
  decoder are development/CI inputs only; the test opens no network source or
  tuner and establishes neither the production codec contract nor package
  contents.
- An opt-in GTK 4/libadwaita development shell with adaptive, separate device
  and channel sidebars plus a live-TV player pane. Construction remains inert;
  Refresh explicitly starts local discovery, the adjacent add action admits a
  bounded exact-address request, and selecting one device loads only that
  device's lineup. Double-clicking or pressing Enter on an unprotected channel
  starts its generation-owned playback session and presents the player when the
  nested layout is compact, including when setup reports a fixed failure.
  Separate discovery and playback Stop controls, plus the joined close path,
  cancel their owned work. Native volume, mute, and fullscreen widgets support
  pointer and keyboard operation; F11 toggles fullscreen and Escape exits it.
  Fullscreen presentation changes only after compositor confirmation, protects
  nested Back navigation, then restores the prior pages and focus. The broader
  M4.6 accessibility audit remains open. The core library's default build and
  diagnostic remain GTK-free.

The implementation plan, including the UI, lineup, guide, playback, security,
hardware-validation, packaging, and release boundaries, is in
[`docs/plan-v0.1.md`](docs/plan-v0.1.md). The countable done/remaining ledger is
in [`docs/task.md`](docs/task.md), and sanitized hardware observations are
recorded in [`docs/compatibility-v0.1.md`](docs/compatibility-v0.1.md). The
implemented playback boundary and its remaining acceptance work are detailed in
[`docs/playback.md`](docs/playback.md).

## Try discovery

Balun currently requires Rust 1.98 or newer. Ordinary local discovery is the
default:

```bash
cargo run --locked --bin balun-discover
```

Inspect discovered device metadata and lineup summaries without opening a
channel stream or allocating a tuner:

```bash
cargo run --locked --bin balun-discover -- --inspect --local
```

Inspection fetches only bounded device JSON from identity-checked responders.
Advertised URL values are hidden. `DeviceAuth` is never deserialized, persisted,
or printed, and the temporary metadata buffer is wiped after parsing.

Probe one known device address, such as an HDHomeRun reached over WireGuard or
another routed link:

```bash
cargo run --locked --bin balun-discover -- --target 192.168.50.20
```

If the address is unknown, explicitly approve one small private range:

```bash
cargo run --locked --bin balun-discover -- --approved-range 10.42.7.0/24
```

Routed enumeration is never part of ordinary local discovery. The diagnostic
accepts only ranges wholly inside RFC 1918 private space, rejects anything
wider than `/24`, caps the candidate set at 256 addresses, and applies bounded
packet-rate, concurrency, and 15-second overall-deadline defaults. Only scan a
network you own or administer. Prefer a known `--target` address whenever one
is available.

On Linux, Balun can now derive conservative candidates from a stable native
route snapshot. It fails closed on policy routing, VRFs, ambiguous next hops,
or route-table state that cannot be represented safely. The remembered
approval policy, durable private store, and immediate exact-route revalidation
gate are present. A private Linux factory now seals a UDP socket to the exact
fresh interface after name/index readback, and a packet-free admission boundary
registers invalidation before reserve and carries one non-extending monotonic
deadline. A combined route/store epoch state machine, paired gap-free
activation handoffs, a whole-pair Linux owner, and a sealed published-
reservation handoff are present, but they expose no automatic send path yet.
Route-derived targets remain disconnected from the diagnostic until one
production actor replaces and exactly rebaselines the pair after every store
publication, serializes final pre-send revalidation, and gives a consuming
runner exact completion ownership.
Installing the provider does not silently enumerate a network.

Press `Ctrl+C` to cancel discovery.

## Try the desktop shell

The desktop slice requires GTK 4.16, libadwaita 1.6, and GStreamer 1.20 or
newer. On Linux or macOS, with those development libraries and `pkg-config`
available, launch it with:

```bash
cargo run --locked --features desktop --bin balun
```

At startup the player pane checks `playbin3`, `uridecodebin3`, `decodebin3`,
`souphttpsrc`, `tsdemux`, `deinterlace`, and `gtk4paintablesink`. These are
structural checks only: they do not establish the complete packaged codec and
audio-sink contract. Desktop channel activation is implemented; physical
HDHomeRun live-tuning and packaged-runtime acceptance remain open. Fedora
commonly supplies this development/runtime foundation through
`gstreamer1-devel`, the base/good/bad-free plugin packages, and
`gstreamer1-plugin-gtk4`; its Linux-only synthetic CI smoke additionally uses
`gstreamer1-plugin-libav` to decode the pinned MPEG-2 fixture. That libav
package is a development/CI input, not a Balun package manifest or authority to
stage a broad plugin set. Homebrew uses `gstreamer`; MSYS2 CLANG64 uses its
matching `gstreamer`, base/good/bad, and `gst-plugins-rs` packages. Package
names vary, so the runtime factory snapshot is authoritative. See the
[playback foundation](docs/playback.md) for the precise boundary.

This opens Balun's adaptive device, channel, and live-TV panes without starting
network work. Choose **Refresh** to run one bounded local discovery operation.
Use the adjacent add action to probe one known numeric IPv4 or unscoped IPv6
address when broadcast or multicast cannot cross a routed link. This path does
not resolve a hostname, accept a URL, port, or CIDR range, enumerate neighbors,
scan a prefix, or fall back to local discovery. It sends at most two HDHomeRun
UDP request datagrams with 200 ms response windows, accepts at most 16 received
datagrams and one device identity, and permits at most 32 distinct exact
addresses during one application session. A failed target or one with no
accepted valid reply still consumes that traffic allowance, while retrying an
already admitted address does not. After the first valid reply, retries are
bound to that DeviceID. The raw entry is not echoed into validation, status,
or error copy, and the admission target's debug representation is redacted.
After a reply passes
source and identity validation, that responder's locator appears in the normal
device projection and sidebar. Use **Stop** to cancel and join either kind of
active discovery operation.
Selecting a discovered device fetches its identity-checked metadata and lineup
for the channel sidebar; device selection alone does not tune a channel or
allocate a tuner. Double-clicking or pressing Enter on an unprotected channel
then requests its private stream handoff and starts playback. Until a channel
is activated, the live-TV pane retains its fixed idle state. The player header
offers Stop, a focusable volume slider, an independent mute toggle, and a
fullscreen button. F11 toggles fullscreen and Escape exits it; state-dependent
button copy and nested-navigation protection follow the window's confirmed
fullscreen state rather than the request alone. These controls and their
property-level tests do not yet establish audible output, codec/audio-sink
coverage, or live-device acceptance.

On Windows, Refresh sends the same bounded request count from each eligible
interface-bound IPv4 socket using the limited local broadcast. Balun derives
the accepted reply prefix from Windows' direct on-link prefix length instead
of depending on optional derived broadcast metadata. It intentionally omits
link-local IPv6-only responders until scoped HTTP requests are implemented, so
that known unusable-only path is not displayed as if lineup HTTP could use it.
If host firewall or network policy still prevents local replies, use the add
action with the tuner's known IPv4 address; that exact path sends no broadcast.
To collect the bounded discovery, endpoint, and lineup diagnostic without
manually locating an executable, run this explicit Windows-only mode:

```powershell
pwsh -NoProfile -File scripts/build-windows.ps1 -InspectLocal
```

It builds and validates the GTK-free diagnostic, then invokes it with exactly
`--inspect --local`; it accepts no destination or argument passthrough. Native
`-Diagnostic` and `-InspectLocal` builds use the installed Rust host target and
need neither MSYS2 nor the gnullvm target.

For the Windows desktop build, install Rust with the
`x86_64-pc-windows-gnullvm` target and an MSYS2 CLANG64 environment containing
`mingw-w64-clang-x86_64-gtk4`,
`mingw-w64-clang-x86_64-libadwaita`,
`mingw-w64-clang-x86_64-gstreamer`,
`mingw-w64-clang-x86_64-pkg-config`, and
`mingw-w64-clang-x86_64-toolchain`. Then build the release desktop shell from
an ordinary PowerShell terminal with one command:

```powershell
pwsh -NoProfile -File scripts/build-windows.ps1
```

The helper detects a standard MSYS2 installation and manages the CLANG64
compiler, `pkg-config`, target, and output paths. It builds only by default. To
build and launch the exact validated output, use the existing Tributary-style
run flag:

```powershell
pwsh -NoProfile -File scripts/build-windows.ps1 -Run
```

Pass `-Msys2Root C:\path\to\msys64` only if MSYS2 is installed in a location
the helper cannot detect. This is a developer build using the installed MSYS2
runtime, not a portable bundle or installer.

## Development

The default feature set remains GTK- and GStreamer-free. Run the same core
checks used by CI with:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --release --all-targets --locked
```

CI verifies the declared Rust 1.98 minimum across the desktop feature, runs
strict Linux GTK-free debug and release checks, tests the optional GTK-free
playback capability layer, compiles, links, and lints the Linux desktop shell,
exercises its ordinary close/join lifecycle, URI-opaque `PlayerView`
binding/clearing, accessible audio-control state, compositor-confirmed
fullscreen/navigation restoration, and an offline synthetic MPEG-2
decode/render/EOS/`NULL` lifecycle under an isolated headless Wayland
compositor. These layered tests validate control wiring and playbin properties,
not audible output. CI links that shell against native macOS and Windows toolkit
SDKs and keeps compile-checking the GTK-free default code on both platforms.
The local smoke helper can fall back to an isolated Xvfb server when Wayland is
unavailable; X11 is not the default or a Balun runtime requirement. CI also
exercises each platform helper's no-option desktop route. The release candidate
workflow accepts an existing annotated, v-prefixed Semantic Version tag,
verifies it against `Cargo.toml` and `CHANGELOG.md`, and builds the exact tag
commit on all three platforms. It selects `--diagnostic` on Linux/macOS and
`-Diagnostic` on Windows explicitly, producing internal diagnostic workflow
artifacts only; application packages and public artifact publication begin
with the playable GTK/GStreamer slice.

On Linux, `scripts/test-desktop-lifecycle.sh` selects supported headless Weston
plus `wayland-info` before considering Xvfb. In `auto` mode, a Weston instance
which cannot start or pass its bounded readiness probe is unavailable and the
helper reports that failure before using an installed Xvfb fallback. Set
`BALUN_DESKTOP_TEST_BACKEND=wayland` to require that route or `x11` to exercise
the explicit fallback; `auto` is the default. CI requires Wayland and does not
install Xvfb, so a compositor regression cannot be hidden by fallback.

The same-named Tributary-derived Linux, macOS, and Windows helpers use desktop
defaults and are build-only with no options. Their check, Clippy, and coverage
routes also include desktop features by default. Select the GTK-free diagnostic
explicitly with `--diagnostic` on Linux/macOS or `-Diagnostic` on Windows.
Tributary established a launch flag only for its Windows PowerShell helper, so
Balun preserves `-Run` there and deliberately does not invent `--run` for the
shell helpers.

On Linux, the default helper checks the GTK 4.16, libadwaita 1.6, and GStreamer
1.20 development floors, binds Cargo to the validated native Rust host target
and exact repository target directory, builds
`target/<native-target>/release/balun`, and applies the locked metadata and ELF
component gates. Its package switches fail before starting build or network
work until their recipes and complete artifact gates exist:

```bash
scripts/test-build-linux-policy.sh
scripts/build-linux.sh --check
scripts/build-linux.sh
scripts/build-linux.sh --diagnostic
```

On macOS, the default route likewise checks the desktop development floors,
binds the native Apple target, builds
`target/<native-target>/release/balun`, loads the checksum-pinned component
policy through system inspection tools, and validates the resulting Mach-O
without creating an app bundle or DMG. App, DMG, signing, notarization, and
other package-producing switches remain unavailable and fail before external
work:

```bash
scripts/test-build-macos-policy.sh
/bin/bash scripts/build-macos.sh --check
/bin/bash scripts/build-macos.sh
/bin/bash scripts/build-macos.sh --diagnostic
```

Both shell helpers validate a nonempty executable regular file at the expected
non-symlink output path before the platform component gate. They never install
tools or packages. Cargo can still fetch the locked dependency graph when it is
not cached, and a rustup-managed invocation can fetch the selected Rust
toolchain.

On Windows:

```powershell
pwsh -NoProfile -File scripts/test-build-windows-routing.ps1
pwsh -NoProfile -File scripts/build-windows.ps1
pwsh -NoProfile -File scripts/build-windows.ps1 -Run
pwsh -NoProfile -File scripts/build-windows.ps1 -Diagnostic
pwsh -NoProfile -File scripts/build-windows.ps1 -InspectLocal
pwsh -NoProfile -File scripts/build-windows.ps1 -Check
```

The no-option and `-Run` modes build the release desktop shell through an
automatically detected MSYS2 CLANG64 environment; only `-Run` launches it.
`-Diagnostic` preserves the GTK-free diagnostic route, and can be combined
with quick modes such as `-Check`. `-InspectLocal` is the separate, explicit
build-and-run diagnostic with fixed local-inspection arguments. Every compile
route pins an explicit Rust target and repository-local target tree before
validating the expected output as a nonempty regular, non-reparse file. Bundle,
ZIP, Inno Setup, and dependency-update switches remain fail-closed before
external work. The helper does not claim PE validation, a portable runtime
closure, or package validation.

`build-aux/toolchain/rust-toolchain.toml` is the Dependabot proposal source for
the compiler floor. Its deliberately nested location prevents it from acting as
a repository-wide rustup override. Keep its exact patch-zero release aligned
with `Cargo.toml`, the dedicated `MSRV` job, and this README:

```bash
python3 scripts/test_rust_toolchain_policy.py
python3 scripts/sync_rust_toolchain.py --check
# After reviewing a Dependabot compiler proposal:
python3 scripts/sync_rust_toolchain.py --from-toolchain
```

The synchronization helper never rewrites CI jobs which track `stable`, and it
keeps the compiler version independent from the immutable full-SHA toolchain
action pin.

### Release component policy

Balun does not implement optical-disc or protected-channel playback. A shared,
fail-closed filename-token policy rejects libdvdcss, dedicated optical-disc
copy-control/circumvention components, and proprietary DRM modules from current
release inputs. Future bundles must never stage those components merely because
a broad media package contains them, and must enforce the same exclusion over
their staged and final contents. The current Linux-CI check pins the reviewed
policy, then validates
repository names, Rust and Cargo build inputs, executable helpers, workflows,
and recognized native packaging/build inputs. The exact tag checkout runs the
same pinned fixture suite in the release-candidate workflow.

There is no GTK/GStreamer application package to inspect yet, so this is not an
artifact-compliance claim. Packaging work must add platform-specific checks at
staging, native-import traversal, completed-tree inspection, and reopened final
artifact inspection before any package is published. Self-contained bundles
will stage a capability-derived, reviewed GStreamer plugin closure rather than
an entire plugin distribution; that claim does not extend to distro-provided or
shared runtimes. Installing `gstreamer1-plugin-libav` on the Linux development
runner for a pinned synthetic MPEG-2 smoke does not add that package wholesale
to a future bundle or relax the libdvdcss, optical-disc, DRM, or circumvention
exclusions. Ordinary codecs, containers, TLS, and general-purpose
cryptography are outside this narrow deny policy and receive their own
compatibility and distribution review. See the [release component
policy](docs/release-component-policy.md) for the exact current and future
enforcement boundary.

Every new build system, dependency source, or package recipe must extend the
input classifier and its negative fixtures in the same change. Input checking
does not replace the mandatory native-import, completed-tree, and reopened
artifact inspection required when application packages are introduced.

The repository now also carries Tributary-derived Linux archive/tree and
Flatpak-commit validators with deterministic positive and negative fixtures.
CI exercises them as preparatory packaging scaffolding only. Linux extracted
trees now have strict entry, path, per-file, and aggregate-byte limits; reject
escaping links and unsupported entry types; and must produce identical,
hidden-inclusive metadata-and-content manifests before and after inspection.
They do not become an artifact claim until each real format also preflights and
contains untrusted extraction, bounds archive-source replacement and resource
amplification, and reopens the completed package. The vendored Flatpak Cargo
generator is checksum- and provenance-pinned; its dependency snapshot must be
regenerated from Balun's own `Cargo.lock`, never copied from Tributary. A
separate synthetic Flatpak policy fixes the initial six-entry permission
contract: Wayland, fallback X11 with IPC, the PulseAudio socket, network, and
the standard GPU/DRI device grant. The PulseAudio socket exposes capabilities
beyond Balun's intended audio-output use, and the DRI grant is broader than
render nodes alone, but both remain the practical reviewed Flatpak boundary.
It grants no host/media filesystem, `--device=all`, GVfs, secrets, MPRIS, or
unreviewed bus access; the eventual real manifest must pass the same validator.
The adapted macOS inspection core is likewise preparatory: it
validates bounded Mach-O dependency output and stable completed bundle trees,
but no `Balun.app` or DMG exists yet.
The file-by-file decisions and atomic landing conditions are recorded in the
[Tributary build-infrastructure port ledger](docs/tributary-build-infrastructure.md).

## Scope

Balun is a viewer, not a DVR or tuner administration tool. Recording,
timeshift, protected-channel playback, firmware management, transcoding, and a
merged cross-device channel list are outside the v0.1 scope.

## License

Balun is licensed under [GPL-3.0-or-later](LICENSE).
