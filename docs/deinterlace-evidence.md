# Deinterlacing evidence and remaining gaps

V2.5 and [issue #78] remain open. Balun still selects software YADIF with
automatic field order, all fields, automatic mode, and no pattern locking.
This evidence changes the validation and claims, not that runtime policy.

## Synthetic results

The fixtures call the production configuration helper on a real GStreamer
`deinterlace` element. A live `appsrc` supplies synthetic I420 frames; a
non-synchronizing `fakesink` records output pixels, timestamps, and durations.
No tuner, broadcast recording, display, or network traffic is involved.

The buffer flags follow the documented
[GStreamer video-buffer contract](https://gstreamer.freedesktop.org/documentation/video/video-frame.html#GstVideoBufferFlags).
An interlaced buffer identifies its field order with TFF; its absence indicates
bottom-first. In mixed caps, the interlaced flag distinguishes an interlaced
buffer from a progressive one.

Local evidence on 2026-09-17 used Linux and GStreamer **1.28.7**:

| Fixture | Result and scope |
| --- | --- |
| Existing 528×480 mixed and 1920×1080 interleaved static detail | YADIF retains the two-line pattern and negotiates full field rate; the linear comparison blurs that pattern |
| Existing 1280×720 progressive caps | All 12 frames retain their pixels at the input frame rate |
| Mixed caps, 12 progressive buffers | Every single-line-detail frame passes once, with exact original pixels, timestamps, and durations |
| Mixed caps, top-first and bottom-first interlaced buffers | Eight fully surrounded input frames preserve the 16 successive field values and half-frame timing in both orders |
| Alternating progressive/interlaced runs in one stream | **Fails acceptance:** duplicate progressive content and nonmonotonic timestamps occur; some expected field values are absent |

The field-order fixture encodes a distinct increasing luminance in each field.
Its assertion covers the eight middle frames of a 12-frame run, so it cannot
be satisfied by merely reporting progressive output caps or producing a
minimum frame count. It does not claim correct startup or EOS cadence.

The passing fixtures are ordinary desktop tests on the native CI matrix. Local
execution passed all five normal deinterlacing tests, and native CI passed before
merging; a local Linux run is not evidence for macOS or Windows output.

## Unresolved transition reproducer

`mixed_stream_switches_between_progressive_and_interlaced_buffers` retains the
acceptance assertion that each progressive input appears exactly once with its
original timing. It is explicitly ignored in the normal suite because it fails
against the current local native runtime. Its presence is a recorded open
failure, not an accepted exception or evidence that V2.5 is complete.

```bash
cargo test --locked --features desktop --lib \
  playback::deinterlace::tests::mixed_stream_switches_between_progressive_and_interlaced_buffers \
  -- --ignored --exact --nocapture
```

The fixture feeds two progressive frames, two top-first interlaced frames, two
progressive frames, two bottom-first interlaced frames, and four progressive
frames. With the production settings, local GStreamer 1.28.7 emits 18 outputs
where the input sequence represents 16 display intervals. Progressive input 4
appears three times; the two field values of input 6 are absent. The printed
numeric content/timing trace also includes backward timestamp steps. The
acceptance command exits unsuccessfully at the first duplicate-frame assertion.

A separate all-interlaced run emitted 26 outputs for 24 input fields, with
timestamp anomalies at startup and EOS. Its middle-field checks pass. These are
observations of a direct native-element fixture; their exact effect in Balun's
decoder/playsink graph and on rendered broadcasts still needs measurement.

An experiment with the element's passive pattern-locking option did not remove
the transition duplicates and changed the duration of progressive buffers.
That experiment was reverted. There is no evidence here to adopt a new locking
policy, change the deinterlacer, or claim an inverse-telecine fix.

## Remaining acceptance

Keep the following work visible under V2.5/#78:

- Investigate the transition and startup/EOS timing behavior, retain a minimal
  reproduction, and validate any native-version or pipeline correction before
  promoting the ignored acceptance test into the required suite.
- Exercise the actual decoder/playsink graph, including field-order changes,
  repeated-field and one-field flags, and film cadence. A raw-frame fixture does
  not prove decoder flag propagation or correct inverse telecine.
- Collect sanitized comparative 480i/1080i/720p captures and CPU/resource
  measurements on the packaged platform matrix. Synthetic test elapsed time is
  not a playback CPU benchmark or a visual-quality result.
- Decide GPU and film work from those measurements, retaining the existing
  one-stream, teardown, and native-failure boundaries.

[issue #78]: https://github.com/jm2/balun/issues/78
