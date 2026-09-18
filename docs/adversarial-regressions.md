# Sustained adversarial regression testing

H4.1 adds deterministic mutation fuzzing and generated properties to the normal
test suite. These tests call production parsers and pure policy transitions;
they do not expose new public parsing or approval APIs. They do not contact a
tuner, resolve a hostname, or execute an installer or extracted payload.

This is bounded mutation fuzzing, not coverage-guided fuzzing. It supplements
the existing forced concurrency, real-socket, native packaging, and malformed
input regressions. H4.2 separately measures missing coverage; P4.1 and H4.3
still require actual packaged hardware and accessibility evidence.

## Corpus and properties

| Suite | Seed corpus and generated cases | Required property |
| --- | --- | --- |
| Packets | The checked-in synthetic discovery golden reply; raw mutations, payload mutations inside a valid CRC frame, and generated unknown TLVs with both length encodings | Accepted identity remains concrete; frame/TLV work is bounded; iterators terminate and remain exhausted; unknown tags preserve identity |
| Metadata and URLs | Sanitized `discover-hdhr4-2us.json`, valid generated tuner counts/ignored fields, wrong field types, and numeric/hostname/credential/query/path URL seeds | Accepted metadata keeps the expected identity; JSON errors omit private values; every accepted URL is HTTP, responder-pinned, and free of credentials, query and fragment |
| Lineups | Sanitized `lineup-hdhr4-2us.json`, shuffled generated channel rows, duplicates, short limits and private mistyped fields | Accepted rows remain bounded, unique, sorted and responder-pinned; duplicate identity and excess rows fail; JSON diagnostics retain no private values |
| Route budgets | Generated private `/24` through `/32` routes, more-specific blockers, reordered routes, down interfaces and multiple explicit ranges | A blocker cannot add authority; input order cannot change targets; down interfaces add none; the aggregate candidate cap holds |
| Approval sequences | Two synthetic route fingerprints; 64 operations per case, including new/reused run IDs, stale/duplicate completions, rollback, expiry and explicit reapproval | Planning alone changes no authority; issued identities never repeat; active/cooldown reservations are respected; a topology change requires approval; stale work changes nothing |
| Native packages | Generated valid thin/universal Mach-O headers, dependency graphs, external imports and byte mutations | Valid bundled graphs pass; external imports fail; malformed input is rejected by the parser's expected error boundary; accepted headers/references remain bounded |
| Installer manifests | Generated file/directory records, permutations, missing/duplicate/unsafe/case-colliding entries, changed types/sizes/hashes | Record order is irrelevant; exact payload identity is required; invalid names and changed membership or content fail |

The JSON corpus already has a
[sanitization record](../tests/fixtures/hdhr/provenance.md). Other fixtures are
synthetic and constructed in the test code. Generated private markers are
deliberately fake. No raw network observations enter these suites or artifacts.

## Execution budgets

Ordinary Rust debug/release tests run 128 cases per property with seed
`20260917`. The Linux quality job also runs 128 native-package and 128 installer
manifest cases. Its overall timeout is 20 minutes.

[The extended workflow](../.github/workflows/adversarial.yml) runs daily at
04:31 UTC and supports manual dispatch. Scheduled runs use the workflow run ID
as a changing seed. Each of the five Rust properties and the native-package
property gets 8,192 cases; installer manifests get 2,048. This totals 51,200
cases, with a 15-minute job timeout. Tests retain the same input bounds in
extended runs; more cases do not expand network or package authority.

Mutation buffers are capped at 1,461 bytes for raw packets, 64 KiB for JSON,
4,097 bytes for URL inputs, and 8 KiB for native headers. Structured installer
cases contain at most 16 files plus directories. The production parsers retain
their own separate limits. Existing focused tests exercise their large-input
boundaries without allocating the largest allowed payload on every fuzz case.

Configuration rejects zero cases, invalid numbers, seeds outside the common
signed 64-bit range, and case ranges beyond 100,000. A failed property stops
its suite immediately. A job timeout also fails CI; it is never treated as
successful exhaustion of a corpus.

## Reproduction and retained regressions

Run the smoke suites from the repository root:

```bash
export TMPDIR="${TMPDIR:-/var/tmp}"
cargo test --locked --lib adversarial_ -- --nocapture
python3 -B scripts/test_adversarial_packages.py
pwsh -NoProfile -File scripts/test-adversarial-installer.ps1
```

Every case derives its own generator from the suite, seed, and case index.
Failures print those values and, when `BALUN_ADVERSARIAL_FAILURE_DIR` is set,
write a small replay record there. CI uploads only those records for 14 days;
it does not upload a profile, package tree, packet capture or original JSON.
The workflow log identifies the source commit and tool versions.

Check out that commit and replay the failing suite with the reported values:

```bash
export BALUN_ADVERSARIAL_SEED=35244415212
export BALUN_ADVERSARIAL_START=48
export BALUN_ADVERSARIAL_CASES=1
python3 -B scripts/test_adversarial_packages.py
```

Use the same Python/PowerShell versions when replaying their generators. For a
Rust failure, run `cargo test --locked --lib adversarial_ -- --nocapture` with
the same variables; suite names in the log identify the property. For a longer
local run set `BALUN_ADVERSARIAL_CASES=8192` and reset `START` to zero.

The maintainer triages a failure before merging. Minimize a real counterexample
into a named deterministic test or a sanitized checked-in corpus entry in the
fixing PR, retain the seed/case in that test's explanation, and rerun both the
replay and the full affected suite. A changed assertion requires justification
against the production contract; a flaky seed must not simply be excluded.

## Initial validation

The initial smoke suites passed locally on Linux. Extended runs used seed
`35244415212` with the scheduled corpus sizes. Native package properties inspect
synthetic bytes on Linux; the separate macOS and Windows jobs remain responsible
for native loader, sandbox, filesystem, and packaged runtime behavior.
