#!/usr/bin/env python3
"""Native copy ownership and post-relocation membership regressions."""

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

import native_copy_ledger as copies


class NativeCopyLedgerTests(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("TMPDIR") or (None if os.name == "nt" else "/var/tmp")
        work = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.cellar = self.root / "Cellar"
        self.source = self.cellar / "fixture" / "1.2_3" / "lib" / "library.dylib"
        self.source.parent.mkdir(parents=True)
        self.source.write_bytes(bytes.fromhex("cffaedfe") + b"synthetic native bytes")
        self.tree = self.root / "Balun.app"
        self.destination = self.tree / "Contents" / "Frameworks" / "library.dylib"
        self.destination.parent.mkdir(parents=True)
        shutil.copyfile(self.source, self.destination)
        self.path = self.root / "native-copies.json"
        self.ledger = copies.read_ledger(self.path, missing=True)
        self.deadline = time.monotonic() + 10

    def record(self, source=None, destination=None, *, project=False):
        copies.record_copy(self.ledger, self.tree, source or self.source,
                           destination or self.destination, self.cellar, self.deadline, project=project)

    def freeze(self):
        return copies.freeze(self.ledger, self.tree, self.deadline)

    def test_copies_retain_actual_owner_and_both_content_identities(self):
        self.record()
        self.destination.write_bytes(bytes.fromhex("cffaedfe") + b"relocated and signed native bytes")
        frozen = self.freeze()
        record = frozen["records"][0]
        self.assertEqual(record["origin"], {"kind": "homebrew", "member": "fixture/1.2_3/lib/library.dylib"})
        self.assertEqual(record["path"], "Contents/Frameworks/library.dylib")
        self.assertEqual(record["copied"]["sha256"], hashlib.sha256(self.source.read_bytes()).hexdigest())
        self.assertEqual(record["final"]["sha256"], hashlib.sha256(self.destination.read_bytes()).hexdigest())
        self.assertNotIn(str(self.root), json.dumps(frozen))
        copies.write_ledger(self.path, frozen)
        with self.assertRaises(copies.Invalid):
            copies.read_ledger(self.path)

    def test_source_mismatch_unowned_and_duplicate_copies_fail(self):
        self.destination.write_bytes(b"MZdifferent input")
        with self.assertRaisesRegex(copies.Invalid, "do not match"):
            self.record()
        shutil.copyfile(self.source, self.destination)
        outside = self.root / "unowned.dylib"
        shutil.copyfile(self.source, outside)
        with self.assertRaisesRegex(copies.Invalid, "outside"):
            self.record(source=outside)
        self.record()
        with self.assertRaisesRegex(copies.Invalid, "already recorded"):
            self.record()
        with self.assertRaises(copies.Invalid):
            self.record(destination=outside)

    def test_unknown_missing_and_changed_final_members_fail(self):
        self.record()
        extra = self.destination.with_name("unknown-scanner")
        shutil.copyfile(self.source, extra)
        with self.assertRaisesRegex(copies.Invalid, "unknown or missing"):
            self.freeze()
        extra.unlink()
        self.destination.unlink()
        with self.assertRaisesRegex(copies.Invalid, "unknown or missing"):
            self.freeze()
        shutil.copyfile(self.source, self.destination)
        snapshot = copies.snapshot_file
        def mutate_after_snapshot(path, *args, **kwargs):
            result = snapshot(path, *args, **kwargs)
            if path == self.destination:
                path.write_bytes(path.read_bytes() + b"changed")
            return result
        with patch.object(copies, "snapshot_file", mutate_after_snapshot), self.assertRaises(copies.Invalid):
            self.freeze()

    def test_recursive_copies_record_only_native_members(self):
        note = self.source.parent / "cache.txt"
        note.write_text("resource without native code")
        shutil.copyfile(note, self.destination.parent / note.name)
        copies.record_tree(self.ledger, self.tree, self.source.parent, self.destination.parent,
                           self.cellar, self.deadline)
        self.assertEqual(len(self.ledger["records"]), 1)
        self.assertEqual(len(self.freeze()["records"]), 1)

    @unittest.skipIf(os.name == "nt", "Unix source-prefix symlink fixture")
    def test_expected_installed_links_resolve_but_staged_aliases_fail(self):
        installed_link = self.root / "opt-library"
        installed_link.symlink_to(self.source)
        self.record(source=installed_link)
        self.assertEqual(self.ledger["records"][0]["origin"]["member"], "fixture/1.2_3/lib/library.dylib")
        self.destination.unlink()
        self.destination.symlink_to(self.source)
        with self.assertRaises(copies.Invalid):
            self.freeze()

    def test_project_origin_requires_explicit_selection(self):
        project = self.root / "balun"
        shutil.copyfile(self.source, project)
        self.record(source=project, project=True)
        self.assertEqual(self.freeze()["records"][0]["origin"], {"kind": "project", "member": "balun"})

    def test_record_schema_and_budgets_reject_before_output(self):
        self.record()
        copies.write_ledger(self.path, self.ledger)
        self.assertEqual(copies.read_ledger(self.path), self.ledger)
        bad = copy.deepcopy(self.ledger)
        bad["records"][0]["origin"]["kind"] = []
        self.path.write_text(json.dumps(bad))
        with self.assertRaises(copies.Invalid):
            copies.read_ledger(self.path)
        bad = copy.deepcopy(self.ledger)
        bad["records"].append(bad["records"][0])
        self.path.write_text(json.dumps(bad))
        with self.assertRaises(copies.Invalid):
            copies.read_ledger(self.path)
        with patch.object(copies, "MAX_DOCUMENT", 1), self.assertRaises(copies.Invalid):
            copies.write_ledger(self.path, self.ledger)
        with patch.object(copies, "MAX_NATIVE_BYTES", 1), self.assertRaises(copies.Invalid):
            self.freeze()
        with patch.object(copies, "MAX_FILES", 0), self.assertRaises(copies.Invalid):
            self.record()

    def test_cli_freezes_only_complete_copies_and_reports_fixed_errors(self):
        command = [sys.executable, "-B", copies.__file__]
        common = ["--ledger", str(self.path), "--tree", str(self.tree)]
        subprocess.run(command + ["record", *common, "--source", str(self.source),
                       "--destination", str(self.destination), "--cellar", str(self.cellar)],
                       check=True, timeout=10, capture_output=True)
        subprocess.run(command + ["freeze", *common], check=True, timeout=10, capture_output=True)
        self.assertEqual(json.loads(self.path.read_text())["state"], "frozen")
        run = subprocess.run(command + ["record", *common, "--source", "secret-marker-123"],
                             capture_output=True, text=True, timeout=10)
        self.assertEqual(run.returncode, 1)
        self.assertEqual(run.stdout, "")
        self.assertNotIn("secret-marker-123", run.stderr)
        self.assertNotIn("Traceback", run.stderr)


if __name__ == "__main__":
    unittest.main()
