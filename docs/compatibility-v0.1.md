# Balun v0.1 Compatibility Notes

- Status: Active
- Last updated: 2026-09-02

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

These observations establish local discovery, stable device separation,
responder pinning, metadata identity, lineup parsing, favorite/DRM
compatibility for the two listed model/firmware pairs, and live ATSC 1.0
playback with audio on one Windows host. They do not yet establish:

- Channel-change latency or tuner-release timing.
- The exact per-platform decoder set, or HEVC and E-AC-3 support.
- Protected-channel playback.
- In-band PSIP/EIT guide availability.
- Secondary-site HDHR3-PRIME or HDHR5-4K behavior.
- UniFi Site Magic, WireGuard, or other routed multi-site discovery.
- Linux or macOS live-TV behavior, or the second Windows host.
- Deferred Australian HDHR5-4DT compatibility.

Those remain explicit rows in the real-hardware matrix in
[`plan-v0.1.md`](plan-v0.1.md) and the P0 records in [`task.md`](task.md).
