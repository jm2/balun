#!/usr/bin/env python3
"""Compiler drift regressions for the release workflow's explicit Rust inputs."""

import copy
import os
from pathlib import Path
import tempfile
import unittest

import yaml

import check_release_rust as policy


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
        self.workflow = {"jobs": {
            job: {"steps": [{"uses": "dtolnay/rust-toolchain@" + "a" * 40,
                             "with": {"toolchain": "1.98.0"}}]}
            for job in policy.JOBS}}

    def write_workflow(self, value=None):
        """Serialize a fixture without sharing mutable nested job definitions."""
        (self.root / policy.WORKFLOW).write_text(yaml.safe_dump(self.workflow if value is None else value))

    def test_reviewed_release_and_separate_patch_update(self):
        """A reviewed patch release may advance without rewriting the MSRV floor."""
        self.write_workflow()
        self.assertEqual(policy.check(self.root), "1.98.0")
        manifest = self.root / policy.MANIFEST
        manifest.write_text(manifest.read_text().replace("1.98.0", "1.98.1"))
        with self.assertRaises(policy.Invalid):
            policy.check(self.root)
        for definition in self.workflow["jobs"].values():
            definition["steps"][0]["with"]["toolchain"] = "1.98.1"
        self.write_workflow()
        self.assertEqual(policy.check(self.root), "1.98.1")
        self.assertIn('rust-version = "1.98"', (self.root / "Cargo.toml").read_text())

    def test_changed_mutable_missing_and_expression_inputs_reject(self):
        """The action must receive exactly the recorded literal compiler release."""
        for version in ("stable", "nightly", "1.99.0", "1.98", "${{ env.RUST_VERSION }}", None, 1.98):
            with self.subTest(version=version):
                value = copy.deepcopy(self.workflow)
                options = value["jobs"]["macos"]["steps"][0]["with"]
                if version is None:
                    options.clear()
                else:
                    options["toolchain"] = version
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
                    value["jobs"]["build"]["steps"].clear()
                elif mutation == "extra-job":
                    value["jobs"]["new-native-job"] = copy.deepcopy(value["jobs"]["build"])
                else:
                    value["jobs"]["build"]["steps"] *= 2
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
        path.write_text(path.read_text().replace("toolchain: 1.98.0", "toolchain: stable\n        toolchain: 1.98.0", 1))
        with self.assertRaises(ValueError):
            policy.check(self.root)


if __name__ == "__main__":
    unittest.main()
