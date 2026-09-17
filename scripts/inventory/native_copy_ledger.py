#!/usr/bin/env python3
"""Bind selected native copies to installed owners before relocation changes bytes.

The packaging helper controls this trusted local workspace and serializes calls.
This records copy equality and final staged membership, not source authentication.
"""

import argparse
import json
import os
from pathlib import Path
import stat
import sys
import time

from native_inventory import (Invalid, MAX_DOCUMENT, MAX_FILES, digest, fields, member_path,
                              read_document, require, size)
from observe_native import (MAX_MEMBER, MAX_NATIVE_BYTES, TIME_BUDGET, checkpoint, ordinary,
                            scan, signature, snapshot_file)


def read_ledger(path, *, missing=False, frozen=False):
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        require(missing, "copy ledger is missing")
        return {"schema": 1, "state": "collecting", "records": []}
    ordinary(metadata)
    ledger = read_document(path)
    fields(ledger, "schema state records")
    require(type(ledger["schema"]) is int and ledger["schema"] == 1
            and ledger["state"] == ("frozen" if frozen else "collecting"),
            "copy ledger has the wrong schema or collection state")
    require(isinstance(ledger["records"], list) and len(ledger["records"]) <= MAX_FILES,
            "copy record count exceeds budget")
    seen = set()
    total = 0
    final_total = 0
    for record in ledger["records"]:
        fields(record, "path origin copied final" if frozen else "path origin copied")
        name = member_path(record["path"])
        require(name.casefold() not in seen, "copy destinations collide")
        seen.add(name.casefold())
        fields(record["copied"], "size sha256")
        total += size(record["copied"]["size"])
        digest(record["copied"]["sha256"])
        if frozen:
            fields(record["final"], "size sha256")
            final_total += size(record["final"]["size"])
            digest(record["final"]["sha256"])
        origin = record["origin"]
        fields(origin, "kind member")
        require(isinstance(origin["kind"], str) and origin["kind"] in {"project", "homebrew"},
                "unknown copy owner kind")
        member_path(origin["member"])
        if origin["kind"] == "project":
            require(origin["member"] == "balun", "unknown project copy owner")
        else:
            require(len(origin["member"].split("/")) >= 3, "installed copy owner is incomplete")
    require(total <= MAX_NATIVE_BYTES and final_total <= MAX_NATIVE_BYTES,
            "copied native bytes exceed budget")
    require(not frozen or ledger["records"], "frozen copy ledger is empty")
    require(signature(path.lstat()) == signature(metadata), "copy ledger changed while reading")
    return ledger


def write_ledger(path, ledger):
    data = (json.dumps(ledger, sort_keys=True, indent=2) + "\n").encode()
    require(len(data) <= MAX_DOCUMENT, "copy ledger exceeds byte budget")
    # An interrupted write invalidates this build's ledger and the next read
    # fails closed. Publication of package artifacts occurs only after freezing.
    flags = os.O_WRONLY | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0)
    try:
        ordinary(path.lstat())
    except FileNotFoundError:
        flags |= os.O_EXCL
    with os.fdopen(os.open(path, flags, 0o600), "wb") as stream:
        ordinary(os.fstat(stream.fileno()))
        stream.truncate(0)
        stream.write(data)


def destination_path(tree, destination):
    tree, destination = Path(os.path.abspath(tree)), Path(os.path.abspath(destination))
    ordinary(tree.lstat(), directory=True)
    require(destination.is_relative_to(tree), "copy destination is outside the package tree")
    name = member_path(destination.relative_to(tree).as_posix())
    for parent in destination.parents:
        if parent == tree:
            break
        ordinary(parent.lstat(), directory=True)
    return destination, name


def record_copy(ledger, tree, source, destination, cellar, deadline, *, project=False):
    require(len(ledger["records"]) < MAX_FILES, "copy record count exceeds budget")
    destination, name = destination_path(tree, destination)
    require(name.casefold() not in {item["path"].casefold() for item in ledger["records"]},
            "copy destination was already recorded")
    source_input = source
    source = source.resolve(strict=True)
    if project:
        origin = {"kind": "project", "member": "balun"}
    else:
        cellar = cellar.resolve(strict=True)
        require(source.is_relative_to(cellar), "native source is outside the installed Cellar")
        installed = member_path(source.relative_to(cellar).as_posix())
        require(len(installed.split("/")) >= 3, "native source has no installed keg owner")
        origin = {"kind": "homebrew", "member": installed}
    source_metadata, destination_metadata = source.lstat(), destination.lstat()
    remaining = MAX_NATIVE_BYTES - sum(item["copied"]["size"] for item in ledger["records"])
    copied = snapshot_file(source, source_metadata, deadline, MAX_MEMBER,
                           native_only=True, native_budget=remaining)
    require(copied is not None, "copy source is not native content")
    staged = snapshot_file(destination, destination_metadata, deadline, MAX_MEMBER,
                           native_only=True, native_budget=remaining)
    require(staged == copied, "copied native bytes do not match their recorded source")
    require(source_input.resolve(strict=True) == source
            and signature(source.lstat()) == signature(source_metadata), "native source changed during copy recording")
    ledger["records"].append({"path": name, "origin": origin, "copied": copied})


def record_tree(ledger, tree, source, destination, cellar, deadline):
    destination, _ = destination_path(tree, destination)
    initial = scan(destination, deadline)
    for name, metadata in sorted(initial.items()):
        if not name or stat.S_ISDIR(metadata.st_mode):
            continue
        native = snapshot_file(destination / name, metadata, deadline, MAX_MEMBER,
                               native_only=True, native_budget=MAX_NATIVE_BYTES)
        if native is not None:
            record_copy(ledger, tree, source / name, destination / name, cellar, deadline)
    require({name: signature(value) for name, value in initial.items()}
            == {name: signature(value) for name, value in scan(destination, deadline).items()},
            "copied native tree changed during recording")


def freeze(ledger, tree, deadline):
    initial = scan(tree, deadline)
    native = {}
    remaining = MAX_NATIVE_BYTES
    for name, metadata in sorted(initial.items()):
        if not name or stat.S_ISDIR(metadata.st_mode):
            continue
        content = snapshot_file(tree / name, metadata, deadline, MAX_MEMBER,
                                native_only=True, native_budget=remaining)
        if content is not None:
            native[name] = content
            remaining -= content["size"]
    require(native and native.keys() == {record["path"] for record in ledger["records"]},
            "unknown or missing staged native member")
    require({name: signature(value) for name, value in initial.items()}
            == {name: signature(value) for name, value in scan(tree, deadline).items()},
            "staged tree changed during final content binding")
    checkpoint(deadline)
    return {"schema": 1, "state": "frozen",
            "records": [dict(record, final=native[record["path"]])
                        for record in sorted(ledger["records"], key=lambda item: item["path"])]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("record", "record-tree", "freeze"))
    parser.add_argument("--ledger", required=True, type=Path)
    parser.add_argument("--tree", required=True, type=Path)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--destination", type=Path)
    parser.add_argument("--cellar", type=Path)
    parser.add_argument("--project", action="store_true")
    args = parser.parse_args()
    try:
        deadline = time.monotonic() + TIME_BUDGET
        require(not Path(os.path.abspath(args.ledger)).is_relative_to(Path(os.path.abspath(args.tree))),
                "copy ledger must be outside the package tree")
        ledger = read_ledger(args.ledger, missing=args.operation != "freeze")
        if args.operation == "freeze":
            ledger = freeze(ledger, args.tree, deadline)
        else:
            require(args.source is not None and args.destination is not None
                    and (args.cellar is not None or args.project), "copy arguments are incomplete")
            if args.operation == "record-tree":
                require(not args.project and args.cellar is not None, "tree copies require installed owners")
                record_tree(ledger, args.tree, args.source, args.destination, args.cellar, deadline)
            else:
                record_copy(ledger, args.tree, args.source, args.destination, args.cellar, deadline,
                            project=args.project)
        write_ledger(args.ledger, ledger)
    except (OSError, ValueError, RecursionError, RuntimeError):
        print("Native copy ledger rejected: invalid, changed, or unowned build input", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
