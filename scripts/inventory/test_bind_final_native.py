#!/usr/bin/env python3
"""Final payload comparisons use a copy ledger prepared before reopening."""

import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import bind_final_native as final
from native_copy_ledger import freeze, read_ledger, record_copy, write_ledger
from native_inventory import Invalid


class FinalNativeBindingTests(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("TMPDIR") or (None if os.name == "nt" else "/var/tmp")
        work = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.staged = self.root / "staged"
        self.staged.mkdir()
        self.source = self.root / "Cellar/fixture/1.0/bin/scanner"
        self.source.parent.mkdir(parents=True)
        self.source.write_bytes(bytes.fromhex("cffaedfe") + b"native source fixture")
        self.member = self.staged / "scanner"
        shutil.copyfile(self.source, self.member)
        self.ledger = self.root / "copies.json"
        ledger = read_ledger(self.ledger, missing=True)
        deadline = time.monotonic() + 10
        record_copy(ledger, self.staged, self.source, self.member, self.root / "Cellar", deadline)
        self.member.write_bytes(bytes.fromhex("cffaedfe") + b"final relocated fixture")
        write_ledger(self.ledger, freeze(ledger, self.staged, deadline))
        self.reopened = self.root / "reopened"
        shutil.copytree(self.staged, self.reopened)
        # No extractor is exercised: the native adapter proves this relationship.
        self.artifact = self.root / "Balun.dmg"
        self.artifact.write_bytes(b"inert completed artifact fixture")

    def bind(self):
        return final.bind(self.reopened, self.artifact, "macos-aarch64", "0.1.1", self.ledger)

    def test_final_members_and_completed_artifact_hash_are_bound(self):
        result = self.bind()
        self.assertEqual(result["artifact"]["sha256"], hashlib.sha256(self.artifact.read_bytes()).hexdigest())
        record = read_ledger(self.ledger, frozen=True)["records"][0]
        self.assertEqual(result["native_files"], [{"path": "scanner", **record["final"]}])
        self.assertNotEqual(record["copied"], record["final"])
        # The independent observation depends on the frozen ledger, not the
        # original staged app or installed input continuing to exist.
        shutil.rmtree(self.staged)
        self.source.unlink()
        self.assertEqual(self.bind(), result)
        # Report naming follows the helper's native Rust target, including
        # local Intel builds; this does not add an official release platform.
        intel = final.bind(self.reopened, self.artifact, "macos-x86_64", "0.1.1", self.ledger)
        self.assertEqual(intel["platform"], "macos-x86_64")

    def test_changed_unknown_and_missing_reopened_members_reject(self):
        member = self.reopened / "scanner"
        member.write_bytes(bytes.fromhex("cffaedfe") + b"different native bytes")
        with self.assertRaisesRegex(Invalid, "differ"):
            self.bind()
        shutil.copyfile(self.member, member)
        extra = self.reopened / "unknown-helper"
        shutil.copyfile(self.member, extra)
        with self.assertRaisesRegex(Invalid, "differ"):
            self.bind()
        member.unlink()
        with self.assertRaisesRegex(Invalid, "differ"):
            self.bind()

    def test_source_hash_cannot_replace_final_snapshot(self):
        shutil.copyfile(self.source, self.reopened / "scanner")
        with self.assertRaisesRegex(Invalid, "differ"):
            self.bind()

    def test_ledger_must_be_frozen_external_and_unchanged(self):
        frozen = read_ledger(self.ledger, frozen=True)
        collecting = copy.deepcopy(frozen)
        collecting["state"] = "collecting"
        del collecting["records"][0]["final"]
        write_ledger(self.ledger, collecting)
        with self.assertRaises(Invalid):
            self.bind()
        write_ledger(self.ledger, frozen)
        interior = self.reopened / "copies.json"
        shutil.copyfile(self.ledger, interior)
        with self.assertRaisesRegex(Invalid, "outside"):
            final.bind(self.reopened, self.artifact, "macos-aarch64", "0.1.1", interior)
        interior.unlink()
        observe = final.observe
        def change_ledger(*args):
            result = observe(*args)
            changed = copy.deepcopy(frozen)
            changed["records"][0]["origin"]["member"] = "different/1.0/bin/scanner"
            write_ledger(self.ledger, changed)
            return result
        with patch.object(final, "observe", change_ledger), self.assertRaisesRegex(Invalid, "changed"):
            self.bind()

    def test_cli_emits_only_complete_reports_and_fixed_errors(self):
        command = [sys.executable, "-B", final.__file__, "--tree", str(self.reopened),
                   "--artifact", str(self.artifact), "--copy-ledger", str(self.ledger),
                   "--platform", "macos-aarch64", "--version", "0.1.1"]
        run = subprocess.run(command, check=True, capture_output=True, text=True, timeout=10)
        self.assertEqual(json.loads(run.stdout), self.bind())
        (self.reopened / "secret-marker-scanner").write_bytes(b"MZextra native bytes")
        run = subprocess.run(command, capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 1)
        self.assertEqual(run.stdout, "")
        self.assertNotIn("secret-marker", run.stderr)
        self.assertNotIn("Traceback", run.stderr)


if __name__ == "__main__":
    unittest.main()
