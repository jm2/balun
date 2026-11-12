#!/usr/bin/env python3
"""Fail when a pinned Python CI tool has a known OSV advisory.

cargo-audit covers Cargo.lock and npm audit covers the Node lint lock. The
hash-pinned pip requirement files have no equivalent built-in check, so this
queries the OSV database for every exact `name==version` pin they declare.
"""

import argparse
import json
from pathlib import Path
import re
import sys
import urllib.request

REQUIREMENTS = (
    Path("scripts/coverage/requirements.txt"),
    Path("build-aux/toolchain/yaml-lint-requirements.txt"),
    Path("build-aux/flatpak/generator-lock-requirements.txt"),
)
PIN = re.compile(r"^([A-Za-z0-9][A-Za-z0-9._-]*)==([A-Za-z0-9][A-Za-z0-9.+!_-]*)(?=[\s;\\]|$)")
OSV_BATCH = "https://api.osv.dev/v1/querybatch"


def pins(text):
    """Return the exact pins in a requirement file; reject unpinned entries.

    Option lines (`--hash`, `-c constraints`) are skipped: these files are the
    resolved locks, so every installed package appears as an exact pin.
    """
    found = []
    for number, line in enumerate(text.splitlines(), 1):
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", "-")):
            continue
        match = PIN.match(stripped)
        if not match:
            raise ValueError(f"line {number} is not an exact name==version pin")
        found.append((match[1].lower().replace("_", "-"), match[2]))
    return found


def query(packages):
    body = json.dumps({"queries": [
        {"package": {"name": name, "ecosystem": "PyPI"}, "version": version}
        for name, version in packages
    ]}).encode()
    request = urllib.request.Request(OSV_BATCH, data=body,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=60) as response:
        results = json.load(response)["results"]
    if len(results) != len(packages):
        raise RuntimeError("OSV returned a different number of results")
    return [sorted(v["id"] for v in result.get("vulns", [])) for result in results]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("files", nargs="*", type=Path, default=REQUIREMENTS)
    args = parser.parse_args()

    packages = sorted({pin for path in args.files for pin in pins(path.read_text())})
    findings = [(pin, ids) for pin, ids in zip(packages, query(packages)) if ids]
    for (name, version), ids in findings:
        print(f"{name}=={version}: {', '.join(ids)}")
    print(f"Checked {len(packages)} pinned Python packages; "
          f"{len(findings)} with known advisories.")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
