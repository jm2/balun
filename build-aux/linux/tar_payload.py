#!/usr/bin/env python3
"""Bound Debian/Arch decoding and preflight tar members before native extraction.

Only the documented package subset is admitted. The input and decoded file are
private to one serialized local build validation job; native parsers are not
sandboxed. No member is created by this module.
"""

import argparse
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time

from rpm_payload import (Invalid, Limits, checkpoint, decode as bounded_decode,
                         exact, member_name, require, validate_tree)


def text_field(raw):
    """Read a fixed tar text field without accepting hidden suffix bytes."""
    value, separator, tail = raw.partition(b"\0")
    require(not separator or not any(tail))
    require(all(32 <= byte < 127 for byte in value))
    return value


def octal(raw):
    """Admit conventional octal fields, excluding binary/negative extensions."""
    require(re.fullmatch(b" *[0-7]*[\0 ]*", raw) is not None)
    value = raw.strip(b"\0 ")
    return int(value, 8) if value else 0


def pax_fields(data):
    """Parse one bounded per-member PAX header with explicit supported keys."""
    fields = {}
    while data:
        length, separator, _ = data.partition(b" ")
        require(separator and re.fullmatch(b"[1-9][0-9]{0,4}", length) is not None)
        size = int(length)
        require(len(length) + 4 <= size <= len(data) and data[size - 1:size] == b"\n")
        key, separator, value = data[len(length) + 1:size - 1].partition(b"=")
        require(separator and key in {b"path", b"linkpath", b"mtime", b"atime", b"ctime"})
        require(key not in fields and value)
        if key not in {b"path", b"linkpath"}:
            require(re.fullmatch(br"[0-9]{1,11}(?:\.[0-9]{1,9})?", value) is not None)
        fields[key] = value
        data = data[size:]
    return fields


def preflight(stream, *, limits=Limits(), deadline=None):
    """Validate every header, extent, path and link before a tar extractor runs."""
    if deadline is None:
        deadline = time.monotonic() + limits.seconds
    stream.seek(0, os.SEEK_END)
    length = stream.tell()
    require(1024 <= length <= limits.expanded and length % 512 == 0)
    stream.seek(0)
    members, links = {}, {}
    pending = None
    headers = 0
    root_seen = False
    while True:
        checkpoint(deadline)
        header = exact(stream, 512)
        if not any(header):
            require(pending is None and not any(exact(stream, 512)))
            tail = stream.read(65537)
            require(len(tail) <= 65536 and not any(tail) and not stream.read(1))
            break
        headers += 1
        require(headers <= 2 * (limits.members + 1))
        require(octal(header[148:156]) == sum(header[:148]) + 8 * 32 + sum(header[156:]))
        magic = header[257:265]
        require(magic in {b"ustar\x0000", b"ustar  \0", b"\0" * 8})
        name = text_field(header[:100])
        if magic == b"\0" * 8:
            # cargo-deb 3.7.0 uses V7 headers for short package paths.
            require(not any(header[257:]))
        elif magic == b"ustar\x0000":
            prefix = text_field(header[345:500])
            require(not any(header[500:]))
            if prefix:
                name = prefix + b"/" + name
        else:
            # GNU basic headers are supported, but sparse/long-name extensions
            # and extra GNU header fields are outside this package subset.
            require(not any(header[345:]))
        mode = octal(header[100:108])
        require(mode <= 0o777)
        for start, end in [(108, 116), (116, 124), (136, 148)]:
            octal(header[start:end])
        require(octal(header[329:337]) == octal(header[337:345]) == 0)
        text_field(header[265:297])
        text_field(header[297:329])
        size = octal(header[124:136])
        kind = header[156:157]
        link = text_field(header[157:257])
        require(kind in {b"0", b"\0", b"5", b"2", b"x"})
        require(kind == b"2" or not link)
        if kind == b"x":
            require(pending is None and 0 < size <= 16384)
            # This is a metadata label, never an extracted member pathname.
            # Python's PAX writer uses the conventional ././@PaxHeader label.
            require(0 < len(name) <= limits.path)
            pending = pax_fields(exact(stream, size))
            require(not any(exact(stream, -size % 512)))
            continue
        if pending is not None:
            name = pending.get(b"path", name)
            link = pending.get(b"linkpath", link)
            require(kind == b"2" or b"linkpath" not in pending)
            pending = None
        if kind == b"5" and name.endswith(b"/"):
            name = name[:-1]
        if name == b".":
            require(kind == b"5" and not root_seen)
            root_seen = True
        else:
            name = member_name(name, limits.path)
            require(len(name.split("/")) <= limits.depth)
            require(name not in members and len(members) < limits.members)
            members[name] = {b"5": stat.S_IFDIR, b"2": stat.S_IFLNK}.get(kind, stat.S_IFREG)
        if kind in {b"5", b"2"}:
            require(size == 0)
        else:
            require(size <= limits.file)
        if kind == b"2":
            links[name] = link
        # A bounded seek suffices for ordinary file data: its bytes do not
        # affect tar framing. Validate the physical extent and block padding.
        require(stream.tell() + size + (-size % 512) <= length - 1024)
        stream.seek(size, os.SEEK_CUR)
        require(not any(exact(stream, -size % 512)))
    validate_tree(members, links, limits, deadline)
    return len(members)


def decode(package, destination, kind, *, limits=Limits()):
    """Capture the selected native producer once, then validate those bytes."""
    commands = {
        "deb-control": ["dpkg-deb", "--ctrl-tarfile", str(package)],
        "deb-data": ["dpkg-deb", "--fsys-tarfile", str(package)],
        "arch": ["zstd", "--decompress", "--stdout", "--no-progress", "--", str(package)],
    }
    bounded_decode(package, destination, limits=limits, command=commands[kind], validate=preflight)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--kind", required=True, choices=["deb-control", "deb-data", "arch"])
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        decode(args.input, args.output, args.kind)
    except (OSError, ValueError, subprocess.SubprocessError):
        print("Tar payload rejected: invalid, oversized, or stalled decode", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
