#!/usr/bin/env python3
"""Compiler drift regressions for the release workflow's manifest-read Rust inputs."""

import copy
import os
from pathlib import Path
import tempfile
import unittest

import yaml

import check_release_rust as policy


def read_step():
    """The reviewed manifest read a release job runs before installing Rust."""
    return {"name": "Read the reviewed release compiler", "id": policy.READ_ID,
            "shell": "bash", "run": policy.READ_SCRIPT}


def install_step():
    """The Rust action receiving the read step's output."""
    return {"uses": "dtolnay/rust-toolchain@" + "a" * 40,
            "with": {"toolchain": policy.TOOLCHAIN_INPUT}}


class ReleaseRustTests(unittest.TestCase):
    """Exercise independent mutations of a complete reviewed release-job fixture."""

    def setUp(self):
        """Keep temporary policy files on disk-backed or platform-owned scratch."""
        scratch = os.environ.get("TMPDIR") or (None if os.name == "nt" else "/var/tmp")
        self.directory = tempfile.TemporaryDirectory(prefix="balun-release-rust-", dir=scratch)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for relative in (policy.MANIFEST, policy.WORKFLOW):
            (self.root / relative).parent.mkdir(parents=True, exist_ok=True)
        (self.root / policy.MANIFEST).write_text('[toolchain]\nchannel = "1.98.0"\nprofile = "minimal"\n')
        (self.root / "Cargo.toml").write_text('[package]\nrust-version = "1.98"\n')
        self.workflow = {"jobs": {job: {"steps": [read_step(), install_step()]}
                                  for job in policy.JOBS}}

    def write_workflow(self, value=None):
        """Serialize a fixture without sharing mutable nested job definitions."""
        (self.root / policy.WORKFLOW).write_text(yaml.safe_dump(self.workflow if value is None else value))

    def test_manifest_alone_advances_the_release(self):
        """A Dependabot-style manifest bump needs no workflow or MSRV edit."""
        self.write_workflow()
        self.assertEqual(policy.check(self.root), "1.98.0")
        manifest = self.root / policy.MANIFEST
        for release in ("1.98.1", "1.99.0"):
            manifest.write_text(f'[toolchain]\nchannel = "{release}"\nprofile = "minimal"\n')
            self.assertEqual(policy.check(self.root), release)
        self.assertIn('rust-version = "1.98"', (self.root / "Cargo.toml").read_text())

    def test_literal_mutable_missing_and_other_expression_inputs_reject(self):
        """The action must receive exactly the manifest read step's output."""
        for version in ("1.98.0", "stable", "nightly", "${{ env.RUST_VERSION }}",
                        "${{ steps.other.outputs.toolchain }}", None, 1.98):
            with self.subTest(version=version):
                value = copy.deepcopy(self.workflow)
                options = value["jobs"]["macos"]["steps"][1]["with"]
                if version is None:
                    options.clear()
                else:
                    options["toolchain"] = version
                self.write_workflow(value)
                with self.assertRaises(policy.Invalid):
                    policy.check(self.root)

    def test_missing_late_changed_or_duplicate_reads_reject(self):
        """The install must follow one exact reviewed manifest read in its own job."""
        for mutation in ("missing", "after-install", "script", "shell", "duplicate", "stray"):
            with self.subTest(mutation=mutation):
                value = copy.deepcopy(self.workflow)
                steps = value["jobs"]["windows"]["steps"]
                if mutation == "missing":
                    del steps[0]
                elif mutation == "after-install":
                    steps.reverse()
                elif mutation == "script":
                    steps[0]["run"] = 'echo "toolchain=stable" >> "$GITHUB_OUTPUT"\n'
                elif mutation == "shell":
                    steps[0]["shell"] = "pwsh"
                elif mutation == "duplicate":
                    steps.insert(0, read_step())
                else:
                    value["jobs"]["unrecorded"] = {"steps": [read_step()]}
                self.write_workflow(value)
                with self.assertRaises(policy.Invalid):
                    policy.check(self.root)

    def test_removed_added_or_duplicate_installs_reject(self):
        """Job membership cannot silently lose a pin or gain an unchecked install."""
        for mutation in ("missing-job", "missing-action", "extra-job", "duplicate-action"):
            with self.subTest(mutation=mutation):
                value = copy.deepcopy(self.workflow)
                if mutation == "missing-job":
                    del value["jobs"]["build"]
                elif mutation == "missing-action":
                    del value["jobs"]["build"]["steps"][1]
                elif mutation == "extra-job":
                    value["jobs"]["new-native-job"] = copy.deepcopy(value["jobs"]["build"])
                else:
                    value["jobs"]["build"]["steps"].append(install_step())
                self.write_workflow(value)
                with self.assertRaises(policy.Invalid):
                    policy.check(self.root)

    def test_manifest_format_and_minimum_are_enforced(self):
        """Floating, malformed, and below-floor selections cannot become release pins."""
        self.write_workflow()
        for version in ("stable", "1.98", "1.98.0-beta.1", "01.98.0", "1.097.0", "1.97.9"):
            with self.subTest(version=version):
                (self.root / policy.MANIFEST).write_text(
                    f'[toolchain]\nchannel = "{version}"\nprofile = "minimal"\n')
                with self.assertRaises(policy.Invalid):
                    policy.check(self.root)

    def test_malformed_and_duplicate_workflow_fields_reject(self):
        """Invalid shapes and ambiguous duplicate keys cannot hide changed inputs."""
        for value in (None, [], {"jobs": []}, {"jobs": {"build": {"steps": "not a list"}}}):
            (self.root / policy.WORKFLOW).write_text(yaml.safe_dump(value))
            with self.assertRaises(policy.Invalid):
                policy.check(self.root)
        self.write_workflow()
        path = self.root / policy.WORKFLOW
        lines = path.read_text().splitlines(keepends=True)
        index = next(i for i, line in enumerate(lines) if line.strip() == "shell: bash")
        lines.insert(index, lines[index].replace("bash", "pwsh"))
        path.write_text("".join(lines))
        with self.assertRaises(ValueError):
            policy.check(self.root)

    def test_the_release_workflow_passes(self):
        """The repository's own release workflow reads the manifest in every job."""
        root = Path(__file__).resolve().parent.parent
        self.assertEqual(policy.check(root), policy.selection(root))


if __name__ == "__main__":
    unittest.main()
