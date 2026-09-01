# Tributary build-infrastructure port ledger

Balun uses Tributary's `scripts/` and `build-aux/` trees as its release-
engineering baseline. This is a heavy structural port, not an unreviewed
directory copy: reusable helpers retain their shape, while product identity,
runtime capabilities, permissions, artifacts, and release gates are adapted to
Balun's narrower live-TV scope.

This ledger prevents a deferred file from being forgotten and prevents a copied
recipe from making a packaging claim before its referenced application exists.

Equivalent ports keep the Tributary filename. A new filename is permitted only
when Balun introduces or splits out a materially different responsibility; the
ledger must record that distinction. Product-named package recipes and manifests
replace only their Tributary identity segment with Balun's identity.

## Identity and product substitutions

| Tributary value | Balun value |
| --- | --- |
| `Tributary` | `Balun` |
| `tributary` | `balun` |
| `jm2/tributary` | `jm2/balun` |
| `io.github.tributary.Tributary` | `io.github.jm2.Balun` |
| Product description | `A lightweight cross-platform HDHomeRun live TV viewer` |
| GUI binary | `balun` (never `balun-discover`) |

The Windows installer must receive a new deterministic application GUID rather
than reuse Tributary's. Platform runtime probes must cover Balun's video,
network, MPEG-TS, decoder, and sink capabilities rather than Tributary's audio-
library assumptions.

## Current portable tranche

| Reference file | Balun disposition |
| --- | --- |
| `flatpak/LICENSE.flatpak-cargo-generator` | Vendored unchanged |
| `flatpak/flatpak-cargo-generator.py` | Vendored unchanged and checksum-pinned |
| `flatpak/flatpak-cargo-generator.sha256` | Vendored unchanged |
| `flatpak/generate-cargo-sources.sh` | Reused unchanged |
| `flatpak/flatpak-cargo-generator.PROVENANCE` | Adapted provenance |
| `flatpak/generator-requirements.txt` | Adapted comments; versions remain pinned |
| `flatpak/cargo-sources.json` | Never copied or committed; regenerate from Balun's lockfile |
| `flatpak/validate-permissions.sh` | Adapted exact Balun permission allowlist; requires an explicit manifest |
| `flatpak/test-permissions.sh` | Adapted synthetic permission fixtures; makes no package claim |
| `linux/validate-package-compliance.sh` | Adapted preparatory validator |
| `linux/test-package-compliance.sh` | Adapted synthetic tests |
| `linux/validate-package-metadata.sh` | Incremental current-input list |
| `flatpak/validate-bundle-compliance.sh` | Adapted preparatory app-commit validator |
| `packaging/forbidden-bundled-components.txt` | Keep Balun's stricter policy; never overwrite |

The artifact validators above are scaffolding, not evidence that a Balun package
exists. Linux completed-tree inspection is bounded, rejects unsafe links and
unsupported entry types, and compares hidden-inclusive metadata-and-content
snapshots. Before a real package invokes it, add archive-member preflight,
source snapshotting and resource budgets, extractor-specific containment, and
final artifact reopening for that format.

## Scripts port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `build-linux.sh` | Adapted for the current headless diagnostic; GUI and native/Flatpak package modes fail before build work until their complete gates land |
| `build-macos.sh` | Adapted for the current native headless diagnostic and pinned Mach-O inspection; app, DMG, signing, and notarization modes fail before external work until their complete gates land |
| `build-windows.ps1` | Adapted for the current headless diagnostic; bundle, ZIP, Inno, update, and launch paths fail before external work until their complete gates land |
| `macos-icon-bundle-policy.sh` | Adapted preparatory helper with Balun identity/temp names |
| `macos-package-policy.sh` | Adapted inspection-only core with bounded Mach-O output and completed-tree checks; broad copy-any-allowed-plugin staging API removed |
| `sync_rust_toolchain.py` | Port independently against Balun's actual MSRV declarations |
| `sync_fuzz_lock.py` | Defer until Balun has a separate fuzz workspace and lockfile |
| `test-macos-icon-bundle-policy.sh` | Adapted synthetic test for the icon helper |
| `test-macos-package-policy.sh` | Adapted synthetic test; denied fixtures are derived from the shared policy |
| `test_dependency_update_policy.py` | Split by feature: the applicable compiler portion is `test_rust_toolchain_policy.py`; fuzz and automerge tests remain deferred or inapplicable until their owners exist |

Balun additionally has deterministic command-routing tests for the current
headless Linux, macOS, and Windows helpers. Tributary has no equivalent
scripts, so `test-build-linux-policy.sh`, `test-build-macos-policy.sh`, and
`test-build-windows-routing.ps1` are new, narrow responsibilities rather than
renamed upstream ports.

The compiler proposal manifest lives at
`build-aux/toolchain/rust-toolchain.toml`. Dependabot supports a configured
subdirectory, and this non-privileged location avoids both a repository-wide
rustup override and GitHub's rejected attempt to update the original copy under
`.github`.

## Package-file port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `flatpak/io.github.tributary.Tributary.yml` | Redesign as `io.github.jm2.Balun.yml` with GUI, assets, runtime probe, permissions, and package CI |
| `flatpak/test-permissions.sh` | Landed first against a synthetic manifest so the permission contract is reviewable before packaging |
| `flatpak/validate-permissions.sh` | Adapted to Balun's exact permission allowlist; the real manifest must invoke it atomically when it lands |
| `inno/tributary.iss` | Redesign as `balun.iss` with new GUID, resources, version mapping, and reopened-installer gate |
| `arch/PKGBUILD` | Deferred with native distro packages |
| `rpm/tributary.spec` | Deferred with native distro packages |

Balun's Flatpak baseline needs Wayland, fallback X11, IPC, PulseAudio, and
network access for local HDHomeRun discovery/HTTP and explicit XMLTV URLs. It
does not inherit blanket music/removable-media access, GVfs, Secret Service,
Rhythmbox compatibility, audio-file associations, or Tributary's MPRIS name.
A user-selected XMLTV file should use the file/document portal. The standard
GPU/DRI permission is broader than render nodes alone, and the PulseAudio
socket exposes more than Balun's intended output use; both are explicit,
reviewed tradeoffs for practical accelerated video and audio playback, not
narrower capability claims.

## Required real-package order

1. Validate the checksum-pinned repository and packaging inputs.
2. Create fresh staging from a reviewed, capability-derived runtime closure.
3. Inspect each native dependency edge without loading target code.
4. Inspect a bounded, hidden-inclusive completed tree and reject unsafe links.
5. Run the relocated packaged runtime probe with synthetic video.
6. Repeat tree and dependency checks after every writer or signing step.
7. Create the package container.
8. Reopen the final artifact and repeat its metadata, import, and payload gates.
9. Upload only validated artifacts and enforce an exact inventory.
10. Grant release-write permission only to the final publisher, which checks out
    no project source.
