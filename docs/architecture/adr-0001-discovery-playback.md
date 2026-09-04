# ADR-0001: Bounded native discovery and application-owned playback transport

- Status: Accepted
- Date: 2026-09-02
- Milestone: `v0.1.0`

## Context

Balun must find multiple HDHomeRun devices without merging their identities,
including devices reachable across a routed tunnel where link-local broadcast
and multicast do not cross. Discovery must remain quiet while idle, have an
inspectable packet budget, and never turn route or neighbor data into implicit
authority for a broad scan.

An authorized channel stream is plain HTTP from a validated device responder.
That does not make ambient proxy routing acceptable: sending a private tuner
address or channel path to a system-configured proxy would cross Balun's
responder-pinning boundary. The endpoint also must not appear in GTK-facing
state, GStreamer diagnostics, public errors, or native object properties.

Balun already uses `playbin3` with a private `gtk4paintablesink`. Retaining that
path preserves GStreamer's demuxing, decoding, synchronization, audio, stream
selection, and bus integration while leaving codec support to an explicit,
later package contract.

## Decision

### Discovery

Balun implements the documented HDHomeRun discovery frame, TLV fields, CRC,
and DeviceID validation in safe Rust behind its own discovery boundary. It
does not link `libhdhomerun` for v0.1.

One explicit local refresh sends tuner-only requests on eligible,
interface-bound sockets and collects replies for a fixed window. IPv4 uses the
Windows-compatible limited broadcast on Windows and the validated directed
broadcast elsewhere. Supported non-link-local IPv6 discovery is scoped to the
eligible interface. Results are deduplicated by DeviceID while retaining each
independently expiring locator and origin.

Routed discovery proceeds from least to most expansive authority:

1. probe exact cached, manually entered, or explicitly resolved targets;
2. inspect an active private tunnel route without sending packets;
3. preview and require remembered user approval for a bounded route run; and
4. offer exact-address or smaller-range entry when the route is too large.

Automatic IPv4 enumeration is capped at one `/24` and 256 candidates. It sends
only HDHomeRun UDP discovery frames, initially at 64 datagrams per second with
bounded concurrency, jitter, deadline, cancellation, cooldown, and backoff.
IPv6 prefixes are never enumerated. Public, default, loopback, link-local,
multicast, and directly connected LAN routes are ineligible. Approval is tied
to a privacy-preserving network fingerprint and is revoked when that identity
or monitored route authority changes.

No route-derived sender is enabled on a platform until its provider can bind
each packet to the approved interface/route and synchronously revoke that
authority. Exact numeric-address discovery remains the portable routed path in
the meantime.

### Playback

Balun retains `playbin3`, `decodebin3`, the private `gtk4paintablesink`, and
the generation-owned playback session. Production live playback will give
GStreamer only the fixed, endpoint-free URI `appsrc://balun`. `playbin3` must
resolve it to the exact built-in `appsrc` factory; the session rejects any
other or repeated source.

A Balun-owned HTTP transport consumes the existing one-shot opaque handoff and
keeps the responder-pinned URL entirely in Rust-owned private state. It builds
a `reqwest` client with automatic and explicit proxies disabled, redirects and
Referer disabled, a fixed user agent, and bounded connection and idle-read
deadlines. HTTP status and transport failures are reduced immediately to the
existing fixed playback categories; native or HTTP error text and endpoint
data are never forwarded.

The accepted `appsrc` is configured as a live, non-seekable byte stream with
fixed MPEG-TS caps and bounded byte/buffer capacity. A small bounded channel
separates asynchronous body reads from a dedicated blocking feeder, so neither
GTK's main context nor the controller runtime can block on GStreamer
backpressure. Arbitrary HTTP chunks are split to a fixed maximum buffer size.

Tune replacement, Stop, device change, terminal failure, and application
shutdown cancel the HTTP operation first, drop the response/socket, stop the
feeder, move the exact pipeline to `NULL`, and join every owned worker within a
common bound. Cancellation is not reported as EOS or a playback failure.
Natural EOF and malformed or truncated transport streams remain truthful,
typed terminal outcomes. A teardown that cannot prove both socket and pipeline
settlement quarantines the playback owner rather than starting a successor.

Implementation status: the `appsrc` transport replaced the intermediate
`souphttpsrc` policy on 2026-09-02 with proxy-trap, bounded-backpressure,
cancellation, rapid-replacement, and joined-teardown tests plus a `playbin3`
decode of the checked-in fixture from the constant URI on Linux. The macOS
lane runs the same loopback suite, and all three lanes run the helpers'
installed-runtime probes for the constant-URI `appsrc` contract, so the
transport record (archived M2.9) is complete; packaged-runtime probes are P3
work in [`task.md`](../task.md).

## Consequences and required evidence

- The default library and diagnostic remain free of GTK and GStreamer. The
  desktop feature drives the built-in `appsrc` through its schema-validated
  generic GObject properties and action signals instead of adding the typed
  `gstreamer-app` binding, so no additional native development dependency is
  required on any platform.
- A live `appsrc` feed does not post `playbin3` buffering messages; the
  session's buffering state stays reserved for runtimes that publish it and
  the connecting state covers preroll.
- The native GStreamer graph stores only a constant nonsecret URI. The device
  endpoint stays inside the one generation that owns its request.
- Direct transport is portable and testable without mutating process-global
  proxy configuration.
- Playback owns an additional HTTP worker and feeder lifecycle. Pipeline
  `NULL` alone is no longer sufficient proof that a tuner was released.
- A poisoned ambient-proxy test must show the trap receives zero requests while
  the exact local origin receives the stream request.
- Linux, macOS, and Windows development/package probes must prove that the
  supported GStreamer floor maps the constant URI to exact `appsrc` and accepts
  the configured live MPEG-TS feed.
- Tests must cover redirect refusal, 404, 503, other rejection, stalled body,
  unexpected EOF, bounded queue growth, cancellation while reads or pushes are
  blocked, rapid replacement, endpoint-free diagnostics, and joined teardown.
- If a supported runtime disproves the built-in `appsrc` URI contract, the
  fallback is a private safe-Rust `PushSrc`/`URIHandler` element. A manually
  assembled `appsrc ! queue2 ! decodebin3 ! playsink` graph is a later fallback
  because it substantially expands pad, stream-selection, A/V, and teardown
  ownership.

## Rejected alternatives

- **`libhdhomerun` as the primary discovery layer:** mature, but adds a C build
  and LGPL distribution surface while making Balun's exact packet budgets and
  route authority harder to express. It can be revisited if hardware evidence
  exposes a compatibility gap in the documented protocol.
- **Implicit subnet, neighbor-table, or permanent background scanning:** lacks
  user authority, creates noisy and surprising traffic, and does not scale to
  IPv6. Neighbor data may rank already-authorized exact candidates but cannot
  grant probe authority.
- **Depending on tunnel broadcast/multicast forwarding or cloud discovery:**
  neither is portable or necessary; exact and bounded approved unicast works
  across ordinary layer-3 routes without disclosing topology to a third party.
- **`souphttpsrc` with an empty proxy property:** clears only the element's
  explicit proxy. It does not prove that libsoup/GIO will bypass the system
  proxy resolver.
- **Replacing the process-global GIO proxy resolver:** global resolver choice
  is cached and environment-sensitive, and the investigated simple-resolver
  registration lacks a safe complete extension implementation. It would also
  affect unrelated application traffic.
- **A loopback HTTP relay:** still leaves the GStreamer-to-loopback hop subject
  to proxy behavior and adds listener, token, firewall, and shutdown surfaces.
- **A custom GStreamer source as the first choice:** feasible in safe Rust, but
  duplicates source lifecycle and URI-handler machinery already supplied by
  built-in `appsrc`.
- **Giving the device URI directly to GStreamer:** unnecessarily stores the
  endpoint in native properties and diagnostics and delegates routing to a
  proxy stack Balun cannot constrain portably.

## Revisit conditions

Revisit the discovery choice if sanitized physical-device fixtures demonstrate
that the documented protocol cannot identify a supported target within the
fixed budget. Revisit the playback source only if a supported GStreamer runtime
fails the explicit `appsrc` acceptance contract or measured live streams show
that its bounded push model cannot meet teardown and first-frame targets.
