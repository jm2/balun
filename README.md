<img src="data/icons/hicolor/128x128/apps/io.github.jm2.Balun.png" width="96" alt="Balun icon">

# Balun

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![CI](https://github.com/jm2/balun/actions/workflows/ci.yml/badge.svg)](https://github.com/jm2/balun/actions/workflows/ci.yml)

A lightweight, cross-platform **HDHomeRun live TV viewer** written in Rust with **GTK 4**,
**libadwaita**, and **GStreamer**.

Balun lists every HDHomeRun on your network in one sidebar, shows the selected device's own
channel lineup in a second, and plays unprotected channels in a live-video pane. Lineups are never
merged across devices, no stream URL ever reaches the user interface, and the media framework
receives neither a device address nor a stream URL. Playback errors may name the device and its
address so you know which tuner failed.

> **Pre-alpha.** Balun plays live TV on development builds (verified on Windows and Linux
> against real tuners), but there are no packages yet and macOS live-device acceptance is
> still open. The countable status is in [`docs/task.md`](docs/task.md).

## Features

| Feature | Status |
|---------|--------|
| Local HDHomeRun discovery (IPv4 broadcast, IPv6 multicast) | ✅ |
| Find a routed tuner by IP address or hostname (WireGuard and other tunnels) | ✅ Remembered across launches |
| Approved private-range enumeration (`balun-discover` only, `/24` or narrower) | ✅ |
| Multiple devices, each with its own channel lineup | ✅ |
| Device metadata and lineup inspection without allocating a tuner | ✅ |
| Adaptive three-pane GTK 4 / libadwaita window | ✅ |
| Window size and maximized state remembered across launches | ✅ |
| Live playback of unprotected channels (`playbin3` + `gtk4paintablesink`) | ✅ Verified on Windows and Linux against real tuners; macOS acceptance pending |
| Stop, volume, mute, and fullscreen controls | ✅ |
| Favorite, HD, and protected channel badges | ✅ Protected channels are listed but disabled |
| Playback errors that name the device and channel | ✅ |
| Channel search and favorites-only filter | ✅ |
| Fixed, endpoint-free playback error messages | ✅ |
| Windows local discovery | ✅ |
| Route-derived tunnel discovery (Linux) | ✅ Approve each route set once; live proof on a tunnel pending |
| Program guide (in-band PSIP/EIT, XMLTV) | ❌ v0.2 candidate |
| Hostname entry | ✅ Resolved to at most four unicast addresses; remembered by name |
| Audible output and complete codec contract | ⚠️ Audio verified on Windows and Linux; codec contract open until P0.5, see the support matrix |
| ATSC 3.0 channels | ⚠️ HEVC video needs gst-libav or a platform decoder; AC-4 audio has no open decoder |
| Protected (DRM) channels | ❌ Out of scope |
| Packages (Flatpak, deb, rpm, DMG, winget) | 🚧 A Flatpak bundle is built and validated in CI; nothing is published yet |
| Cross-platform: Linux, macOS, Windows | ✅ Windows verified with real tuners; Linux covered by CI runtime smokes; macOS build-tested only |
| Light & dark mode | ✅ Automatic (libadwaita) |

The product plan is [`docs/plan-v0.1.md`](docs/plan-v0.1.md), the countable ledger is
[`docs/task.md`](docs/task.md), sanitized hardware observations are in
[`docs/compatibility-v0.1.md`](docs/compatibility-v0.1.md), the evidence-backed support matrix is
[`docs/support-v0.1.md`](docs/support-v0.1.md), and the playback contract is in
[`docs/playback.md`](docs/playback.md).

---

## Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│ GTK 4 / libadwaita UI                                       │
│ device sidebar · channel sidebar · live-TV player           │
├─────────────────────────────────────────────────────────────┤
│ Controller: one Tokio worker publishing URL-free snapshots  │
├──────────────────────────────┬──────────────────────────────┤
│ HDHomeRun core               │ Playback                     │
│ discovery · registry ·       │ session · source policy ·    │
│ device HTTP · lineup         │ HTTP transport → appsrc →    │
│                              │ playbin3 → gtk4paintablesink │
└──────────────────────────────┴──────────────────────────────┘
```

The GTK layer only ever sees stable device and channel identities plus immutable, URL-free
snapshots published by one controller thread. Stream URLs stay inside the controller and the
playback library.

GStreamer never receives a device address either. `playbin3` is given the constant
`appsrc://balun` URI, and Balun's own HTTP client, with proxies, redirects, and DNS disabled,
fetches the MPEG-TS stream and feeds the built-in `appsrc`.
[ADR-0001](docs/architecture/adr-0001-discovery-playback.md) records that decision.

---

## Installation

No packages are published yet; build from source as described below. CI builds and validates
a Flatpak bundle on every change, and the release-candidate workflow builds x86_64 and aarch64
bundles as internal artifacts.

---

## Building from Source

### Prerequisites (all platforms)

- [Rust 1.98 or newer](https://rustup.rs) (stable toolchain) — the declared MSRV in `Cargo.toml`,
  verified by a dedicated CI job
- **GTK 4.16+** and **libadwaita 1.6+**
- **GStreamer 1.20+** with the base, good, bad, and gst-plugins-rs (`gtk4paintablesink`) plugins;
  gst-libav supplies the usual MPEG-2, H.264, AC-3, and AAC broadcast decoders
- `pkg-config`

The default feature set builds the GTK-free core library and `balun-discover`; the desktop
application needs `--features desktop`.

### Linux

**Debian / Ubuntu:**

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev libgstreamer1.0-dev \
  gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-gtk4 gstreamer1.0-libav pkg-config build-essential
```

**Fedora:**

```bash
sudo dnf install gtk4-devel libadwaita-devel gstreamer1-devel \
  gstreamer1-plugins-base gstreamer1-plugins-good gstreamer1-plugins-bad-free \
  gstreamer1-plugin-gtk4 gstreamer1-plugin-libav pkgconf-pkg-config gcc binutils
```

**Arch Linux:**

```bash
sudo pacman -S gtk4 libadwaita gstreamer gst-plugins-base gst-plugins-good \
  gst-plugins-bad gst-plugin-gtk4 gst-libav pkgconf base-devel
```

Then build:

```bash
cargo build --release --locked --features desktop --bin balun
# or use the helper script:
./scripts/build-linux.sh
```

The helper checks the development-library floors and the GStreamer plugin files before building,
names the package for anything missing, and writes `target/<native-target>/release/balun`.

### macOS

Requires [Homebrew](https://brew.sh):

```bash
brew install gtk4 libadwaita pkgconf gstreamer
./scripts/build-macos.sh
```

The `gstreamer` formula supplies the base, good, bad, and gst-plugins-rs plugins. The helper
validates the resulting Mach-O against the release component policy and writes
`target/<native-target>/release/balun`; it does not create an app bundle or DMG yet.

### Windows

Requires [MSYS2](https://www.msys2.org) with the CLANG64 environment:

```powershell
# In an MSYS2 CLANG64 shell:
pacman -S mingw-w64-clang-x86_64-gtk4 \
          mingw-w64-clang-x86_64-libadwaita \
          mingw-w64-clang-x86_64-gstreamer \
          mingw-w64-clang-x86_64-gst-plugins-base \
          mingw-w64-clang-x86_64-gst-plugins-good \
          mingw-w64-clang-x86_64-gst-plugins-bad \
          mingw-w64-clang-x86_64-gst-plugins-rs \
          mingw-w64-clang-x86_64-gst-libav \
          mingw-w64-clang-x86_64-pkg-config \
          mingw-w64-clang-x86_64-toolchain
```

Then, in PowerShell:

```powershell
# Ensure Rust's LLVM target is installed:
rustup target add x86_64-pc-windows-gnullvm

# Build the desktop shell (add -Run to launch it):
.\scripts\build-windows.ps1
```

The helper detects a standard MSYS2 installation (pass `-Msys2Root C:\path\to\msys64` otherwise),
verifies the plugin files, and writes `target\x86_64-pc-windows-gnullvm\release\balun.exe`. This
is a developer build against the installed MSYS2 runtime, not a portable bundle or installer.

---

## Running

```bash
# Desktop application:
cargo run --locked --features desktop --bin balun
```

### Discovery diagnostic

`balun-discover` is the GTK-free command-line tool behind the desktop's discovery. It never opens
a channel stream or allocates a tuner.

```bash
# Ordinary local discovery:
cargo run --locked --bin balun-discover

# Also fetch device metadata and lineup summaries:
cargo run --locked --bin balun-discover -- --inspect --local

# Probe one known device address, for example across WireGuard:
cargo run --locked --bin balun-discover -- --target 192.168.50.20

# Enumerate one explicitly approved private range:
cargo run --locked --bin balun-discover -- --approved-range 10.42.7.0/24

# Report route-provider availability and tunnel candidate counts without sending a packet:
cargo run --locked --bin balun-discover -- --providers
```

`--approved-range` accepts only RFC 1918 space no wider than `/24`, caps the scan at 256
candidates with a bounded packet rate and concurrency, and stops after 15 seconds. Only scan a
network you own or administer, and prefer `--target` whenever the address is known. `Ctrl+C`
cancels any run.

On Windows, `.\scripts\build-windows.ps1 -InspectLocal` builds the diagnostic and runs exactly
`--inspect --local`.

---

## Development

### Git Hooks

Balun includes a pre-commit hook that runs `cargo fmt --check` to prevent formatting errors from
being committed. To enable it after cloning:

```bash
git config core.hooksPath hooks
```

### Developer Build Scripts

All three platform build scripts support quick-exit modes for formatting, type-checking, linting,
coverage, and the installed-runtime playback probes:

```bash
# Linux / macOS:
./scripts/build-linux.sh --fmt             # or build-macos.sh --fmt
./scripts/build-linux.sh --check           # or build-macos.sh --check
./scripts/build-linux.sh --clippy          # or build-macos.sh --clippy
./scripts/build-linux.sh --coverage        # or build-macos.sh --coverage
./scripts/build-linux.sh --probe-playback  # or build-macos.sh --probe-playback
./scripts/build-linux.sh --diagnostic      # GTK-free balun-discover build
```

```powershell
# Windows (PowerShell):
.\scripts\build-windows.ps1 -Fmt
.\scripts\build-windows.ps1 -Check
.\scripts\build-windows.ps1 -Clippy
.\scripts\build-windows.ps1 -Test
.\scripts\build-windows.ps1 -Coverage
.\scripts\build-windows.ps1 -ProbePlayback
.\scripts\build-windows.ps1 -Diagnostic
.\scripts\build-windows.ps1 -InspectLocal
.\scripts\build-windows.ps1 -Run
```

The check, Clippy, and coverage modes include the desktop feature by default, and Clippy runs with
`-D warnings` in both the debug and release profiles. `--probe-playback` (`-ProbePlayback`) runs
the same installed-runtime probes CI uses on every platform and cannot be combined with
`--diagnostic`. Packaging switches such as `--flatpak`, `--deb`, `--rpm`, `--dmg`, `-Bundle`, and
`-InnoSetup` exit before any work until packaging lands. The helpers keep Tributary's filenames
and flags; [`docs/tributary-build-infrastructure.md`](docs/tributary-build-infrastructure.md) is
the port ledger.

### Testing & Code Quality

```bash
# Core checks (GTK- and GStreamer-free), as run by CI:
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --release --all-targets --locked

# Real-tuner proofs (opt-in, never run by CI; see src/playback/live_hardware.rs):
BALUN_LIVE_HARDWARE=1 cargo test --features desktop --lib live_hardware -- \
  --ignored --nocapture --test-threads=1

# Desktop and playback lifecycle smokes under an isolated headless compositor:
scripts/test-desktop-lifecycle.sh

# Helper self-tests:
scripts/test-build-linux-policy.sh
scripts/test-build-macos-policy.sh
pwsh -NoProfile -File scripts/test-build-windows-routing.ps1
```

`test-desktop-lifecycle.sh` prefers a headless Weston session and falls back to Xvfb only when
Weston is unavailable. Set `BALUN_DESKTOP_TEST_BACKEND` to `wayland` or `x11` to require one
backend; `auto` is the default, and CI requires Wayland.

CI automatically runs on every push/PR:

- **Linux quality** — release-component, packaging, helper, and toolchain policy tests, then
  `cargo fmt`, strict Clippy, and debug plus release tests
- **Security audit** — `cargo audit` against the RustSec advisory database
- **Lint** — markdownlint, taplo (TOML), yamllint, and actionlint with the configs at the
  repository root
- **Desktop metadata** — validates the desktop entry and AppStream metainfo and checks the icon
  set
- **Flatpak (x86_64)** — generates locked cargo sources, builds the bundle on the GNOME 50
  runtime, reopens it, validates its payload, and probes the installed bundle's runtime for
  the playback factories
- **MSRV** — `cargo check --all-features` on the declared Rust 1.98 minimum
- **Linux desktop** — builds, lints, and tests the desktop shell, runs `--probe-playback`, and
  drives the Wayland lifecycle smokes
- **macOS and Windows compile smoke** — native toolkit SDKs, playback transport tests, helper
  desktop builds, and `-ProbePlayback`
- **Release candidate** (manual) — verifies an annotated `v` tag against every version
  declaration, builds the diagnostic on all three platforms and the Flatpak bundles, requires the
  exact artifact inventory with SHA-256 sums, and creates a draft release from a job that checks
  out no source; every action it runs is pinned to an immutable commit

### Rust toolchain policy

`build-aux/toolchain/rust-toolchain.toml` is the Dependabot proposal source for the compiler
floor; its nested location keeps it from acting as a repository-wide rustup override.

```bash
python3 scripts/test_rust_toolchain_policy.py
python3 scripts/sync_rust_toolchain.py --check
# After reviewing a Dependabot compiler proposal:
python3 scripts/sync_rust_toolchain.py --from-toolchain
```

### Cutting a release

Bump the version in `Cargo.toml` (and `Cargo.lock`), add the `## [version]` changelog section and
its compare link, add the `<release>` to the AppStream metainfo, then confirm they agree before
tagging:

```bash
python3 scripts/release_check.py --tag v0.1.0-alpha.1
```

Push a signed, annotated `v` tag and run the **Release candidate** workflow with it. The workflow
repeats the check, builds every artifact from that one commit, verifies the exact inventory, and
creates a draft GitHub release with `SHA256SUMS.txt`; publishing it is a manual step.

### Release component policy

Balun does not play DVDs, Blu-ray discs, or DRM-protected channels, and a shared, fail-closed
policy keeps dedicated optical-disc decryption and proprietary DRM components out of every release
input. Ordinary codecs, containers, TLS, and general-purpose cryptography are outside that deny
list. See the [release component policy](docs/release-component-policy.md) for the current and
future enforcement boundary.

---

## Project Structure

```text
src/
├── main.rs                 # Desktop application entry point
├── lib.rs                  # GTK-free core library
├── app.rs                  # GTK application lifecycle (io.github.jm2.Balun)
├── bin/
│   └── balun-discover.rs   # GTK-free discovery and inspection diagnostic
├── domain/
│   ├── device.rs           # HDHomeRun DeviceID identity and checksum
│   └── channel.rs          # Guide number and device-scoped ChannelKey
├── hdhr/
│   ├── protocol.rs         # UDP discovery frames, TLVs, and CRCs
│   ├── http.rs             # Bounded, responder-pinned discover.json / lineup.json client
│   ├── lineup.rs           # Streaming, bounded lineup parsing
│   ├── inspection.rs       # Identity-safe device inspection with locator fallback
│   ├── resolver.rs         # Selected-device snapshot resolution
│   ├── fallback.rs         # Preferred-first locator ordering
│   └── fake_device.rs      # Loopback fake HDHomeRun for end-to-end tests
├── discovery/
│   ├── mod.rs              # Bounded discovery orchestration
│   ├── client.rs           # Bounded UDP probe client
│   ├── local.rs            # Per-interface broadcast and multicast endpoints
│   ├── manual.rs           # Exact-address target validation
│   ├── registry.rs         # Device registry with locator claims and expiry
│   ├── routed.rs           # Approved-range scan budgets and candidate limits
│   ├── routed/linux.rs     # Interface-pinned UDP socket construction
│   ├── routes.rs           # Route snapshot types and providers
│   ├── routes/linux/       # rtnetlink route snapshots and change monitor
│   ├── approval.rs         # Route-derived approval policy
│   └── approval/           # Durable store, fresh-route gate, admission, observers
├── controller/
│   ├── runtime.rs          # Controller thread, command ingress, snapshot publishing
│   ├── state.rs            # Immutable URL-free device and channel projections
│   └── handoff.rs          # One-shot, URL-redacted stream handoff
├── settings/
│   └── mod.rs              # Versioned, atomic settings.json store
├── playback/
│   ├── runtime.rs          # GStreamer initialization and factory snapshot
│   ├── session.rs          # Generation-owned playbin3 session and teardown
│   ├── source_policy.rs    # Fail-closed appsrc source-setup policy
│   ├── transport.rs        # Balun-owned HTTP transport feeding appsrc
│   ├── pipeline_failure.rs # Endpoint-free failure classification
│   └── fake_device_e2e.rs  # End-to-end proofs against the fake device
└── ui/
    ├── window.rs           # Adaptive three-pane window and controller bridge
    ├── device_sidebar.rs   # Device list with Refresh, Find by address, and Stop
    ├── channel_sidebar.rs  # Selected device's channel list and badges
    ├── exact_discovery_dialog.rs # Find device by address dialog
    ├── player_view.rs      # Live-TV picture, status, and playback controls
    ├── settings_session.rs # Loads settings once and saves window state on close
    └── objects.rs          # GObject wrappers for the sidebar models

scripts/
├── build-linux.sh          # Linux build helper
├── build-macos.sh          # macOS build helper (Mach-O policy gate)
├── build-windows.ps1       # Windows build helper (MSYS2 CLANG64)
├── test-desktop-lifecycle.sh # Headless Wayland/Xvfb desktop and playback smokes
├── test-build-*            # Helper routing tests
├── macos-*-policy.sh       # macOS bundle and icon policy helpers
└── sync_rust_toolchain.py  # Rust floor synchronization

build-aux/
├── flatpak/                # Flatpak manifest, pinned Cargo-source generator, permission/bundle validators
├── linux/                  # Linux package payload and metadata validators
├── packaging/              # Shared forbidden-component policy and validator
└── toolchain/              # Dependabot-tracked Rust floor proposal

data/
├── io.github.jm2.Balun.desktop      # Desktop entry
├── io.github.jm2.Balun.metainfo.xml # AppStream metadata
├── icons/hicolor/          # Application icon (scalable, symbolic, 16-512 px)
├── balun.iconset/          # macOS icon source
└── balun.ico               # Windows icon

hooks/pre-commit             # cargo fmt check
tests/                       # Synthetic MPEG-2 fixture and display-backed playback test
docs/                        # Plan, task ledger, playback contract, compatibility notes, ADRs
```

---

## Usage

### Discovering devices

At launch Balun probes only the addresses you previously added; everything else waits until you
ask. **Refresh devices** runs one bounded local discovery over every attached interface.
**Find device by address** probes one numeric IPv4 or unscoped IPv6 address, or a hostname, for a
tuner behind WireGuard or another routed link. It accepts no port or range; a name resolves to at
most four unicast addresses that are probed one at a time, each probe sends at most two requests,
and up to 32 distinct addresses are admitted per session. A tuner that answers is remembered, by
name when you entered a name, and probed again at the next launch. **Stop device discovery**
cancels either kind and any remaining launch probes.

On Windows, local discovery uses the limited broadcast from each interface. If a host firewall
blocks the replies, use **Find device by address** with the tuner's IPv4 address.

### Watching a channel

Select a device to load its channel lineup; selecting alone never tunes. Type in the search field
above the list to match a channel number or name, or press the star to show favorites only.
Double-click a channel or press Enter to play it. Protected channels are listed but cannot be
activated. The player header has **Stop**, a volume slider, a mute toggle, and a fullscreen
button; volume and mute carry across channel changes for the running session.

### Keyboard Shortcuts

| Shortcut | Action |
|----------|--------|
| `F11` | Toggle fullscreen |
| `Escape` | Exit fullscreen |

Shortcuts are recognized only without modifier keys.

---

## Scope

Balun is a viewer, not a DVR or tuner administration tool. Recording, timeshift, protected-channel
playback, firmware management, transcoding, and a merged cross-device channel list are outside the
v0.1 scope.

---

## Contributing

[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the build, the pull-request flow, the ledger and docs
register, and the contracts a change must not weaken. Report vulnerabilities privately as
described in [`SECURITY.md`](SECURITY.md), not in a public issue.

---

## License

Balun is licensed under the [GNU General Public License v3.0 or later](LICENSE).
