# Locked Node lint tools

This private npm project supplies only Balun's CI Markdown and TOML checks. It
uses markdownlint-cli2 **0.23.2** and @taplo/cli **0.7.0**, locking all **87**
dependency packages with exact versions, public registry tarball URLs, and
SHA-512 integrity values. The initial closure was resolved on 2026-09-19 with
Node 22.23.1 and npm 10.9.8. Node 22 or newer is required and enforced at install
time. No dependency declares an install script in this lock.

Upgrading the Markdown linter from 0.18.1 clears advisory findings in its
pinned js-yaml and markdown-it dependencies.
Version 0.23.2 also pins smol-toml at affected version 1.7.0;
a version-scoped override selects **smol-toml 1.8.0** for
[GHSA-7w5x-hrqm-74c2](https://github.com/advisories/GHSA-7w5x-hrqm-74c2).
Review/remove that override when changing the direct linter version. The final
lock's npm advisory query reported zero known vulnerabilities on 2026-09-19;
this point-in-time result is not a guarantee about future advisories.

CI copies both manifests into a fresh runner temporary directory and uses
`npm ci --ignore-scripts --engine-strict --include=dev --no-audit --no-fund`.
This requires the checked-in lock to agree with the manifest, verifies downloaded tarball
integrity, and disables dependency lifecycle scripts. The workflow invokes the
installed binaries directly from the repository root, preserving the existing
lint configuration and file arguments. It does not use `npx` to resolve a fresh
transitive tree on every run.

For a local check, keep generated files outside the repository:

```sh
lint_root=$(mktemp -d -p "${TMPDIR:-/var/tmp}" balun-node-lint.XXXXXX)
cp build-aux/toolchain/node-lint/package.json \
  build-aux/toolchain/node-lint/package-lock.json "$lint_root/"
npm ci --prefix "$lint_root" --cache "$lint_root/npm-cache" \
  --ignore-scripts --engine-strict --include=dev --no-audit --no-fund
"$lint_root/node_modules/.bin/markdownlint-cli2" "**/*.md"
"$lint_root/node_modules/.bin/taplo" check \
  Cargo.toml build-aux/toolchain/rust-toolchain.toml taplo.toml
"$lint_root/node_modules/.bin/taplo" fmt --check \
  Cargo.toml build-aux/toolchain/rust-toolchain.toml taplo.toml
rm -rf -- "$lint_root"
```

Update both manifests in one reviewed PR. Resolve the desired exact direct
versions with `npm install --package-lock-only --ignore-scripts`, review every
changed transitive version/URL/integrity field, then perform a fresh `npm ci`
and run both tools plus `npm audit --package-lock-only`. Check ordinary TOML
configuration loading when changing the parser override. Keep package lifecycle
scripts disabled. Dependabot proposes weekly grouped updates for this directory;
minor and patch updates merge automatically once the required checks pass, and
major updates wait for review.

The initial validation also removed a transitive lock entry and replaced a
tarball's integrity value in separate temporary copies. Installation rejected
the missing entry with `EUSAGE` and the downloaded hash mismatch with
`EINTEGRITY`. These checks concern the dependency lock and downloaded content,
not publisher identity. Node, npm, their bootstrap, and the hosted runner image
remain outside this pin; H2.2 is not complete. Signing/provenance remains deferred.
