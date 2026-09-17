# Native media failure boundary

H3.5 evidence, September 17, 2026. The isolated failure study is implemented;
the recovery policy and any accepted residual limits remain a maintainer decision.
H3.5 remains unchecked. This does not establish a new decoder sandbox.

## Measured synchronous-call limit

`GstreamerPipeline::stop` cancels the owned transport, sets native properties,
collects native diagnostics, calls `set_state(NULL)`, and then waits for state
settlement and worker joins against its five-second deadline. The deadline can
limit those later waits only after earlier synchronous calls return.

[GStreamer's state API](https://gstreamer.freedesktop.org/documentation/gstreamer/gstelement.html)
defines a timeout for waiting on asynchronous state changes. Its
[state design](https://gstreamer.freedesktop.org/documentation/additional/design/states.html)
also runs element transition callbacks during a state request. A later timed
wait cannot interrupt a callback that is still executing in `set_state`.

The test `synchronous_native_stalls_outlive_the_later_state_wait_deadline` starts
an owned child containing a real GStreamer pipeline and a synthetic element.
Its transition callback deliberately parks forever, separately during startup
(`NULL` to `READY`) and teardown (`READY` to `NULL`). After the child confirms
entry into that callback, the parent observes it for the production five-second
bound plus 200 ms. Both children remain blocked and require process termination.
The parent kills and reaps only its own child, including on assertion failure.

This passed locally with GStreamer 1.28.7 on Linux. It is controlled fault
injection, not evidence that a shipped codec currently hangs. It tests the native
API call ordering used by the application, not every callback or GUI operation.
The ordinary Linux desktop and macOS playback suites run the parent test;
Windows CI now runs the desktop suite through the native build helper on both
architectures. Platform success must come from those jobs, not this Linux result.

The child fixture is ignored as a standalone test. The parent launches it by
exact name with private test-only environment variables. Directly running the
ignored suite without those variables returns immediately. The fixture accepts
no media, launches no tuner request, and is absent from production builds.

```bash
export TMPDIR="${TMPDIR:-/var/tmp}"
cargo test --locked --features desktop --lib native_failure_study -- --nocapture
```

## Other current boundaries

The application supplies GStreamer a constant internal source URI and owns the
HTTP transport. This protects endpoint handling; it does not make decoder input
trusted. GStreamer, plugins, codecs, graphics drivers, and sinks run inside the
application process. Rust's prohibition on unsafe code in this crate does not
isolate their memory access, crashes, or unbounded native execution.

Transport connection, header, and idle-read timeouts cover network progress.
Receiving the first body bytes starts the live source clock. Neither event proves
a decoded frame, audible samples, or continued useful media. The current session
has no independent first-frame or useful-media progress deadline. V2.1 must
measure these phases and define missing-progress behavior before claiming that
a trickle of unusable bytes is bounded by an end-to-end tune timeout.

Native property access, diagnostic queries, state transitions, and finalizers
can all execute synchronous native code. Moving only `set_state` to a detached
thread would leave those calls and resource ownership unresolved. A timer on the
same blocked GTK context also cannot supply independent recovery.

## Decision required

The maintainer must choose between the following concrete scopes before H3.5
can be completed:

- Retain the current in-process decoder boundary with explicit limited claims.
  The five-second value governs asynchronous settlement and owned-worker waits
  after synchronous native work returns; it is not a universal close/release
  guarantee under a native hang. Require useful-media instrumentation and revisit
  the boundary before beta. Proposed owner: `jm2`.
- Require recovery from an indefinitely blocked native call. Prototype an owned
  decoder process with an independent supervisor and enforce kill/reap deadlines.
  Re-establish one-stream ownership, rendering/audio integration, endpoint
  privacy, process resource limits, and platform packaging before adopting it.
  A child process alone is not a security sandbox; a confinement policy would
  need its own concrete threat model and platform tests.

Neither choice is treated as approved here. Once chosen, reconcile the playback,
security, support, and release claims in H3.4, and retain the stall fixture as
evidence of the boundary that the implementation actually enforces.
