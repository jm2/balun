# Balun v0.1 support matrix

- Status: Release candidate
- Last updated: 2026-09-04

This matrix is derived from the sanitized evidence in
[`compatibility-v0.1.md`](compatibility-v0.1.md) and the ledger in [`task.md`](task.md). A cell
that reads "not yet verified" means no evidence has been recorded for that combination, not that
it is unsupported. Every ✅ is traceable to a section of the compatibility notes.

## Platforms

| Platform | Build | Local discovery | Live TV with audio | Packages |
| --- | --- | --- | --- | --- |
| Linux | CI and development host | ✅ Both primary-site devices | ✅ Development build, one host | 🚧 Configured: Flatpak x86_64/aarch64; deb amd64/arm64; rpm x86_64/aarch64; Arch x86_64 |
| Windows | CI and development hosts | ✅ One host; second host awaits retest | ✅ Development build, one host | 🚧 Configured: ZIP and installer for x86_64 and ARM64 |
| macOS | CI and development host | ✅ Both primary-site devices | ✅ Development build, one host | 🚧 Configured: Apple Silicon DMG |

The table names the configured `0.1.0` inventory. The release workflow is configured to build,
inspect, reopen, and checksum every package from the final tag, but the final tagged set has not
yet been produced or published on the [Releases](https://github.com/jm2/balun/releases) page. The
Live TV column records development-build evidence, not packaged live-tuner acceptance. A macOS
packaged-hardware validation path exists, but cross-platform packaged acceptance remains P4.1.
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
| Local broadcast and multicast | ✅ Linux, macOS, and Windows | Nothing; press **Refresh devices** |
| Exact address | ✅ Probed primary and secondary devices at exact addresses over LAN and tunnel | One IPv4 or unscoped IPv6 address, no port or range |
| Hostname | Covered by tests and exact unicast target proofs | One name, resolved to at most four unicast addresses |
| Remembered targets | ✅ Re-probed across launches; secondary tuners rediscovered | Nothing after the first successful probe |
| Approved private range (`balun-discover` only) | ✅ Diagnostic only | One RFC 1918 range no wider than `/24` that you own or administer |
| Opt-in route-table-derived tunnel search (Linux) | ✅ Verified over routed tunnel; candidate preview, approval, and traffic budget measured | Explicit approval of the previewed candidates and packet budget |
| Route-table-derived tunnel search (macOS, Windows) | ❌ Not in v0.1 | Use an exact address or hostname instead |

Local discovery sends nothing at launch beyond the remembered targets. Local broadcast and
multicast, exact IP and hostname targets, and remembered targets work on every supported platform;
only the opt-in route-table provider and route-change monitor are Linux-only. The approved-range
scan is a diagnostic for a network you administer, not a desktop feature.

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

- Protected (DRM) channels are listed with a badge but cannot be played.
- No program guide. The tested CONNECT's per-channel streams carry no PSIP tables, so a guide
  needs a full-multiplex crawl or XMLTV; both are v0.2 candidates.
- No recording, timeshift, transcoding, or tuner configuration.
- Lineups are never merged across devices.
- ATSC 3.0 AC-4 playback is not guaranteed on any platform.
- The opt-in route-table-derived tunnel search is Linux-only; macOS and Windows still support
  local, exact-address, hostname, and remembered-target discovery.

## Evidence

- Platforms: [Windows live-TV trial](compatibility-v0.1.md#windows-live-tv-trial),
  [Linux live-TV acceptance](compatibility-v0.1.md#linux-live-tv-acceptance),
  [macOS live-TV acceptance](compatibility-v0.1.md#macos-live-tv-acceptance), and
  [Windows package smoke](compatibility-v0.1.md#windows-package-smoke); ledger P0.1 to P0.3 and
  P3.2 to P3.4. Cross-platform packaged live-tuner acceptance remains P4.1.
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
  "Explicitly outside v0.1" list in [`task.md`](task.md).
