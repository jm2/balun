#!/usr/bin/env python3
"""Tar admission regressions; only inert valid fixtures reach native extractors."""

from dataclasses import replace
import io
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import tar_payload as payload


def member(name, data=b"", *, kind=tarfile.REGTYPE, link="", mode=0o644,
           size=None, pax=None, format=tarfile.USTAR_FORMAT):
    entry = tarfile.TarInfo(name)
    entry.type = kind
    entry.linkname = link
    entry.mode = mode
    entry.size = len(data) if size is None else size
    entry.pax_headers = pax or {}
    return entry.tobuf(format=format) + data + b"\0" * (-len(data) % 512)


def archive(*members):
    return b"".join(members) + b"\0" * 1024


def change_header(data, offset, replacement):
    header = bytearray(data[:512])
    header[offset:offset + len(replacement)] = replacement
    header[148:156] = b" " * 8
    header[148:156] = f"{sum(header):06o}\0 ".encode()
    return bytes(header) + data[512:]


class PreflightTests(unittest.TestCase):
    def check(self, data, limits=payload.Limits()):
        return payload.preflight(io.BytesIO(data), limits=limits)

    def test_ustar_gnu_and_pax_basic_packages(self):
        for format in [tarfile.USTAR_FORMAT, tarfile.GNU_FORMAT, tarfile.PAX_FORMAT]:
            with self.subTest(format=format):
                self.assertEqual(self.check(archive(
                    member("./", kind=tarfile.DIRTYPE, mode=0o755, format=format),
                    member("./usr/share/data", b"fixture", format=format),
                    member("./usr/share/link", kind=tarfile.SYMTYPE, link="data", format=format),
                )), 2)

    def test_pax_paths_linkpaths_and_fractional_times(self):
        name = "usr/share/" + "long" * 35
        self.assertEqual(self.check(archive(
            member(name, b"data", format=tarfile.PAX_FORMAT,
                   pax={"mtime": "1700000000.123456789", "atime": "0", "ctime": "0.1"}),
            member("usr/share/link", kind=tarfile.SYMTYPE, link="long" * 35,
                   format=tarfile.PAX_FORMAT),
        )), 2)

    def test_prefix_and_directory_normalization(self):
        name = "directory/" * 12 + "file"
        self.assertEqual(self.check(archive(member(name, b"data"))), 1)
        self.assertEqual(self.check(archive(member("./dir/", kind=tarfile.DIRTYPE))), 1)

    def test_v7_headers_used_by_pinned_cargo_deb(self):
        old = change_header(member("./dir/data", b"fixture"), 257, b"\0" * 255)
        self.assertEqual(self.check(archive(old)), 1)
        with self.assertRaises(payload.Invalid):
            self.check(archive(change_header(old, 400, b"unexpected")))

    def test_escaping_ambiguous_and_duplicate_names_reject(self):
        for name in ["/outside", "../outside", "a/../../outside", "a/./b", "a//b",
                     "a\\b", "././a", "a\nprivate-marker", "a\0hidden", "", "file/"]:
            with self.subTest(name=name), self.assertRaises(payload.Invalid):
                self.check(archive(member(name)))
        for members in [(member("a"), member("./a")),
                        (member("dir/", kind=tarfile.DIRTYPE), member("dir")),
                        (member("./", kind=tarfile.DIRTYPE), member(".", kind=tarfile.DIRTYPE))]:
            with self.subTest(members=members), self.assertRaises(payload.Invalid):
                self.check(archive(*members))

    def test_pax_overrides_are_validated_and_duplicates_cannot_hide(self):
        for key, value in [("path", "../outside"), ("path", "a\nb"), ("path", "é"),
                           ("linkpath", "file"), ("size", "100"), ("GNU.sparse.size", "10"),
                           ("SCHILY.xattr.user.fixture", "value"), ("mtime", "nan"),
                           ("mtime", "-1"), ("mtime", "9" * 100)]:
            with self.subTest(key=key, value=value), self.assertRaises(payload.Invalid):
                self.check(archive(member("safe", pax={key: value}, format=tarfile.PAX_FORMAT)))
        with self.assertRaises(payload.Invalid):
            self.check(archive(member("same"), member("other", pax={"path": "same"},
                                                     format=tarfile.PAX_FORMAT)))

    def test_pax_framing_unknown_and_repeated_extensions_reject(self):
        for data in [b"0 path=x\n", b"99999 path=x\n", b"10 path=x!", b"10 path=x\n10 path=y\n",
                     b"10 path=x\nignored", b"9 size=0\n", b"10 path=\n", b"00 path=x\n"]:
            with self.subTest(data=data), self.assertRaises(payload.Invalid):
                self.check(archive(member("pax", data, kind=tarfile.XHDTYPE), member("file")))
        extension = member("pax", b"10 path=x\n", kind=tarfile.XHDTYPE)
        for data in [archive(extension), archive(extension, extension, member("file")),
                     archive(member("pax", b"x" * 16385, kind=tarfile.XHDTYPE), member("file"))]:
            with self.subTest(size=len(data)), self.assertRaises(payload.Invalid):
                self.check(data)

    def test_special_hardlinked_sparse_privileged_and_global_headers_reject(self):
        for kind in [tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.BLKTYPE, tarfile.FIFOTYPE,
                     tarfile.GNUTYPE_SPARSE, tarfile.GNUTYPE_LONGNAME, tarfile.GNUTYPE_LONGLINK,
                     tarfile.XGLTYPE, b"?"]:
            with self.subTest(kind=kind), self.assertRaises(payload.Invalid):
                self.check(archive(member("entry", kind=kind)))
        for mode in [0o4644, 0o2644, 0o1644, 0o10644]:
            with self.subTest(mode=mode), self.assertRaises(payload.Invalid):
                self.check(change_header(archive(member("entry")), 100, f"{mode:07o}\0".encode()))
        for kind in [tarfile.DIRTYPE, tarfile.SYMTYPE]:
            with self.subTest(kind=kind), self.assertRaises(payload.Invalid):
                self.check(archive(member("entry", b"data", kind=kind)))

    def test_parent_file_or_link_rejects_in_either_order(self):
        for parent in [member("prefix"), member("prefix", kind=tarfile.SYMTYPE, link="target")]:
            for members in [(parent, member("prefix/child")), (member("prefix/child"), parent)]:
                with self.subTest(members=members), self.assertRaises(payload.Invalid):
                    self.check(archive(*members))

    def test_only_confined_direct_regular_links_are_admitted(self):
        for target in ["/outside", "../../outside", "missing", "directory", "a//b",
                       "missing/../file", "file/../file", "link", "a\\b"]:
            with self.subTest(target=target), self.assertRaises(payload.Invalid):
                self.check(archive(member("file", b"data"),
                                   member("directory", kind=tarfile.DIRTYPE),
                                   member("link", kind=tarfile.SYMTYPE, link=target)))
        for first in [True, False]:
            members = [member("a/data", b"data"),
                       member("a/b/link", kind=tarfile.SYMTYPE, link="../data")]
            self.assertEqual(self.check(archive(*(members if first else members[::-1]))), 2)

    def test_header_checksums_numeric_formats_and_text_suffixes(self):
        good = archive(member("file", b"data"))
        for bad in [b"X" + good[1:],
                    change_header(good, 100, b"-000001\0"),
                    change_header(good, 124, b"\x80" + b"\0" * 11),
                    change_header(good, 124, b"0 000000004\0"),
                    change_header(good, 5, b"hidden"),
                    change_header(good, 257, b"wrong!00"),
                    change_header(good, 329, b"0000001\0"),
                    change_header(good, 500, b"x")]:
            with self.subTest(bad=bad[:157]), self.assertRaises(payload.Invalid):
                self.check(bad)

    def test_truncation_padding_and_concatenation_reject(self):
        good = archive(member("file", b"data"))
        for bad in [b"", good[:500], good[:-512], good + good, good + b"\0",
                    good[:516] + b"x" + good[517:],
                    archive(member("file", size=1000000)), good + b"\0" * 66048]:
            with self.subTest(size=len(bad)), self.assertRaises(payload.Invalid):
                self.check(bad)

    def test_entry_file_expansion_path_depth_and_time_limits(self):
        good = archive(member("a/b/file", b"data"))
        self.assertEqual(self.check(good, replace(payload.Limits(), members=3, depth=3)), 1)
        for field, limit in [("members", 2), ("file", 3), ("expanded", len(good) - 1),
                             ("path", 7), ("depth", 2), ("seconds", 0)]:
            with self.subTest(field=field), self.assertRaises(payload.Invalid):
                self.check(good, replace(payload.Limits(), **{field: limit}))


class ProducerTests(unittest.TestCase):
    def test_native_producer_routes_preflight_private_output_and_cleanup(self):
        with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp") as work:
            root = Path(work)
            fixture = root / "fixture"
            fixture.write_bytes(archive(member("file", b"data")))
            for tool in ["dpkg-deb", "zstd"]:
                program = root / tool
                program.write_text('#!/bin/sh\ncat "$TEST_TAR_FIXTURE"\n')
                program.chmod(0o755)
            with patch.dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                            TEST_TAR_FIXTURE=str(fixture)):
                for kind in ["deb-control", "deb-data", "arch"]:
                    destination = root / kind
                    payload.decode(fixture, destination, kind)
                    self.assertEqual(destination.read_bytes(), fixture.read_bytes())
                    self.assertEqual(stat.S_IMODE(destination.stat().st_mode), 0o600)
                fixture.write_bytes(b"invalid private-marker-5432")
                destination = root / "rejected"
                result = subprocess.run([sys.executable, "-B", str(Path(payload.__file__).resolve()),
                                         "--kind", "arch", "--input", str(fixture),
                                         "--output", str(destination)], capture_output=True, check=False)
                self.assertEqual(result.returncode, 1)
                self.assertNotIn(b"private-marker", result.stdout + result.stderr)
                self.assertFalse(destination.exists())


class NativeToolsTests(unittest.TestCase):
    def setUp(self):
        work = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(work.cleanup)
        self.root = Path(work.name)
        self.tree = self.root / "input"
        data = self.tree / "usr/share/balun"
        data.mkdir(parents=True)
        (data / "data.txt").write_bytes(b"inert fixture\n")
        (data / "link.txt").symlink_to("data.txt")

    def check_package(self, package, kind, mode):
        decoded = self.root / "payload.tar"
        payload.decode(package, decoded, kind)
        extracted = self.root / "extracted"
        extracted.mkdir()
        subprocess.run(["tar", "--extract", "--file", str(decoded), "--directory", str(extracted),
                        "--no-same-owner", "--no-same-permissions"], check=True, timeout=10)
        self.assertEqual((extracted / "usr/share/balun/data.txt").read_bytes(), b"inert fixture\n")
        self.assertTrue((extracted / "usr/share/balun/link.txt").is_symlink())
        self.assertEqual((extracted / "usr/share/balun/link.txt").read_bytes(), b"inert fixture\n")
        validator = Path(__file__).with_name("validate-package-compliance.sh").resolve()
        subprocess.run([str(validator), mode, str(package)], check=True, timeout=60)

    @unittest.skipUnless(shutil.which("tar"), "GNU tar unavailable")
    def test_native_extraction_of_admitted_v7_layout(self):
        # Pinned cargo-deb uses zeroed V7 extension fields for short names.
        data = archive(change_header(member("./dir/data", b"inert fixture\n"),
                                     257, b"\0" * 255))
        self.assertEqual(payload.preflight(io.BytesIO(data)), 1)
        decoded = self.root / "v7.tar"
        decoded.write_bytes(data)
        extracted = self.root / "v7"
        extracted.mkdir()
        subprocess.run(["tar", "--extract", "--file", str(decoded), "--directory", str(extracted),
                        "--no-same-owner", "--no-same-permissions"], check=True, timeout=10)
        self.assertEqual((extracted / "dir/data").read_bytes(), b"inert fixture\n")

    @unittest.skipUnless(all(shutil.which(tool) for tool in ["dpkg-deb", "tar"]), "Debian tools unavailable")
    def test_real_debian_package_control_and_data(self):
        control = self.tree / "DEBIAN"
        control.mkdir()
        (control / "control").write_text(
            "Package: balun-fixture\nVersion: 1.0\nArchitecture: all\n"
            "Maintainer: Fixture <fixture@example.invalid>\nDescription: inert fixture\n")
        (control / "postinst").write_text("#!/bin/sh\nexit 0\n")
        (control / "postinst").chmod(0o755)
        package = self.root / "fixture.deb"
        subprocess.run(["dpkg-deb", "--build", "--root-owner-group", str(self.tree), str(package)],
                       check=True, timeout=30, stdout=subprocess.DEVNULL)
        payload.decode(package, self.root / "control.tar", "deb-control")
        self.check_package(package, "deb-data", "--deb")

    @unittest.skipUnless(all(shutil.which(tool) for tool in ["bsdtar", "zstd", "tar"]), "Arch tools unavailable")
    def test_real_arch_pax_package(self):
        (self.tree / ".PKGINFO").write_text("pkgname = balun-fixture\npkgver = 1.0\ndepend = glibc\n")
        (self.tree / ".INSTALL").write_text("post_install() { :; }\n")
        raw = self.root / "input.tar"
        # Arch builders have no SELinux labels; exclude host xattrs/ACLs from
        # this fixture while retaining normal bsdtar PAX fractional timestamps.
        subprocess.run(["bsdtar", "--no-fflags", "--no-read-sparse", "--no-xattrs", "--no-acls",
                        "-cf", str(raw), "-C", str(self.tree), "."], check=True, timeout=10)
        package = self.root / "fixture.pkg.tar.zst"
        subprocess.run(["zstd", "--quiet", str(raw), "-o", str(package)], check=True, timeout=10)
        self.check_package(package, "arch", "--arch")


if __name__ == "__main__":
    unittest.main()
