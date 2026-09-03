# Sanitized HDHomeRun device fixtures

These files were captured on 2026-09-03 from the two primary-site tuners
listed in [`docs/compatibility-v0.1.md`](../../../docs/compatibility-v0.1.md):
an HDHomeRun CONNECT (`HDHR4-2US`, firmware 20260313) and an HDHomeRun
CONNECT 4K (`HDHR5-4K`, firmware 20260326). They keep the real field set,
field order, value types, and channel counts so the parsers are tested
against current firmware, and they carry no identity, topology, credential,
or channel data:

- `DeviceID` is replaced by a checksum-valid test identity, `DeviceAuth` by a
  placeholder, and every URL host by an address from the RFC 5737
  documentation range.
- Guide numbers are renumbered per major channel in order (ATSC 1.0 majors
  from `2`, ATSC 3.0 majors from `100`), guide names are synthesized from the
  new numbers, and stream URLs are rebuilt from them.
- `VideoCodec`, `AudioCodec`, `HD`, `DRM`, `Favorite`, `SignalQuality`, and
  `SignalStrength` are unchanged.

`discover-*.json` and `lineup-*.json` are the device's `discover.json` and
`lineup.json` bodies. The `http-*.txt` files are the status line and headers
the CONNECT returned for an unknown path, an unknown channel number, and a
stream request while both tuners were in use; only the `Date` header was
removed. The 404 body for an unknown path is a generic HTML page and is not
kept.

The parser unit tests in `src/hdhr/http.rs` and `src/hdhr/lineup.rs` read the
JSON files. None of these files is a runtime resource or packaged.
