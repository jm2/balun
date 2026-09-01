#!/usr/bin/env python3
"""Deterministic tests for Balun's Rust compiler synchronization policy."""

from __future__ import annotations

import sys
import unittest
from contextlib import contextmanager
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Iterator
from unittest.mock import patch


SCRIPT_DIRECTORY = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIRECTORY))

import sync_rust_toolchain  # noqa: E402


ACTION_SHA = "a" * 40


def ci_fixture(line: str, action_sha: str = ACTION_SHA) -> str:
    return f"""name: CI

jobs:
  quality:
    name: Linux quality
    steps:
      - name: Test Rust toolchain policy
        run: |
          python3 scripts/test_rust_toolchain_policy.py
          python3 scripts/sync_rust_toolchain.py --check
      - name: Install Rust
        uses: dtolnay/rust-toolchain@{action_sha} # master
        with:
          toolchain: stable

  msrv:
    # Cargo.toml's declared floor is rustc {line}.
    name: MSRV
    steps:
      - name: Install Rust toolchain ({line})
        uses: dtolnay/rust-toolchain@{action_sha} # master
        with:
          toolchain: {line}.0

  platform-smoke:
    steps:
      - name: Install Rust
        uses: dtolnay/rust-toolchain@{action_sha} # master
        with:
          toolchain: stable
"""


def readme_fixture(line: str) -> str:
    return (
        f"Balun currently requires Rust {line} or newer.\n"
        f"CI verifies the declared Rust {line} minimum.\n"
    )


@contextmanager
def policy_fixture(
    *, cargo_line: str = "1.94", proposal_line: str = "1.95"
) -> Iterator[dict[str, Path]]:
    with TemporaryDirectory() as temporary:
        root = Path(temporary)
        workflow_directory = root / ".github" / "workflows"
        workflow_directory.mkdir(parents=True)
        manifest = root / "Cargo.toml"
        toolchain = root / "build-aux" / "toolchain" / "rust-toolchain.toml"
        ci = workflow_directory / "ci.yml"
        release = workflow_directory / "release.yml"
        readme = root / "README.md"

        manifest.write_text(
            '[package]\nname = "fixture"\nversion = "0.0.0"\n'
            f'rust-version = "{cargo_line}"\n',
            encoding="utf-8",
        )
        toolchain.parent.mkdir(parents=True)
        toolchain.write_text(
            f'[toolchain]\nchannel = "{proposal_line}.0"\nprofile = "minimal"\n',
            encoding="utf-8",
        )
        ci.write_text(ci_fixture(cargo_line), encoding="utf-8")
        release.write_text(
            "steps:\n"
            f"  - uses: dtolnay/rust-toolchain@{ACTION_SHA} # master\n"
            "    with:\n"
            "      toolchain: stable\n",
            encoding="utf-8",
        )
        readme.write_text(readme_fixture(cargo_line), encoding="utf-8")

        paths = {
            "manifest": manifest,
            "toolchain": toolchain,
            "ci": ci,
            "release": release,
            "readme": readme,
            "workflows": workflow_directory,
        }
        with patch.multiple(
            sync_rust_toolchain,
            MANIFEST=manifest,
            TOOLCHAIN_MANIFEST=toolchain,
            CI=ci,
            WORKFLOW_DIRECTORY=workflow_directory,
            README=readme,
        ):
            yield paths


class RustToolchainPolicyTests(unittest.TestCase):
    def test_candidate_comes_from_exact_patch_zero_release(self) -> None:
        self.assertEqual(
            sync_rust_toolchain.candidate_from_toolchain(
                '[toolchain]\nchannel = "1.94.0"\nprofile = "minimal"\n'
            ),
            "1.94",
        )

    def test_nonrelease_toolchain_channel_fails_closed(self) -> None:
        for channel in ["stable", "1.94", "1.94.1", "01.94.0"]:
            with self.subTest(channel=channel):
                with self.assertRaises(sync_rust_toolchain.PolicyError):
                    sync_rust_toolchain.candidate_from_toolchain(
                        f'[toolchain]\nchannel = "{channel}"\n'
                    )

    def test_action_pins_must_be_full_matching_master_commits(self) -> None:
        valid = (
            f"uses: dtolnay/rust-toolchain@{ACTION_SHA} # master\n"
            f"uses: dtolnay/rust-toolchain@{ACTION_SHA} # master\n"
        )
        self.assertEqual(
            sync_rust_toolchain.exact_action_pins(valid), [ACTION_SHA, ACTION_SHA]
        )

        invalid_sources = [
            valid.replace(ACTION_SHA, "a" * 12, 1),
            valid.replace(ACTION_SHA, "b" * 40, 1),
            valid.replace("# master", "# 1.94.0", 1),
            valid.replace(ACTION_SHA, ACTION_SHA.upper(), 1),
            "uses: dtolnay/rust-toolchain@master\n",
            "name: no toolchain action\n",
        ]
        for source in invalid_sources:
            with self.subTest(source=source):
                with self.assertRaises(sync_rust_toolchain.PolicyError):
                    sync_rust_toolchain.exact_action_pins(source)

    def test_exact_msrv_input_is_unique(self) -> None:
        source = ci_fixture("1.94")
        self.assertEqual(
            sync_rust_toolchain.exact_ci_releases(source), ["1.94.0"]
        )
        second_exact = source.replace(
            "toolchain: stable", "toolchain: 1.94.0", 1
        )
        with self.assertRaises(sync_rust_toolchain.PolicyError):
            sync_rust_toolchain.exact_ci_releases(second_exact)

    def test_toolchain_proposal_updates_only_authoritative_input(self) -> None:
        with policy_fixture() as paths:
            target = sync_rust_toolchain.candidate_from_toolchain()
            sync_rust_toolchain.synchronize(target)
            sync_rust_toolchain.check_consistency()

            manifest = paths["manifest"].read_text(encoding="utf-8")
            ci = paths["ci"].read_text(encoding="utf-8")
            release = paths["release"].read_text(encoding="utf-8")
            readme = paths["readme"].read_text(encoding="utf-8")
            toolchain = paths["toolchain"].read_text(encoding="utf-8")

            self.assertIn('rust-version = "1.95"', manifest)
            self.assertIn('channel = "1.95.0"', toolchain)
            self.assertEqual(ci.count("toolchain: 1.95.0"), 1)
            self.assertEqual(ci.count("toolchain: stable"), 2)
            self.assertEqual(release.count("toolchain: stable"), 1)
            self.assertEqual((ci + release).count(ACTION_SHA), 4)
            self.assertNotIn("1.94", ci + readme + manifest)

    def test_manual_set_updates_proposal_manifest_too(self) -> None:
        with policy_fixture(proposal_line="1.94") as paths:
            sync_rust_toolchain.synchronize(
                "1.95", update_toolchain_manifest=True
            )
            sync_rust_toolchain.check_consistency()
            self.assertIn(
                'channel = "1.95.0"',
                paths["toolchain"].read_text(encoding="utf-8"),
            )

    def test_proposal_mismatch_fails_without_writes(self) -> None:
        with policy_fixture() as paths:
            before = {
                name: path.read_bytes()
                for name, path in paths.items()
                if name != "workflows"
            }
            with self.assertRaises(sync_rust_toolchain.PolicyError):
                sync_rust_toolchain.synchronize("1.96")
            after = {
                name: path.read_bytes()
                for name, path in paths.items()
                if name != "workflows"
            }
            self.assertEqual(after, before)

    def test_stable_check_name_is_required(self) -> None:
        with policy_fixture(proposal_line="1.94") as paths:
            ci = paths["ci"].read_text(encoding="utf-8")
            paths["ci"].write_text(
                ci.replace("    name: MSRV", "    name: MSRV (1.94)"),
                encoding="utf-8",
            )
            with self.assertRaises(sync_rust_toolchain.PolicyError):
                sync_rust_toolchain.check_consistency()


if __name__ == "__main__":
    unittest.main()
