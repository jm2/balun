#!/usr/bin/env python3
"""Enforce reviewed compiler selection in the rustup-based release jobs (PyYAML).

The manifest is the one declaration. Each release job reads its channel with the
exact reviewed step below and passes that step's output to the Rust action, so a
Dependabot proposal changes only the manifest and can pass CI unaided.
"""

import argparse
from pathlib import Path
import re
import sys
import tomllib

import yaml

from check_builder_images import UniqueLoader

MANIFEST = Path("build-aux/release-toolchain/rust-toolchain.toml")
WORKFLOW = Path(".github/workflows/release.yml")
JOBS = frozenset({"build", "macos", "windows", "linux-deb", "linux-rpm"})
RELEASE = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
READ_ID = "release-rust"
READ_SCRIPT = (
    """toolchain="$(sed -n 's/^channel = "\\([0-9.]*\\)"$/\\1/p' """
    """build-aux/release-toolchain/rust-toolchain.toml)"\n"""
    'test -n "$toolchain"\n'
    'echo "toolchain=$toolchain" >> "$GITHUB_OUTPUT"\n'
)
TOOLCHAIN_INPUT = "${{ steps.release-rust.outputs.toolchain }}"


class Invalid(ValueError):
    """Release compiler declarations are incomplete or inconsistent."""


def require(condition):
    """Reject a policy mismatch without reflecting workflow input in output."""
    if not condition:
        raise Invalid("release compiler policy mismatch")


def selection(root):
    """Read an exact release and enforce compatibility with the declared floor."""
    manifest = tomllib.loads((root / MANIFEST).read_text())
    require(set(manifest) == {"toolchain"})
    toolchain = manifest["toolchain"]
    require(isinstance(toolchain, dict) and set(toolchain) == {"channel", "profile"})
    release = toolchain["channel"]
    require(isinstance(release, str) and RELEASE.fullmatch(release) is not None)
    require(toolchain["profile"] == "minimal")
    package = tomllib.loads((root / "Cargo.toml").read_text()).get("package")
    require(isinstance(package, dict))
    floor = package.get("rust-version")
    require(isinstance(floor, str) and re.fullmatch(r"[1-9][0-9]*\.(0|[1-9][0-9]*)", floor))
    require(tuple(map(int, release.split("."))) >= tuple(map(int, floor.split("."))) + (0,))
    return release


def check(root):
    """Require each recorded release job to install exactly the manifest's release."""
    release = selection(root)
    workflow = yaml.load((root / WORKFLOW).read_text(), Loader=UniqueLoader)
    require(isinstance(workflow, dict) and isinstance(workflow.get("jobs"), dict))
    observed = set()
    for job, definition in workflow["jobs"].items():
        require(isinstance(definition, dict))
        steps = definition.get("steps", [])
        require(isinstance(steps, list))
        read = False
        for step in steps:
            require(isinstance(step, dict))
            if step.get("id") == READ_ID:
                # One exact reviewed read per job, before its install.
                require(job in JOBS and not read)
                require(step.get("shell") == "bash" and step.get("run") == READ_SCRIPT)
                read = True
                continue
            action = step.get("uses", "")
            require(isinstance(action, str))
            if not action.lower().startswith("dtolnay/rust-toolchain@"):
                continue
            # The existing synchronization policy checks the shared action SHA.
            # Here, a second install in a job or an unrecorded job is a drift.
            require(job in JOBS and job not in observed and read)
            options = step.get("with")
            require(isinstance(options, dict) and options.get("toolchain") == TOOLCHAIN_INPUT)
            observed.add(job)
    require(observed == JOBS)
    return release


def main():
    """Check trusted repository files without downloading or installing a toolchain."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        release = check(args.root)
    except (OSError, ValueError, TypeError, RecursionError, yaml.YAMLError):
        print("Release Rust policy rejected: keep every release job reading the manifest",
              file=sys.stderr)
        return 1
    print(f"Reviewed Rust {release} selection is read from the manifest by all "
          f"{len(JOBS)} rustup release jobs")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
