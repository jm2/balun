#!/usr/bin/env python3
"""Measure the Linux desktop test build with matching Rust/LLVM tools."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def output(command):
    return subprocess.check_output(command, text=True, cwd=ROOT).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if sys.platform != "linux":
        parser.error("the initial coverage baseline is Linux-specific")
    compiler = output(["rustc", "-vV"])
    version = re.search(r"LLVM version: (\S+)", compiler).group(1)
    host = re.search(r"host: (\S+)", compiler).group(1)
    bundled = Path(output(["rustc", "--print", "sysroot"])) / "lib/rustlib" / host / "bin"
    tools = {}
    for name in ("llvm-cov", "llvm-profdata"):
        candidate = bundled / name
        selected = str(candidate) if candidate.exists() else shutil.which(name)
        if not selected or not re.search(r"\b" + re.escape(version) + r"\b", output([selected, "--version"])):
            raise SystemExit(f"Install llvm-tools-preview for this Rust toolchain; {name} must match LLVM {version}")
        tools[name] = selected
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="balun-rust-coverage-",
                                     dir=os.environ.get("TMPDIR") or "/var/tmp") as temporary:
        work = Path(temporary)
        environment = dict(os.environ, CARGO_TARGET_DIR=str(work / "target"),
                           XDG_CACHE_HOME=str(work / "cache"),
                           RUSTFLAGS="-C instrument-coverage",
                           CARGO_ENCODED_RUSTFLAGS="-C\x1finstrument-coverage",
                           LLVM_PROFILE_FILE=str(work / "build-raw/%m-%p.profraw"))
        command = ["cargo", "test", "--locked", "--features", "desktop", "--all-targets"]
        messages = work / "build.jsonl"
        with messages.open("w") as stream:
            subprocess.run(command + ["--no-run", "--message-format=json"], cwd=ROOT,
                           env=environment, stdout=stream, check=True, timeout=1200)
        executables = []
        for line in messages.read_text().splitlines():
            item = json.loads(line)
            if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test") and item.get("executable"):
                executables.append(item["executable"])
        if not executables:
            raise SystemExit("Cargo produced no test binaries")
        # Build scripts can also be instrumented; only test executions enter this profile.
        environment["LLVM_PROFILE_FILE"] = str(work / "test-raw/%m-%p.profraw")
        for executable in executables:
            subprocess.run([executable, "--quiet"], cwd=ROOT, env=environment, check=True, timeout=300)
        profiles = list((work / "test-raw").glob("*.profraw"))
        if not profiles:
            raise SystemExit("Tests emitted no coverage profiles")
        profile = work / "tests.profdata"
        subprocess.run([tools["llvm-profdata"], "merge", "-sparse", *map(str, profiles),
                        "-o", str(profile)], check=True, timeout=120)
        export = work / "rust.json"
        command = [tools["llvm-cov"], "export", f"--instr-profile={profile}",
                   "--ignore-filename-regex=/(.cargo/registry|rustc)/"]
        for executable in executables:
            command.extend(["--object", executable])
        with export.open("w") as stream:
            subprocess.run(command, stdout=stream, check=True, timeout=120)
        (args.output / "tools.txt").write_text(compiler + "\n" + output(["c++filt", "--version"]) + "\n")
        subprocess.run([sys.executable, "-B", str(Path(__file__).with_name("summarize.py")),
                        "--rust", str(export), "--output", str(args.output / "summary.json"),
                        "--baseline", str(Path(__file__).with_name("baseline.json"))],
                       cwd=ROOT, check=True, timeout=120)


if __name__ == "__main__":
    main()
