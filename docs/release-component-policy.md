# Release component policy

Last reviewed: 2026-09-03

Balun plays unprotected live television supplied by HDHomeRun devices. It does
not implement DVD or Blu-ray playback, protected-channel playback, or a
proprietary content-decryption module. Release inputs and future application
packages must therefore exclude dedicated optical-disc access/copy-control
plugins, optical-disc copy-control/decryption components, and proprietary DRM
modules that Balun does not use.

This is a conservative release-engineering boundary, not a conclusion about
the legal status of a component or filename and not legal advice. Licenses,
patents, and distribution rules still require review on their own facts.

## Shared policy

[`build-aux/packaging/forbidden-bundled-components.txt`](../build-aux/packaging/forbidden-bundled-components.txt)
is the single machine-readable, case-insensitive filename-token policy. Its
reviewed bytes are pinned by checksum in the validator. It starts with
Tributary's current optical-disc and proprietary-DRM deny list and adds the
Widevine, DTCP, and OpenCDM names relevant to protected television paths. The
policy remains intentionally narrow.

Ordinary video and audio codecs, MPEG-TS and other container support, TLS, and
general-purpose cryptography are not denied by this policy. They are necessary
for normal unprotected playback or transport security and remain subject to
separate compatibility, license, patent, and provenance review. This policy is
not a broad codec allowlist or ban.

As in Tributary, generic `libbluray` remains outside this denylist: it provides
non-decrypting Blu-ray access and navigation and is not itself an AACS or BD+
decryption implementation. Balun does not need or plan to bundle it; this
distinction prevents the narrow recognizable-name policy from making a broader
claim than it can support. Dedicated playback plugins and the separate AACS,
BD+, DVD copy-control, and proprietary-DRM implementations remain denied.

## Enforcement available now

The repository validator makes only claims that can be checked against source
and packaging inputs; the Flatpak bundle and the Windows package add their own
artifact gates below. The repository validator checks that:

- the shared policy must be present, regular NUL-free UTF-8 text, non-empty,
  bounded, well formed, checksum-pinned, and free of case-insensitive
  duplicates;
- repository path and immediate symlink-target names are checked against every
  token, with bounded, control-free names;
- Rust and Cargo build inputs, executable text helpers, CI workflows/actions,
  standard Make/CMake/Meson inputs, `build-aux` and packaging files,
  release/build scripts, and recognized native package recipes are checked for
  textual references to denied tokens;
- each recognized input is checked as bounded NUL-free UTF-8, all tokens are
  matched in one pass, and file-count plus cumulative-byte budgets cap the
  complete scan; and
- the deterministic validator and its negative fixtures run in Linux CI, and
  the same pinned fixture suite checks the immutable tag checkout used by the
  current release-candidate workflow.

Run the current checks with:

```bash
build-aux/packaging/test-release-component-policy.sh
build-aux/packaging/validate-release-components.sh --repository
```

This gate detects recognizable names and declared inputs. It does not inspect
native imports, infer the behavior of renamed code, recursively unpack an
innocuously named nested archive, or attest a package that does not exist yet.
It assumes the checked-out source tree is not being replaced concurrently
during validation.

The current macOS build helper additionally loads this pinned policy before
building and applies bounded, multi-architecture `otool` inspection to the
selected native desktop or diagnostic output. Its synthetic tests also
validate stable, typed app-tree manifests. This is useful preparatory
enforcement, but it is not a `Balun.app` or DMG claim; bundle staging, signing
mutations, and reopened-container checks remain mandatory with the real
package.

The Windows ZIP (`scripts/build-windows.ps1 -Bundle`, `-Zip`) implements all
four mandatory gates: the helper loads this pinned policy before any copy,
stages only the reviewed capability-derived plugin closure and the DLLs those
binaries import while refusing forbidden or reparse-point sources and
destinations, fails the bounded `llvm-readobj` import traversal on any denied
dependency rather than omitting it, reinspects the completed tree after the
packaged runtime probe, and reopens the ZIP to require its entry set and sizes
to equal the staged tree. The installer (`-InnoSetup`) shares the first three
gates: its payload is the staged tree that passed them immediately before
compilation, and the compiled `balun-setup.exe` is reopened only for its
version resource, because the helper has no tool that extracts an Inno Setup
archive without running it. That narrower installer reopening is a documented
gap against the fourth gate, not a claim of payload inspection; the
packaged-artifact validation record P4.1 installs and exercises the result.
The generic `libbluray` that `avformat` imports remains allowed under the
distinction above.

## Mandatory package gates

Adding any self-contained GTK/GStreamer package also adds all of these
platform-specific fail-closed gates in the same change; source/input validation
alone is not sufficient:

1. **Staging:** on platforms where Balun bundles GStreamer, derive an allowlist
   from reviewed runtime capabilities and stage only the required playback
   core, HTTP source, MPEG-TS parser/demuxer, selected parser/decoder, and
   video/audio sink plugin closure. Check every selected plugin, native
   library, source path, and destination before it enters the application tree.
2. **Native imports:** inspect PE, Mach-O, or ELF dependency references and
   the complete copied dependency closure with bounded platform-native tools.
3. **Completed tree:** recursively inspect the final app-owned tree, including
   hidden entries and stale incremental files, immediately before container
   creation; reject unsafe symlink, junction, or reparse-point boundaries.
4. **Reopened artifact:** reopen the completed ZIP, installer payload, disk
   image, Flatpak commit, or native package and validate its names, metadata,
   imports, and app-owned payload before upload or publication.

The shared denylist remains defense in depth at every stage; a capability
allowlist does not replace it. The allowlist requirement applies to native
libraries and plugins copied into Balun's own bundle. Native distribution
packages and separately delivered system or Flatpak runtimes remain external
dependency boundaries: Balun must validate its own package metadata and
app-owned payload and document those runtime dependencies, but must not claim
to have allowlisted or inspected the contents of a shared repository/runtime.

Each platform helper must load the same repository policy and fail when the
policy or a required inspector is unavailable. Signing, notarization, and
container tools do not replace these checks.

## Review boundary

Weakening the denied list, adding a media framework or native dependency
source, adding an archive that the gates do not reopen, or enabling optical-disc
or protected-content playback requires a dedicated design and distribution
review. Update the shared policy, this document, its tests, and the changelog
together; never add a silent platform-only exception.

Every new build-system input, executable helper family, dependency source, or
package recipe must extend the repository classifier and add a negative fixture
in the same change. Every new package format must also add its staging,
native-import, completed-tree, and reopened-artifact gates in that change.
