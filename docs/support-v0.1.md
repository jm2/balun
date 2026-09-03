# Balun v0.1 support matrix

- Status: Active
- Last updated: 2026-09-03

This matrix is derived from the sanitized evidence in
[`compatibility-v0.1.md`](compatibility-v0.1.md) and the ledger in [`task.md`](task.md). A cell
that reads "not yet verified" means no evidence has been recorded for that combination, not that
it is unsupported. Every ✅ is traceable to a section of the compatibility notes.

## Platforms

| Platform | Build | Local discovery | Live TV with audio | Packages |
| --- | --- | --- | --- | --- |
| Linux | CI and development host | ✅ Both primary-site devices | ✅ Development build, one host | ❌ Not yet published |
| Windows | CI and development hosts | ✅ One host; second host awaits retest | ✅ Development build, one host | ❌ Not yet published |
| macOS | CI only | Not yet verified | Not yet verified | ❌ Not yet published |

Flatpak, Windows, and macOS packages are ledger records P3.2 to P3.4. Until they land, every
platform builds from source as the README describes, and the packaged codec closure is not frozen.

## Devices

| Model | Firmware observed | Site | Discovery, metadata, lineup | Live TV |
| --- | --- | --- | --- | --- |
| HDHR4-2US (CONNECT) | 20260313 | Primary | ✅ IPv4 | ✅ ATSC 1.0 on Linux; tune and release budgets measured |
| HDHR5-4K (CONNECT 4K) | 20260326 | Primary | ✅ IPv4 and IPv6 | ⚠️ ATSC 3.0 fails closed on AC-4; ATSC 1.0 not recorded separately |
| HDHR3-PRIME | — | Secondary | Not yet verified | Not yet verified |
| HDHR5-4K | — | Secondary | Not yet verified | Not yet verified |
| HDHR5-4DT | — | Deferred | Not yet verified | Not yet verified |

The Windows trial played ATSC 1.0 with audio from the primary-site tuners without recording which
device served it. Protected-channel playback is not verified on any device and is out of scope.
The deferred HDHR5-4DT is an Australian unit; no regional or DVB-T support is claimed.

## Discovery

| Method | Evidence | What you provide |
| --- | --- | --- |
| Local broadcast and multicast | ✅ Linux and Windows | Nothing; press **Refresh devices** |
| Exact address | ✅ Probed each primary-site device at its own address | One IPv4 or unscoped IPv6 address, no port or range |
| Hostname | Not yet verified on hardware; covered by tests | One name, resolved to at most four unicast addresses |
| Remembered targets | Not yet verified on hardware; covered by tests | Nothing after the first successful probe |
| Approved private range (`balun-discover` only) | ✅ Diagnostic only | One RFC 1918 range no wider than `/24` that you own or administer |
| Route-derived tunnel discovery (Linux) | 🚧 Library runner on `main`; approval flow in review | Explicit approval of the previewed candidates and packet budget |
| Route-derived discovery (macOS, Windows) | ❌ Not in v0.1 | An exact address or hostname instead |

Local discovery sends nothing at launch beyond the remembered targets. The approved-range scan is
a diagnostic for a network you administer, not a desktop feature.

## Codecs

Decoders come from the installed GStreamer runtime; Balun bundles none and transcodes nothing.

| Stream type | Linux | Windows | macOS |
| --- | --- | --- | --- |
| MPEG-2 video | ✅ `avdec_mpeg2video` | ✅ `avdec_mpeg2video` | Not yet verified |
| AC-3 audio | ✅ `a52dec` | ✅ `avdec_ac3` | Not yet verified |
| H.264 video | Decoder present (`openh264dec`); not tuned on record | Decoders present (`d3d12h264dec`, `avdec_h264`); not tuned on record | Not yet verified |
| MPEG-1/2 audio | Decoder present (`mpg123audiodec`); not tuned on record | Decoder present (`mpg123audiodec`); not tuned on record | Not yet verified |
| AAC audio | Decoder present (`avdec_aac`); not tuned on record | Decoder present (`avdec_aac`); not tuned on record | Not yet verified |
| E-AC-3 audio | ❌ No decoder installed | Decoder present (`avdec_eac3`); not tuned on record | Not yet verified |
| HEVC video | ❌ Fedora's gst-libav build has no HEVC decoder | ⚠️ Decoders present (`d3d12h265dec`, `avdec_h265`); ATSC 3.0 fails on AC-4 first | ⚠️ Not proven |
| AC-4 audio | ❌ No open decoder | ❌ No open decoder | ❌ No open decoder |

- HEVC decoders exist on Windows (Direct3D and libav) but not in Fedora's gst-libav; HEVC playback
  is not proven on any platform because every ATSC 3.0 channel tried so far fails on AC-4 first.
- AC-4 has no open decoder, so ATSC 3.0 audio fails closed with a message that names the codec.
- H.264, MPEG-1/2 audio, and AAC decoders are installed on Linux and Windows, but no broadcast
  carrying them has been tuned on record. E-AC-3 decodes on Windows only.

## Limitations

- Protected (DRM) channels are listed with a badge but cannot be played.
- No program guide. The tested CONNECT's per-channel streams carry no PSIP tables, so a guide
  needs a full-multiplex crawl or XMLTV; both are v0.2 candidates.
- No recording, timeshift, transcoding, or tuner configuration.
- Lineups are never merged across devices.
- ATSC 3.0 AC-4 playback is not guaranteed on any platform.
- Routed discovery on macOS and Windows is exact-address or hostname only.

## Evidence

- Platforms: [Windows live-TV trial](compatibility-v0.1.md#windows-live-tv-trial),
  [Linux live-TV acceptance](compatibility-v0.1.md#linux-live-tv-acceptance); ledger P0.1 to
  P0.3 and P3.2 to P3.4.
- Devices: [Primary-site metadata and lineup probe](compatibility-v0.1.md#primary-site-metadata-and-lineup-probe),
  [Tune and teardown budgets](compatibility-v0.1.md#tune-and-teardown-budgets),
  [Boundaries of this result](compatibility-v0.1.md#boundaries-of-this-result); ledger P0.6 and
  P4.2.
- Discovery: [Initial Windows desktop discovery trial](compatibility-v0.1.md#initial-windows-desktop-discovery-trial),
  [Linux live-TV acceptance](compatibility-v0.1.md#linux-live-tv-acceptance) for the
  exact-address probe,
  [Linux route-provider smoke](compatibility-v0.1.md#linux-route-provider-smoke); ledger P0.7,
  P1.2, and P2.1 to P2.5.
- Codecs: [Linux plugin and codec contract](compatibility-v0.1.md#linux-plugin-and-codec-contract),
  [Linux decoder and sink inventory](compatibility-v0.1.md#linux-decoder-and-sink-inventory),
  [Windows decoder and sink inventory](compatibility-v0.1.md#windows-decoder-and-sink-inventory),
  [Windows live-TV trial](compatibility-v0.1.md#windows-live-tv-trial); ledger P0.5.
- Limitations: [In-band guide spike](compatibility-v0.1.md#in-band-guide-spike) and the
  "Explicitly outside v0.1" list in [`task.md`](task.md).
