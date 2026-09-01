# Tributary build-infrastructure port ledger

Balun uses Tributary's `scripts/` and `build-aux/` trees as its release-
engineering baseline. This is a heavy structural port, not an unreviewed
directory copy: reusable helpers retain their shape, while product identity,
runtime capabilities, permissions, artifacts, and release gates are adapted to
Balun's narrower live-TV scope.

This ledger prevents a deferred file from being forgotten and prevents a copied
recipe from making a packaging claim before its referenced application exists.

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
| `linux/validate-package-compliance.sh` | Adapted preparatory validator |
| `linux/test-package-compliance.sh` | Adapted synthetic tests |
| `linux/validate-package-metadata.sh` | Incremental current-input list |
| `flatpak/validate-bundle-compliance.sh` | Adapted preparatory app-commit validator |
| `packaging/forbidden-bundled-components.txt` | Keep Balun's stricter policy; never overwrite |

The artifact validators above are scaffolding, not evidence that a Balun package
exists. Before a real package invokes them, add bounded archive and tree work,
extraction containment, unsafe-link rejection, stable before/after snapshots,
and final artifact reopening for that format.

## Scripts port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `build-linux.sh` | Redesign with the GUI/runtime slice; omit deferred native distro-package modes |
| `build-macos.sh` | Redesign with `Balun.app`, runtime probe, exact plugin closure, and reopened DMG checks |
| `build-windows.ps1` | Redesign with `balun.exe`, resources, exact plugin closure, and reopened ZIP/installer checks |
| `macos-icon-bundle-policy.sh` | Adapted preparatory helper with Balun identity/temp names |
| `macos-package-policy.sh` | Port reusable inspection core; remove broad copy-any-allowed-plugin staging API |
| `sync_rust_toolchain.py` | Port independently against Balun's actual MSRV declarations |
| `sync_fuzz_lock.py` | Defer until Balun has a separate fuzz workspace and lockfile |
| `test-macos-icon-bundle-policy.sh` | Adapted synthetic test for the icon helper |
| `test-macos-package-policy.sh` | Land with the package helper; derive denied fixtures from the shared policy |
| `test_dependency_update_policy.py` | Split by feature; land focused MSRV, fuzz, and automation tests with their owners |

## Package-file port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `flatpak/io.github.tributary.Tributary.yml` | Redesign as `io.github.jm2.Balun.yml` with GUI, assets, runtime probe, permissions, and package CI |
| `flatpak/test-permissions.sh` | Redesign and land atomically with the Balun manifest |
| `flatpak/validate-permissions.sh` | Redesign to Balun's exact permission allowlist |
| `inno/tributary.iss` | Redesign as `balun.iss` with new GUID, resources, version mapping, and reopened-installer gate |
| `arch/PKGBUILD` | Deferred with native distro packages |
| `rpm/tributary.spec` | Deferred with native distro packages |

Balun's Flatpak baseline needs Wayland, fallback X11, IPC, PulseAudio, and
network access for local HDHomeRun discovery/HTTP and explicit XMLTV URLs. It
does not inherit blanket music/removable-media access, GVfs, Secret Service,
Rhythmbox compatibility, audio-file associations, or Tributary's MPRIS name.
A user-selected XMLTV file should use the file/document portal.

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
