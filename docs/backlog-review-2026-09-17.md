# Backlog reassessment — 2026-09-17

Historical assessment of `main` at `068778b`, with the later CI repair recorded
below. Counts, defect descriptions, and pending decisions describe that snapshot;
the [current ledger](task.md) supersedes this assessment for implementation status.

## Decision

Retain all 58 outcomes in [the ledger](task.md), with 29 complete. The five H0
defects still apply to `main` at `068778b`. The nine hardening issues (#85–#93)
and five feature issues (#63, #64, #71, #74, #78) cover the adopted work without
duplicate issues. None can be closed from the current source alone.

Repair the existing CI failures before merging backlog changes. Continue with
the established H0 order, then eligible H1/H3 work. Keep assurance, measured
feature work, and physical acceptance subject to their existing prerequisites.
Implementation is assigned to Codex at the maintainer's request; the maintainer
retains the policy and physical-acceptance decisions listed below.

## Scope and evidence

The assessment compared every active ledger record with all 14 open GitHub
issue bodies and their comments, the open dependency PR #95, the adopted
September review, ADR-0002, and relevant application and packaging code. It
also inspected CI results for the preceding merged PR #94 and PR #95.

This is a reassessment of delivery scope and the cited defects. It does not
replace H3.4's security review at a fixed post-fix commit, audit all native
dependencies, or establish new physical-tuner or platform results.

| Track | Assessment and action |
| --- | --- |
| H0.1 / #85 | The pinned socket checks authority before awaiting `send_to`. Revalidate after readiness and on each nonblocking retry; retain the deterministic revocation, deadline, and pin-failure requirements. |
| H0.2 / #88 | Handoff consumption, worker startup/publication, and retirement still use separate state transitions. Serialize ownership and prove overlapping teardown joins every admitted worker before optimization. |
| H0.3 / #89 | `lookup_host` still runs on Tokio's blocking pool, and the controller implicitly drops its runtime. Bound actual outstanding work and shutdown, including timed-out waiters. |
| H0.4 / #86 | Final closure checks still accept unhandled install names and omit pixbuf loaders. Require exhaustive native-member inspection and final signed/reopened validation. |
| H0.5 / #87 | The Windows probe receipt still covers four anchors. Bind the entire tree or rerun the probe; keep completed-installer extraction separate. |
| H1.1 / #90 | Finished installer payload comparison remains absent and depends on H0.5. Resource/version inspection is insufficient. |
| H1.2 / #92 | The Windows policy loader still reads unbounded text without the shared digest. This independent correction can proceed once H0 is addressed. |
| H1.3–H1.4 / #91 | Inventory generation and advisory response are distinct outcomes under one issue. Keep both unchecked until final membership and the maintainer-approved response process are exercised. |
| H2 | Source identity, input pinning, provenance, and archive containment remain separate guarantees. The maintainer directed that all new signing/provenance work be held; H2.1 and H2.3 are paused, with no trusted identities designated. |
| H3.1 / #93 | Device and lineup errors still retain raw serde errors. Fix every public diagnostic path and the inaccurate historical URL-echo claim. |
| H3.2–H3.5 | Keep settings trust, older finding dispositions, refreshed evidence, and native-call/media-progress boundaries separate. An accepted risk requires a stated owner and review trigger, not an implementation checkbox alone. |
| H4 | Retain fuzz/property testing, meaningful coverage, and packaged accessibility as separate evidence tracks. Ordinary unit tests cannot complete screen-reader or packaged live acceptance. |
| V2.1–V2.3 / #63 | Measure before optimization; retain lifecycle prerequisites. Treat the old under-18 ms release measurement as an observation, not a proven universal bound. Pipeline reuse remains a decision informed by a prototype. |
| V2.4 / #71 | The issue now correctly states the proposed 512-candidate/1,024-datagram budget. A larger deadline and explicit approval contract still need a maintainer decision. |
| V2.5 / #78 | YADIF, progressive passthrough tests, and the method/output-frame-rate diagnostic already exist. Remaining work is comparative mixed-field/film/resource evidence and any resulting GPU decision. |
| V2.6 / #74 | Keep all 13 specified locales, pluralization, fallback, accessibility strings, and layout evidence. Verify the referenced Tributary implementation before choosing integration details. |
| V2.7 / #64 | Keep platform feasibility ahead of shell implementation. SDK, codec, distribution, licensing, and effort statements in the proposal are hypotheses to verify, not accepted evidence. |
| P0–P4 | Preserve the 29 historical completions and the single open P4.1. Development-build playback and a published alpha do not complete packaged acceptance. |

## Execution prerequisites and issue corrections

- [PR #96](https://github.com/jm2/balun/pull/96) merged at `794fd02`, resolving two
  existing CI failures: the compiler-floor manifest contained `1.98.1` despite
  the enforced `X.Y.0` policy, and the macOS closure required the unavailable
  `libgstfdkaac.dylib`. All ten CI jobs passed, including the native app/decoder
  probe with the retained AAC providers. This completed prerequisite does not
  complete H0.4; H0 is now the implementation focus.
- Review the independent dependency updates in
  [PR #95](https://github.com/jm2/balun/pull/95) after rebasing onto that repair.
  A bot's "review skipped" check is not a clean review; obtain an actual review
  and green CI before merging it.
- Reconcile #63's diagnostic wording with ADR-0002: permitted device labels,
  addresses, and DeviceID suffixes are distinct from prohibited credentials,
  query values, and stream URLs in GTK-facing snapshots. Preserve the private
  transport boundary. Replace its universal timing claim with measured targets.
- Rebaseline #78's diagnostics requirement on `deinterlace::describe` and the
  `pipeline playing` log, which already report method and output frame rate.
  Validate those diagnostics in remaining platform evidence.
- Mark #64's distribution and codec assumptions as unverified feasibility work;
  do not infer licensing or platform support from the proposed delivery channel.

## Work that needs maintainer input

| Record | Input needed before dependent work or completion |
| --- | --- |
| H2.1, H2.3 | Held by explicit maintainer direction on September 17: no designated signers and no new signing or provenance work. Resume only on a new instruction. Preserve the initial alpha exception. |
| H1.4, H3.3, H3.5 | Approval of concrete response deadlines, owned exceptions, and any remaining native-failure limitations after evidence is prepared. |
| V2.4 | Approval of one proposed candidate/attempt/rate/deadline/consent contract before implementing expanded scan authority. |
| P4.1, H4.3, V2.1, V2.5 | Versioned packaged tests on physical Linux Wayland/X11, macOS, and Windows systems with accessible tuners, plus screen-reader and visual/resource observations where required. Prepare candidates and sanitized instructions first. |
| V2.7 | Available target hardware/toolchains and a repository/distribution decision if feasibility supports implementing platform shells. |

Continue independent work while these decisions are pending. Hold the affected
implementation or acceptance; do not substitute silence for a decision, invent
hardware evidence, or mark partially implemented outcomes complete.

## Merge and accounting rules

Each repair or reviewable implementation slice uses a pull request. Before
merging, require the full applicable CI matrix to pass for its current source,
an actual completed bot review, and no unresolved actionable review findings.
Rerun affected checks and obtain review of substantive follow-up changes.
The maintainer has authorized merges under these conditions for this burndown.

An accepted assessment does not close an implementation outcome. Keep the
literal ledger count at 29/58 until code, relevant tests, documentation, and
required acceptance evidence land. For multi-record issues, close the issue
only when all adopted outcomes and acceptance criteria are satisfied.
