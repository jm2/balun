# Codebase review and backlog expansion proposal

Reviewed: 2026-09-09 (US Eastern), completed against the September 10 UTC issue snapshot.

## Adoption status

The maintainer accepted the task expansion after this review. [The active ledger](task.md)
now contains 21 H records for defects and assurance, seven V2 roadmap records, and the
original 30 P records. Its literal count is 29/58; the historical P count remains 29/30.
P4.1 is carried forward once. All newly adopted implementation records remain unchecked.

The five P2 findings are tracked in #85–#89. Additional adopted work has separate issues:
[finished installer inspection #90](https://github.com/jm2/balun/issues/90),
[native inventory and advisory response #91](https://github.com/jm2/balun/issues/91),
[Windows policy parity #92](https://github.com/jm2/balun/issues/92), and
[value-free JSON diagnostics #93](https://github.com/jm2/balun/issues/93).
The first two are assurance gaps; the latter two are P3 defects. Creating their records
does not mean the underlying fixes, platform acceptance, or assurance checks have landed.

Issues #63, #64, #71, #74, and #78 now include their adopted ledger scope and prerequisites.
The inventory tables below preserve the review's original baseline; the current issue bodies
correct the subnet budget, recognize implemented YADIF, and gate optimization on lifecycle fixes.

## Assessment

**At the initial review snapshot, `task.md` and the issues were incomplete for continued
development.** The original 29/30 ledger records historical v0.1.0 delivery rather than
current security assurance or the newer roadmap. The adopted H and V2 tracks now make
the identified follow-up work explicit while preserving that history.

The implementation has substantial defensive structure: bounded parsers and transports,
responder-pinned HTTP, device-scoped identity, explicit scan approval, generation checks,
atomic settings, and extensive deterministic tests. The important omissions concern races
at asynchronous ownership boundaries, proof that packaged runtimes match validated inputs,
and maintenance of the security evidence after new features land.

This is a source and local verification review, not a certification or a finding that all
published artifacts are vulnerable. Priorities below distinguish demonstrated defects from
proposed assurance work. No critical vulnerability or remote code execution was demonstrated.

## Scope and baseline

- Checkout: `1f7a1e4709e37895bf7cb41b85d3753742e75da7`, initially clean, on
  `codex/macos-runtime-fixes`.
- GitHub main at review: `5ea0f2fe3b6019de51c5739a0358f236f5d87379`. It is three commits
  ahead and changes only `build-aux/toolchain/rust-toolchain.toml` relative to the checkout;
  the reviewed application and packaging findings apply to that main revision.
- Three specialist subagents reviewed discovery/protocol/HTTP, playback/controller/UI,
  and packaging/CI. The primary review covered settings, diagnostics, documentation,
  issue deduplication, finding validation, and the proposal.
- The review combined architecture and trust-boundary inspection across the codebase with
  targeted local reproductions. It did not exhaustively exercise every branch or every
  dependency's native implementation.
- The GitHub issue inventory was fetched with all states and comments: five issues, all open,
  no closed issues and no issue comments at the initial snapshot. Pull requests were not
  counted as backlog issues. New findings were checked against that inventory before filing.
- Tests and reproductions used synthetic fixtures and loopback traffic. No tuner, private
  subnet scan, production deployment, or real hardware test was performed.

## Confirmed findings and GitHub tracking

All findings in this table are P2 / medium priority. Fix them before the next release that
claims the affected guarantee; they do not imply an emergency release or a demonstrated
compromise. The linked issues contain evidence, preconditions, and acceptance criteria.

| Finding | Evidence and practical limit | Tracking |
| --- | --- | --- |
| Routed authority can expire while a UDP send is pending | `src/discovery/routed/linux.rs:112` verifies authority before awaiting socket readiness. A loopback reproduction of that ordering sent after revocation; a second reproduction included the outer cancellation select. The native Linux pin and full routed runner were not executed here. | [#85](https://github.com/jm2/balun/issues/85) |
| macOS native closure validation accepts an external dependency | `scripts/build-macos.sh:879` filters dependency names; the final cases at line 963 are not exhaustive and omit pixbuf loaders from their input set. A real minimal Mach-O app passed both gates, then failed to launch when its external fixture dylib was absent. This does not establish that a published DMG has such a reference. | [#86](https://github.com/jm2/balun/issues/86) |
| Windows installer reuse is not bound to the complete probed payload | The receipt at `scripts/build-windows.ps1:2108` hashes four anchors. Unchanged production receipt functions accepted replacement and deletion of a non-anchor runtime file. This proves stale receipt acceptance, not a full native Windows installer bypass. | [#87](https://github.com/jm2/balun/issues/87) |
| A retired source callback can start transport after teardown has taken its worker snapshot | `src/playback/source_policy.rs:215` consumes the handoff before starting/publishing transport, without coordinating with retirement at line 135. A forced overlap using the real callback/transport delivered a loopback stream GET after the first retirement returned `None`. The schedule was injected; real-hardware frequency is unknown. | [#88](https://github.com/jm2/balun/issues/88) |
| A timed-out hostname resolver can hold controller/window shutdown open | `src/discovery/hostname.rs:188` times out the async waiter, while the controller runtime at `src/controller/runtime.rs:453` is implicitly dropped and waits for blocking resolver work. An injected blocked resolver reproduced timeout followed by a blocked shutdown. No real DNS failure was induced. | [#89](https://github.com/jm2/balun/issues/89) |

The Windows receipt behavior was described as an implementation choice in the build notes;
its consequence for reuse of a previously probed tree had no issue. This is separate from
the already acknowledged gap in extracting and inspecting the finished installer payload.

### Reproduction notes

The routed reproduction first polled a fresh Tokio UDP send to `Pending`, revoked its
authority, and resumed it. Output was
`initial_poll=Pending authority_before_resume=false verify_calls=1 sent_bytes=19 received_bytes=19`.
With the production-style unbiased cancellation select also present, a 100-iteration run
observed 53 sends after both revocation and token cancellation. That count is a scheduling
observation, not an estimated probability in the application. The fix must check authority
around the actual nonblocking send attempt after readiness, including retries.

For macOS, unchanged helper functions inspected a harmless fixture app importing a dylib
outside its bundle. The final closure message reported zero missing references; the shared
component validator allowed it. Removing only the external fixture library produced
`Library not loaded` and exit -6. Complete closure resolution must cover every native member,
each architecture, actual rpaths, and final signed/reopened artifacts.

For Windows, the receipt check succeeded after changing and after deleting a synthetic
non-anchor GStreamer core DLL. Structural checks can reject some invalid files but cannot
bind a valid replacement's behavior to a previous runtime probe. Either a complete payload
manifest or a fresh probe is required; a local receipt is not an authenticity signature.

For source retirement, a temporary-copy test inserted a barrier after handoff consumption
and before transport startup. Retirement returned `None`; after resuming the real callback,
the real transport sent a stream GET to the test's listener while the policy remained
retired. A second retirement was necessary to retrieve and join the workers. The fix needs
one coordinated lifecycle transition; an extra atomic flag read alone leaves another race.

For hostname shutdown, a temporary-copy test substituted a controlled blocking resolver job
for the lookup expression, matching the work class used by locked Tokio 1.53.1. The normal
timeout arrived at five seconds, but shutdown was still waiting 300 ms later and completed
only when the blocking job was released. Define bounded resolver admission and shutdown,
including jobs whose async waiter has already timed out; stale results must remain inert.

## Coverage of the existing ledger and issues

| Existing record | Current assessment | Proposed action |
| --- | --- | --- |
| P0 evidence and P0.4 timing | Hardware observations exist, but some timings predate the paused startup hold; compatibility notes explicitly say to retake first-frame timing. | Preserve historical results; collect versioned package and per-phase measurements under P4.1 and #63. |
| P1 viewer completion | Core behavior is implemented and tested. Friendly-name storage exists, but there is no user naming flow; the plan calls it reserved storage. | Do not turn reserved storage into an unrequested feature promise. Track it separately only if wanted. |
| P2 routed discovery | Substantial implementation and hardware evidence exist; #85 identifies a gap in the immediate pre-send revocation guarantee. | Add a regression and a current security delta; do not infer correctness from the old completed checkbox. |
| P3 packages | Broad build and inspection infrastructure exists, but #86/#87 and acknowledged installer inspection gaps remain. | Separate package production from complete native dependency and payload assurance. |
| P4.1 packaged acceptance | Correctly remains unchecked. Linux/Windows recorded live trials used development builds; there is no complete cross-platform packaged result. | Carry the same obligation forward as a release gate, without creating a duplicate task. |
| P4.3 security review | A completed review of an older revision is historical evidence. The document still contains obsolete claims about unwired routed code, nonexistent macOS packaging, and diagnostic-only release output. | Publish a current review with explicit open findings and superseded statements, rather than treating the historical pass as permanent. |
| P4.5 release | Publishing is complete, but its text says “Pass every gate” while P4.1 remains open. | Record the actual alpha acceptance exception and stop describing publication as proof of all acceptance gates. |
| Beta deferrals | SBOM, provenance, fuzzing, and coverage are listed only as deferrals. | Give each an outcome, owner, milestone, and acceptance evidence. Native dependency inventory should precede the next assurance claim. |

### Existing GitHub feature issues

| Issue | Coverage and necessary adjustment |
| --- | --- |
| [#63: Faster channel tuning and switching](https://github.com/jm2/balun/issues/63) | Covers timing, coalescing, last-frame hold, pipeline reuse, and large lineups. Split measurement and low-risk UI work from pipeline changes. Speculative adjacent pre-tuning conflicts with the current one-stream/no-unrequested-allocation contract and needs a separate explicit design decision. |
| [#64: Mobile and TV ports](https://github.com/jm2/balun/issues/64) | Broad future epic, not a v0.1 completion item. Validate each platform's toolchain/network/sink boundary before adopting its effort estimates. The proposed GTK-free playback refactor is still relevant: the session and transport remain gated by `desktop`. |
| [#71: Search an approved subnet](https://github.com/jm2/balun/issues/71) | Proposes wider explicit authority than current route-derived discovery. Reconcile `/23` and 512 hosts with the existing 256-candidate cap and the full datagram/deadline budget before implementation. |
| [#74: i18n/l10n](https://github.com/jm2/balun/issues/74) | Covers translation framework and UI copy. Add pluralization, fallback, long-string layouts, accessibility labels, and catalog validation to its evidence; no duplicate issue needed. |
| [#78: Deinterlacing quality](https://github.com/jm2/balun/issues/78) | Its baseline says default linear filtering, but `src/playback/deinterlace.rs` now configures YADIF. Reconcile the implemented software step and retain explicit decisions/evidence for GPU paths, mixed interlace, film handling, and resource use. Do not close the whole issue solely because YADIF landed. |

For #71, 512 candidates at two requests each require up to 1,024 request datagrams. At the
current 64-datagram/second policy, a complete run needs roughly 16 seconds of request budget
plus response/teardown allowance, not the issue's roughly eight seconds. The default deadline
is 15 seconds. Approval copy, cooldowns, cancellation, candidate caps, and tests must use the
same reviewed budget. The existing implementation still rejects this larger scope.

## Additional adopted gaps and their limits

### Privacy and local state

- **P3: fixed diagnostic categories.** `DeviceHttpError::Json` and `LineupError::Json` can
  expose serde error text through inspection diagnostics. The older review acknowledges
  this but incorrectly says it cannot contain a URL. A synthetic mistyped `TunerCount`
  string reproduced a URL/query marker in the displayed error. Use value-free parse
  categories and regression fixtures with secret-shaped strings in every field. This does
  not demonstrate extraction of a genuine `DeviceAuth` value; that unknown field is ignored.
- **P3: settings file opening and replacement semantics.** The final-component symlink
  check in `SettingsStore::load` precedes an ordinary `File::open`. The interval and parent
  directory traversal are not an atomic no-follow guarantee. Define the local-user/shared
  directory threat model and test descriptor-based validation, concurrent replacement,
  newer-schema preservation after startup, and platform permissions if those guarantees
  are required. No cross-user exploit was demonstrated under the normal private profile.
- **P3: Windows policy loader parity.** The PowerShell loader accepts any nonempty syntactic
  token list, unlike the shared checksum-pinned loader. A one-token synthetic replacement
  loaded successfully. Add bounded, strict, regular-file snapshot validation and the shared
  digest. The immutable-source CI prepare gate is a compensating control; this is a local
  packaging consistency gap, not a demonstrated release workflow compromise.
- Give existing low findings an owner and disposition: sender jitter versus fixed pacing,
  CLI loopback/budget differences, approval-key anti-correlation limits, and newer-schema
  approval-store diagnostics. Revalidate their current status rather than copying an old list
  into a new “pass.”

### Release and dependency assurance

- Inventory the **shipped native runtime**, including GStreamer, FFmpeg, GTK, GLib, plugins,
  and helpers, with versions, source identity, hashes, and per-artifact membership. The
  Cargo audit does not cover these binaries. Define native advisory triage and rebuild
  expectations; distinguish bundled code from distro/Flatpak-managed runtime dependencies.
- Extract the finished Windows installer with a pinned, non-executing tool; compare the
  actual payload with the intended tree and repeat the component/import checks. Checking
  the executable's version resource is not payload inspection, as current policy admits.
- Harden native archive preflight and extraction containment before accepting artifacts from
  outside the trusted local build boundary. Current Linux validators explicitly assume
  trusted build outputs; do not characterize this deferred hardening as an exposed upload
  vulnerability.
- Turn signed tags, reviewed source ancestry, immutable builder images, tool/transitive
  input pinning, and provenance into explicit release policy decisions with machine checks.
  Existing checksums establish byte consistency; they do not establish builder identity.
  Preserve the documented initial unsigned-tag exception as history.
- Correct the older security review's claim that a privileged Flatpak container is simply
  container-local isolation. Its host is an ephemeral CI runner; a read-only token and no
  secrets are useful controls, but privileged containers are not a host security boundary.

### Reliability, testing, and maintainability

- Add deterministic scheduling tests at ownership transitions, not just calls made entirely
  before or after cancellation. Add sustained fuzz/property testing for TLV framing, JSON shapes,
  URL normalization, route budgets, approval state transitions, and package manifests.
- Under #63, measure time to decoded/rendered media and continued progress, including HTTP
  200 responses that keep delivering unusable or trickle bytes. The HTTP idle-read timeout
  does not by itself establish a useful-media progress deadline.
- Define the native-decoder threat model. Rust's `unsafe_code = "forbid"` does not sandbox
  GStreamer and its native codecs, which process network-supplied media in the app process.
  An isolated decoder process is a design candidate, not a confirmed missing bug fix.
- Fault-inject blocking native state changes and slow settings I/O. A timeout around a
  later wait does not necessarily bound a preceding synchronous native call. No native
  decoder hang was reproduced in this review; treat it as an explicit failure-mode study.
- Keep display-backed accessibility and lifecycle checks separate from ordinary desktop
  unit tests. Exercise keyboard focus, large fonts/long translations, error recovery,
  window close, device changes, and stale snapshot behavior on packaged candidates.
- Use targeted module boundaries for future refactors: controller scheduling, approval
  persistence/observation, and native package validation are complex. File size alone is
  not a bug; split code when doing the associated fixes and preserve behavior evidence.

## Adopted implementation order

The active ledger uses H0–H4 and V2 identifiers, with issue links where separate tracking
is useful. The maintainer owns triage and release decisions; the implementation owner is
recorded when work is claimed. Track names define target milestones without invented dates.
Records stay unchecked until their applicable evidence lands on main. Historical findings
and verification results in this document describe the reviewed revision, not later fixes.

| Order | Proposed outcome | Completion evidence |
| --- | --- | --- |
| 1 | Close #85, #88, and #89 as separate fix records. | Regression for each demonstrated schedule; cancellation, tuner ownership, resolver shutdown, and successor admission remain correct. |
| 2 | Close macOS closure and Windows receipt defects (#86/#87). | Negative native/payload fixtures, clean-runtime probes, and platform CI against the actual final tree. |
| 3 | Complete finished-installer inspection and Windows policy parity. | Reopened payload equals the validated tree; altered policy/inputs fail before packaging. |
| 4 | Refresh the security review and threat model. | Current commit, reviewed surfaces, links to tests/issues, and explicit owner/expiry for each accepted exception. |
| 5 | Produce native dependency inventory and advisory triage. | Per-release artifact-to-component mapping and a documented rebuild/triage decision for relevant advisories. |
| 6 | Enforce the chosen source/build authenticity policy. | Signature/ancestry tests, pinned builder/tool inputs or recorded exceptions, and provenance tied to final artifact hashes. |
| 7 | Add adversarial parser/state-machine and packaging regression coverage. | Bounded CI smoke corpus, scheduled extended runs, retained regressions, and measured coverage of critical branches. |
| 8 | Finish P4.1 on the actual candidates. | Linux Wayland/X11, macOS, and Windows launch/discover/tune/switch/Stop/close plus startup, idle, and switch measurements, with artifact hashes and sanitized evidence. |
| 9 | Reconcile #63/#71/#78 and prioritize the feature roadmap. | Updated baselines, approved authority/tuner budget decisions, dependencies, and measurable acceptance. |
| 10 | Schedule #74 and then independently gated mobile/TV milestones under #64. | Localization layout/accessibility evidence; platform-specific compile, network, sink, and lifecycle proofs before larger commitments. |

Orders 3–7 can proceed independently where their inputs permit. Runtime correctness and
artifact integrity should precede optimizations that reuse pipelines or broaden scanning.
P4.1 requires hardware and platform evidence beyond this source review; no checkbox was
completed on its behalf.

## Verification performed

| Check | Result and limit |
| --- | --- |
| `cargo test --locked --offline` | 422 library and 9 diagnostic tests passed. Initial sandbox run blocked ten localhost fixtures; rerunning with socket access passed. |
| `cargo test --locked --offline --features desktop --all-targets` | 516 library, 60 desktop-binary, and 9 diagnostic tests passed; 19 tests ignored for explicit hardware/display/runtime requirements. |
| `cargo clippy --all-targets --all-features --locked --offline -- -D warnings` | Passed. |
| Cargo audit 0.22.2 | 219 locked dependencies; zero advisories and zero warnings. RustSec database commit `b50980aad8b8f14f77e25a97b32dd94bf008b0af`, updated 2026-09-09. Native bundled dependencies were not audited by this command. |
| Python release/toolchain policy tests | 13 release tests and 8 toolchain tests passed. |
| macOS build/package policy fixtures | Passed; real Mach-O closure reproduction demonstrated the uncovered case. |
| Flatpak permission fixtures | Passed. |
| Windows routing, Linux helper routing, hardware privacy fixtures | Passed for applicable host-independent behavior. Linux helper fixtures used canonical `TMPDIR=/private/tmp` after a macOS symlink-path expectation mismatch. This was not native Windows/Linux execution. |
| Targeted source-retirement and resolver-shutdown regressions | Each passed in a temporary copy with deterministic scheduling/resolver instrumentation; no production files changed. |
| Shared component/Linux native package suites | Could not run under this host's BSD tooling; require the Linux/GNU CI environment. No passing result claimed. |

The host was macOS with Rust 1.98.1, GTK 4.22.4, libadwaita 1.9.3, and GStreamer 1.28.7.
No full release build, published-artifact inspection, live hardware acceptance, native Linux
route execution, native Windows execution, or comprehensive native dependency audit was
performed. Green tests establish the exercised behavior and do not invalidate the reproduced
gaps that lack regressions.
