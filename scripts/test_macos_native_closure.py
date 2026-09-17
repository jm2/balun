#!/usr/bin/env python3
"""Portable malformed/closure fixtures plus real Mach-O tests on macOS."""

import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import macos_native_closure as closure


def command(kind, value):
    prefix = 24 if kind in closure.LOAD_DYLIB | {0xD} else 12
    payload = value.encode() + b"\0"
    length = (prefix + len(payload) + 7) & ~7
    return struct.pack("<III", kind, length, prefix) + bytes(prefix - 12) + payload + bytes(length - prefix - len(payload))


def macho(imports=(), rpaths=(), *, cpu=0x100000C, kind=6):
    commands = [struct.pack("<II", 0x1B, 24) + bytes(16)]
    commands += [command(0xC, value) for value in imports]
    commands += [command(0x8000001C, value) for value in rpaths]
    data = b"".join(commands)
    return struct.pack("<IIIIIIII", 0xFEEDFACF, cpu, 3 if cpu == 0x1000007 else 0,
                       kind, len(commands), len(data), 0, 0) + data


class BundleFixture(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp")
        self.addCleanup(self.temporary.cleanup)
        self.parent = Path(self.temporary.name)
        self.app = self.parent / "Relocated App With Spaces.app"
        self.exe = self.write("Contents/MacOS/Balun-bin", macho(kind=2))

    def write(self, name, data):
        target = self.app / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        return target


class ClosureTests(BundleFixture):
    def test_bundled_system_framework_and_transitive_inherited_rpath(self):
        self.exe.write_bytes(macho(["@rpath/libfirst.dylib", "/usr/lib/libSystem.B.dylib"],
                                  ["@executable_path/../Frameworks"], kind=2))
        self.write("Contents/Frameworks/libfirst.dylib", macho(["@rpath/libsecond.dylib"]))
        self.write("Contents/Frameworks/libsecond.dylib", macho([
            "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation"]))
        self.assertEqual(closure.validate(self.app), 3)

    def test_arbitrary_external_prefix_is_rejected_even_when_file_exists(self):
        external = self.parent / "custom-vendor-prefix" / "libfoo.dylib"
        external.parent.mkdir()
        external.write_bytes(macho())
        self.exe.write_bytes(macho([str(external)], kind=2))
        with self.assertRaisesRegex(closure.Invalid, "absolute non-system"):
            closure.validate(self.app)

    def test_unresolved_tokens_bare_paths_and_system_path_escape(self):
        for name in ("@rpath/missing.dylib", "@loader_path/missing.dylib",
                     "@executable_path/missing.dylib", "missing.dylib",
                     "/usr/lib/../../custom/libfoo.dylib"):
            with self.subTest(name=name):
                self.exe.write_bytes(macho([name], kind=2))
                with self.assertRaises(closure.Invalid):
                    closure.validate(self.app)

    def test_unused_pixbuf_loader_is_still_inspected(self):
        self.write("Contents/Resources/lib/gdk-pixbuf/loaders/libpixbufloader.so",
                   macho(["/custom-prefix/libfoo.dylib"], kind=8))
        with self.assertRaisesRegex(closure.Invalid, "absolute non-system"):
            closure.validate(self.app)

    def test_external_rpath_cannot_shadow_a_bundled_dependency(self):
        self.exe.write_bytes(macho(["@rpath/libfoo.dylib"],
                                  [str(self.parent), "@executable_path/../Frameworks"], kind=2))
        self.write("Contents/Frameworks/libfoo.dylib", macho())
        with self.assertRaisesRegex(closure.Invalid, "escapes app"):
            closure.validate(self.app)

    def test_loader_path_and_symlink_are_canonicalized(self):
        self.exe.write_bytes(macho(["@loader_path/../Frameworks/A.framework/A"], kind=2))
        library = self.write("Contents/Frameworks/A.framework/Versions/A/A", macho())
        alias = self.app / "Contents/Frameworks/A.framework/A"
        alias.symlink_to("Versions/A/A")
        self.assertEqual(closure.validate(self.app), 2)
        alias.unlink()
        external = self.parent / "external.dylib"
        shutil.copyfile(library, external)
        alias.symlink_to(external)
        with self.assertRaisesRegex(closure.Invalid, "escapes app"):
            closure.validate(self.app)

    def test_architecture_mismatch_is_rejected(self):
        self.exe.write_bytes(macho(["@loader_path/../Frameworks/libfoo.dylib"], kind=2))
        self.write("Contents/Frameworks/libfoo.dylib", macho(cpu=0x1000007))
        walk = os.walk
        for reverse in (False, True):
            def ordered_walk(*args, **kwargs):
                for directory, dirs, files in walk(*args, **kwargs):
                    dirs.sort(reverse=reverse)
                    yield directory, dirs, sorted(files, reverse=reverse)

            with mock.patch.object(closure.os, "walk", ordered_walk):
                with self.assertRaisesRegex(closure.Invalid, "compatible (architecture|executable context)"):
                    closure.validate(self.app)

    def test_every_fat_slice_is_inspected(self):
        first = macho(kind=2)
        second = macho(["/unbundled/libfoo.dylib"], cpu=0x1000007, kind=2)
        fat = struct.pack(">IIIIIII", 0xCAFEBABE, 2, 0x100000C, 0, 48, len(first), 0)
        fat += struct.pack(">IIIII", 0x1000007, 3, 48 + len(first), len(second), 0)
        self.exe.write_bytes(fat + first + second)
        with self.assertRaisesRegex(closure.Invalid, "absolute non-system"):
            closure.validate(self.app)

    def test_malformed_headers_commands_and_strings_fail_closed(self):
        valid = macho(["@loader_path/foo.dylib"], kind=2)
        cases = [valid[:16], valid[:-1], b"\xca\xfe\xba\xbe" + struct.pack(">I", 999)]
        huge = bytearray(valid)
        struct.pack_into("<I", huge, 20, closure.MAX_COMMAND_BYTES + 1)
        cases.append(huge)
        bad_command = bytearray(valid)
        struct.pack_into("<I", bad_command, 36, 0)
        cases.append(bad_command)
        cases.append(macho(["@loader_path/control\n.dylib"], kind=2))
        for data in cases:
            with self.subTest(data=bytes(data[:32])):
                self.exe.write_bytes(data)
                with self.assertRaises(closure.Invalid):
                    closure.validate(self.app)

    def test_dyld_environment_is_rejected(self):
        data = bytearray(macho(kind=2))
        struct.pack_into("<I", data, 32, 0x27)
        self.exe.write_bytes(data)
        with self.assertRaisesRegex(closure.Invalid, "unsupported native load"):
            closure.validate(self.app)


@unittest.skipUnless(sys.platform == "darwin", "requires real Apple linker and sandbox")
class NativeClosureTests(BundleFixture):
    def run_tool(self, *command):
        return subprocess.run(command, check=True, capture_output=True, text=True)

    def test_real_macho_external_unresolved_pixbuf_and_clean_launch(self):
        vendor = self.parent / "Arbitrary Vendor Prefix"
        vendor.mkdir()
        source = vendor / "fixture.c"
        source.write_text("int value(void) { return 42; }\n", encoding="utf-8")
        library = vendor / "libfixture.dylib"
        self.run_tool("clang", "-dynamiclib", str(source), "-Wl,-headerpad_max_install_names",
                      "-install_name", str(library), "-o", str(library))
        source.write_text("extern int value(void); int main(void) { return value() == 42 ? 0 : 1; }\n",
                          encoding="utf-8")
        self.run_tool("clang", str(source), str(library), "-Wl,-headerpad_max_install_names",
                      "-o", str(self.exe))
        self.run_tool(str(self.exe))
        with self.assertRaisesRegex(closure.Invalid, "absolute non-system"):
            closure.validate(self.app)

        profile = Path(__file__).resolve().parent.parent / "build-aux/macos-runtime-probe.sb"
        sandbox = ["/usr/bin/sandbox-exec", "-D", f"BUILD_PREFIX={vendor}", "-f", str(profile)]
        # Prove the same probe profile actually denies the build-host library.
        denied = subprocess.run(sandbox + ["/bin/cat", str(library)], capture_output=True)
        self.assertNotEqual(denied.returncode, 0)
        denied = subprocess.run(sandbox + [str(self.exe)], capture_output=True)
        self.assertNotEqual(denied.returncode, 0)

        bundled = self.app / "Contents/Frameworks/libfixture.dylib"
        bundled.parent.mkdir(parents=True)
        shutil.copyfile(library, bundled)
        internal = "@executable_path/../Frameworks/libfixture.dylib"
        self.run_tool("install_name_tool", "-change", str(library), internal, str(self.exe))
        self.run_tool("codesign", "--force", "--sign", "-", str(self.exe))
        self.assertEqual(closure.validate(self.app), 2)
        self.run_tool(*(sandbox + [str(self.exe)]))
        library.unlink()
        self.run_tool(*(sandbox + [str(self.exe)]))

        for missing in ("@rpath/missing.dylib", "@loader_path/missing.dylib"):
            self.run_tool("install_name_tool", "-change", internal, missing, str(self.exe))
            with self.assertRaises(closure.Invalid):
                closure.validate(self.app)
            self.run_tool("install_name_tool", "-change", missing, internal, str(self.exe))

        loader = self.app / "Contents/Resources/lib/gdk-pixbuf/loaders/libpixbufloader.so"
        loader.parent.mkdir(parents=True)
        self.run_tool("clang", "-bundle", str(source), str(bundled), "-o", str(loader))
        with self.assertRaisesRegex(closure.Invalid, "absolute non-system"):
            closure.validate(self.app)


if __name__ == "__main__":
    unittest.main()
