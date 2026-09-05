#!/usr/bin/env python3
"""Deterministic tests for Balun's release version agreement check."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIRECTORY))

import release_check  # noqa: E402

VERSION = "0.1.0-alpha.1"
ARCH_VERSION = "0.1.0pre.1.alpha.0.1"
RPM_VERSION = "0.1.0~alpha.1"


def write_fixture(
    root: Path,
    *,
    manifest_version: str = VERSION,
    lock_version: str = VERSION,
    pkgbuild_version: str = ARCH_VERSION,
    spec_version: str = RPM_VERSION,
    changelog_version: str | None = VERSION,
    changelog_link: bool = True,
    metainfo_version: str | None = VERSION,
) -> None:
    (root / "Cargo.toml").write_text(
        "[workspace]\nmembers = []\n\n[package]\nname = \"balun\"\n"
        f'version = "{manifest_version}"\nedition = "2024"\n\n[dependencies]\nserde = "1"\n',
        encoding="utf-8",
    )
    (root / "Cargo.lock").write_text(
        'version = 4\n\n[[package]]\nname = "adw"\nversion = "0.9.0"\n\n'
        f'[[package]]\nname = "balun"\nversion = "{lock_version}"\n\n'
        '[[package]]\nname = "serde"\nversion = "1.0.0"\n',
        encoding="utf-8",
    )
    (root / "build-aux" / "arch").mkdir(parents=True)
    (root / "build-aux" / "arch" / "PKGBUILD").write_text(
        f"pkgname=balun\npkgver={pkgbuild_version}\npkgrel=1\n",
        encoding="utf-8",
    )
    (root / "build-aux" / "rpm").mkdir(parents=True)
    (root / "build-aux" / "rpm" / "balun.spec").write_text(
        f"Name:           balun\nVersion:        {spec_version}\nRelease:        1%{{?dist}}\n",
        encoding="utf-8",
    )
    changelog = "# Changelog\n\n## [Unreleased]\n\n"
    if changelog_version is not None:
        changelog += f"## [{changelog_version}] — 2026-09-02\n\n### Added\n\n- **Thing** — text.\n\n"
    if changelog_version is not None and changelog_link:
        changelog += f"[{changelog_version}]: https://example.invalid/compare/{changelog_version}\n"
    (root / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    (root / "data").mkdir(exist_ok=True)
    releases = ""
    if metainfo_version is not None:
        releases = (
            f'    <release version="{metainfo_version}" type="development" date="2026-09-02">\n'
            "      <description><p>Snapshot.</p></description>\n    </release>\n"
            '    <release version="0.0.1" date="2026-01-01" />\n'
        )
    (root / "data" / "io.github.jm2.Balun.metainfo.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n<component type="desktop-application">\n'
        f"  <id>io.github.jm2.Balun</id>\n  <releases>\n{releases}  </releases>\n</component>\n",
        encoding="utf-8",
    )


class ReleaseCheckTests(unittest.TestCase):
    def test_tag_versions_are_semver_with_v_prefix(self) -> None:
        self.assertEqual(release_check.tag_version("v0.1.0-alpha.1"), "0.1.0-alpha.1")
        self.assertEqual(release_check.tag_version("v1.2.3"), "1.2.3")
        for tag in ["0.1.0", "v01.0.0", "v1.0", "v1.0.0-", "release-1", ""]:
            with self.subTest(tag=tag):
                self.assertIsNone(release_check.tag_version(tag))

    def test_agreeing_repository_passes(self) -> None:
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_fixture(root)
            self.assertEqual(release_check.check_release(root, f"v{VERSION}"), [])
            self.assertEqual(release_check.main(["--tag", f"v{VERSION}", "--repository", temporary]), 0)

    def test_every_disagreement_is_reported_at_once(self) -> None:
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_fixture(
                root,
                manifest_version="0.1.0-alpha.2",
                lock_version="0.1.0",
                pkgbuild_version="0.1.0_alpha.3",
                spec_version="0.1.0",
                changelog_version="0.1.0-alpha.2",
                metainfo_version="0.1.0",
            )
            problems = release_check.check_release(root, f"v{VERSION}")
            self.assertEqual(len(problems), 7, problems)
            self.assertTrue(any("Cargo.toml version 0.1.0-alpha.2" in p for p in problems))
            self.assertTrue(any("Cargo.lock records balun 0.1.0" in p for p in problems))
            self.assertTrue(any("PKGBUILD pkgver is 0.1.0_alpha.3" in p for p in problems))
            self.assertTrue(any("balun.spec Version is 0.1.0, not 0.1.0~alpha.1" in p for p in problems))
            self.assertTrue(any("no '## [0.1.0-alpha.1]' section" in p for p in problems))
            self.assertTrue(any("no '[0.1.0-alpha.1]:' compare link" in p for p in problems))
            self.assertTrue(any("newest release is 0.1.0" in p for p in problems))
            self.assertEqual(release_check.main(["--tag", f"v{VERSION}", "--repository", temporary]), 1)

    def test_missing_changelog_link_and_missing_metainfo_release_fail(self) -> None:
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_fixture(root, changelog_link=False, metainfo_version=None)
            problems = release_check.check_release(root, f"v{VERSION}")
            self.assertEqual(
                problems,
                [
                    f"CHANGELOG.md has no '[{VERSION}]:' compare link",
                    "data/io.github.jm2.Balun.metainfo.xml lists no <release>",
                ],
            )

    def test_invalid_tag_fails_before_reading_files(self) -> None:
        with TemporaryDirectory() as temporary:
            problems = release_check.check_release(Path(temporary), "0.1.0")
            self.assertEqual(problems, ["tag '0.1.0' is not a v-prefixed Semantic Version"])

    def test_missing_files_are_a_setup_error(self) -> None:
        with TemporaryDirectory() as temporary:
            self.assertEqual(release_check.main(["--tag", f"v{VERSION}", "--repository", temporary]), 2)

    def test_lock_version_ignores_other_packages(self) -> None:
        lockfile = '[[package]]\nname = "balun-helper"\nversion = "9.9.9"\n\n[[package]]\nname = "balun"\nversion = "1.2.3"\n'
        self.assertEqual(release_check.cargo_lock_version(lockfile, "balun"), "1.2.3")
        self.assertIsNone(release_check.cargo_lock_version(lockfile, "other"))

    def test_manifest_version_comes_only_from_the_package_table(self) -> None:
        manifest = '[workspace]\nversion = "0.0.0"\n\n[package]\nname = "balun"\nversion = "1.2.3"\n\n[dependencies.x]\nversion = "5"\n'
        self.assertEqual(release_check.cargo_manifest_version(manifest), "1.2.3")
        self.assertIsNone(release_check.cargo_manifest_version("[dependencies]\nserde = \"1\"\n"))

    def test_pkgbuild_version_must_be_a_literal_top_level_assignment(self) -> None:
        self.assertEqual(
            release_check.arch_pkgbuild_version("pkgname=balun\npkgver=1.2.3\npkgrel=1\n"),
            "1.2.3",
        )
        self.assertIsNone(release_check.arch_pkgbuild_version("_pkgver=1.2.3\n"))
        self.assertIsNone(release_check.arch_pkgbuild_version('pkgver="1.2.3" # generated\n'))

    def test_rpm_spec_version_must_be_a_literal_declaration(self) -> None:
        self.assertEqual(release_check.rpm_spec_version("Name: balun\nVersion:        1.2.3\n"), "1.2.3")
        self.assertIsNone(release_check.rpm_spec_version("Version: %{upstream_version} # macro\n"))
        self.assertIsNone(release_check.rpm_spec_version("# Version: 1.2.3\n"))

    def test_rpm_version_encodes_prereleases_with_a_tilde(self) -> None:
        self.assertEqual(release_check.rpm_version_for_semver("1.2.3"), "1.2.3")
        self.assertEqual(release_check.rpm_version_for_semver("1.2.3-alpha.1"), "1.2.3~alpha.1")
        with self.assertRaises(ValueError):
            release_check.rpm_version_for_semver("1.2.3+build")

    def test_arch_pkgver_uses_a_stable_upgrade_safe_prerelease_marker(self) -> None:
        self.assertEqual(release_check.arch_pkgver_for_semver("1.2.3"), "1.2.3")
        self.assertEqual(
            release_check.arch_pkgver_for_semver("1.2.3-alpha.beta.1+build-two"),
            "1.2.3pre.1.alpha.1.beta.0.1+build_two",
        )
        self.assertEqual(
            release_check.arch_pkgver_for_semver("1.2.3-alpha"),
            "1.2.3pre.1.alpha",
        )
        self.assertEqual(
            release_check.arch_pkgver_for_semver("1.2.3-1"),
            "1.2.3pre.0.1",
        )
        self.assertEqual(
            release_check.arch_pkgver_for_semver("1.2.3+build-two"),
            "1.2.3+build_two",
        )

    def test_arch_pkgver_refuses_unorderable_prerelease_identifiers(self) -> None:
        # SemVer orders 1.2.3-alpha.a before 1.2.3-alpha-a and 1.2.3-alpha10
        # before 1.2.3-alpha2, but Arch treats the hyphen's only substitute as
        # a separator and compares digit runs numerically, so neither identifier
        # can keep its order and such tags are refused instead.
        for version in ("1.2.3-alpha-a", "1.2.3-alpha10", "1.2.3-rc1.x"):
            with self.assertRaises(ValueError, msg=version):
                release_check.arch_pkgver_for_semver(version)
        with TemporaryDirectory() as temporary:
            root = Path(temporary)
            write_fixture(
                root,
                manifest_version="1.2.3-alpha-a",
                lock_version="1.2.3-alpha-a",
                changelog_version="1.2.3-alpha-a",
                metainfo_version="1.2.3-alpha-a",
            )
            problems = release_check.check_release(root, "v1.2.3-alpha-a")
            self.assertEqual(len(problems), 1, problems)
            self.assertIn("no Arch pkgver encoding", problems[0])
            self.assertIn("'alpha-a'", problems[0])


if __name__ == "__main__":
    unittest.main()
