#!/usr/bin/env python3
"""Adversarial newc paths/types/budgets and real bounded producer regressions."""

from dataclasses import replace
import io
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import rpm_payload as payload


def member(name, data=b"", mode=stat.S_IFREG | 0o644, *, nlink=1, crc=False, size=None):
    name = name.encode() + b"\0"
    fields = [1, mode, 0, 0, nlink, 0, len(data) if size is None else size,
              0, 0, 0, 0, len(name), sum(data) & 0xffffffff if crc else 0]
    header = (b"070702" if crc else b"070701") + b"".join(f"{value:08x}".encode() for value in fields)
    return header + name + b"\0" * (-(110 + len(name)) % 4) + data + b"\0" * (-len(data) % 4)


def archive(*members):
    return b"".join(members) + member("TRAILER!!!") + b"\0" * 20


class PreflightTests(unittest.TestCase):
    def check(self, data, limits=payload.Limits()):
        return payload.preflight(io.BytesIO(data), limits=limits)

    def test_regular_directory_and_build_id_link_accept_in_any_order(self):
        link = member("./usr/lib/.build-id/aa/bb", b"../../../bin/balun", stat.S_IFLNK | 0o777)
        binary = member("./usr/bin/balun", b"binary", stat.S_IFREG | 0o755)
        root = member(".", mode=stat.S_IFDIR | 0o755, nlink=2)
        directory = member("usr", mode=stat.S_IFDIR | 0o755, nlink=3)
        for first, last in [(link, binary), (binary, link)]:
            self.assertEqual(self.check(archive(root, first, directory, last)), 3)

    def test_crc_validates_data(self):
        good = archive(member("file", b"abc", crc=True))
        self.assertEqual(self.check(good), 1)
        with self.assertRaises(payload.Invalid):
            self.check(good.replace(b"abc", b"abd"))

    def test_escaping_and_ambiguous_paths_reject(self):
        for name in ["/outside", "../outside", "a/../../outside", "a/./b", "a//b",
                     "a\\b", "././a", "a\nsecret", "a\0hidden", "é", ""]:
            with self.subTest(name=name), self.assertRaises(payload.Invalid):
                self.check(archive(member(name)))
        with self.assertRaises(payload.Invalid):
            self.check(archive(member("same"), member("./same")))

    def test_special_hardlinked_and_privileged_members_reject(self):
        for mode, nlink in [(stat.S_IFIFO, 1), (stat.S_IFCHR, 1), (stat.S_IFBLK, 1),
                            (stat.S_IFSOCK, 1), (stat.S_IFREG | 0o4644, 1),
                            (stat.S_IFREG | 0x80000000, 1),
                            (stat.S_IFREG, 2), (stat.S_IFREG, 0)]:
            with self.subTest(mode=mode, nlink=nlink), self.assertRaises(payload.Invalid):
                self.check(archive(member("entry", mode=mode, nlink=nlink)))
        with self.assertRaises(payload.Invalid):
            self.check(archive(member("directory", b"data", stat.S_IFDIR)))

    def test_parent_file_or_link_rejects_before_any_extraction_in_either_order(self):
        for kind, data in [(stat.S_IFREG, b"x"), (stat.S_IFLNK, b"destination")]:
            parent = member("prefix", data, kind | 0o755)
            child = member("prefix/escaped", b"x")
            for members in [(parent, child), (child, parent)]:
                with self.subTest(kind=kind, members=members), self.assertRaises(payload.Invalid):
                    self.check(archive(*members))

    def test_only_confined_links_to_included_regular_files_are_admitted(self):
        for target in [b"/outside", b"../../outside", b"missing", b"directory", b"a//b", b"a\0b", b"missing/../file", b"file/../file"]:
            with self.subTest(target=target), self.assertRaises(payload.Invalid):
                self.check(archive(member("link", target, stat.S_IFLNK | 0o777),
                                   member("directory", mode=stat.S_IFDIR | 0o755),
                                   member("file", b"content")))
        with self.assertRaises(payload.Invalid):
            self.check(archive(member("a", b"b", stat.S_IFLNK), member("b", b"a", stat.S_IFLNK)))

    def test_truncation_bad_headers_padding_and_concatenation_reject(self):
        good = archive(member("file", b"abc"))
        for bad in [b"", good[:50], good[:-100], b"070707" + good[6:],
                    good[:6] + b"g" + good[7:], good + b"second archive",
                    good[:115] + b"x" + good[116:],
                    archive(member("oversize", size=1000000))]:
            with self.subTest(size=len(bad)), self.assertRaises(payload.Invalid):
                self.check(bad)

    def test_member_file_expansion_path_and_deadline_budgets(self):
        good = archive(member("entry", b"data"))
        for field, limit in [("members", 0), ("file", 3), ("expanded", len(good) - 1),
                             ("path", 3), ("seconds", 0)]:
            with self.subTest(field=field), self.assertRaises(payload.Invalid):
                self.check(good, replace(payload.Limits(), **{field: limit}))

    def test_implicit_directories_and_depth_consume_the_tree_budget(self):
        deep = archive(member("a/b/file", b"data"))
        self.assertEqual(self.check(deep, replace(payload.Limits(), members=3, depth=3)), 1)
        for limits in [replace(payload.Limits(), members=2), replace(payload.Limits(), depth=2)]:
            with self.subTest(limits=limits), self.assertRaises(payload.Invalid):
                self.check(deep, limits)
        # The directory declared later must not be charged twice.
        self.assertEqual(self.check(archive(member("a/file"), member("a", mode=stat.S_IFDIR)),
                                    replace(payload.Limits(), members=2)), 2)


class ProducerTests(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.destination = self.root / "payload"
        self.source = self.root / "fixture"
        self.source.write_bytes(archive(member("file", b"content")))

    def test_real_producer_success_preserves_exact_payload_and_private_mode(self):
        command = [sys.executable, "-B", "-c", "import sys; sys.stdout.buffer.write(open(sys.argv[1], 'rb').read())", str(self.source)]
        payload.decode(self.source, self.destination, command=command)
        self.assertEqual(self.destination.read_bytes(), self.source.read_bytes())
        self.assertEqual(stat.S_IMODE(self.destination.stat().st_mode), 0o600)

    def test_failed_oversized_stalled_and_invalid_producers_are_reaped_and_removed(self):
        original = subprocess.Popen
        children = []
        def start(*args, **kwargs):
            process = original(*args, **kwargs)
            children.append(process)
            return process
        for script, limits in [
            ("raise SystemExit(7)", payload.Limits()),
            ("import sys; sys.stdout.write('x'*10000)", replace(payload.Limits(), expanded=128)),
            ("import time; time.sleep(60)", replace(payload.Limits(), seconds=0.1)),
            ("print('private-marker-4321')", payload.Limits()),
        ]:
            with self.subTest(script=script), patch.object(payload.subprocess, "Popen", start):
                with self.assertRaises((payload.Invalid, subprocess.TimeoutExpired)):
                    payload.decode(self.source, self.destination, limits=limits,
                                   command=[sys.executable, "-B", "-c", script])
            self.assertFalse(self.destination.exists())
            self.assertIsNotNone(children[-1].poll())

    def test_existing_destination_and_spawn_failure_are_safe(self):
        self.destination.write_text("keep")
        with self.assertRaises(FileExistsError):
            payload.decode(self.source, self.destination)
        self.assertEqual(self.destination.read_text(), "keep")
        self.destination.unlink()
        with self.assertRaises(FileNotFoundError):
            payload.decode(self.source, self.destination, command=[str(self.root / "missing")])
        self.assertFalse(self.destination.exists())

    def test_closed_stdout_but_running_producer_is_bounded(self):
        script = "import os,time; os.close(1); time.sleep(60)"
        start = time.monotonic()
        with self.assertRaises(payload.Invalid):
            payload.decode(self.source, self.destination, limits=replace(payload.Limits(), seconds=0.1),
                           command=[sys.executable, "-B", "-c", script])
        self.assertLess(time.monotonic() - start, 5)
        self.assertFalse(self.destination.exists())

    def test_child_holding_pipe_after_parent_exit_is_still_bounded(self):
        # The producer's child inherits stdout. Even after its parent exits,
        # the owner must time out, kill the entire group, and discard output.
        script = "import os,time; pid=os.fork(); time.sleep(60) if pid==0 else os._exit(0)"
        start = time.monotonic()
        with self.assertRaises(payload.Invalid):
            payload.decode(self.source, self.destination, limits=replace(payload.Limits(), seconds=0.1),
                           command=[sys.executable, "-B", "-c", script])
        self.assertLess(time.monotonic() - start, 5)
        self.assertFalse(self.destination.exists())


@unittest.skipUnless(all(shutil.which(tool) for tool in ("rpmbuild", "rpm2cpio", "cpio", "rpm")),
                     "native RPM/CPIO tools unavailable")
class NativeToolsTests(unittest.TestCase):
    def test_real_rpm_payload_and_build_id_style_link_roundtrip(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp") as temporary:
            root = Path(temporary)
            (root / "tmp").mkdir()
            spec = root / "fixture.spec"
            spec.write_text("""Name: balun-preflight-fixture
Version: 1
Release: 1
Summary: Inert local archive inspection fixture
License: MIT
BuildArch: noarch
%description
Inert data and a confined relative link for package validation tests.
%install
mkdir -p %{buildroot}/usr/share/balun %{buildroot}/usr/lib/.build-id/aa
printf 'fixture data\\n' > %{buildroot}/usr/share/balun/fixture.txt
ln -s ../../../share/balun/fixture.txt %{buildroot}/usr/lib/.build-id/aa/bb
%files
/usr/share/balun/fixture.txt
/usr/lib/.build-id/aa/bb
""")
            result = subprocess.run(["rpmbuild", "--define", f"_topdir {root / 'build'}",
                                     "--define", f"_tmppath {root / 'tmp'}", "--define",
                                     "__os_install_post %{nil}", "-bb", str(spec)],
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout.decode(errors="replace"))
            packages = list((root / "build" / "RPMS").rglob("*.rpm"))
            self.assertEqual(len(packages), 1)
            decoded = root / "payload.cpio"
            payload.decode(packages[0], decoded)
            extracted = root / "extracted"
            extracted.mkdir()
            with decoded.open("rb") as source:
                subprocess.run(["cpio", "-idm", "--quiet", "--no-preserve-owner"],
                               stdin=source, cwd=extracted, check=True, timeout=10)
            self.assertEqual((extracted / "usr/share/balun/fixture.txt").read_bytes(), b"fixture data\n")
            link = extracted / "usr/lib/.build-id/aa/bb"
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.read_bytes(), b"fixture data\n")
            validator = Path(__file__).with_name("validate-package-compliance.sh")
            subprocess.run([str(validator.resolve()), "--rpm", str(packages[0])],
                           check=True, timeout=60)


if __name__ == "__main__":
    unittest.main()
