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
  and other unambiguous tunnel links. Native macOS and Windows automatic
  providers remain intentionally unavailable until their route-domain and
  safe-API requirements can be proved.
- A GTK-free route-approval policy that fingerprints the exact private
  targets, tunnel identity, route scopes, and traffic budget with an
  installation key, fails closed on ambiguous paths or clock rollback, and
  reserves single-use scan authority before it can be released.
- A private, bounded approval store with cross-process locking, atomic durable
  reservations, strict quarantine, global run sequencing, exact and revoke-all
  controls, and no persisted raw topology.
- A packet-free fresh-route gate that consumes the stored authority, permits
  only a transient interface-ID replacement, and caps work to the remaining
  reservation lease. No automatic socket runner is connected yet.
- A fail-closed Linux rtnetlink event-monitor building block that subscribes
  before a route snapshot, authenticates kernel senders, applies strict
  drain/barrier budgets, coalesces reconciliation, and invalidates authority
  before notification. It remains deliberately unwired.
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
or route-table state that cannot be represented safely. The remembered
approval policy, durable private store, and immediate exact-route revalidation
gate are present. A private Linux factory now seals a UDP socket to the exact
fresh interface after name/index readback, and a packet-free admission boundary
registers invalidation before reserve and carries one non-extending monotonic
deadline. Neither capability exposes an automatic send path yet. Route-derived
targets remain disconnected from the diagnostic until a live route-observer
session owns the route-event monitor, a separate cross-process approval-store
observer is complete, final pre-send revalidation is serialized, and a
consuming runner has exact completion ownership. Installing the provider does
not silently enumerate a network.

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

`.github/rust-toolchain.toml` is the Dependabot proposal source for the compiler
floor; it is not a repository-wide rustup override. Keep its exact patch-zero
release aligned with `Cargo.toml`, the dedicated `MSRV` job, and this README:

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
fail-closed filename-token policy rejects dedicated optical-disc copy-control
components and proprietary DRM modules from current release inputs. Future
bundles are required to enforce the same exclusion over their staged and final
contents. The current Linux-CI check pins the reviewed policy, then validates
repository names, Rust and Cargo build inputs, executable helpers, workflows,
and recognized native packaging/build inputs. The exact tag checkout runs the
same pinned fixture suite in the release-candidate workflow.

There is no GTK/GStreamer application package to inspect yet, so this is not an
artifact-compliance claim. Packaging work must add platform-specific checks at
staging, native-import traversal, completed-tree inspection, and reopened final
artifact inspection before any package is published. Self-contained bundles
will stage a capability-derived, reviewed GStreamer plugin closure rather than
an entire plugin distribution; that claim does not extend to distro-provided or
shared runtimes. Ordinary codecs, containers, TLS, and general-purpose
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
CI exercises them as preparatory packaging scaffolding only. They do not become
an artifact claim until each real package adds bounded extraction and traversal,
containment and symlink checks, stable completed-tree snapshots, and a final
reopen-and-validate step. The vendored Flatpak Cargo generator is checksum- and
provenance-pinned; its dependency snapshot must be regenerated from Balun's own
`Cargo.lock`, never copied from Tributary.
The file-by-file decisions and atomic landing conditions are recorded in the
[Tributary build-infrastructure port ledger](docs/tributary-build-infrastructure.md).

## Scope

Balun is a viewer, not a DVR or tuner administration tool. Recording,
timeshift, protected-channel playback, firmware management, transcoding, and a
merged cross-device channel list are outside the v0.1 scope.

## License

Balun is licensed under [GPL-3.0-or-later](LICENSE).
