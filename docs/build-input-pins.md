# Build input pins

H2.2 remains open. This first slice pins the **starting job-container images**
used by CI and Linux release builds. It does not freeze the full build environment
or claim reproducible artifacts. New signing and provenance work remains paused
by maintainer direction.

## Container inventory and enforcement

[`builder-images.json`](../build-aux/toolchain/builder-images.json) records the
exact reference for every job container in `.github/workflows`. Each reference
retains its descriptive tag and adds an immutable SHA-256 image-index digest.
The registry resolves the digest; moving the tag cannot silently change this
starting image. The multi-platform index retains the required architecture choices.

| Image tag | Jobs | Required Linux architectures |
| --- | --- | --- |
| `ghcr.io/flathub-infra/flatpak-github-actions:gnome-50` | CI Flatpak | amd64 |
| `fedora:44` | CI MSRV/desktop; release RPM | amd64, arm64 |
| `debian:sid` | Release Debian | amd64, arm64 |
| `archlinux:base-devel` | Release Arch | amd64 |

The initial pins were resolved from those existing tags on 2026-09-18 UTC.
The raw index bytes were hashed, retrieved again by digest, and the required
platform manifests were fetched and checked against the index's digest entries.
This verifies registry content identity and availability at observation time;
it is not a signature or publisher-identity check.

The lint job runs `scripts/check_builder_images.py` using the same PyYAML
installation as yamllint. It scans every workflow's job containers, accepts both
GitHub YAML container forms, requires literal digest references, and compares
exact workflow/job membership and references with the inventory. A changed
registry or digest, mutable tag, expression, new/unrecorded container, removed
container, missing workflow, or duplicate YAML/JSON key fails. Runtime image
selection from expressions is deliberately outside this initial contract.

```sh
python3 -B scripts/check_builder_images.py
python3 -B scripts/test_builder_images.py
```

These scripts inspect trusted checked-in workflow/policy files; they are not
untrusted-YAML parsers or a replacement for actionlint. The policy itself is
reviewed source: changing it together with a workflow proposes a new pin and
still requires review and passing CI. The checker does not decide who approved
a commit. It covers `jobs.<job>.container`, not service containers or Docker
images invoked by action implementations and shell commands.

## Updating a pin

Resolve the intended tag using a registry client, inspect its supported
platforms, and retain the **index** digest when a job spans architectures.
For example, [Skopeo's raw inspection](https://github.com/containers/skopeo/blob/main/docs/skopeo-inspect.1.md)
returns the manifest/index bytes whose SHA-256 identifies that object:

```sh
skopeo inspect --raw docker://docker.io/library/fedora:44 | sha256sum
skopeo inspect --raw docker://docker.io/library/fedora@sha256:<reviewed-index-digest>
```

Update all corresponding workflow references and inventory entries in one PR.
Explain the intended input update, confirm required platforms, and run the
policy tests, workflow lint, and native CI jobs. Release-only container updates
also need their affected package candidate builds before release acceptance.
There is no automatic tag-to-digest refresh during builds. If a registry removes
a pinned object, the job fails; it must not fall back to the mutable tag.

## Remaining H2.2 scope

The following inputs are still mutable or incompletely pinned. They are pending
work, not newly approved exceptions:

- GitHub-hosted runner images and the preinstalled host tools/kernel.
- Packages installed afterward through APT, DNF, pacman/MSYS2, and Homebrew,
  including their transitive native dependencies and repository snapshots.
- Release Rust/compiler selection, Flatpak runtime/SDK/extensions, and native
  packagers and helper dependencies not already covered by existing exact pins.
- Bootstrap tools and remaining Python dependency closures,
  where a top-level version or action commit does not identify every input.

Existing Cargo locks, action commits, component-policy hashes, and Windows
installer-tool pins remain separate controls. H2.2 completes only after the
remaining input inventory, enforceable pins or explicitly reviewed exceptions,
and drift rejection evidence are in place.

## Node lint dependency closure

The Markdown and TOML checks use a reviewed npm lock containing all 87 dependency
packages, with exact versions, tarball URLs, and SHA-512 integrity values. Fresh
CI installs use `npm ci` with lifecycle scripts disabled and invoke installed
binaries directly. The Markdown linter and its TOML parser advance to clear the
observed dependency advisories; Taplo remains at 0.7.0. Updating, local
validation, and the remaining Node/npm bootstrap boundary are documented in
the [lint tool manifest directory](../build-aux/toolchain/node-lint/README.md).
