#!/usr/bin/env python3
"""Bound rpm2cpio output and preflight newc members before the native extractor.

The package and output parent belong to the existing serialized, private build
validation job. This does not sandbox RPM/CPIO parsers or admit hostile packages.
"""

import argparse
from dataclasses import dataclass
import os
from pathlib import Path
import re
import selectors
import signal
import stat
import subprocess
import sys
import time


class Invalid(ValueError):
    """The decoded payload is outside the package inspection contract."""


@dataclass(frozen=True)
class Limits:
    expanded: int = 1_073_741_824
    members: int = 8192
    file: int = 268_435_456
    path: int = 2048
    depth: int = 64
    seconds: float = 60


def require(condition):
    if not condition:
        raise Invalid("package payload rejected")


def checkpoint(deadline):
    require(time.monotonic() < deadline)


def exact(stream, size):
    data = stream.read(size)
    require(len(data) == size)
    return data


def member_name(raw, limit):
    require(0 < len(raw) <= limit and all(32 <= byte < 127 for byte in raw))
    name = raw.decode("ascii")
    if name.startswith("./"):
        name = name[2:]
    require(not name.startswith("/") and "\\" not in name)
    require(all(part not in {"", ".", ".."} for part in name.split("/")))
    return name


def link_target(name, raw, limit, directories):
    require(0 < len(raw) <= limit and all(32 <= byte < 127 for byte in raw))
    target = raw.decode("ascii")
    require(not target.startswith("/") and "\\" not in target)
    parts = name.split("/")[:-1]
    target_parts = target.split("/")
    for index, part in enumerate(target_parts):
        require(part != "")
        if part == "..":
            require(bool(parts))
            parts.pop()
        elif part != ".":
            parts.append(part)
        if index < len(target_parts) - 1:
            require("/".join(parts) in directories)
    require(bool(parts))
    return "/".join(parts)


def preflight(stream, *, limits=Limits(), deadline=None):
    """Validate the entire bounded newc/CRC payload without creating members."""
    if deadline is None:
        deadline = time.monotonic() + limits.seconds
    stream.seek(0, os.SEEK_END)
    require(0 < stream.tell() <= limits.expanded)
    stream.seek(0)
    members, links = {}, {}
    total = 0
    root_seen = False
    while True:
        checkpoint(deadline)
        header = exact(stream, 110)
        require(header[:6] in {b"070701", b"070702"})
        require(re.fullmatch(b"[0-9A-Fa-f]{104}", header[6:]) is not None)
        fields = [int(header[offset:offset + 8], 16) for offset in range(6, 110, 8)]
        _, mode, _, _, nlink, _, size, _, _, rdevmajor, rdevminor, namesize, checksum = fields
        require(1 < namesize <= limits.path + 1)
        raw = exact(stream, namesize)
        require(raw[-1:] == b"\0" and b"\0" not in raw[:-1])
        require(not any(exact(stream, -(110 + namesize) % 4)))
        if raw == b"TRAILER!!!\0":
            require(size == 0 and checksum == 0)
            tail = stream.read(65537)
            require(len(tail) <= 65536 and not any(tail) and not stream.read(1))
            break
        kind = stat.S_IFMT(mode)
        require(kind in {stat.S_IFREG, stat.S_IFDIR, stat.S_IFLNK})
        require(mode & ~0o170777 == 0 and rdevmajor == rdevminor == 0)
        require(nlink >= 1 and (kind == stat.S_IFDIR or nlink == 1))
        if raw[:-1] in {b".", b"./"}:
            require(not root_seen and kind == stat.S_IFDIR and size == 0)
            root_seen = True
            name = ""
        else:
            name = member_name(raw[:-1], limits.path)
            require(len(name.split("/")) <= limits.depth)
            require(name not in members and len(members) < limits.members)
            members[name] = kind
        if kind == stat.S_IFDIR:
            require(size == 0)
        elif kind == stat.S_IFLNK:
            require(size <= limits.path)
        else:
            require(size <= limits.file)
        total += size
        require(total <= limits.expanded)
        computed = 0
        if kind == stat.S_IFLNK:
            data = exact(stream, size)
            links[name] = data
            computed = sum(data)
        else:
            remaining = size
            while remaining:
                checkpoint(deadline)
                data = exact(stream, min(65536, remaining))
                remaining -= len(data)
                if header[:6] == b"070702":
                    computed = (computed + sum(data)) & 0xffffffff
        require(checksum == (computed & 0xffffffff) if header[:6] == b"070702" else checksum == 0)
        require(not any(exact(stream, -size % 4)))
    validate_tree(members, links, limits, deadline)
    return len(members)


def validate_tree(members, links, limits, deadline):
    """Apply the shared Linux package path/link and implicit-directory contract."""
    directories = {""} | {name for name, kind in members.items() if kind == stat.S_IFDIR}
    entries = set(members)
    for name in members:
        checkpoint(deadline)
        parts = name.split("/")
        for index in range(1, len(parts)):
            # No entry can be created through an archive file or symlink,
            # regardless of the order in which its header appears.
            parent = "/".join(parts[:index])
            require(parent not in members or members[parent] == stat.S_IFDIR)
            directories.add(parent)
            entries.add(parent)
            require(len(entries) <= limits.members)
    for name, raw in links.items():
        checkpoint(deadline)
        target = link_target(name, raw, limits.path, directories)
        # Balun's package links point directly to an included regular file.
        # Reject chains, directory links and dangling links in this first slice.
        require(members.get(target) == stat.S_IFREG)
    checkpoint(deadline)


def decode(package, destination, *, limits=Limits(), command=None, validate=None):
    """Bound one owned producer; remove our partial output on any failure."""
    deadline = time.monotonic() + limits.seconds
    created = False
    complete = False
    process = None
    try:
        fd = os.open(destination, os.O_RDWR | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        created = True
        with os.fdopen(fd, "w+b") as output:
            process = subprocess.Popen(command or ["rpm2cpio", str(package)],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                       stderr=subprocess.DEVNULL, start_new_session=True)
            with process.stdout, selectors.DefaultSelector() as selector:
                os.set_blocking(process.stdout.fileno(), False)
                selector.register(process.stdout, selectors.EVENT_READ)
                total = 0
                while True:
                    checkpoint(deadline)
                    if not selector.select(min(0.2, max(0, deadline - time.monotonic()))):
                        continue
                    data = os.read(process.stdout.fileno(), min(65536, limits.expanded - total + 1))
                    if not data:
                        break
                    total += len(data)
                    require(total <= limits.expanded)
                    output.write(data)
                # Keep the group leader waitable until cleanup has killed the
                # owned process group. Reaping earlier could allow its PID to
                # be reused while preflight runs, making a later killpg unsafe.
                while True:
                    checkpoint(deadline)
                    status = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                    if status is not None:
                        require(status.si_code == os.CLD_EXITED and status.si_status == 0)
                        break
                    time.sleep(min(0.01, max(0, deadline - time.monotonic())))
            output.flush()
            (preflight if validate is None else validate)(output, limits=limits, deadline=deadline)
        checkpoint(deadline)
        complete = True
    finally:
        try:
            if process is not None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait()
        finally:
            if created and not complete:
                Path(destination).unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    try:
        decode(args.input, args.output)
    except (OSError, ValueError, subprocess.SubprocessError):
        print("RPM payload rejected: invalid, oversized, or stalled decode", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
