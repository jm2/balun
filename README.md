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
> discovery, device-registry, and lineup foundation plus a diagnostic command.
> It does not yet contain the desktop viewer, EPG, or playback implementation.

## Current foundation

- Validated HDHomeRun DeviceID and discovery packet framing, TLV, and CRC
  handling.
- Per-interface IPv4 broadcast and scoped IPv6 multicast discovery.
- Exact-address unicast discovery for routed networks, including tunnels where
  broadcast and multicast are unavailable.
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
- Device-scoped natural channel identities and compatibility with both the
  documented lineup `Tags` field and current `Favorite`, `DRM`, and `HD`
  fields.
- A cross-platform route snapshot and candidate policy with deterministic fake
  providers, plus a native Linux rtnetlink provider that recognizes WireGuard
  and other unambiguous tunnel links. Native macOS and Windows providers are
  still pending.
- GTK-free library boundaries and deterministic fake-device tests.

The implementation plan, including the UI, lineup, guide, playback, security,
hardware-validation, packaging, and release boundaries, is in
[`docs/plan-v0.1.md`](docs/plan-v0.1.md). Sanitized hardware observations are
recorded in [`docs/compatibility-v0.1.md`](docs/compatibility-v0.1.md).

## Try discovery

Balun currently requires Rust 1.94 or newer. Ordinary local discovery is the
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
or route-table state that cannot be represented safely. Route-derived targets
are not wired to the diagnostic until the remembered user-approval and
cooldown flow lands; installing the provider does not silently enumerate a
network.

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
