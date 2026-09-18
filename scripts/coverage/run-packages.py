#!/usr/bin/env python3
"""Run the portable package gate suites using explicitly installed coverage tools."""

import argparse
import os
from pathlib import Path
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--pester-module", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="balun-package-coverage-",
                                     dir=os.environ.get("TMPDIR") or "/var/tmp") as temporary:
        work = Path(temporary)
        environment = dict(os.environ, COVERAGE_FILE=str(work / "python.data"),
                           XDG_CACHE_HOME=str(work / "cache"), PYTHONDONTWRITEBYTECODE="1")

        def run(command):
            subprocess.run(command, env=environment, cwd=ROOT, check=True, timeout=300)

        coverage = [sys.executable, "-m", "coverage"]
        run(coverage + ["run", "--branch", "--source=scripts", "-m", "unittest", "discover",
                        "-s", "scripts", "-p", "test_macos_native_closure.py"])
        run(coverage + ["run", "--append", "--branch", "--source=scripts", "scripts/test_adversarial_packages.py"])
        python_report, powershell_report = work / "python.json", work / "powershell.xml"
        run(coverage + ["json", "--include=*/macos_native_closure.py", "-o", str(python_report)])
        run(["pwsh", "-NoProfile", "-File", str(Path(__file__).with_suffix(".ps1")),
             "-PesterModule", str(args.pester_module.resolve()), "-OutputPath", str(powershell_report)])
        run([sys.executable, "-B", str(Path(__file__).with_name("summarize.py")),
             "--python", str(python_report), "--powershell", str(powershell_report),
             "--output", str(args.output / "summary.json"),
             "--baseline", str(Path(__file__).with_name("baseline.json"))])


if __name__ == "__main__":
    main()
