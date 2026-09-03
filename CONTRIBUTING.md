# Contributing to Balun

Balun is a small project with a strict scope and a countable ledger. This page says how a change
gets from an idea to `main`; everything it points at lives in this repository.

## Build and test

Follow [Building from Source](README.md#building-from-source) for your platform and
[Testing & Code Quality](README.md#testing--code-quality) for the checks CI runs. The core
library and `balun-discover` build without GTK; the desktop application needs
`--features desktop`. Enable the formatting pre-commit hook once after cloning:

```bash
git config core.hooksPath hooks
```

Markdown, TOML, YAML, and workflow files are linted in CI with the configs at the repository
root; `npx --yes markdownlint-cli2@0.18.1 "**/*.md"` reproduces the Markdown check locally.

## Pull requests

- Work on a topic branch; nothing goes straight to `main`.
- Use conventional-commit subjects: `feat:`, `fix:`, `docs:`, `test:`, or `ci:`, and name the
  ledger record in parentheses when one applies, as in `docs: publish the support matrix (P4.4)`.
- CI must be green. CodeRabbit reviews every pull request automatically; fix or answer each
  finding and resolve its thread.
- Keep a pull request to one ledger record where possible. Split a large record into reviewable
  slices rather than weakening a contract to make it fit.
- The repository owner merges.

## Ledger discipline

- Work from the earliest unchecked record in [`docs/task.md`](docs/task.md) whose prerequisites
  are satisfied.
- Tick a record only when its code, deterministic tests, documentation, and changelog entry have
  all landed; partial work stays unchecked.
- Recount the `Current status` line whenever a record is added, split, completed, or removed.
- Records stay one to three lines. Evidence, measurements, and status prose go in
  [`docs/compatibility-v0.1.md`](docs/compatibility-v0.1.md), the changelog, or a design document.

## Documentation register

- Every user-visible outcome gets one README Features row, new or updated, and one
  `CHANGELOG.md` bullet under `[Unreleased]`.
- Detail lives in `docs/`: the plan, the playback contract, the compatibility notes, the support
  matrix, and the ADRs. README and changelog entries stay short and name no internal Rust types.
- Wrap prose at 100 columns; tables and code blocks are exempt.

## Contracts a change must not weaken

- **Network admission** — Discovery replies are accepted only from the probed prefix and port,
  device HTTP goes only to an accepted responder on the observed ports, and every routed scan
  needs explicit approval and stays inside its packet budget.
- **DeviceID identity** — One validated DeviceID per tuner; every channel is scoped to exactly
  one device, and lineups are never merged.
- **Tuner release** — Every switch, Stop, device change, and window close releases the stream
  inside the bound in [`docs/playback.md`](docs/playback.md).
- **Privacy** — `DeviceAuth` is never parsed, stored, or printed; stream URLs never reach the
  user interface; settings and approvals hold no credentials or raw topology.
- **Package inspection** — The [release component policy](docs/release-component-policy.md) and
  the artifact validators stay fail-closed.

## Hardware notes

Real-device observations go in `docs/compatibility-v0.1.md` and must not contain device IDs,
addresses, RF frequencies, guide numbers, channel names, credentials, or raw network topology.
Model names, firmware versions, and counts are fine. Fixtures follow the same rule; see
[`tests/fixtures/hdhr/provenance.md`](tests/fixtures/hdhr/provenance.md).

## Rust toolchain

The minimum supported Rust version is declared in `Cargo.toml`, mirrored in the README and the
MSRV CI job, and proposed by Dependabot through `build-aux/toolchain/rust-toolchain.toml`. Run
`python3 scripts/sync_rust_toolchain.py --check` after touching any of them, and raise the floor
only through that helper.

## License

Balun is licensed under [GPL-3.0-or-later](LICENSE). By contributing you agree that your
contribution is licensed under the same terms. There is no contributor license agreement.
