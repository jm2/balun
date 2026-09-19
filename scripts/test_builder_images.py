#!/usr/bin/env python3
"""Prove unreviewed workflow image drift and inventory omissions fail closed."""

import json
import os
from pathlib import Path
import tempfile
import unittest

import check_builder_images as pins


class BuilderImageTests(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        (self.root / pins.WORKFLOWS).mkdir(parents=True)
        (self.root / pins.POLICY).parent.mkdir(parents=True)
        self.image = "example/builder:stable@sha256:" + "a" * 64
        self.policy = {"schema": 1, "workflows": {"ci.yml": {"build": self.image}}}
        self.write_policy()
        self.workflow = self.root / pins.WORKFLOWS / "ci.yml"
        self.write_workflow()

    def write_policy(self):
        (self.root / pins.POLICY).write_text(json.dumps(self.policy))

    def write_workflow(self, image=None, *, shorthand=False):
        container = image or self.image
        if not shorthand:
            container = {"image": container, "options": "--privileged"}
        self.workflow.write_text(json.dumps({"jobs": {"build": {"container": container}}}))

    def test_exact_digest_accepts_both_container_syntaxes(self):
        for shorthand in [False, True]:
            self.write_workflow(shorthand=shorthand)
            self.assertEqual(pins.check(self.root), 1)

    def test_tag_expression_and_changed_digest_or_registry_reject(self):
        for image in ["example/builder:stable", "${{ matrix.image }}",
                      self.image.replace("a" * 64, "b" * 64),
                      self.image.replace("example/", "another/")]:
            with self.subTest(image=image), self.assertRaises(pins.Invalid):
                self.write_workflow(image)
                pins.check(self.root)

    def test_inventory_cannot_authorize_an_unpinned_image(self):
        self.policy["workflows"]["ci.yml"]["build"] = "example/builder:stable"
        self.write_policy()
        self.write_workflow("example/builder:stable")
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)

    def test_new_workflow_or_job_container_requires_inventory(self):
        other = self.root / pins.WORKFLOWS / "other.yaml"
        other.write_text(self.workflow.read_text())
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)
        other.unlink()
        self.workflow.write_text(json.dumps({"jobs": {
            "build": {"container": self.image}, "extra": {"container": self.image}}}))
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)

    def test_removed_container_or_workflow_rejects_stale_inventory(self):
        self.workflow.write_text('jobs: {build: {runs-on: ubuntu-24.04}}\n')
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)
        self.workflow.unlink()
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)

    def test_duplicate_yaml_and_json_keys_reject(self):
        self.workflow.write_text(f'jobs:\n  build:\n    container: {self.image}\n    container: busybox\n')
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)
        self.write_workflow()
        policy = json.dumps(self.policy).replace('"schema": 1', '"schema": 1, "schema": 1')
        (self.root / pins.POLICY).write_text(policy)
        with self.assertRaises(pins.Invalid):
            pins.check(self.root)

    def test_invalid_schema_and_container_shapes_reject(self):
        for schema in [True, 2, "1"]:
            self.policy["schema"] = schema
            self.write_policy()
            with self.subTest(schema=schema), self.assertRaises(pins.Invalid):
                pins.check(self.root)
        self.policy["schema"] = 1
        self.write_policy()
        for container in [None, {}, [], {"image": 1}]:
            self.workflow.write_text(json.dumps({"jobs": {"build": {"container": container}}}))
            with self.subTest(container=container), self.assertRaises(pins.Invalid):
                pins.check(self.root)

    def test_explicitly_updated_inventory_accepts_reviewed_digest_change(self):
        replacement = self.image.replace("a" * 64, "b" * 64)
        self.policy["workflows"]["ci.yml"]["build"] = replacement
        self.write_policy()
        self.write_workflow(replacement)
        self.assertEqual(pins.check(self.root), 1)


if __name__ == "__main__":
    unittest.main()
