#!/usr/bin/env python3
"""Hash native members in an already reopened, validated local package tree.

This does not extract archives, load native code, validate architecture/closure,
or authenticate the relationship between a supplied artifact and tree. The
trusted packaging adapter must supply the tree reopened by its existing gate.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import time

from native_inventory import Invalid, MAX_DOCUMENT, MAX_FILES, PLATFORMS, member_path, require, text

MAX_MEMBER = 1024**3
MAX_NATIVE_BYTES = 4 * 1024**3
MAX_ARTIFACT = 4 * 1024**3
MAX_DEPTH = 64
TIME_BUDGET = 300
CHUNK = 1024**2
WINDOWS_STAT = os.name == "nt"
MACHO = {bytes.fromhex(value) for value in (
    "feedface", "cefaedfe", "feedfacf", "cffaedfe",
    "cafebabe", "bebafeca", "cafebabf", "bfbafeca")}


def checkpoint(deadline):
    require(time.monotonic() < deadline, "observation exceeded its elapsed-time budget")


def signature(metadata):
    mode = metadata.st_mode
    timestamp = metadata.st_ctime_ns
    if WINDOWS_STAT:
        # CPython path stat adds execute bits from .exe/.bat/.cmd/.com names;
        # fstat has no filename. Its ctime can also be ChangeTime while path
        # stat preserves CreationTime. Birth time is comparable across both.
        if stat.S_ISREG(mode):
            mode &= ~0o111
        timestamp = getattr(metadata, "st_birthtime_ns", timestamp)
    return (metadata.st_dev, metadata.st_ino, mode, metadata.st_nlink,
            metadata.st_size, metadata.st_mtime_ns, timestamp,
            getattr(metadata, "st_file_attributes", 0))


def ordinary(metadata, *, directory=False):
    require(not stat.S_ISLNK(metadata.st_mode)
            and not getattr(metadata, "st_file_attributes", 0) & 0x400,
            "package aliases and reparse points are not admitted")
    if directory:
        require(stat.S_ISDIR(metadata.st_mode), "package root or member is not a directory")
    else:
        require(stat.S_ISREG(metadata.st_mode), "package member is not a regular file")
        require(metadata.st_nlink == 1, "hard-linked package files are not admitted")


def scan(root, deadline):
    """Capture every directory/file identity without following member aliases."""
    root_metadata = root.lstat()
    ordinary(root_metadata, directory=True)
    entries = {"": root_metadata}
    folded = set()
    pending = [(root, "", 0)]
    while pending:
        directory, prefix, depth = pending.pop()
        checkpoint(deadline)
        with os.scandir(directory) as children:
            for child in children:
                checkpoint(deadline)
                require(len(entries) <= MAX_FILES, "package member count exceeds its budget")
                name = member_path(prefix + child.name)
                require(name.casefold() not in folded, "package member paths collide")
                folded.add(name.casefold())
                # Windows DirEntry.stat omits inode/device/link-count fields.
                # A fresh no-follow stat is required for the identity contract.
                metadata = os.stat(child.path, follow_symlinks=False)
                is_directory = stat.S_ISDIR(metadata.st_mode)
                ordinary(metadata, directory=is_directory)
                entries[name] = metadata
                if is_directory:
                    require(depth < MAX_DEPTH, "package directory depth exceeds its budget")
                    pending.append((root / name, name + "/", depth + 1))
                else:
                    require(metadata.st_size <= MAX_MEMBER, "package member exceeds its size budget")
    return entries


def native_prefix(prefix):
    # Classify file content, including extensionless scanners and helpers.
    # Actual executable structure and closure were checked by the package gate.
    return prefix[:4] == b"\x7fELF" or prefix[:4] in MACHO or prefix[:2] == b"MZ"


def snapshot_file(path, expected, deadline, limit, *, native_only=False, native_budget=None):
    """Compare the opened identity before reading and again after bounded hashing."""
    ordinary(expected)
    require(expected.st_size <= limit, "file exceeds its observation size budget")
    flags = os.O_RDONLY | getattr(os, "O_BINARY", 0) | getattr(os, "O_NOFOLLOW", 0)
    flags |= getattr(os, "O_NONBLOCK", 0)
    checkpoint(deadline)
    descriptor = os.open(path, flags)
    with os.fdopen(descriptor, "rb") as stream:
        opened = os.fstat(stream.fileno())
        ordinary(opened)
        require(signature(opened) == signature(expected), "file changed before observation")
        prefix = stream.read(4)
        is_native = native_prefix(prefix)
        if native_only and not is_native:
            require(not re.search(r"(?i)(?:\.(?:exe|dll|dylib)|\.so(?:\.[0-9]+)*)$", path.name),
                    "named native member lacks a recognized executable header")
            result = None
        else:
            require(expected.st_size > 0, "observed content must not be empty")
            if native_only:
                require(expected.st_size <= native_budget, "native content exceeds its total size budget")
            digest = hashlib.sha256(prefix)
            consumed = len(prefix)
            while True:
                checkpoint(deadline)
                block = stream.read(min(CHUNK, limit - consumed + 1))
                if not block:
                    break
                consumed += len(block)
                require(consumed <= limit, "file grew beyond its observation size budget")
                digest.update(block)
            require(consumed == expected.st_size, "file size changed during observation")
            result = {"size": consumed, "sha256": digest.hexdigest()}
        after = os.fstat(stream.fileno())
        ordinary(after)
        require(signature(after) == signature(expected)
                and after.st_ctime_ns == opened.st_ctime_ns, "file changed during observation")
        require(signature(path.lstat()) == signature(expected), "named file changed during observation")
        checkpoint(deadline)
        return result


def observe(tree, artifact, platform, version):
    require(platform in PLATFORMS, "unsupported target platform")
    text(version)
    # Existing ancestors belong to the trusted build workspace. Final roots and
    # members are checked directly; abspath does not resolve their aliases.
    tree = Path(os.path.abspath(tree))
    artifact = Path(os.path.abspath(artifact))
    require(not artifact.is_relative_to(tree),
            "completed artifact must be outside its reopened payload tree")
    member_path(artifact.name)
    deadline = time.monotonic() + TIME_BUDGET
    artifact_metadata = artifact.lstat()
    ordinary(artifact_metadata)
    require(0 < artifact_metadata.st_size <= MAX_ARTIFACT, "artifact exceeds its size budget")
    initial = scan(tree, deadline)
    members = []
    total = 0
    for name, metadata in sorted(initial.items()):
        if not name or stat.S_ISDIR(metadata.st_mode):
            continue
        result = snapshot_file(tree / name, metadata, deadline, MAX_MEMBER, native_only=True,
                               native_budget=MAX_NATIVE_BYTES - total)
        if result is not None:
            total += result["size"]
            require(total <= MAX_NATIVE_BYTES, "native content exceeds its total size budget")
            members.append({"path": name, **result})
    require(members, "package tree contains no native members")
    artifact_content = snapshot_file(artifact, artifact_metadata, deadline, MAX_ARTIFACT)
    final = scan(tree, deadline)
    require({name: signature(metadata) for name, metadata in initial.items()}
            == {name: signature(metadata) for name, metadata in final.items()},
            "package membership or content changed during observation")
    require(signature(artifact.lstat()) == signature(artifact_metadata),
            "artifact changed during observation")
    return {"schema": 1, "platform": platform,
            "artifact": {"name": artifact.name, "version": version, **artifact_content},
            "native_files": members}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        result = observe(args.tree, args.artifact, args.platform, args.version)
        encoded = json.dumps(result, sort_keys=True, indent=2) + "\n"
        require(len(encoded.encode()) <= MAX_DOCUMENT, "observation document exceeds its size budget")
    except (Invalid, OSError, ValueError):
        # Fixed output: do not echo filenames, artifact bytes, or OS error text.
        print("Native observation rejected: invalid, changed, or unavailable package input", file=sys.stderr)
        return 1
    sys.stdout.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
