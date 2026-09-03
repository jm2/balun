# Balun v0.1 Compatibility Notes

- Status: Active
- Last updated: 2026-09-03

This document records sanitized, reproducible observations that refine the
v0.1 implementation plan. Device IDs, network addresses, channel names,
channel counts, stream URLs, and authorization values are deliberately
omitted.

## Primary-site metadata and lineup probe

The GTK-free `balun-discover --inspect --local` diagnostic was exercised
against the two accessible primary-site devices. The operation performed
HDHomeRun UDP discovery followed by bounded GET requests for `discover.json`
and `lineup.json`. It did not request a channel stream, allocate a tuner, or
change device configuration.

| Reported model | Tuners | Firmware observed | IPv4 discovery | IPv6 discovery | Metadata and lineup |
| --- | ---: | --- | --- | --- | --- |
| HDHR4-2US | 2 | 20260313 | Pass | Not observed from this host | Pass |
| HDHR5-4K | 4 | 20260326 | Pass | Pass | Pass |

Multiple IPv4 and IPv6 observations for the HDHR5-4K collapsed to one stable
DeviceID without merging it with the HDHR4-2US. The lineup parser kept channel
identity scoped to the originating DeviceID and natural-sorted its virtual
channel numbers.

## Initial Windows desktop discovery trial

The first desktop build was exercised on two Windows hosts against the two
accessible primary-site tuners. One host displayed one of the two devices; the
other displayed none. Selecting the single displayed device failed before any
lineup HTTP attempt because all of its retained locators were unsupported by
the HTTP boundary.

The code path allowed scoped link-local IPv6 discovery while intentionally
rejecting scoped IPv6 HTTP. It also depended on optional netmask/broadcast
values separately derived by the interface library and used directed IPv4
broadcast on Windows. The follow-up endpoint policy now:

- derives the IPv4 network from Windows' direct on-link prefix length;
- sends the same bounded requests to `255.255.255.255` from each eligible,
  specifically bound Windows IPv4 socket while retaining strict source-prefix
  and source-port validation;
- retains directed subnet broadcast on other platforms; and
- omits link-local IPv6 discovery until the HTTP client can preserve its scope.

These changes are covered by platform-neutral endpoint-policy tests, but the
result is not recorded as a Windows pass until both real hosts are retested.
Host firewall or network-profile filtering remains a separate possibility if a
host still receives no valid replies.

A later Windows desktop build launched successfully but reported the
`gtk4paintablesink` factory missing, because the MSYS2 `gst-plugins-rs`
package had not been installed alongside the core `gstreamer` package. The
build helpers now check the structural plugin files before building and name
the package for each missing one, so that state fails before a launch.

One of the two Windows hosts has since completed discovery, lineup loading, and
live playback with the corrected policy (see the trial below); the second host
remains to be retested.

A same-day Linux regression after this endpoint-policy change again discovered
both accessible primary-site devices over IPv4 and completed identity-checked
metadata and lineup inspection for both. The 4K device's usable non-link-local
IPv6 observations also remained available. This proves the policy did not
regress the known-good Linux LAN, but it does not predict Windows firewall or
adapter behavior.

## Windows live-TV trial

On 2026-09-02 the owner exercised the Windows desktop development build
against the accessible primary-site tuners. Local discovery, device selection,
and lineup loading worked on that host, confirming the limited-broadcast
correction above. Activating unprotected ATSC 1.0 channels produced picture and
audio, and channel switching, Stop, and window close behaved as expected.

ATSC 3.0 channels failed closed with the missing-codec category because the
tested MSYS2 GStreamer runtime has no AC-4 audio decoder. The open GStreamer
plugin sets ship an `ac4parse` parser but no decoder, and Balun bundles no
proprietary one, so this is recorded as a known limitation in the changelog
rather than a Balun defect.

This is an owner-reported observation. It did not measure first-frame,
channel-switch, or tuner-release times, and it did not enumerate the exact
decoder set in use; P0.4 and P0.5 in [`task.md`](task.md) record those.

## Linux live-TV acceptance

On 2026-09-03 the opt-in live-hardware proofs in
[`src/playback/live_hardware.rs`](../src/playback/live_hardware.rs) ran on
the Linux development host (Fedora 44, GStreamer 1.28.6, Rust 1.98) against
the primary-site tuners. They are display-free: the production controller
services discover, select, and authorize the stream, and `playbin3` renders
video into a `fakesink` and audio into a `fakesink` or, for the audio run,
into `pulsesink` on the host's PipeWire session. Every proof passed on its
first run.

- Local discovery found both devices, and the controller selected the
  HDHR4-2US through to a ready lineup.
- An unprotected ATSC 1.0 channel decoded to raw 1920×1080 progressive video
  and raw 48 kHz 5.1 audio, and the same audio rendered through `pulsesink`,
  so the desktop audio path works end to end on Linux.
- The unprotected ATSC 3.0 channel on the HDHR5-4K failed closed with the
  missing-codec category. GStreamer asked for decoders for `audio/x-ac4` and
  `video/x-h265`: Fedora's `gstreamer1-plugin-libav` carries no HEVC decoder,
  and no open AC-4 decoder exists. The tuner was released afterwards.
- A targeted probe at each discovered device's own address, with its
  DeviceID, returned exactly that device.

The proofs run only with `BALUN_LIVE_HARDWARE=1`, print no address, name,
channel name, or URL, and write their metadata captures under the build
directory with synthesized channel names. The GTK window was not part of
this run; its Stop and close paths are covered by the fake-device end-to-end
tests and by the Windows trial above.

## Tune and teardown budgets

Measured by the same proofs on the HDHR4-2US, with the production transport
deadlines tightened to 2 s connect and 5 s idle. Times are wall-clock on a
wired host and vary with the broadcast's group-of-pictures length.

| Measurement | Observed |
| --- | --- |
| Controller stream handoff | under 1 ms |
| First decoded video frame after PLAYING (measured before the paused hold; to be re-taken) | 0.6 s to 1.3 s |
| Stable video and audio decode after PLAYING | about 1.3 s |
| Switch to a second channel (retire, handoff, first frame) | 0.64 s |
| Client-side tuner release (NULL settled and transport joined) | under 7 ms |

Release is the client-side bound: the stream socket is closed and the
transport thread joined, after which the device frees the tuner on its own
idle timer. Every release in the run, including the one after the fail-closed
ATSC 3.0 tune, stayed far inside the 5 s class in [`playback.md`](playback.md).

## Linux plugin and codec contract

The ATSC 1.0 pipeline on Fedora 44 with GStreamer 1.28.6 autoplugged these
factories, with the hosted-CI hardware MPEG-2 decoders demoted so the
software path is the one recorded: `appsrc`, `typefind`, `tsdemux`,
`parsebin`, `multiqueue`, `mpegvideoparse`, `avdec_mpeg2video`,
`deinterlace`, `videoconvert`, `videoscale`, `videobalance`, `ac3parse`,
`a52dec`, `audioconvert`, `audioresample`, `volume`, `streamsynchronizer`,
`playsink`, and the sinks. `decodebin3`, `uridecodebin3`, `urisourcebin`,
`queue`, `tee`, `capsfilter`, and `identity` are `playbin3` internals.

ATSC 3.0 needs `avdec_h265` or a platform HEVC decoder plus an AC-4 decoder;
the first is absent from Fedora's libav plugin build and the second from
every open plugin set. The Windows and macOS factory sets are still
unrecorded, so P0.5 in [`task.md`](task.md) stays open.

### Linux decoder and sink inventory

`scripts/build-linux.sh --probe-playback` on the Linux development host
(Fedora, GStreamer 1.28.6, 2026-09-03) printed the following inventory,
which is the Linux half of P0.5. Highest-ranked factory first.

| Stream type | Decoders present |
| --- | --- |
| MPEG-2 video | `avdec_mpeg2video` (libav), `mpeg2dec` |
| H.264 video | `openh264dec` |
| HEVC video | none |
| MPEG-1/2 audio | `mpg123audiodec`, `avdec_mp2float` (libav) |
| AAC audio | `avdec_aac` (libav), `faad`, `fdkaacdec` |
| AC-3 audio | `a52dec`, `avdec_ac3` (libav) |
| E-AC-3 audio | none |
| AC-4 audio | none |

Audio sinks: `pulsesink` (selected by `autoaudiosink`), `alsasink`,
`oss4sink`, `openalsink`, `osssink`. The foundation factories all come from
GStreamer 1.28.6 except `gtk4paintablesink` from gst-plugins-rs 0.15.2. The
Windows and macOS inventories from the same probe complete P0.5.

### Windows decoder and sink inventory

`scripts\build-windows.ps1 -ProbePlayback` on the Windows development host
(MSYS2 CLANG64, GStreamer 1.28.6, 2026-09-03) printed the following. Highest-
ranked factory first; Direct3D decoders outrank the software ones.

| Stream type | Decoders present |
| --- | --- |
| MPEG-2 video | `avdec_mpeg2video` (libav) |
| H.264 video | `d3d12h264dec`, `d3d11h264dec`, `avdec_h264` (libav), `openh264dec` |
| HEVC video | `d3d12h265dec`, `d3d11h265dec`, `avdec_h265` (libav), `libde265dec` |
| MPEG-1/2 audio | `mpg123audiodec`, `mfmp3dec` (Media Foundation), `avdec_mp2float` (libav) |
| AAC audio | `avdec_aac` (libav), `faad`, `mfaacdec` (Media Foundation), `fdkaacdec` |
| AC-3 audio | `avdec_ac3` (libav) |
| E-AC-3 audio | `avdec_eac3` (libav) |
| AC-4 audio | none |

Audio sinks: `wasapi2sink` (selected by `autoaudiosink`), `wasapisink`,
`waveformsink`, `openalsink`, `directsoundsink`. The foundation factories all
come from GStreamer 1.28.6 except `gtk4paintablesink` from gst-plugins-rs
0.15.3. Unlike the Linux host, Windows can decode HEVC video, so an ATSC 3.0
channel there fails closed on AC-4 audio alone; E-AC-3 is also decodable
but no such channel has been tuned on record. The macOS inventory is the
remaining half of P0.5.

## Windows package smoke

On 2026-09-03 `scripts\build-windows.ps1 -Zip` on the Windows development host
staged the package from the MSYS2 CLANG64 runtime above and passed every gate.
The staged tree held the 27 allowlisted plugins and 158 dependent DLLs (213 MB
on disk, 84 MB as a ZIP), the reopened `balun.exe` carried the seven-image icon
and the `0.1.0-alpha.1` version resource, and the packaged runtime probe
decoded the synthetic MPEG-2 fixture inside the tree with a fresh registry and
only `System32` on `PATH`. Launched the same way by hand, the packaged
application presented its window. No tuner was contacted; tuning from the
package is recorded under P4.1.

## In-band guide spike

On 2026-09-03 the HDHomeRun CONNECT's stream forms were each captured for
about twelve seconds with a plain HTTP client and inventoried by PID and
table identifier, without decoding any content:

- Per-channel streams (`/auto/v<guide>` and
  `/tuner<n>/ch<frequency>-<program>`) carry only the PAT, the program's PMT,
  and its video and audio elementary streams. The PSIP base PID is absent, so
  no MGT, VCT, STT, EIT, or ETT reaches a player that tunes the way Balun
  does.
- Full-multiplex streams (`/tuner<n>/ch<frequency>` and `/auto/ch<frequency>`)
  carry the whole broadcast at about 19 Mb/s: every program in the multiplex,
  the PSIP base PID with the MGT, TVCT, and STT, EIT-0 through EIT-3 on their
  own PIDs, and the matching ETT tables.

On the tested CONNECT, in-band guide data therefore survives only when a
whole multiplex is requested, which occupies a tuner at the full broadcast
rate and needs the RF frequency, which the lineup does not expose. A v0.2
in-band guide would have to crawl each multiplex briefly, or the guide comes
from XMLTV; a now/next overlay taken from the playing stream is ruled out for
that device. Other HDHomeRun models and ATSC 3.0 signalling were not examined.

## Observed JSON compatibility

Both `discover.json` documents exposed the following non-secret fields with
the expected types:

- `FriendlyName`, `ModelNumber`, `FirmwareName`, and `FirmwareVersion` as
  strings.
- `TunerCount` as a number.
- `DeviceID`, which Balun matched against the UDP discovery identity.

The documents also contain fields that Balun treats as transport claims or
credentials. Advertised URL hosts are rebound to the accepted UDP responder,
and `DeviceAuth` is never placed in the public model or diagnostic output. The
bounded metadata response storage is wiped after parsing.

Across the two lineup documents, current firmware exposed this field set:

- Required identity and presentation fields: `GuideNumber`, `GuideName`, and
  `URL`.
- Optional codec fields: `VideoCodec` and `AudioCodec`.
- Optional status fields: `Favorite`, `DRM`, `HD`, `SignalQuality`, and
  `SignalStrength`.

The observed `Favorite`, `DRM`, and `HD` fields use numeric `1` when present
and are otherwise omitted. The older documented comma-separated `Tags` field
was absent. Balun accepts both representations, plus boolean equivalents for
fixture and firmware compatibility, while rejecting other flag values.

Every accepted channel URL used the responder host, port 5004, and an
`/auto/v<GuideNumber>` path. Balun now enforces all three properties without
connecting to the stream.

Sanitized copies of both documents, and the CONNECT's 404 and 503 responses,
are in [`tests/fixtures/hdhr/`](../tests/fixtures/hdhr/provenance.md); the
parser unit tests read them, including the responder-host pin on the 4K
lineup.

## Linux route-provider smoke

The native Linux rtnetlink provider was exercised separately on the
development host. It completed its bounded, deadline-protected double snapshot
and normalization successfully. The smoke diagnostic reported only success;
it did not print or persist interface names, addresses, routes, or candidate
targets, and it sent no discovery packets.

This establishes only that native route inspection works on one Linux routing
configuration. It does not approve or exercise a route-derived scan, and it is
not evidence yet for WireGuard or UniFi Site Magic discovery.

## Boundaries of this result

These observations establish local discovery, stable device separation,
responder pinning, metadata identity, lineup parsing, favorite/DRM
compatibility for the two listed model/firmware pairs, live ATSC 1.0
playback with audio on one Windows host and one Linux host, and the Linux
tune, switch, and release budgets. They do not yet establish:

- The macOS decoder set, and HEVC or E-AC-3 playback on any platform.
- Protected-channel playback.
- Secondary-site HDHR3-PRIME or HDHR5-4K behavior.
- UniFi Site Magic, WireGuard, or other routed multi-site discovery.
- macOS live-TV behavior, or the second Windows host.
- Deferred Australian HDHR5-4DT compatibility.

Those remain explicit rows in the real-hardware matrix in
[`plan-v0.1.md`](plan-v0.1.md) and the P0 records in [`task.md`](task.md).
