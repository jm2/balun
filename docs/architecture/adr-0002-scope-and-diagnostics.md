# ADR-0002: Hardware-first delivery order, guide deferral, and device-naming diagnostics

- Status: Accepted
- Date: 2026-09-02
- Milestone: `v0.1.0-alpha.1`

## Context

A holistic review on 2026-09-02 compared Balun's plan and ledger with
Tributary's. Three days of work had produced 46k lines, of which about 40% of
production code is routed-discovery authority with no caller, and 10.9k lines
of packaging helpers with no package, while the plan's own first milestone, the
real-hardware spike, was three-tenths done and the docs never recorded a real
channel playing. Live TV had in fact been tested by the owner on Windows and
worked, with ATSC 3.0 failing closed for lack of an AC-4 decoder.

The review also found that the "no endpoint text anywhere" rule protects a
non-secret. An HDHomeRun stream URL is `http://<address>:5004/auto/v<number>`
with no credential. The only secret the device exposes, `DeviceAuth`, is
already never deserialized. Endpoint-free errors cost diagnosability: an
unreachable tuner reported only "Device or stream unavailable".

## Decision

1. **Order.** The milestone plan is replaced by five dependency-ordered phases:
   P0 evidence and contract on real hardware, P1 viewer completion, P2 routed
   discovery on Linux, P3 packages, P4 the alpha release. Hardware evidence
   comes first because it decides the codec contract that packaging needs.
   The 64-record ledger is archived at 24/64 as
   [`task-foundation-2026-09.md`](../task-foundation-2026-09.md) and
   [`task.md`](../task.md) restarts with 30 records.
2. **Guide data moves to v0.2.** In-band PSIP/EIT, XMLTV, and the HDHomeRun
   XMLTV API leave v0.1. A one-day spike in P0 records whether guide tables
   survive the device's PID filter on an active stream; that outcome gates the
   v0.2 candidate.
3. **Routed-discovery authority stays and is completed in v0.1** for Linux:
   the existing approval store, observers, and sealed socket are connected to
   a monitored runner with approval and progress UX. macOS and Windows keep
   fail-closed providers and use exact or hostname targets.
4. **Errors and diagnostics may name the device.** Failure copy, the
   `balun-discover` output, and logs may include a device's friendly name,
   address, and DeviceID suffix. `DeviceAuth`, credentials, and query values
   remain redacted. Stream URLs still do not enter GTK-facing snapshots, and
   the `appsrc` transport with its no-proxy client is unchanged. No new
   zeroizing or typestate machinery is added for URL secrecy.

## Consequences

- `PlayerView` failure copy and the diagnostic output change under P1.3; tests
  that asserted endpoint-free text are rewritten to assert only that
  `DeviceAuth` and credentials are absent.
- The plan document keeps one dated "Current baseline" summary, refreshed when
  records complete, and no other implementation-status prose; countable
  status lives in `task.md`, evidence in `compatibility-v0.1.md`, and
  outcomes in the changelog.
- ADR-0001's discovery and playback decisions stand; only its consequence that
  endpoint data is "never forwarded" is narrowed to `DeviceAuth` and
  credentials.
- Guide-related module boundaries in the original plan are not created until
  v0.2 work starts.

## Rejected alternatives

- **Deleting the unwired routed-discovery code** would have removed about 9.6k
  production lines and their tests. The owner's two-site deployment is the
  primary use case for routed discovery, so the code is kept and finished
  rather than replaced by manual entry alone.
- **Keeping guide data in v0.1** would put an unproven data source ahead of
  packages and real-hardware acceptance.
- **Keeping endpoint-free errors** preserves a rule whose only beneficiary is
  a URL that contains nothing secret, at the cost of unusable error messages.

## Revisit conditions

Revisit the guide deferral when the P0.8 spike result is recorded. Revisit
the routed-discovery decision if P2 cannot reach a working Linux runner within
the documented traffic budget. Revisit device-naming diagnostics if a future
device firmware places credentials in lineup or stream URLs.
