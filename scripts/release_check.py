#!/usr/bin/env python3
"""Check that a proposed release tag agrees with every version declaration.

The release-candidate workflow runs this on the tagged checkout before any
package is built. It reads only committed files and reports every mismatch at
once so a maintainer fixes them in one commit:

- the tag is a v-prefixed Semantic Version;
- ``Cargo.toml`` and ``Cargo.lock`` carry that version, while the Arch PKGBUILD
  carries its stable-upgrade-safe, hyphen-free ``pkgver`` encoding;
- ``CHANGELOG.md`` has a ``## [<version>]`` section and its compare link; and
- the AppStream metainfo lists that version as its newest ``<release>``.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

SEMVER_TAG = re.compile(
    r"^v(?P<version>(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?(\+[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?)$"
)
CARGO_MANIFEST = Path("Cargo.toml")
CARGO_LOCKFILE = Path("Cargo.lock")
CHANGELOG = Path("CHANGELOG.md")
METAINFO = Path("data/io.github.jm2.Balun.metainfo.xml")
ARCH_PKGBUILD = Path("build-aux/arch/PKGBUILD")
PACKAGE_NAME = "balun"


class ReleaseCheckError(Exception):
    """A file needed by the check is missing or unreadable."""


def read_text(root: Path, relative: Path) -> str:
    path = root / relative
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        raise ReleaseCheckError(f"{relative} could not be read: {error.strerror}") from error
    except UnicodeDecodeError as error:
        raise ReleaseCheckError(f"{relative} is not valid UTF-8") from error


def tag_version(tag: str) -> str | None:
    """Return the version named by a v-prefixed Semantic Version tag."""
    match = SEMVER_TAG.match(tag)
    return match.group("version") if match else None


def cargo_manifest_version(manifest: str) -> str | None:
    in_package = False
    for line in manifest.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.match(r'^version\s*=\s*"([^"]+)"\s*$', stripped)
            if match:
                return match.group(1)
    return None


def cargo_lock_version(lockfile: str, package: str) -> str | None:
    """Return the version of ``package`` from a Cargo lockfile."""
    for block in lockfile.split("[[package]]"):
        name = re.search(r'^name\s*=\s*"([^"]+)"\s*$', block, re.M)
        version = re.search(r'^version\s*=\s*"([^"]+)"\s*$', block, re.M)
        if name and version and name.group(1) == package:
            return version.group(1)
    return None


def arch_pkgbuild_version(pkgbuild: str) -> str | None:
    """Return the literal top-level ``pkgver`` from an Arch PKGBUILD."""
    match = re.search(r"^pkgver=([^\s#]+)\s*$", pkgbuild, re.M)
    return match.group(1) if match else None


def arch_pkgver_for_semver(version: str) -> str:
    """Encode a Semantic Version as a stable-upgrade-safe Arch ``pkgver``."""
    release, plus, build = version.partition("+")
    core, hyphen, prerelease = release.partition("-")
    encoded = core
    if hyphen:
        # vercmp sorts a letter suffix before the corresponding stable
        # version. Within that suffix, numeric identifiers must sort before
        # text identifiers to retain SemVer's identifier-type precedence.
        identifiers = (
            f"0.{identifier}"
            if identifier.isdigit()
            else f"1.{identifier.replace('-', '_')}"
            for identifier in prerelease.split(".")
        )
        encoded += "pre." + ".".join(identifiers)
    if plus:
        encoded += "+" + build.replace("-", "_")
    return encoded


def changelog_has_release(changelog: str, version: str) -> tuple[bool, bool]:
    """Whether the changelog has the version's section and its compare link."""
    section = re.search(rf"^## \[{re.escape(version)}\]", changelog, re.M) is not None
    link = re.search(rf"^\[{re.escape(version)}\]: \S+", changelog, re.M) is not None
    return section, link


def metainfo_newest_release(metainfo: str) -> str | None:
    match = re.search(r'<release\s[^>]*version="([^"]+)"', metainfo)
    return match.group(1) if match else None


def check_release(root: Path, tag: str) -> list[str]:
    """Return every mismatch between ``tag`` and the repository at ``root``."""
    version = tag_version(tag)
    if version is None:
        return [f"tag {tag!r} is not a v-prefixed Semantic Version"]

    problems: list[str] = []
    manifest_version = cargo_manifest_version(read_text(root, CARGO_MANIFEST))
    if manifest_version is None:
        problems.append(f"{CARGO_MANIFEST} has no [package] version")
    elif manifest_version != version:
        problems.append(f"{CARGO_MANIFEST} version {manifest_version} does not match tag {tag}")

    lock_version = cargo_lock_version(read_text(root, CARGO_LOCKFILE), PACKAGE_NAME)
    if lock_version is None:
        problems.append(f"{CARGO_LOCKFILE} has no {PACKAGE_NAME} package")
    elif lock_version != version:
        problems.append(f"{CARGO_LOCKFILE} records {PACKAGE_NAME} {lock_version}, not {version}")

    pkgbuild_version = arch_pkgbuild_version(read_text(root, ARCH_PKGBUILD))
    expected_pkgbuild_version = arch_pkgver_for_semver(version)
    if pkgbuild_version is None:
        problems.append(f"{ARCH_PKGBUILD} has no literal pkgver")
    elif pkgbuild_version != expected_pkgbuild_version:
        problems.append(
            f"{ARCH_PKGBUILD} pkgver is {pkgbuild_version}, "
            f"not {expected_pkgbuild_version} for {version}"
        )

    section, link = changelog_has_release(read_text(root, CHANGELOG), version)
    if not section:
        problems.append(f"{CHANGELOG} has no '## [{version}]' section")
    if not link:
        problems.append(f"{CHANGELOG} has no '[{version}]:' compare link")

    newest = metainfo_newest_release(read_text(root, METAINFO))
    if newest is None:
        problems.append(f"{METAINFO} lists no <release>")
    elif newest != version:
        problems.append(f"{METAINFO} newest release is {newest}, not {version}")

    return problems


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tag", required=True, help="proposed release tag, e.g. v0.1.0")
    parser.add_argument(
        "--repository",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="repository root (default: this checkout)",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        problems = check_release(args.repository, args.tag)
    except ReleaseCheckError as error:
        print(f"release check could not run: {error}", file=sys.stderr)
        return 2
    if problems:
        for problem in problems:
            print(f"release check: {problem}", file=sys.stderr)
        return 1
    print(f"release check: {args.tag} agrees with every version declaration")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
