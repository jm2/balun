# Balun

[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![CI](https://github.com/jm2/balun/actions/workflows/ci.yml/badge.svg)](https://github.com/jm2/balun/actions/workflows/ci.yml)

A lightweight cross-platform HDHomeRun live TV viewer

Balun is a greenfield Rust, GTK 4, libadwaita, and GStreamer application for
watching unprotected live television from one or more HDHomeRun devices. Each
device will keep its own channel lineup: the planned main window has a narrow
device sidebar, a second sidebar for that device's channels and available
program information, and a live-video area.

> **Pre-alpha status:** the repository currently contains the GTK-free
> discovery foundation and a diagnostic command. It does not yet contain the
> desktop viewer, channel lineup, EPG, or playback implementation.

## Current foundation

- Validated HDHomeRun DeviceID and discovery packet framing, TLV, and CRC
  handling.
- Per-interface IPv4 broadcast and scoped IPv6 multicast discovery.
- Exact-address unicast discovery for routed networks, including tunnels where
  broadcast and multicast are unavailable.
- Explicit, bounded enumeration of an approved RFC 1918 IPv4 range no wider
  than `/24`.
- Cancellation, response and device limits, duplicate accounting, and
  diagnostic packet statistics.
- GTK-free library boundaries and deterministic fake-device tests.

The implementation plan, including the UI, lineup, guide, playback, security,
hardware-validation, packaging, and release boundaries, is in
[`docs/plan-v0.1.md`](docs/plan-v0.1.md).

## Try discovery

Balun currently requires Rust 1.94 or newer. Ordinary local discovery is the
default:

```bash
cargo run --locked --bin balun-discover
```

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
packet-rate and concurrency defaults. Only scan a network you own or administer.
Prefer a known `--target` address whenever one is available.

Press `Ctrl+C` to cancel discovery.

## Development

The headless foundation has no GTK or GStreamer build dependency yet. Run the
same core checks used by CI with:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo test --release --all-targets --locked
```

CI verifies the declared Rust 1.94 minimum, runs strict Linux debug and release
checks, and compile-checks the headless code on macOS and Windows. The release
candidate workflow accepts an existing annotated, v-prefixed Semantic Version
tag, verifies it against `Cargo.toml` and `CHANGELOG.md`, and builds the exact
tag commit on all three platforms. It intentionally produces internal
diagnostic workflow artifacts only; application packages and public artifact
publication begin with the playable GTK/GStreamer slice.

## Scope

Balun is a viewer, not a DVR or tuner administration tool. Recording,
timeshift, protected-channel playback, firmware management, transcoding, and a
merged cross-device channel list are outside the v0.1 scope.

## License

Balun is licensed under [GPL-3.0-or-later](LICENSE).
