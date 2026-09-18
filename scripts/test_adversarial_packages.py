#!/usr/bin/env python3
"""Mutate native headers and generate dependency graphs without executing payloads."""

import os
from pathlib import Path
import struct
import tempfile

from adversarial_support import mutate, run
import macos_native_closure as closure
from test_macos_native_closure import macho


def main():
    with tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR") or "/var/tmp") as temporary:
        app = Path(temporary) / "Synthetic Package.app"
        executable = app / "Contents/MacOS/Balun"
        library = app / "Contents/Frameworks/libfixture.dylib"
        executable.parent.mkdir(parents=True)
        library.parent.mkdir(parents=True)

        def property_check(generator):
            cpu = generator.choice((0x1000007, 0x100000C))
            library.write_bytes(macho(cpu=cpu))
            valid = macho(["@rpath/libfixture.dylib", "/usr/lib/libSystem.B.dylib"],
                          ["@executable_path/../Frameworks"], cpu=cpu, kind=2)
            executable.write_bytes(valid)
            assert closure.validate(app) == 2
            # A generated external import is always rejected even on an unused plugin.
            bad = macho([f"/outside-prefix-{generator.randrange(1 << 32)}/libfixture.dylib"],
                        cpu=cpu)
            library.write_bytes(bad)
            try:
                closure.validate(app)
            except closure.Invalid:
                pass
            else:
                raise AssertionError("unbundled dependency was accepted")

            # Valid thin and universal seeds reach both header/table parsers.
            other_cpu = 0x100000C if cpu == 0x1000007 else 0x1000007
            other = macho(cpu=other_cpu, kind=2)
            subtype = 3 if cpu == 0x1000007 else 0
            other_subtype = 3 if other_cpu == 0x1000007 else 0
            fat = struct.pack(">IIIIIII", 0xCAFEBABE, 2, cpu, subtype, 48, len(valid), 0)
            fat += struct.pack(">IIIII", other_cpu, other_subtype, 48 + len(valid), len(other), 0)
            seed = generator.choice((valid, fat + valid + other))
            executable.write_bytes(mutate(generator, seed, 8192))
            try:
                images = closure.inspect(executable)
            except (closure.Invalid, UnicodeError):
                # Strict UTF-8 rejection is caught by the production CLI too.
                return
            if images is not None:
                assert 1 <= len(images) <= 64
                for image in images:
                    assert image.kind in (2, 6, 8)
                    assert len(image.imports) + len(image.rpaths) <= 65536
                    for reference in (*image.imports, *image.rpaths):
                        assert 0 < len(reference) <= 4096
                        assert all(ord(char) >= 32 and ord(char) != 127 for char in reference)

        run("native-packages", property_check)


if __name__ == "__main__":
    main()
