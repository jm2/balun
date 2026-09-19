# Build input pins

H2.2 remains open. The implemented slices pin the **starting job-container images**
and the Rust release selected by rustup-based candidate jobs. They do not freeze
the full build environment or claim reproducible artifacts. New signing and provenance work remains paused
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

## Release compiler selection

[`release-toolchain/rust-toolchain.toml`](../build-aux/release-toolchain/rust-toolchain.toml)
records Rust `1.98.0` for the five rustup-based release jobs: discovery diagnostics,
macOS, Windows, Debian, and RPM. Their pinned action receives that literal release,
so a new `stable` release does not silently change those compilers. The initial
selection is the already-tested compiler floor; the [official release manifest](https://static.rust-lang.org/dist/channel-rust-1.98.0.toml)
lists the required Linux, macOS, and Windows host targets as available.

The lint job runs `scripts/check_release_rust.py` and its regression suite. It
checks the separate manifest, canonical exact release, compatibility with the
Cargo compiler floor, one matching literal input per recorded job, and exact job
membership. Changed, floating, expression-based, missing, duplicate, or additional
Rust action selections reject. The existing synchronization policy still checks
the action's common immutable commit. These inspect trusted repository files;
they do not execute or install the compiler.

Dependabot proposes release compiler updates separately, including patch fixes.
Update the manifest and all five action inputs together, then run the checker,
its tests, workflow lint, and the complete CI matrix. Before release acceptance,
build the affected package candidates with the selected compiler. The compiler
floor proposal, rolling `stable` CI, developer toolchain selection, and the exact
Rust coverage toolchain remain separate. Advancing the release pin does not
automatically raise the MSRV, and an MSRV above the release pin fails this check.

This pins the selected version, not independently reviewed distribution-file
digests or rustup's bootstrap/update behavior. Rustup's existing distribution
download validation remains in use. Arch's distribution compiler and Flatpak's
SDK compiler are outside these five jobs. Shell-installed tools, containers,
transitive inputs, and host tools require their own controls below. This is not
a complete environment lock or a signing/provenance change.

## Remaining H2.2 scope

The following inputs are still mutable or incompletely pinned. They are pending
work, not newly approved exceptions:

- GitHub-hosted runner images and the preinstalled host tools/kernel.
- Packages installed afterward through APT, DNF, pacman/MSYS2, and Homebrew,
  including their transitive native dependencies and repository snapshots.
- Rust distribution/bootstrap content, Arch's compiler, Flatpak runtime/SDK/extensions, and native
  packagers and helper dependencies not already covered by existing exact pins.
- Bootstrap tools and transitive Python/npm dependencies where a top-level
  version or action commit does not identify every downloaded input.

Existing Cargo locks, action commits, component-policy hashes, and Windows
installer-tool pins remain separate controls. H2.2 completes only after the
remaining input inventory, enforceable pins or explicitly reviewed exceptions,
and drift rejection evidence are in place.
