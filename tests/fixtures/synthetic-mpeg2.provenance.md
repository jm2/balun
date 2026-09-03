# Synthetic MPEG-2 transport-stream fixture

`synthetic-mpeg2.ts` is a deterministic, video-only test fixture generated
locally with GStreamer 1.28.6. It contains a 160-by-96, 25 fps `ball`
videotest pattern encoded as MPEG-2 video and carried in MPEG-TS. It contains
no audio, external media, or network-derived content.

Generation command:

```sh
gst-launch-1.0 -q -e \
  videotestsrc num-buffers=25 pattern=ball \
  ! video/x-raw,format=I420,width=160,height=96,framerate=25/1 \
  ! avenc_mpeg2video bitrate=300000 gop-size=12 \
  ! mpegvideoparse ! mpegtsmux \
  ! filesink location=synthetic-mpeg2.ts
```

Integrity:

- Size: 18,424 bytes
- MPEG-TS packets: 98 packets of 188 bytes
- SHA-256: `275b423803fff994845dc61b2e3b5e2b474ce10962f37bf8918b62b10c6a8191`
- BLAKE3: `78a4a8a94c2f928609427ffb8f69274c03bdb1f833ce2ae13e201735a09719c2`

This fixture exists for the display-backed development/CI playback test and
for the hidden packaged-runtime probe that the Windows packaging helper runs
against a staged package, which embeds these bytes in the desktop executable
for that purpose only. It is not a runtime application resource and must not
be staged as a separate file in any application package; source and test
archives may retain it as test data. The encoder used to create it is
likewise a development tool, not a Balun runtime or packaging requirement.
