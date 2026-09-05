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
| `linux/validate-package-compliance.sh` | Adapted gate for ELF files, completed trees, and locally produced deb, RPM, and Arch packages |
| `linux/test-package-compliance.sh` | Adapted synthetic tests for metadata, trees, and native package formats |
| `linux/validate-package-metadata.sh` | Current input list includes Cargo, Flatpak, and Arch package declarations |
| `flatpak/validate-bundle-compliance.sh` | Adapted completed-bundle importer and app-commit validator |
| `packaging/forbidden-bundled-components.txt` | Keep Balun's stricter policy; never overwrite |
| `packaging/validate-release-components.sh` | New Balun-wide input classifier and checksum-pinned deny-policy validator; Tributary has no same-purpose helper |
| `packaging/test-release-component-policy.sh` | New adversarial fixture suite for Balun's release-component input policy; Tributary has no same-purpose helper |

The artifact validators above are active package gates, not a substitute for a
successful native build or runtime probe. Linux completed-tree inspection is
bounded, rejects unsafe links and unsupported entry types, and compares
hidden-inclusive metadata-and-content snapshots. The v0.1 native and Flatpak
jobs accept only artifacts they just built locally; archive-member preflight,
source snapshotting before extraction, extractor-specific containment, and
hostile-bundle resource budgets remain future hardening.

## Scripts port ledger

| Tributary file | Status and landing condition |
| --- | --- |
| `build-linux.sh` | Adapted to a no-option locked release desktop build with explicit `--diagnostic`, desktop-default quick modes, GTK/libadwaita/GStreamer floor checks, an exact native target, repository-local Cargo output, and ELF inspection. `--deb`, `--rpm`, and `--arch-pkg` require pinned preinstalled packagers, reuse the reviewed build or recipe, validate the locally produced package, and never install dependencies; Flatpak remains workflow-owned. |
| `build-macos.sh` | Adapted to a no-option native locked release desktop build with explicit `--diagnostic`, desktop-default quick modes, dependency and plugin gates, exact target binding, and pinned Mach-O inspection. `--app` assembles and ad-hoc signs the reviewed transitive dylib and 22-plugin closure, then runs a relocated isolated runtime probe; `--dmg` creates and reopens the drag-to-Applications image. Notarization remains unavailable. |
| `build-windows.ps1` | Adapted to Tributary's desktop-default semantics with strict x86_64 CLANG64 and ARM64 CLANGARM64 profiles. The selected Rust target binds the MSYS2 environment, package prefix, PE machine type, probe receipt, and Inno architecture. `-Run` alone adds the console-attached developer feature to a release-profile build; package modes omit it and remain GUI-subsystem applications. `-Diagnostic` and `-InspectLocal` keep their narrow routes; `-Bundle`, `-Zip`, and `-InnoSetup` (with `-SkipBundle` and `-NoCargoBuild`) stage, probe, and reopen the matching package. `-Package` and `-Installer` remain invalid; `-CargoUpdate` stays unavailable. |
| `macos-icon-bundle-policy.sh` | Adapted bundle icon gate with Balun identity and private temporary names |
| `macos-package-policy.sh` | Adapted bounded Mach-O, completed-tree, signing, and reopened-DMG gate; broad copy-any-allowed-plugin staging API removed |
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
installed-runtime playback probes in the release profile, and every native CI
profile invokes them after the helper desktop build. The lifecycle helper now runs the
ordinary Linux close/join smoke and the checked-in MPEG-2 render/EOS/`NULL` smoke in
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
runtime-plugin gate to Balun's factory contract. The build-only route warns
when libav is absent; the frozen package closures instead require their
reviewed decoders. Quick modes and the diagnostic route skip the plugin check,
and the installed-runtime probe remains the complete codec inventory rather
than treating a file-presence gate as one. A 2026-09-02 parity audit
against Tributary's helpers restored the release-profile Clippy pass in every
lint mode, the actionable cargo, rustc, GNU readelf, and development-package
install hints, and a read-only Windows check that the selected GNU-LLVM Rust
target is installed with a `rustup target add` hint instead of automatic
installation. Deliberate remaining differences are documented in each
helper's help text: `-Test` runs the debug profile because CI runs both,
Homebrew's pkg-config resolves its own prefix so the macOS helper never queries
Homebrew, and `cargo update` is run directly instead of through a helper mode.

Linux and macOS retain the GTK- and GStreamer-free tool behind `--diagnostic`;
Windows uses `-Diagnostic`. Tributary defines a launch flag only for its
Windows PowerShell helper; Balun keeps `-Run` there and offers the same
convenience as `--run` on Linux and macOS, which replaces the helper with the
built desktop once its gates pass. Linux and macOS derive, validate, and pass the native
Rust host target explicitly before applying their ELF or Mach-O gates. Windows
binds either the x86_64 or ARM64 GNU-LLVM target to the matching MSYS2 profile
and checks every staged PE machine type. This prevents inherited Cargo
configuration or a mixed toolchain from moving or substituting the artifact
being inspected.

The Linux native-package modes invoke only pinned packagers already on the
host and hand their locally produced outputs to the format-specific validator.
The macOS modes derive the app's runtime closure, ad-hoc sign it, run the
isolated relocated probe, and reopen the DMG. Windows owns the compiler,
`pkg-config`, target, output-path, PE import, resource, tree, probe, archive,
and installer gates described below. The release-candidate workflow selects
diagnostic routes explicitly and admits only the exact public package
inventory.

The packaged macOS launcher asks the signed `Balun-bin` to derive its canonical
install-key hash, so an ordinary app launch does not execute Perl. Perl remains
a build/check-only tool for bounded package-policy validation, including the
Arch recipe's `checkdepends`; it is not an installed runtime dependency.

The Fedora desktop CI image installs `gstreamer1-plugin-libav` solely to decode
the pinned synthetic MPEG-2 test fixture. That host development dependency is
not a runtime allowlist or permission to copy a broad plugin package into an
artifact. Every staged closure remains subject to completed-tree/import
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
| `data/io.github.tributary.Tributary.metainfo.xml` | Landed as `data/io.github.jm2.Balun.metainfo.xml` with the v0.1 release and sanitized application screenshot. |
| `data/icons/hicolor/*`, `data/tributary.iconset/`, `data/tributary.ico` | Landed as `data/icons/hicolor/*` (plus scalable and symbolic SVG sources), `data/balun.iconset/`, and `data/balun.ico`, all rendered from one SVG. |
| `flatpak/io.github.tributary.Tributary.yml` | Landed as `io.github.jm2.Balun.yml`: GNOME 50 runtime, the six reviewed permissions, the ffmpeg-full extension for broadcast decoders, build-time and installed-bundle probes of the seven structural factories, desktop/metainfo/icon installation, and the app-payload gate; CI builds x86_64 and the release-candidate workflow builds x86_64 and aarch64 |
| `flatpak/test-permissions.sh` | Landed first against a synthetic manifest so the permission contract is reviewable before packaging |
| `flatpak/validate-permissions.sh` | Adapted to Balun's exact permission allowlist and invoked against the real manifest before bundle construction |
| `inno/tributary.iss` | Landed as `build-aux/inno/balun.iss`: a deterministic UUID version 5 application GUID, `bin\balun.exe` shortcut and uninstall targets, exact x64/ARM64 architecture selection, a numeric `VersionInfoVersion` beside the textual package version, and the product, description, and copyright fields reopened after compiling |
| `arch/PKGBUILD` | Landed as `build-aux/arch/PKGBUILD` for the x86_64 release artifact; `release_check.py` binds its literal `pkgver` to the tag. |
| `rpm/tributary.spec` | Not copied: Balun's x86_64/aarch64 RPM assets and requirements are declared through `Cargo.toml`'s `generate-rpm` metadata. Debian amd64/arm64 metadata lives beside it. |

Balun's Flatpak baseline needs Wayland, fallback X11, IPC, PulseAudio, and
network access for local HDHomeRun discovery and HTTP. It
does not inherit blanket music/removable-media access, GVfs, Secret Service,
Rhythmbox compatibility, audio-file associations, or Tributary's MPRIS name.
Any future user-selected XMLTV file should use the file/document portal. The standard
GPU/DRI permission is broader than render nodes alone, and the PulseAudio
socket exposes more than Balun's intended output use; both are explicit,
reviewed tradeoffs for practical accelerated video and audio playback, not
narrower capability claims.

## Windows package

`build-windows.ps1 -Bundle`, `-Zip`, and `-InnoSetup` port Tributary's Windows
packaging stages in Tributary's order: shared policy, staging, gst-plugin-scanner,
bounded non-executing PE import closure with `llvm-readobj`, GTK icons and
compiled schemas, the completed-tree policy, the packaged runtime probe, the
receipt, the final PE import and application-resource gates, the ZIP, and Inno
Setup. The deliberate differences are:

- **Architecture profile.** Balun supports one strict x86_64 CLANG64 profile
  and one strict ARM64 CLANGARM64 profile. The Rust target selects the MSYS2
  directory and package prefix, expected COFF machine, receipt fields, and
  Inno architecture. Mixed declarations, host tools, application binaries,
  plugins, scanners, or DLLs fail before the runtime probe.
- **Layout.** Tributary stages a flat tree and sets `GST_PLUGIN_PATH`,
  `GST_PLUGIN_SYSTEM_PATH`, `GST_PLUGIN_SCANNER`, and `GST_REGISTRY` in-process
  at launch. Balun forbids unsafe code, and writing the process environment is
  not possible in safe Rust, so the package keeps the MSYS2 prefix shape
  (`bin\balun.exe` beside every DLL, `lib\gstreamer-1.0`,
  `libexec\gstreamer-1.0\gst-plugin-scanner.exe`, `share`). GStreamer derives
  the plugin directory and the scanner from its own DLL location and prepends
  that directory to the scanner's `PATH`, GLib locates `share` the same way, and
  an ordinary launch needs no variable at all. The packaged application uses
  GStreamer's default per-user registry cache.
- **Closure.** Tributary copies every plugin in the MSYS2 plugin directory
  minus the deny list. Balun's [release component policy](release-component-policy.md)
  requires a capability-derived allowlist, so the helper stages exactly the 27
  plugins listed in its `$GStreamerPluginClosure` table (the seven structural
  factories, the MPEG-TS parsers and converters, the decoders in the Windows
  half of the P0.5 codec contract, and the Windows audio sinks) and prunes any
  other plugin or unreachable DLL from an incremental tree. Every entry is
  required, so a missing `gst-libav` fails the run instead of warning.
- **Probe.** Tributary's probe sets its own environment and poisons proxy
  variables. Balun's helper owns the environment: it removes every `GST_*`,
  GIO, and proxy variable, sets `PATH` to `System32` only and `GST_REGISTRY` to
  a fresh cache, and the Rust probe rejects any other inherited policy key,
  preflights the bundled scanner, and plays the synthetic MPEG-2 fixture from a
  loopback server through the production `appsrc` source policy and stream
  transport (which disables proxies itself) to end of stream. Every autoplugged
  element must come from a bundled plugin file.
- **Reopened artifacts.** Tributary reopens the ZIP for forbidden names only.
  Balun additionally requires the ZIP entry set and sizes to equal the staged
  tree, and reopens the installer's version resource for the product name and
  the exact package version. The installer payload is the tree validated
  immediately before compilation.
- **Receipt.** The unshipped `dist\balun-windows.probe-v2` receipt binds the
  probed tree to the selected Rust target, MSYS2 environment, Inno architecture,
  `bin\balun.exe`, `libgstgtk4.dll`, `libgstwasapi2.dll`, and `libgstlibav.dll`;
  `-InnoSetup -SkipBundle` accepts the tree only while it matches and still
  repeats every non-executing gate.
- **Resources.** `build.rs` ports Tributary's `winresource` and
  `embed-resource` step for the seven-image `data/balun.ico` and the package
  version; the helper requires exactly that resource set before staging and
  again after the probe.

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
