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
| `packaging/validate-release-components.sh` | New Balun-wide input classifier and checksum-pinned deny-policy validator; Tributary has no same-purpose helper |
| `packaging/test-release-component-policy.sh` | New adversarial fixture suite for Balun's release-component input policy; Tributary has no same-purpose helper |

The artifact validators above are scaffolding, not evidence that a Balun package
exists. Linux completed-tree inspection is bounded, rejects unsafe links and
unsupported entry types, and compares hidden-inclusive metadata-and-content
snapshots. Before a real package invokes it, add archive-member preflight,
source snapshotting and resource budgets, extractor-specific containment, and
final artifact reopening for that format.

## Scripts port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `build-linux.sh` | Adapted to a no-option, build-only locked release desktop build with explicit `--diagnostic`, desktop-default quick modes, GTK/libadwaita/GStreamer development-floor checks, a structural GStreamer runtime plugin-file gate with a libav warning, an exact native target and repository-local Cargo target path, and Linux ELF inspection; native/Flatpak package modes fail before build work until their complete gates land |
| `build-macos.sh` | Adapted to a no-option, build-only native locked release desktop build with explicit `--diagnostic`, desktop-default quick modes, GTK/libadwaita/GStreamer development-floor checks, a structural GStreamer runtime plugin-file gate with a libav warning, exact native-target output binding, and pinned Mach-O inspection; app, DMG, signing, and notarization modes fail before external work until their complete gates land |
| `build-windows.ps1` | Adapted to Tributary's desktop-default flag semantics: no flags auto-detect MSYS2 CLANG64, require the GTK/libadwaita/GStreamer development floors and the structural GStreamer runtime plugin DLLs (Tributary's runtime-plugin gate adapted to Balun's factory contract, with a gst-libav warning), and build a locked release `balun.exe` without launching, `-Run` is the sole desktop launch route, `-Diagnostic` selects the GTK-free tool, and the new-purpose `-InspectLocal` builds, validates, and runs only its fixed local inspection; every compiling route pins its Rust target and repository-local output while bundle, ZIP, Inno, and update paths remain fail-closed |
| `macos-icon-bundle-policy.sh` | Adapted preparatory helper with Balun identity/temp names |
| `macos-package-policy.sh` | Adapted inspection-only core with bounded Mach-O output and completed-tree checks; broad copy-any-allowed-plugin staging API removed |
| `sync_rust_toolchain.py` | Ported against Balun's MSRV declarations and wired into CI and the release-candidate workflow |
| `sync_fuzz_lock.py` | Defer until Balun has a separate fuzz workspace and lockfile |
| `test-macos-icon-bundle-policy.sh` | Adapted synthetic test for the icon helper |
| `test-macos-package-policy.sh` | Adapted synthetic test; denied fixtures are derived from the shared policy |
| `test_dependency_update_policy.py` | Split by feature: the applicable compiler portion is `test_rust_toolchain_policy.py`; fuzz and automerge tests remain deferred or inapplicable until their owners exist |
| `hooks/pre-commit` (repository root, outside `scripts/`) | Ported without Tributary's fuzz-manifest formatting check; opt in with `git config core.hooksPath hooks` |

Balun additionally has deterministic command-routing tests for the Linux,
macOS, and Windows desktop/diagnostic routes.
Tributary has no equivalent scripts, so `test-build-linux-policy.sh`,
`test-build-macos-policy.sh`, `test-build-windows-routing.ps1`, and
`test-desktop-lifecycle.sh` are new, narrow responsibilities rather than
renamed upstream ports. The `--probe-playback` (Linux and macOS) and
`-ProbePlayback` (Windows) helper modes are likewise new-purpose,
fixed-argument routes: they apply the plugin-file gate and run the two
installed-runtime playback probes in the release profile, and all three CI
lanes invoke them after the helper desktop build. The lifecycle helper now runs the ordinary Linux
close/join smoke and the checked-in MPEG-2 render/EOS/`NULL` smoke in
separate, bounded headless-Wayland/session-bus processes; Weston is the default
CI route and Xvfb is only an optional local fallback. CI explicitly requires
the Wayland route and does not install Xvfb; the helper's `auto`, `wayland`, and
`x11` selector keeps local fallback intentional. That is application
acceptance, not a packaging helper or a cross-platform runtime claim, and does
not make X11 a Balun runtime dependency.

All three helpers now make a locked, build-only desktop executable their route
when invoked without options, and their compile-oriented quick modes select
desktop features by default. Those desktop routes fail closed unless
`pkg-config` reports GTK 4.16, libadwaita 1.6, and GStreamer 1.20. Before a
desktop build, each helper additionally checks the plugin files behind Balun's
seven structural factories in the runtime's plugin directory and names the
providing package for any missing file, adapting Tributary's Windows
runtime-plugin gate to Balun's factory contract; libav decoders only warn
because P0.5 has not frozen the decoder contract, and quick modes plus the
diagnostic route skip the plugin check. The helpers still do not mistake these
checks for a complete codec or bundle runtime probe. A 2026-09-02 parity audit
against Tributary's helpers restored the release-profile Clippy pass in every
lint mode, the actionable cargo, rustc, GNU readelf, and development-package
install hints, and a read-only Windows check that the gnullvm Rust target is
installed with a `rustup target add` hint in place of Tributary's automatic
installation. Deliberate remaining differences are documented in each helper's
help text: `-Test` runs the debug profile because CI runs both, only the
x86_64 CLANG64 environment is supported until ARM64 Windows is exercised,
Homebrew's pkg-config resolves its own prefix so the macOS helper never
queries Homebrew, and `cargo update` is run directly instead of through a
helper mode. Linux and macOS retain the
GTK- and GStreamer-free tool behind
`--diagnostic`; Windows uses `-Diagnostic`. Tributary defines an explicit launch
flag only for its Windows PowerShell helper, so Balun preserves `-Run` there and
does not invent a shell `--run`. Linux and macOS derive, validate, and pass the
native Rust host target explicitly before applying their ELF or Mach-O gates;
Windows does the same for native diagnostics and already fixes its CLANG64
desktop target. This prevents inherited Cargo target configuration from moving
a fresh artifact away from the path being inspected. Windows owns the compiler,
`pkg-config`, Rust-target, and output-path wiring but makes no PE validation
claim. The release-candidate workflow selects all three diagnostic routes
explicitly. These developer conveniences do not imply a bundle, installer,
redistributable runtime closure, or package validation.

The Fedora desktop CI image installs `gstreamer1-plugin-libav` solely to decode
the pinned synthetic MPEG-2 test fixture. That host development dependency is
not a runtime allowlist or permission to copy a broad plugin package into an
artifact. Every future staged closure remains subject to completed-tree/import
inspection and the shared libdvdcss, optical-disc, DRM, and circumvention deny
policy.

The compiler proposal manifest lives at
`build-aux/toolchain/rust-toolchain.toml`. Dependabot supports a configured
subdirectory, and this non-privileged location avoids both a repository-wide
rustup override and GitHub's rejected attempt to update the original copy under
`.github`.

## Package-file port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `data/io.github.tributary.Tributary.desktop` | Landed as `data/io.github.jm2.Balun.desktop` with Balun's identity, categories, and keywords; no MIME associations because Balun opens no files. |
| `data/io.github.tributary.Tributary.metainfo.xml` | Landed as `data/io.github.jm2.Balun.metainfo.xml`; screenshots are added once a stable window exists. |
| `data/icons/hicolor/*`, `data/tributary.iconset/`, `data/tributary.ico` | Landed as `data/icons/hicolor/*` (plus scalable and symbolic SVG sources), `data/balun.iconset/`, and `data/balun.ico`, all rendered from one SVG. |
| `flatpak/io.github.tributary.Tributary.yml` | Landed as `io.github.jm2.Balun.yml`: GNOME 50 runtime, the six reviewed permissions, the ffmpeg-full extension for broadcast decoders, a build-time probe of the seven structural factories, desktop/metainfo/icon installation, and the app-payload gate; CI builds x86_64 and the release-candidate workflow builds x86_64 and aarch64 |
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
