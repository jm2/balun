#!/usr/bin/env python3
"""Deterministic snapshot races and resource bounds, without native extractors."""

import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import archive_snapshot as snapshot


class SnapshotTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "input archive"
        self.output = self.root / "snapshot"
        self.source.write_bytes(b"original archive bytes")

    def rejected(self, **kwargs):
        with self.assertRaises((OSError, snapshot.SnapshotError)):
            snapshot.copy_snapshot(self.source, self.output, **kwargs)
        self.assertFalse(self.output.exists())

    def test_stable_input_is_frozen_with_private_permissions_and_short_writes(self):
        write = os.write
        with mock.patch.object(snapshot.os, "write", side_effect=lambda fd, data: write(fd, data[:3])):
            snapshot.copy_snapshot(self.source, self.output)
        self.assertEqual(self.output.read_bytes(), self.source.read_bytes())
        self.assertEqual(stat.S_IMODE(self.output.stat().st_mode), 0o600)
        self.assertEqual(self.output.stat().st_nlink, 1)
        self.source.write_bytes(b"later replacement")
        self.assertEqual(self.output.read_bytes(), b"original archive bytes")

    def test_empty_input_is_copied_for_the_native_format_validator_to_reject(self):
        self.source.write_bytes(b"")
        snapshot.copy_snapshot(self.source, self.output)
        self.assertEqual(self.output.read_bytes(), b"")

    def test_aliases_special_files_and_oversize_inputs_fail_before_output_creation(self):
        target = self.root / "target"
        self.source.rename(target)
        self.source.symlink_to(target)
        self.rejected()
        self.source.unlink()
        os.link(target, self.source)
        self.rejected()
        self.source.unlink()
        os.mkfifo(self.source)
        self.rejected()
        self.source.unlink()
        self.source.mkdir()
        self.rejected()
        self.source.rmdir()
        target.rename(self.source)
        self.rejected(limit=3)

    def test_opened_identity_must_match_the_preopen_file(self):
        original_open = os.open

        def replace_before_open(path, flags, *args):
            if Path(path) == self.source:
                replacement = self.root / "replacement"
                replacement.write_bytes(b"same sized replacement")
                replacement.replace(self.source)
            return original_open(path, flags, *args)

        with mock.patch.object(snapshot.os, "open", side_effect=replace_before_open):
            self.rejected()

    def test_replacement_growth_truncation_and_inplace_edits_fail_and_remove_partial_output(self):
        for change in ("replacement", "growth", "truncation", "same-size"):
            with self.subTest(change=change):
                self.source.write_bytes(b"original archive bytes")
                read = os.read
                changed = False

                def mutate_during_read(fd, count):
                    nonlocal changed
                    result = read(fd, count)
                    if not changed:
                        changed = True
                        if change == "replacement":
                            self.source.rename(self.root / "old")
                            self.source.write_bytes(b"original archive bytes")
                        elif change == "growth":
                            with self.source.open("ab") as file:
                                file.write(b"extra")
                        elif change == "truncation":
                            self.source.write_bytes(b"short")
                        else:
                            self.source.write_bytes(b"modified archive bytes")
                            value = self.source.stat()
                            os.utime(self.source, ns=(value.st_atime_ns, value.st_mtime_ns + 1_000_000))
                    return result

                with mock.patch.object(snapshot.os, "read", side_effect=mutate_during_read):
                    self.rejected(limit=len(b"original archive bytes"))

    def test_deadline_or_write_failure_removes_partial_output(self):
        with mock.patch.object(snapshot.time, "monotonic", side_effect=[0, 31]):
            self.rejected()
        with mock.patch.object(snapshot.os, "write", return_value=0):
            self.rejected()
        with mock.patch.object(snapshot.os, "write", side_effect=OSError("private path")):
            self.rejected()

    def test_existing_output_or_output_alias_is_never_overwritten_or_removed(self):
        self.output.write_bytes(b"keep")
        with self.assertRaises(FileExistsError):
            snapshot.copy_snapshot(self.source, self.output)
        self.assertEqual(self.output.read_bytes(), b"keep")
        self.output.unlink()
        self.output.symlink_to(self.source)
        with self.assertRaises(FileExistsError):
            snapshot.copy_snapshot(self.source, self.output)
        self.assertTrue(self.output.is_symlink())
        self.assertEqual(self.source.read_bytes(), b"original archive bytes")

    def test_cli_failure_is_fixed_and_leaves_no_output(self):
        self.source.unlink()
        result = subprocess.run([sys.executable, "-B", str(Path(snapshot.__file__)),
                                 "--input", str(self.source), "--output", str(self.output)],
                                capture_output=True, text=True, timeout=5, check=False)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "Linux archive snapshot rejected\n")
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
