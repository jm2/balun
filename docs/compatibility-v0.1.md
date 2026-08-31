# Balun v0.1 Compatibility Notes

- Status: Active
- Last updated: 2026-08-31

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

This probe establishes local discovery, stable device separation, responder
pinning, metadata identity, lineup parsing, and favorite/DRM compatibility for
the two listed model/firmware pairs. It does not yet establish:

- MPEG-TS playback, channel-change latency, or tuner release behavior.
- MPEG-2, H.264, HEVC, AC-3, AAC, E-AC-3, or AC-4 runtime support.
- ATSC 3.0 or protected-channel playback.
- In-band PSIP/EIT guide availability.
- Secondary-site HDHR3-PRIME or HDHR5-4K behavior.
- UniFi Site Magic, WireGuard, or other routed multi-site discovery.
- macOS or Windows runtime behavior.
- Deferred Australian HDHR5-4DT compatibility.

Those remain explicit follow-up rows in the real-hardware matrix in
[`plan-v0.1.md`](plan-v0.1.md).
