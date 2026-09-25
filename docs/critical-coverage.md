# Critical-path coverage baseline

H4.2 establishes a measured Linux baseline and CI ratchets for admission,
cancellation, identity, privacy, and package gates. The scope is explicit in
[`scope.json`](../scripts/coverage/scope.json), with reviewed counts in
[`baseline.json`](../scripts/coverage/baseline.json). Missing locations appear
in the CI `critical-coverage-rust` and `critical-coverage-packages` artifacts.
Reports expire after 14 days; the baseline and named regressions remain in git.

Coverage identifies code that tests did not execute. It does not prove that an
assertion is correct, a race is impossible, or a native package is safe. The
forced ordering and negative fixtures remain the behavioral evidence; these
measurements help find omissions in that evidence.

## What is measured

| Surface | Measurement | Recorded baseline |
| --- | --- | --- |
| Discovery admission | Rust source regions in two modules | 81.98%–97.69%, with per-file counts rather than one project percentage |
| Resolver, controller, source retirement and transport | Rust source regions in four modules | 88.96%–94.97% |
| Identity registry, device identity, protocol, metadata and lineup | Rust source regions in five modules | 84.62%–96.69% |
| JSON failure conversion and logging setup | Rust source regions | 30/30 and 11/11 respectively |
| Native playback failure classification and diagnostics | Rust source regions | 420/495 (84.85%); display-backed diagnostics remain a visible gap |
| Settings schema and pinned profile transactions | Rust source regions | 218/228 (95.61%) and 546/687 (79.48%) |
| Settings session and bounded worker | Rust source regions | 106/142 (74.65%) and 160/181 (88.40%); window geometry needs a display |
| Mach-O closure parser and CLI | Python executable lines and branch edges | 202/205 lines (98.54%); 54/58 branches (93.10%) |
| Windows policy snapshots and whole-tree receipts | PowerShell executable lines in ten selected gate functions | 78.57%–100%, reported independently per function |
| Installer tool admission, paths, manifests and final gate | PowerShell executable lines in five selected gate functions | 93.94%–100%, reported independently per function |
| Package roots, PE headers, inspector invocation and process termination | PowerShell executable lines in six additional gate functions | Separate counts retain native fallback and timeout-path gaps; no branch percentage is inferred |

Rust uses `-C instrument-coverage`, Rust 1.98.0, and matching LLVM 22.1.8 tools.
The [Rust coverage guide](https://doc.rust-lang.org/rustc/instrument-coverage.html)
describes source regions and LLVM compatibility. This baseline does **not**
claim stable Rust branch coverage. Duplicate source regions from multiple
binaries or generic instantiations count once, covered if any instance ran.
Test functions and listed test-only helpers are excluded. Regions within
otherwise production functions are measured in their instrumented test build;
test-only hooks inside those functions may still contribute regions.

Python uses coverage.py 7.16.1 and its
[branch measurement](https://coverage.readthedocs.io/en/latest/branch.html).
PowerShell uses Pester 6.2.0's
[coverage profiler](https://pester.dev/docs/usage/code-coverage). The latter's
Cobertura `branch-rate` field is not used as a branch measurement. The report
counts actual line hits in the selected production gate functions. Test loaders
retain the original PowerShell AST source locations; reparsing function text
would make those helper calls invisible to coverage.

## Gaps found and addressed

The first measurement found no execution of the pinned installer-tool
admission function. New fixtures now accept the exact two pinned synthetic
files, then reject a relative directory, extra member, changed dependency and
missing dependency before tool execution. Coverage rose from 0/12 to 12/12
lines. The fake pins and inert bytes exist only in the test scope.
Additional regressions reject unsafe inspector invocation paths and prove that
the production termination helper stops a real owned process.

The native-parser report exposed missing CLI and dynamic-linker rejection
paths. New fixtures reject a non-Apple `LC_LOAD_DYLINKER`, verify the permitted
loader, exercise each inspection mode, and ensure invalid UTF-8 exits with a
controlled rejection rather than a traceback. Measured coverage rose from
174/201 to 198/201 lines and from 44/56 to 52/56 branches.
Subsequent reviewed changes preserve a bounded, value-free CLI rejection reason
and retain the primary installer failure when cleanup also fails. Their
regressions bring the parser to 202/205 lines and 54/58 branches, and the final
installer gate to 31/33 lines. The uncovered ceilings remain unchanged.

An independent Rust run found two regions that earlier tests reached only by
scheduling chance. New tests close both the network-change and command sources
before polling the controller, proving the former cannot starve shutdown, and
join a worker twice with the second deadline already expired. The ratchet keeps
its original ceilings; the tests make these ownership outcomes explicit.

The native diagnostic privacy correction replaces arbitrary plugin text with
closed labels and typed fields. Actual tracing-capture regressions raise that
module from 199/450 regions to 420/495 (84.85%); the uncovered ceiling tightens
from 251 to 75. A fresh full desktop coverage run verifies the updated count
and every other existing Rust threshold.
The later closed native error-code check brings this to 420/495 (84.85%),
retaining the 75-region uncovered ceiling.

H3.2 adds four settings surfaces to the ratchet without weakening the existing
18 Rust ceilings. The full measured run passes 665 library, 65 desktop, and
12 CLI tests; 21 display/hardware/child-entry tests remain intentionally ignored
in that run. The new baseline retains unexecuted OS-error, thread-creation,
poison-recovery, and display-dependent geometry paths as visible gaps.

The source-rejection follow-up serializes cancellation with transport publication.
Forced request/disconnection schedules and poisoned-admission tests measure
402/440 regions (91.36%) in source policy, up from 378/421. Its uncovered ceiling
tightens from 43 to 38, and the denominator floor now includes the added lock path.

Worker-captured tune observations raise source policy to 404/442 (91.40%) and
transport to 434/457 (94.97%), retaining their 38- and 23-region uncovered ceilings.
Real HTTP/appsrc tests cover populated timing slots, empty and rejected responses,
and cancellation before request polling. The full instrumented run passes 676
library, 65 desktop, and 12 CLI tests, with the same 21 ignored display/hardware/child
entries, and all 22 Rust ratchets pass. These timing observations do not establish
decoded or rendered media progress.

Remaining gaps are preserved in the reports rather than excluded to raise
percentages. Examples include native diagnostic bus paths, rare OS/thread
creation failures, some filesystem error outcomes, and cycle/limit branches in
the closure walk. New coverage work should select one of these behaviors and
add an assertion about its contract, not merely execute its lines.

The configuration-parent compatibility correction removes a redundant rejection
predicate and three instrumented regions. Existing-parent and alias/navigation
regressions measure 546/687 store regions (79.48%), tightening the uncovered
ceiling from 157 to 141. Other coverage thresholds remain unchanged.

Removing the unused friendly-name schema and API (#153) is a legitimate code
removal: the settings schema measures 218/228 regions (95.61%), down from
326/341. Its denominator floor drops with the deleted code and its uncovered
ceiling tightens from 15 to 10.

Retiring route-derived discovery (V2.9, #181) on 2026-09-24 removes four modules
and their baselines from the scope: `discovery/approval.rs`,
`discovery/approval/store.rs`, and `discovery/routed/linux.rs` are deleted, and
`discovery/routes.rs` now only hosts the network-change monitor. This is a
legitimate code removal. The controller runtime without its routed lane measures
1641/1838 regions (89.28%), from 2030/2276, and the discovery client without the
pinned routed probe measures 496/605 (81.98%), from 485/608. Both denominator
floors drop with the deleted code; the uncovered ceilings tighten from 246 to 197
and from 123 to 109. A same-host measurement of `main` showed which kept regions
only the routed suites had reached. New assertions now cover report merging, a
repeated reply, a refused send, the range-candidate probe, a registry rebuild
over its device limit, and the production settings-directory lookup. Every other
Rust threshold is unchanged.

Typed-subnet search (V2.4) adds `discovery/subnet.rs` (ceiling 30 of 623 regions) and
raises the other touched files' region totals with their ceilings unchanged.

## CI ratchet and review policy

The Linux desktop job measures Rust; Linux quality measures portable package
gates. A missing input, empty denominator, missing selected function, or failed
test fails the measurement. For each metric, CI rejects an increase in the
number of uncovered units or a reduction in the measured denominator. A scope
change requires an explicit baseline edit. Improvements may reduce the recorded
uncovered ceiling in their own PR.

The coverage compiler and tool versions are fixed separately from ordinary
stable-Rust build checks. Compiler changes can change source-region mappings;
update the measurements and explain such changes in a reviewed PR. Legitimate
code removal also requires reviewing the changed denominator. Do not weaken a
ceiling solely to clear a failing check.

The summarizer has focused tests proving duplicate regions do not inflate
counts, test functions are excluded, empty and missing data fail, and the
PowerShell report does not invent branch coverage.

## Reproduce locally

Install the desktop development dependencies from the README. The Rust runner
uses the matching `llvm-tools-preview` component when present, or exactly
matching system LLVM tools. GNU `c++filt` with Rust support is also required.

```bash
export TMPDIR="${TMPDIR:-/var/tmp}"
coverage_work="$(mktemp -d -p "$TMPDIR" balun-coverage.XXXXXX)"
rustup toolchain install 1.98.0 --profile minimal --component llvm-tools-preview
RUSTUP_TOOLCHAIN=1.98.0 python3 -B scripts/coverage/run-rust.py \
  --output "$coverage_work/rust"

python3 -m venv "$coverage_work/python"
PIP_CACHE_DIR="$coverage_work/pip-cache" "$coverage_work/python/bin/pip" \
  install --require-hashes --only-binary=:all: -r scripts/coverage/requirements.txt
export BALUN_COVERAGE_MODULES="$coverage_work/modules"
pwsh -NoProfile -Command 'Save-Module -Name Pester -RequiredVersion 6.2.0 -Repository PSGallery -Path $env:BALUN_COVERAGE_MODULES -Force'
"$coverage_work/python/bin/python" -B scripts/coverage/run-packages.py \
  --output "$coverage_work/packages" \
  --pester-module "$coverage_work/modules/Pester/6.2.0/Pester.psd1"
```

The runners delete their temporary build trees and raw profiles on exit. Keep
the small summaries for review, then remove `"$coverage_work"` when finished.
The Rust build and each test process have explicit timeouts. Package suites
also have process timeouts; exceeding one fails the check.

The coverage.py 7.16.1 requirement includes reviewed hashes for Linux CPython
3.12–3.14 wheels on x86_64 and aarch64. It has no default runtime dependencies
on those interpreters. CI uses the Ubuntu 24.04 runner's Python 3.12. Both CI and
the command above require hashes and binary wheels, so another platform,
interpreter, or source build needs a reviewed input update. Wheel bytes were
downloaded and independently hashed against exact-version PyPI metadata on
2026-09-19; offline resolution was checked for all six interpreter/architecture
pairs. This does not pin the host Python, pip/venv bootstrap, or PowerShell/Pester.

## Limits of this baseline

This is a Linux instrumented-test and portable-package baseline. It does not
measure Windows native C# interop, Apple loader internals, decoder code, shell
or external tool internals, or display/hardware tests that are ignored in the
ordinary suite. The native CI lanes, packaged acceptance (P4.1), and H4.3 are
separate requirements.
No native-platform result is inferred from a portable PowerShell line hit.

In particular, the 100% JSON conversion result says every measured region ran;
the value-free parser and error-chain assertions establish its privacy
behavior. The remaining native-diagnostics gaps stay visible for H3.5's trust
boundary review. Nothing here completes that review or changes a release
signing or provenance policy.
