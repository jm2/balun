# Release component policy

Last reviewed: 2026-08-31

Balun plays unprotected live television supplied by HDHomeRun devices. It does
not implement DVD or Blu-ray playback, protected-channel playback, or a
proprietary content-decryption module. Release inputs and future application
packages must therefore exclude dedicated optical-disc access/copy-control
components and proprietary DRM modules that Balun does not use.

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

## Enforcement available now

Balun does not yet contain a GTK/GStreamer application package. The current
validator therefore makes only claims that can be checked against source and
packaging inputs:

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
