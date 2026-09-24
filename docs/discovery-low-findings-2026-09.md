# Discovery low findings — September 17, 2026

This records the H3.3 dispositions against the earlier security review.
The fixes below are implemented in this change. On September 17, 2026, the
maintainer accepted both bounded exceptions below, including their ownership
and review triggers. H3.3 completes when this change lands on `main`.

Note (2026-09-24): V2.9 retired route-derived discovery and its approval store
([ADR-0003](architecture/adr-0003-retire-route-derived-discovery.md)). The
approval-store and fingerprint-key dispositions below are historical; the
jitter and CLI admission corrections still apply to `balun-discover --approved-range`.

## Implemented corrections

Routed discovery adds independently sampled, positive jitter of zero to 25%
of the existing target-start interval. The first target remains immediate.
Jitter cannot shorten that interval, raise the nominal packet rate, extend the
overall deadline, or bypass cancellation and final socket-authority checks.
An unavailable OS random source selects the maximum delay. This fallback
preserves the rate bound but cannot promise desynchronization in that case.
Virtual-time tests force maximum jitter, cancellation during the wait, and
deadline expiry; boundary tests cover every allowed rate and attempt count.

The CLI admits its complete argument list before running any action. Its
`--target` uses `ExactDiscoveryTarget`, so loopback (including mapped IPv6),
multicast, unspecified, broadcast, scoped/link-local IPv6, URLs, hostnames,
and caller-selected ports fail before a probe. Desktop and CLI exact probes
share `ProbeConfig::exact_target`: two attempts, 200 ms per reply window,
16 received datagrams, and one identity. Local/range probes retain their
separate budgets. An invocation accepts at most 32 actions and one approved
range. Tests cover rejected inputs, whole-command admission, and exact budgets.

The approval store reads a bounded schema header before its strict current
envelope. A well-formed newer document reports `UnsupportedSchema`, even if
its shape has changed, and remains byte-for-byte preserved. Malformed JSON,
duplicate version fields, and zero/current invalid versions retain
`InvalidState`. Every quarantine still refuses authority. Tests exercise
both categories through the actual private store and check preserved bytes.

## Maintainer-approved bounded acceptances

| Boundary | Accepted disposition | Owner and review trigger |
| --- | --- | --- |
| Library loopback support | Retain loopback-capable `DiscoveryClient` as a protocol primitive for existing real-socket fixtures and embedders. Both shipped application entry points require the stricter exact-target parser. This does not permit a user-entered loopback target in the desktop or CLI. | Owner: `jm2`. Revisit before beta and whenever a new network entry point is added. |
| Approval fingerprint key | Retain the key beside the state in the private profile. It prevents correlation of a copied state file without the key; it is not encryption or authentication against someone who reads/replaces both files. Neither belongs in diagnostic exports. Existing owner/mode, no-follow, pinned-directory, and mutation checks remain. | Owner: `jm2`. Revisit before beta, before exporting/syncing the directory, or if hostile same-user processes enter the threat model. |

The key's limitation is now explicit in the store's source documentation.
Moving it elsewhere in the same account alone would not provide a defensible
hostile-same-user boundary. No new credential storage or signing work is proposed.

The existing CLI `--approved-range` remains a separate explicit user command
sent through ordinary routing. This change does not grant desktop subnet
authority or increase the `/24`, candidate, attempt, concurrency, or deadline
limits; the proposed larger subnet search still requires V2.4 approval.
