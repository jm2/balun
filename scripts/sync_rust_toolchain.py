#!/usr/bin/env python3
"""Validate or synchronize Balun's authoritative Rust/MSRV declarations.

Dependabot proposes compiler-floor updates through
`build-aux/toolchain/rust-toolchain.toml`. Those proposals are only an input:
this helper coordinates Cargo.toml, the single exact MSRV CI input, and
developer documentation before the complete CI matrix decides whether the new
compiler is feasible. Jobs which intentionally track `stable` are not
compiler-floor authorities and are never rewritten here.

The `dtolnay/rust-toolchain` action reference is a separate supply-chain
boundary. Every workflow use must remain an immutable, matching full commit
from the action's permanent master history; compiler synchronization never
rewrites that action commit.
"""

from __future__ import annotations

import argparse
import re
import sys
import tomllib
from pathlib import Path
from typing import Iterable


REPOSITORY = Path(__file__).resolve().parents[1]
MANIFEST = REPOSITORY / "Cargo.toml"
TOOLCHAIN_MANIFEST = (
    REPOSITORY / "build-aux" / "toolchain" / "rust-toolchain.toml"
)
CI = REPOSITORY / ".github" / "workflows" / "ci.yml"
WORKFLOW_DIRECTORY = REPOSITORY / ".github" / "workflows"
README = REPOSITORY / "README.md"

LINE_VERSION = re.compile(r"^[1-9][0-9]*\.[0-9]+$")
RELEASE_VERSION = re.compile(r"^([1-9][0-9]*\.[0-9]+)\.0$")
TOOLCHAIN_ACTION_USE = re.compile(
    r"(?m)^\s*uses:\s*dtolnay/rust-toolchain@[^\r\n]+$"
)
EXACT_TOOLCHAIN_ACTION = re.compile(
    r"(?m)^\s*uses:\s*dtolnay/rust-toolchain@([0-9a-f]{40})\s+#\s*master\s*$"
)
EXACT_CI_TOOLCHAIN = re.compile(
    r'(?m)^(\s*toolchain:\s*)([1-9][0-9]*\.[0-9]+\.0)\s*$'
)
MSRV_JOB = re.compile(r"(?m)^  msrv:\s*$")
STABLE_MSRV_CHECK = re.compile(r"(?m)^    name: MSRV\s*$")


class PolicyError(RuntimeError):
    """The Rust version declarations are incomplete or contradictory."""


def manifest_line_version(source: str | None = None) -> str:
    if source is None:
        source = MANIFEST.read_text(encoding="utf-8")
    manifest = tomllib.loads(source)
    value = manifest.get("package", {}).get("rust-version")
    if not isinstance(value, str) or LINE_VERSION.fullmatch(value) is None:
        raise PolicyError(
            "Cargo.toml package.rust-version must be a canonical X.Y string"
        )
    return value


def candidate_from_toolchain(source: str | None = None) -> str:
    if source is None:
        source = TOOLCHAIN_MANIFEST.read_text(encoding="utf-8")
    manifest = tomllib.loads(source)
    value = manifest.get("toolchain", {}).get("channel")
    if not isinstance(value, str):
        raise PolicyError("rust-toolchain.toml must define toolchain.channel")
    match = RELEASE_VERSION.fullmatch(value)
    if match is None:
        raise PolicyError(
            "rust-toolchain.toml toolchain.channel must be a canonical X.Y.0 release"
        )
    return match.group(1)


def read_workflows(*, ci_override: str | None = None) -> list[tuple[str, str]]:
    paths = sorted(WORKFLOW_DIRECTORY.glob("*.yml"))
    paths.extend(sorted(WORKFLOW_DIRECTORY.glob("*.yaml")))
    if not paths:
        raise PolicyError("no GitHub Actions workflows were found")

    workflows: list[tuple[str, str]] = []
    ci_path = CI.resolve()
    for path in paths:
        source = (
            ci_override
            if ci_override is not None and path.resolve() == ci_path
            else path.read_text(encoding="utf-8")
        )
        workflows.append((path.name, source))
    if not any(path.resolve() == ci_path for path in paths):
        raise PolicyError("the CI workflow is missing from the workflow directory")
    return workflows


def exact_action_pins(
    workflows: str | Iterable[tuple[str, str]],
) -> list[str]:
    """Return action commits after rejecting any non-exact workflow use."""

    if isinstance(workflows, str):
        named_sources = [("workflow", workflows)]
    else:
        named_sources = list(workflows)

    pins: list[str] = []
    for name, source in named_sources:
        uses = TOOLCHAIN_ACTION_USE.findall(source)
        exact = EXACT_TOOLCHAIN_ACTION.findall(source)
        if len(uses) != len(exact):
            raise PolicyError(
                f"{name} must pin every dtolnay/rust-toolchain use to a full "
                "lowercase SHA from permanent master history"
            )
        pins.extend(exact)

    if not pins:
        raise PolicyError("no dtolnay/rust-toolchain action references were found")
    if len(set(pins)) != 1:
        raise PolicyError(
            "all dtolnay/rust-toolchain uses must share one immutable action commit"
        )
    return pins


def exact_ci_releases(ci_source: str) -> list[str]:
    releases = EXACT_CI_TOOLCHAIN.findall(ci_source)
    values = [release for _, release in releases]
    if len(values) != 1:
        raise PolicyError(
            "CI must contain exactly one explicit X.Y.0 toolchain input "
            f"for MSRV; found {len(values)}"
        )
    return values


def require_unique(source: str, needle: str, description: str) -> None:
    count = source.count(needle)
    if count != 1:
        raise PolicyError(
            f"{description} must occur exactly once; found {count} copies of {needle!r}"
        )


def check_consistency_sources(
    manifest_source: str,
    toolchain_source: str,
    ci_source: str,
    readme_source: str,
    workflows: Iterable[tuple[str, str]],
) -> None:
    line = manifest_line_version(manifest_source)
    release = f"{line}.0"
    toolchain_line = candidate_from_toolchain(toolchain_source)
    if toolchain_line != line:
        raise PolicyError(
            f"rust-toolchain.toml release {toolchain_line}.0 does not match "
            f"Cargo.toml rust-version {line}"
        )

    exact_action_pins(workflows)
    releases = exact_ci_releases(ci_source)
    if releases != [release]:
        raise PolicyError(
            f"the explicit CI toolchain {releases[0]} does not match "
            f"Cargo.toml rust-version {line}"
        )
    if len(MSRV_JOB.findall(ci_source)) != 1:
        raise PolicyError("CI must contain exactly one 'msrv' job")
    if len(STABLE_MSRV_CHECK.findall(ci_source)) != 1:
        raise PolicyError(
            "MSRV job check name must remain stable as 'MSRV' for external gates"
        )

    for needle, description in [
        (f"Install Rust toolchain ({line})", "MSRV install step"),
        (f"rustc {line}", "MSRV rationale"),
        (
            "python3 scripts/sync_rust_toolchain.py --check",
            "CI Rust synchronization check",
        ),
    ]:
        require_unique(ci_source, needle, description)

    for needle, description in [
        (f"Rust {line} or newer", "README prerequisite"),
        (f"declared Rust {line} minimum", "README CI description"),
    ]:
        require_unique(readme_source, needle, description)


def check_consistency() -> None:
    check_consistency_sources(
        MANIFEST.read_text(encoding="utf-8"),
        TOOLCHAIN_MANIFEST.read_text(encoding="utf-8"),
        CI.read_text(encoding="utf-8"),
        README.read_text(encoding="utf-8"),
        read_workflows(),
    )


def replace_exactly(source: str, old: str, new: str, description: str) -> str:
    count = source.count(old)
    if count != 1:
        raise PolicyError(
            f"cannot safely update {description}: found {count} copies of {old!r}"
        )
    return source.replace(old, new)


def synchronize(target_line: str, *, update_toolchain_manifest: bool = False) -> None:
    if LINE_VERSION.fullmatch(target_line) is None:
        raise PolicyError("target Rust version must use canonical X.Y form")

    manifest_source = MANIFEST.read_text(encoding="utf-8")
    toolchain_source = TOOLCHAIN_MANIFEST.read_text(encoding="utf-8")
    ci_source = CI.read_text(encoding="utf-8")
    readme_source = README.read_text(encoding="utf-8")
    old_line = manifest_line_version(manifest_source)
    target_release = f"{target_line}.0"

    proposed_line = candidate_from_toolchain(toolchain_source)
    if update_toolchain_manifest:
        toolchain_source = replace_exactly(
            toolchain_source,
            f'channel = "{proposed_line}.0"',
            f'channel = "{target_release}"',
            "toolchain manifest channel",
        )
    elif proposed_line != target_line:
        raise PolicyError(
            f"requested Rust {target_line} does not match the toolchain manifest "
            f"proposal {proposed_line}"
        )

    manifest_source = replace_exactly(
        manifest_source,
        f'rust-version = "{old_line}"',
        f'rust-version = "{target_line}"',
        "Cargo rust-version",
    )

    exact_action_pins(read_workflows())
    exact_ci_releases(ci_source)
    ci_source, count = EXACT_CI_TOOLCHAIN.subn(
        lambda match: f"{match.group(1)}{target_release}", ci_source
    )
    if count != 1:
        raise PolicyError("could not normalize the exact MSRV toolchain input")
    ci_source = replace_exactly(
        ci_source,
        f"Install Rust toolchain ({old_line})",
        f"Install Rust toolchain ({target_line})",
        "MSRV install step",
    )
    ci_source = replace_exactly(
        ci_source,
        f"rustc {old_line}",
        f"rustc {target_line}",
        "MSRV rationale",
    )

    readme_source = replace_exactly(
        readme_source,
        f"Rust {old_line} or newer",
        f"Rust {target_line} or newer",
        "README prerequisite",
    )
    readme_source = replace_exactly(
        readme_source,
        f"declared Rust {old_line} minimum",
        f"declared Rust {target_line} minimum",
        "README CI description",
    )

    workflows = read_workflows(ci_override=ci_source)
    check_consistency_sources(
        manifest_source,
        toolchain_source,
        ci_source,
        readme_source,
        workflows,
    )

    TOOLCHAIN_MANIFEST.write_text(toolchain_source, encoding="utf-8")
    MANIFEST.write_text(manifest_source, encoding="utf-8")
    CI.write_text(ci_source, encoding="utf-8")
    README.write_text(readme_source, encoding="utf-8")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--check", action="store_true")
    group.add_argument(
        "--from-toolchain",
        action="store_true",
        help=(
            "adopt the exact Rust release proposed in "
            "build-aux/toolchain/rust-toolchain.toml"
        ),
    )
    group.add_argument("--set", metavar="X.Y", dest="target")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        if args.check:
            check_consistency()
        elif args.from_toolchain:
            synchronize(candidate_from_toolchain())
        else:
            synchronize(args.target, update_toolchain_manifest=True)
        print("Rust toolchain declarations are synchronized")
        return 0
    except (OSError, PolicyError, tomllib.TOMLDecodeError) as error:
        print(f"Rust toolchain policy failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
