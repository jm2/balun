# Flatpak generator dependency lock

H2.2 is deferred (maintainer direction, 2026-09-24). The CI and release Flatpak
jobs install the vendored Cargo source generator's complete default Python
dependency closure from
[`generator-lock-requirements.txt`](../build-aux/flatpak/generator-lock-requirements.txt).
The existing direct pins, aiohttp 3.14.3 and tomlkit 0.15.1, are unchanged.
Their separate requirements file remains the generator helper's direct-version
contract and is also loaded as a constraint by the complete lock. Changing a
direct version without a matching lock update fails resolution.

## Installation boundary

The lock permits 36 reviewed wheel files for Linux CPython 3.12–3.14 on x86_64
and aarch64. There are ten default runtime packages on Python 3.13–3.14 and
eleven on Python 3.12, where aiohttp and aiosignal require typing-extensions.
Optional extras, source distributions, other interpreters, and other platforms
are outside this lock. New supported wheels need a reviewed hash update.

Each job creates a fresh virtual environment without pip, then uses the host
pip to install into that environment with `--require-hashes --only-binary=:all:`.
The generator step explicitly selects that environment through `PYTHON`.
Both steps unset `PYTHONPATH` and `PYTHONHOME` first: the pinned Flathub CI
image exports `PYTHONPATH=/app/lib/python3.13/site-packages`, whose older
aiohttp, tomlkit, and transitive packages would otherwise shadow the locked wheels.
No system package override or unpinned installation fallback is used.
The host pip must support
[`--python`, added in pip 22.3](https://pip.pypa.io/en/stable/topics/python-option/).
Python, pip, the virtual-environment implementation, and the hosted/container
base remain separate H2.2 inputs; this lock does not make the full build immutable.

From the repository root on a supported Linux interpreter with pip available
and `PYTHONPATH`/`PYTHONHOME` unset:

```sh
generator_work=$(mktemp -d -p "${TMPDIR:-/var/tmp}" balun-generator.XXXXXX)
python3 -m venv --without-pip "$generator_work/python"
PIP_CACHE_DIR="$generator_work/pip-cache" python3 -m pip \
  --python "$generator_work/python/bin/python" install \
  --require-hashes --only-binary=:all: \
  --requirement build-aux/flatpak/generator-lock-requirements.txt
python3 -m pip --python "$generator_work/python/bin/python" check
PYTHON="$generator_work/python/bin/python" bash build-aux/flatpak/generate-cargo-sources.sh
rm -rf -- "$generator_work"
```

The helper verifies the vendored generator checksum and direct package versions
before producing the ignored `build-aux/flatpak/cargo-sources.json` build input.
It continues to support independently supplied environments; only the CI and
release installation path enforces this complete wheel lock.

## Validation and updates

The initial wheel set was downloaded from exact-version PyPI metadata on
2026-09-19 and independently SHA-256 checked. Every wheel's default
`Requires-Dist` markers and version bounds were checked for all six target
interpreter/architecture pairs. The Python 3.12-only typing-extensions dependency
is explicit: cross-target pip downloads on a newer host alone do not prove that
older-interpreter markers were included.

Local execution used CPython 3.14 and the private environment above: installation,
`pip check`, and the real generator helper against Balun's Cargo.lock passed.
Other wheel sets were checked for compatibility and hash-verified offline
resolution; that is not execution evidence for those interpreters or architectures.
Negative checks reject changed wheel hashes, omitted transitive pins, and
conflicting direct-version constraints. An OSV query for the eleven locked
package versions returned no known advisories on that date; it does not predict
future advisories or replace ongoing dependency review.

For an update, resolve exact versions, inspect wheel metadata for every supported
Python version, and review the complete transitive closure. Download and hash the
selected wheel files, update filename comments and hashes together, and retain
the direct constraints. Repeat fresh installation, generation, negative checks,
and native CI/package builds. Leave dependency resolution enabled so an omitted
new requirement fails instead of silently disappearing. Signing and provenance
work remain deferred.
