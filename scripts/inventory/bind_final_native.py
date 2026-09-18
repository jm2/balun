#!/usr/bin/env python3
"""Compare a validated reopened payload with its frozen native copy ledger.

The trusted packaging adapter must reopen the supplied completed artifact and
validate its payload before calling this tool. This does not extract archives.
"""

import argparse
import json
import os
from pathlib import Path
import sys

from native_copy_ledger import read_ledger
from native_inventory import MAX_DOCUMENT, PLATFORMS, require
from observe_native import observe


def bind(tree, artifact, platform, version, ledger_path):
    require(not Path(os.path.abspath(ledger_path)).is_relative_to(Path(os.path.abspath(tree))),
            "copy ledger must be outside the reopened payload")
    ledger = read_ledger(ledger_path, frozen=True)
    observed = observe(tree, artifact, platform, version)
    expected = {record["path"]: record["final"] for record in ledger["records"]}
    actual = {member["path"]: {"size": member["size"], "sha256": member["sha256"]}
              for member in observed["native_files"]}
    require(actual == expected, "reopened native members differ from the frozen copies")
    require(read_ledger(ledger_path, frozen=True) == ledger, "copy ledger changed during observation")
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tree", type=Path, required=True)
    parser.add_argument("--artifact", type=Path, required=True)
    parser.add_argument("--copy-ledger", type=Path, required=True)
    parser.add_argument("--platform", choices=sorted(PLATFORMS), required=True)
    parser.add_argument("--version", required=True)
    args = parser.parse_args()
    try:
        result = bind(args.tree, args.artifact, args.platform, args.version, args.copy_ledger)
        encoded = json.dumps(result, sort_keys=True, indent=2) + "\n"
        require(len(encoded.encode()) <= MAX_DOCUMENT, "bound observation exceeds its byte budget")
    except (OSError, ValueError, RecursionError, RuntimeError):
        print("Final native binding rejected: invalid, changed, or mismatched package input", file=sys.stderr)
        return 1
    sys.stdout.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
