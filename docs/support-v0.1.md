# Balun v0.1 support matrix

- Status: v0.2.0 Alpha released 2026-09-25
- Last updated: 2026-09-25

This matrix is derived from the sanitized evidence in
[`compatibility-v0.1.md`](compatibility-v0.1.md) and the ledger in [`task.md`](task.md). A cell
that reads "not yet verified" means no evidence has been recorded for that combination, not that
it is unsupported. Every ✅ is traceable to a section of the compatibility notes.

Note (2026-09-24): V2.9 retired the Linux route-table-derived tunnel search
([ADR-0003](architecture/adr-0003-retire-route-derived-discovery.md)). Its routed-scan evidence
below is historical; remote tuners are now added by subnet search, exact address, or hostname.

## Platforms

| Platform | Build | Local discovery | Live TV with audio | Packages |
| --- | --- | --- | --- | --- |
| Linux | CI and development host | ✅ Both primary-site devices | ✅ Development build, one host | Built: Flatpak x86_64/aarch64; deb amd64/arm64; rpm x86_64/aarch64; Arch x86_64 |
| Windows | CI and development hosts | ✅ One host; second host awaits retest | ✅ Development build, one host | Built: ZIP and installer for x86_64 and ARM64 |
| macOS | CI and development host | ✅ Both primary-site devices | ✅ Development build, one host | Built: Apple Silicon DMG |

The table names the `0.1.0` inventory built, inspected, reopened, and checksummed by the release
workflow and published as "Balun v0.1.0 Alpha" on the
[Releases](https://github.com/jm2/balun/releases) page on 2026-09-05. The
Live TV column records development-build evidence. The maintainer separately accepted the v0.1.1
packages on Linux, macOS, and Windows ([P4.1](compatibility-v0.1.md#packaged-live-tuner-acceptance)).
Architecture-specific entries do not extend the physical-tuner evidence to every CPU and format.
The Windows and macOS packages stage reviewed decoder closures; Flatpak uses its runtime, and the
native Linux packages use the distribution's installed runtime.

## Devices

| Model | Firmware observed | Site | Discovery, metadata, lineup | Live TV |
| --- | --- | --- | --- | --- |
| HDHR4-2US (CONNECT) | 20260313 | Primary | ✅ IPv4 | ✅ ATSC 1.0 on Linux and macOS; tune and release budgets measured |
| HDHR5-4K (CONNECT 4K) | 20260326 | Primary | ✅ IPv4 and IPv6 | ⚠️ ATSC 3.0 fails closed on AC-4; ATSC 1.0 verified |
| HDHR3-PRIME (PRIME) | 20230505 | Secondary | ✅ Routed scan and exact address | ✅ Clear QAM (MPEG-2, H.264); DRM refused; 503 busy handled |
| HDHR5-4K (CONNECT 4K) | 20260326 | Secondary | ✅ Routed scan and exact address | ✅ Routed ATSC 1.0; ATSC 3.0 fails closed on AC-4; distinct identity |
| HDHR5-4DT | — | Deferred | Inaccessible | Deferred (out of v0.1 scope) |

The Windows trial played ATSC 1.0 with audio from the primary-site tuners without recording which
device served it. CableCARD protected-channel playback is refused and permanently out of scope.
The deferred HDHR5-4DT is an Australian unit; no regional or DVB-T support is claimed.

## Discovery

| Method | Evidence | What you provide |
| --- | --- | --- |
| Local broadcast and multicast | ✅ Linux, macOS, and Windows | Nothing; runs once at launch, and **Refresh devices** repeats it |
| Exact address | ✅ Probed primary and secondary devices at exact addresses over LAN and tunnel | One IPv4 or unscoped IPv6 address, no port or range |
| Hostname | Covered by tests and exact unicast target proofs | One name, resolved to at most four unicast addresses |
| Remembered targets | ✅ Re-probed across launches; secondary tuners rediscovered | Nothing after the first successful probe |
| Subnet search (desktop and `balun-discover --approved-range`) | Covered by tests on Linux, macOS, and Windows; awaiting real-network confirmation | One RFC 1918 subnet, `/23` to `/32`, that you own or administer, confirmed before every search |
| Route-table-derived tunnel search | Verified on Linux in v0.1.0 and v0.1.1; retired 2026-09-24 (V2.9) | Use subnet search, an exact address, or a hostname instead |

Local discovery runs once at launch and then only on request; remembered targets are probed after
it settles. Local broadcast and
multicast, exact IP and hostname targets, and remembered targets work on every supported platform.
Subnet search is available only while Balun observes network changes, sends at most 1,020
requests for a `/23`, and stops when the network changes.

## Codecs

Native Linux packages use the distribution's GStreamer runtime, and Flatpak uses its platform
runtime. The macOS and Windows packages carry their reviewed decoder closures. Balun transcodes
nothing.

| Stream type | Linux | Windows | macOS |
| --- | --- | --- | --- |
| MPEG-2 video | ✅ `avdec_mpeg2video` | ✅ `avdec_mpeg2video` | ✅ `avdec_mpeg2video` (libav) |
| AC-3 audio | ✅ `a52dec` | ✅ `avdec_ac3` | ✅ `avdec_ac3` (libav) |
| H.264 video | Decoder present (`openh264dec`); Clear QAM verified | Decoders present (`d3d12h264dec`, `avdec_h264`); Clear QAM verified | Decoders present (`vtdec_hw`, `avdec_h264`); Clear QAM verified |
| MPEG-1/2 audio | Decoder present (`mpg123audiodec`); not tuned on record | Decoder present (`mpg123audiodec`); not tuned on record | Decoders present (`mpg123audiodec`, `atdec`); not tuned on record |
| AAC audio | Decoder present (`avdec_aac`); not tuned on record | Decoder present (`avdec_aac`); not tuned on record | Decoders present (`avdec_aac`, `faad`, `atdec`); not tuned on record |
| E-AC-3 audio | ❌ No decoder installed | Decoder present (`avdec_eac3`); not tuned on record | Decoder present (`avdec_eac3`); not tuned on record |
| HEVC video | ❌ Fedora's gst-libav build has no HEVC decoder | ⚠️ Decoders present (`d3d12h265dec`, `avdec_h265`); ATSC 3.0 fails on AC-4 first | ⚠️ Decoders present (`vtdec_hw`, `avdec_h265`); ATSC 3.0 fails on AC-4 first |
| AC-4 audio | ❌ No open decoder | ❌ No open decoder | ❌ No open decoder |

- HEVC decoders exist on Windows (Direct3D and libav) and macOS (VideoToolbox and libav)
  but not in Fedora's gst-libav; HEVC playback is not proven on any platform because every ATSC 3.0
  channel tried so far fails on AC-4 first.
- AC-4 has no open decoder, so ATSC 3.0 audio fails closed with a message that names the codec.
- H.264 video is verified on Clear QAM channels on the HDHR3-PRIME. MPEG-1/2 audio and AAC decoders
  are installed on Linux, macOS, and Windows. E-AC-3 decodes on Windows and macOS.

## Limitations

- Native decoders and sinks run in-process. A stuck native call can block the UI or close
  path, despite later five-second waits. Network bytes do not prove useful media and no
  independent useful-media deadline is enforced yet. The maintainer accepted these
  [H3.5 limits](native-media-failure-boundary.md), owned by `jm2`, for review before beta
  and after any reproduced native hang.

- Protected (DRM) channels are listed with a badge but cannot be played.
- No program guide. The tested CONNECT's per-channel streams carry no PSIP tables, so a guide
  needs a full-multiplex crawl or XMLTV; both are v0.2 candidates.
- No recording, timeshift, transcoding, or tuner configuration.
- Lineups are never merged across devices.
- ATSC 3.0 AC-4 playback is not guaranteed on any platform.
- Subnet search needs downstream routers to keep directed-broadcast forwarding disabled; Balun
  bounds its own requests but cannot check that setting.

## Evidence

- Platforms: [Windows live-TV trial](compatibility-v0.1.md#windows-live-tv-trial),
  [Linux live-TV acceptance](compatibility-v0.1.md#linux-live-tv-acceptance),
  [macOS live-TV acceptance](compatibility-v0.1.md#macos-live-tv-acceptance), and
  [Windows package smoke](compatibility-v0.1.md#windows-package-smoke); ledger P0.1 to P0.3 and
  P3.2 to P3.4; packaged live-tuner acceptance of v0.1.1 is P4.1.
- Devices:
  [Primary metadata and lineup](compatibility-v0.1.md#primary-site-metadata-and-lineup-probe),
  [Secondary validation](compatibility-v0.1.md#secondary-site-metadata-and-playback-validation),
  [Hardware matrix](compatibility-v0.1.md#multi-site-hardware-and-codec-compatibility-matrix),
  [Tune and teardown budgets](compatibility-v0.1.md#tune-and-teardown-budgets),
  [Boundaries of this result](compatibility-v0.1.md#boundaries-of-this-result); ledger P0.6,
  P4.2, and P4.4.
- Discovery:
  [Windows discovery trial](compatibility-v0.1.md#initial-windows-desktop-discovery-trial),
  [Linux live-TV acceptance](compatibility-v0.1.md#linux-live-tv-acceptance) for the
  exact-address probe,
  [Linux route-provider smoke](compatibility-v0.1.md#linux-route-provider-smoke),
  [Routed discovery](compatibility-v0.1.md#routed-tunnel-discovery-and-multi-site-validation);
  ledger P0.7, P1.2, and P2.1 to P2.5.
- Codecs: [Per-platform contract](compatibility-v0.1.md#per-platform-plugin-and-codec-contract),
  [Linux decoder inventory](compatibility-v0.1.md#linux-decoder-and-sink-inventory),
  [Windows decoder inventory](compatibility-v0.1.md#windows-decoder-and-sink-inventory),
  [macOS decoder inventory](compatibility-v0.1.md#macos-decoder-and-sink-inventory),
  [Windows live-TV trial](compatibility-v0.1.md#windows-live-tv-trial); ledger P0.5.
- Limitations: [In-band guide spike](compatibility-v0.1.md#in-band-guide-spike) and the
  "Outside the adopted implementation scope" list in [`task.md`](task.md).
