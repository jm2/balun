#!/usr/bin/env python3
"""Independent filesystem observation with synthetic, never-executed payloads."""

import copy
from contextlib import contextmanager
import hashlib
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import native_inventory as inventory
import observe_native as observer
from test_native_inventory import fixtures


class ObservationTests(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("TMPDIR") or (None if os.name == "nt" else "/var/tmp")
        temporary = tempfile.TemporaryDirectory(dir=scratch)
        self.addCleanup(temporary.cleanup)
        self.parent = Path(temporary.name)
        self.tree = self.parent / "reopened tree"
        self.tree.mkdir()
        self.artifact = self.parent / "balun.zip"
        self.artifact.write_bytes(b"synthetic final artifact; never extracted or executed")
        self.native = self.write("bin/reader", b"MZ" + b"synthetic PE prefix" * 9)
        self.write("share/readme.txt", b"ordinary data")

    def write(self, name, content):
        path = self.tree / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        return path

    def observe(self):
        return observer.observe(self.tree, self.artifact, "windows-x86_64", "0.1.1")

    def test_every_content_class_and_extensionless_helper_is_hashed(self):
        paths = [self.native]
        for index, magic in enumerate(sorted(observer.MACHO) + [b"\x7fELF"]):
            paths.append(self.write(f"lib/opaque-{index}", magic + b"synthetic native content"))
        self.write("empty-resource", b"")
        self.write("share/.hidden-note", b"data")
        result = self.observe()
        expected = [{"path": path.relative_to(self.tree).as_posix(), "size": path.stat().st_size,
                     "sha256": hashlib.sha256(path.read_bytes()).hexdigest()} for path in paths]
        self.assertEqual(result["native_files"], sorted(expected, key=lambda member: member["path"]))
        self.assertEqual(result["artifact"]["sha256"], hashlib.sha256(self.artifact.read_bytes()).hexdigest())
        self.assertEqual(result["artifact"]["size"], self.artifact.stat().st_size)
        self.assertEqual(result, self.observe())

    def test_incomplete_windows_directory_cache_cannot_replace_file_identity(self):
        expected = self.observe()
        original_scandir = os.scandir

        class CachedEntry:
            def __init__(self, entry):
                self.name, self.path = entry.name, entry.path
                metadata = entry.stat(follow_symlinks=False)
                self.metadata = SimpleNamespace(**{key: getattr(metadata, key) for key in dir(metadata)
                                                   if key.startswith("st_")})
                self.metadata.st_ino = self.metadata.st_dev = self.metadata.st_nlink = 0

            def stat(self, *, follow_symlinks):
                return self.metadata

        @contextmanager
        def windows_cached_entries(path):
            with original_scandir(path) as entries:
                yield [CachedEntry(entry) for entry in entries]

        with patch.object(observer.os, "scandir", windows_cached_entries):
            self.assertEqual(self.observe(), expected)
            os.link(self.native, self.tree / "hard-link")
            with self.assertRaisesRegex(inventory.Invalid, "hard-linked"):
                self.observe()

    def test_observed_manifest_joins_real_file_bytes_to_separate_ownership_records(self):
        observed = self.observe()
        _, original = fixtures()
        catalog = {"schema": 1, "components": [copy.deepcopy(original["components"][0])],
                   "external_components": [],
                   "native_files": [{**member, "component": "runtime"}
                                    for member in observed["native_files"]]}
        inventory.assemble(observed, catalog)
        self.native.write_bytes(b"MZchanged runtime payload")
        with self.assertRaisesRegex(inventory.Invalid, "content differs"):
            inventory.assemble(self.observe(), catalog)
        self.write("plugins/new-decoder", b"\x7fELFunknown final member")
        with self.assertRaisesRegex(inventory.Invalid, "unknown or missing"):
            inventory.assemble(self.observe(), catalog)

    def test_aliases_and_special_files_are_refused(self):
        alias = self.tree / "alias"
        os.link(self.native, alias)
        with self.assertRaises(inventory.Invalid):
            self.observe()
        alias.unlink()
        if os.name != "nt":
            alias.symlink_to(self.parent, target_is_directory=True)
            with self.assertRaises(inventory.Invalid):
                self.observe()
            alias.unlink()
            os.mkfifo(alias)
            with self.assertRaises(inventory.Invalid):
                self.observe()

    @unittest.skipUnless(os.name == "nt", "native Windows junction fixture")
    def test_windows_junction_is_refused(self):
        target = self.parent / "outside"
        target.mkdir()
        link = self.tree / "junction"
        result = subprocess.run(["cmd.exe", "/D", "/C", "mklink", "/J", str(link), str(target)],
                                capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, "native junction fixture creation failed")
        with self.assertRaises(inventory.Invalid):
            self.observe()

    def test_replacement_before_open_fails_before_hashing_the_new_file(self):
        expected = self.native.lstat()
        replacement = self.parent / "replacement"
        replacement.write_bytes(b"MZsecret replacement")
        os.replace(replacement, self.native)
        with self.assertRaisesRegex(inventory.Invalid, "before observation"):
            observer.snapshot_file(self.native, expected, time.monotonic() + 5, observer.MAX_MEMBER)

    def test_content_change_after_open_is_not_reported_as_a_stable_hash(self):
        expected = self.native.lstat()
        original = os.fdopen
        target = self.native

        class ChangedReader:
            def __init__(self, descriptor, mode):
                self.stream = original(descriptor, mode)
                self.changed = False

            def __enter__(self):
                return self

            def __exit__(self, *args):
                self.stream.close()

            def fileno(self):
                return self.stream.fileno()

            def read(self, count):
                result = self.stream.read(count)
                if not self.changed:
                    self.changed = True
                    target.write_bytes(b"MZchanged after the open identity check")
                return result

        with patch.object(observer.os, "fdopen", ChangedReader), self.assertRaises((inventory.Invalid, OSError)):
            observer.snapshot_file(self.native, expected, time.monotonic() + 5, observer.MAX_MEMBER)

    def test_chunked_hash_and_exact_native_budget_allow_unhashed_resources(self):
        expected = self.observe()
        with patch.object(observer, "CHUNK", 1), \
                patch.object(observer, "MAX_NATIVE_BYTES", self.native.stat().st_size):
            self.assertEqual(self.observe(), expected)

    def test_membership_change_or_artifact_change_during_observation_is_refused(self):
        original = observer.snapshot_file
        for mutate in (lambda: self.write("extra-resource", b"new"),
                       lambda: self.artifact.write_bytes(b"changed artifact")):
            with self.subTest(mutate=mutate):
                invoked = False

                def changed(*args, **kwargs):
                    nonlocal invoked
                    result = original(*args, **kwargs)
                    if not invoked:
                        invoked = True
                        mutate()
                    return result

                with patch.object(observer, "snapshot_file", side_effect=changed), self.assertRaises(inventory.Invalid):
                    self.observe()

    def test_native_names_paths_case_collisions_and_empty_payloads_fail(self):
        for name in ("bad.dll", "bad.EXE", "bad.dylib", "bad.so.1"):
            path = self.write(name, b"not an executable")
            with self.subTest(name=name), self.assertRaises(inventory.Invalid):
                self.observe()
            path.unlink()
        if os.name != "nt" and sys.platform != "darwin":
            self.write("BIN/reader", b"MZcase collision")
            with self.assertRaises(inventory.Invalid):
                self.observe()
            (self.tree / "BIN/reader").unlink()
            (self.tree / "BIN").rmdir()
        self.native.unlink()
        with self.assertRaisesRegex(inventory.Invalid, "no native"):
            self.observe()

    def test_count_depth_byte_and_elapsed_budgets_fail_closed(self):
        for setting, limit in (("MAX_FILES", 1), ("MAX_DEPTH", 0), ("MAX_MEMBER", 1),
                               ("MAX_NATIVE_BYTES", 1), ("MAX_ARTIFACT", 1), ("TIME_BUDGET", 0)):
            with self.subTest(setting=setting), patch.object(observer, setting, limit), self.assertRaises(inventory.Invalid):
                self.observe()
        with self.assertRaisesRegex(inventory.Invalid, "outside"):
            observer.observe(self.tree, self.native, "windows-x86_64", "0.1.1")

    def test_cli_failure_is_fixed_and_emits_no_partial_observation(self):
        self.write("SECRET_NATIVE_NAME.dll", b"SECRET_NATIVE_CONTENT")
        command = [sys.executable, "-B", str(Path(observer.__file__)), "--tree", str(self.tree),
                   "--artifact", str(self.artifact), "--platform", "windows-x86_64", "--version", "0.1.1"]
        result = subprocess.run(command, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr,
                         "Native observation rejected: invalid, changed, or unavailable package input\n")


if __name__ == "__main__":
    unittest.main()
