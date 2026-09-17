#!/usr/bin/env python3
"""Collect metadata from the installed Homebrew recipes owning selected files.

This is a trusted build-input collector. Homebrew evaluates installed Ruby
recipes; neither this tool nor brew info is an untrusted-package sandbox.
The result is intermediate input to a future packaging copy ledger, not an SBOM.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

from native_inventory import MAX_DOCUMENT, Invalid, member_path, reference, require, text, unique_object
from observe_native import checkpoint, ordinary, signature, snapshot_file

MAX_METADATA = 1024**2
MAX_QUERY = 4 * 1024**2
MAX_MEMBERS = 4096
MAX_PACKAGES = 256
MAX_MEMBER = 1024**3
MAX_TOTAL = 4 * 1024**3
QUERY_SECONDS = 60
TOTAL_SECONDS = 300


def json_document(data):
    return json.loads(data.decode("utf-8"), object_pairs_hook=unique_object,
                      parse_constant=lambda _: (_ for _ in ()).throw(Invalid("non-finite JSON")))


def read_metadata(path):
    before = path.lstat()
    ordinary(before)
    require(0 < before.st_size <= MAX_METADATA, "installed metadata exceeds its byte budget")
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
    with os.fdopen(os.open(path, flags), "rb") as stream:
        require(signature(os.fstat(stream.fileno())) == signature(before), "installed metadata changed")
        data = stream.read(MAX_METADATA + 1)
        require(len(data) == before.st_size, "installed metadata changed size")
        require(signature(os.fstat(stream.fileno())) == signature(before), "installed metadata changed")
    require(signature(path.lstat()) == signature(before), "installed metadata was replaced")
    return data


def query_formula(recipe, deadline):
    """Ask for the exact installed .rb path, never a current formula by name."""
    env = dict(os.environ, HOMEBREW_NO_AUTO_UPDATE="1", HOMEBREW_NO_ANALYTICS="1")
    # Spool to owned scratch rather than buffering unbounded child output.
    with tempfile.TemporaryFile(dir=os.environ.get("TMPDIR") or "/var/tmp") as output:
        process = subprocess.Popen(["brew", "info", "--json=v2", "--formula", str(recipe)],
                                   stdout=output, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL,
                                   env=env, start_new_session=True)
        try:
            end = min(deadline, time.monotonic() + QUERY_SECONDS)
            while process.poll() is None:
                checkpoint(end)
                require(os.fstat(output.fileno()).st_size <= MAX_QUERY, "Homebrew output exceeds budget")
                time.sleep(0.05)
            require(process.returncode == 0, "Homebrew could not describe the installed recipe")
            require(os.fstat(output.fileno()).st_size <= MAX_QUERY, "Homebrew output exceeds budget")
            checkpoint(end)
            output.seek(0)
            return json_document(output.read(MAX_QUERY + 1))
        finally:
            # Also stop children left behind by a failed metadata query. This is
            # cleanup for trusted tools, not containment against hostile code.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def license_record(value, depth=0):
    require(depth <= 8, "license metadata nesting exceeds budget")
    if isinstance(value, str):
        return text(value, 256)
    # Preserve Homebrew's structured AND/OR/exception metadata without changing
    # its semantics or presenting it as a compliance determination.
    require(isinstance(value, dict) and 0 < len(value) <= 8, "missing or invalid license metadata")
    result = {}
    for key, item in value.items():
        text(key, 256)
        if isinstance(item, list):
            require(0 < len(item) <= 32, "license list exceeds budget")
            result[key] = [license_record(child, depth + 1) for child in item]
        else:
            result[key] = license_record(item, depth + 1)
    require(len(json.dumps(result).encode()) <= MAX_METADATA, "license metadata exceeds budget")
    return result


def package_metadata(keg, name, package_version, deadline, query):
    recipe = keg / ".brew" / (name + ".rb")
    ordinary(recipe.parent.lstat(), directory=True)
    receipt_path = keg / "INSTALL_RECEIPT.json"
    recipe_bytes = read_metadata(recipe)
    receipt_bytes = read_metadata(receipt_path)
    receipt = json_document(receipt_bytes)
    require(isinstance(receipt, dict), "invalid installed receipt")
    source = receipt.get("source")
    require(isinstance(source, dict) and source.get("spec") == "stable"
            and source.get("tap") == "homebrew/core", "only installed stable core recipes are supported")
    versions = source.get("versions")
    require(isinstance(versions, dict), "missing installed source version")
    version = text(versions.get("stable"), 128)
    data = query(recipe, deadline)
    require(isinstance(data, dict) and data.get("casks") == []
            and isinstance(data.get("formulae"), list) and len(data["formulae"]) == 1,
            "Homebrew did not return exactly one formula")
    formula = data["formulae"][0]
    require(isinstance(formula, dict) and formula.get("name") == name,
            "Homebrew returned a different package")
    recipe_hash = hashlib.sha256(recipe_bytes).hexdigest()
    require(formula.get("ruby_source_checksum") == {"sha256": recipe_hash},
            "Homebrew metadata does not describe the installed recipe bytes")
    require(isinstance(formula.get("versions"), dict) and formula["versions"].get("stable") == version,
            "installed receipt and formula versions differ")
    revision = formula.get("revision")
    require(type(revision) is int and 0 <= revision <= 100000, "invalid installed package revision")
    require(package_version == version + ("_" + str(revision) if revision else ""),
            "installed keg and formula versions differ")
    urls = formula.get("urls")
    require(isinstance(urls, dict) and isinstance(urls.get("stable"), dict), "missing source metadata")
    stable = urls["stable"]
    source_url = reference(stable.get("url"))
    checksum = stable.get("checksum")
    source_revision = stable.get("revision")
    if isinstance(checksum, str) and re.fullmatch(r"[0-9a-f]{64}", checksum):
        source_identity = "sha256:" + checksum
    else:
        require(isinstance(source_revision, str) and re.fullmatch(r"[0-9a-f]{40}", source_revision),
                "installed recipe has no immutable primary source identity")
        source_identity = "git:" + source_revision
    license_value = license_record(formula.get("license"))
    require(read_metadata(recipe) == recipe_bytes and read_metadata(receipt_path) == receipt_bytes,
            "installed metadata changed during Homebrew evaluation")
    checkpoint(deadline)
    return {"name": name, "version": version, "package_version": package_version,
            "tap": "homebrew/core", "license": license_value,
            "primary_source": {"reference": source_url, "identity": source_identity},
            "recipe": {"sha256": recipe_hash, "text": recipe_bytes.decode("utf-8")},
            "receipt_sha256": hashlib.sha256(receipt_bytes).hexdigest()}


def collect(cellar, members, *, query=query_formula):
    require(0 < len(members) <= MAX_MEMBERS, "installed member count exceeds budget")
    # Homebrew's prefix/opt links are expected here. Resolve selected source
    # paths into one explicitly supplied trusted Cellar before deriving owners.
    cellar = cellar.resolve(strict=True)
    ordinary(cellar.lstat(), directory=True)
    deadline = time.monotonic() + TOTAL_SECONDS
    packages, files, snapshots, metadata_snapshots = {}, {}, [], []
    total = 0
    for input_path in members:
        checkpoint(deadline)
        path = input_path.resolve(strict=True)
        require(path.is_relative_to(cellar), "selected member is outside the trusted Cellar")
        relative = path.relative_to(cellar)
        require(len(relative.parts) >= 3, "selected member has no installed package owner")
        name, package_version = relative.parts[:2]
        require(re.fullmatch(r"[a-z0-9][a-z0-9+@._-]{0,127}", name), "invalid installed package name")
        text(package_version, 128)
        key = member_path(relative.as_posix())
        require(key.casefold() not in files, "duplicate or case-colliding installed member")
        keg = cellar / name / package_version
        ordinary((cellar / name).lstat(), directory=True)
        ordinary(keg.lstat(), directory=True)
        before = path.lstat()
        total += before.st_size
        require(total <= MAX_TOTAL, "selected native inputs exceed byte budget")
        content = snapshot_file(path, before, deadline, MAX_MEMBER, native_only=True,
                                native_budget=MAX_TOTAL)
        require(content is not None, "selected member is not a native binary")
        package_key = name + "/" + package_version
        if package_key not in packages:
            require(len(packages) < MAX_PACKAGES, "installed package count exceeds budget")
            package = package_metadata(keg, name, package_version, deadline, query)
            packages[package_key] = package
            for metadata_path, expected in ((keg / ".brew" / (name + ".rb"), package["recipe"]["sha256"]),
                                            (keg / "INSTALL_RECEIPT.json", package["receipt_sha256"])):
                metadata_bytes = read_metadata(metadata_path)
                require(hashlib.sha256(metadata_bytes).hexdigest() == expected, "installed metadata changed")
                metadata_snapshots.append((metadata_path, metadata_bytes))
        files[key.casefold()] = {"path": key, "package": package_key, **content}
        snapshots.append((input_path, path, before))
    for original, path, before in snapshots:
        require(original.resolve(strict=True) == path and signature(path.lstat()) == signature(before),
                "installed member changed during collection")
    for path, content in metadata_snapshots:
        require(read_metadata(path) == content, "installed metadata changed before collection completed")
    checkpoint(deadline)
    result = {"schema": 1, "scope": "selected-homebrew-build-inputs",
              "packages": [dict(key=key, **packages[key]) for key in sorted(packages)],
              "members": [files[key] for key in sorted(files)]}
    require(len(json.dumps(result).encode()) <= MAX_DOCUMENT, "metadata report exceeds byte budget")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cellar", required=True, type=Path)
    parser.add_argument("--member", required=True, action="append", type=Path)
    args = parser.parse_args()
    try:
        result = collect(args.cellar, args.member)
        encoded = json.dumps(result, sort_keys=True, indent=2) + "\n"
        require(len(encoded.encode()) <= MAX_DOCUMENT, "metadata report exceeds byte budget")
    except (OSError, ValueError, RecursionError, RuntimeError):
        print("Homebrew metadata rejected: invalid, changed, or unavailable installed input", file=sys.stderr)
        return 1
    sys.stdout.write(encoded)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
