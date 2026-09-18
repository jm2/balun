#!/usr/bin/env python3
"""Copy one bounded, stable Linux archive into a caller-owned private directory.

This freezes input for the existing trusted-local-output inspectors. It does not
preflight archive members, contain native parsers/extractors, or admit hostile
archives. The destination parent belongs to the serialized validation job.
"""

import argparse
import os
from pathlib import Path
import stat
import sys
import time

MAX_ARCHIVE_BYTES = 1_073_741_824
COPY_SECONDS = 30
CHUNK_BYTES = 65_536


class SnapshotError(Exception):
    """The input could not be frozen within the snapshot contract."""


def signature(value):
    return (value.st_dev, value.st_ino, value.st_mode, value.st_nlink,
            value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def copy_snapshot(source, destination, *, limit=MAX_ARCHIVE_BYTES, seconds=COPY_SECONDS):
    """Return only after a private snapshot of one unchanged regular input closes.

    Byte and elapsed-time checks run between synchronous filesystem operations;
    an OS I/O stall is not interruptible by this deadline. No existing output is
    overwritten. Partial outputs created by this call are removed on failure.
    """
    source = Path(source)
    destination = Path(destination)
    deadline = time.monotonic() + seconds
    before = source.lstat()
    if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_size > limit:
        raise SnapshotError("invalid archive input")
    source_fd = os.open(source, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK)
    output_fd = None
    output_created = False
    output_identity = None
    completed = False
    try:
        if signature(os.fstat(source_fd)) != signature(before):
            raise SnapshotError("archive changed before snapshot")
        output_fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL |
                            os.O_CLOEXEC | os.O_NOFOLLOW, 0o600)
        output_created = True
        output_identity = os.fstat(output_fd)
        total = 0
        while True:
            if time.monotonic() > deadline:
                raise SnapshotError("snapshot copy deadline exceeded")
            chunk = os.read(source_fd, min(CHUNK_BYTES, limit - total + 1))
            if not chunk:
                break
            total += len(chunk)
            if total > limit or total > before.st_size:
                raise SnapshotError("archive grew during snapshot")
            remaining = memoryview(chunk)
            while remaining:
                if time.monotonic() > deadline:
                    raise SnapshotError("snapshot copy deadline exceeded")
                count = os.write(output_fd, remaining)
                if count <= 0:
                    raise SnapshotError("snapshot write made no progress")
                remaining = remaining[count:]
        if (total != before.st_size or signature(os.fstat(source_fd)) != signature(before)
                or signature(source.lstat()) != signature(before)):
            raise SnapshotError("archive changed during snapshot")
        closing_fd = output_fd
        output_fd = None
        os.close(closing_fd)
        closing_fd = source_fd
        source_fd = None
        os.close(closing_fd)
        if time.monotonic() > deadline:
            raise SnapshotError("snapshot copy deadline exceeded")
        completed = True
    finally:
        try:
            if output_fd is not None:
                os.close(output_fd)
        finally:
            try:
                if source_fd is not None:
                    os.close(source_fd)
            finally:
                if output_created and not completed:
                    # The private, serialized parent makes this newly created
                    # name ours even if its initial fstat failed. When identity
                    # is known, also avoid deleting an unexpected replacement.
                    try:
                        current = destination.lstat()
                        if (output_identity is None or (current.st_dev, current.st_ino) ==
                                (output_identity.st_dev, output_identity.st_ino)):
                            destination.unlink()
                    except FileNotFoundError:
                        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        copy_snapshot(args.input, args.output)
    except (OSError, SnapshotError):
        print("Linux archive snapshot rejected", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
