# Changelog

All notable changes to Balun are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Headless discovery foundation** — Add the initial Rust library and
  `balun-discover` diagnostic for ordinary local discovery, exact-address
  probes, and explicitly approved routed discovery.
- **HDHomeRun protocol handling** — Validate tuner discovery frames, TLVs,
  CRCs, DeviceIDs, response limits, and stable device observations without a C
  protocol dependency.
- **Stable device and channel identity** — Retain independently expiring
  locators and discovery origins behind one validated DeviceID while keeping
  every channel key scoped to exactly one device.
- **Bounded device inspection** — Add responder-pinned metadata and lineup
  fetching plus `balun-discover --inspect`, which summarizes devices and
  channels without opening a stream or allocating a tuner.
- **Route candidate policy** — Add a platform-neutral route snapshot/provider
  boundary and deterministic policy for exact or approved private tunnel
  candidates; native platform providers remain pending.
- **Cross-platform repository checks** — Add locked formatting, strict Clippy,
  debug and release tests, exact-MSRV validation, macOS and Windows compile
  smoke checks, and immutable-tag release-candidate validation.
- **v0.1 product plan** — Define the separate device/channel sidebars,
  unprotected playback and guide scope, neighbor-friendly tunnel discovery,
  supported hardware sites, architecture, test strategy, and release contract.

### Security

- **Bounded routed discovery** — Require an explicit RFC 1918 range no wider
  than `/24`, cap candidates and concurrency, rate-limit targeted starts, and
  keep point-to-point interfaces out of automatic local enumeration.
- **Untrusted packet boundaries** — Reject malformed, oversized, bad-CRC, and
  inconsistent discovery replies and restrict broadcast or multicast replies
  to the directly attached prefix that was probed.
- **Pinned device HTTP** — Rebind advertised hosts to the validated responder,
  require the observed HDHomeRun metadata and stream ports, reject credentials
  and redirects, bypass proxies and DNS, cap response work, and verify
  `/discover.json` identity before accepting a lineup.
- **Bounded long-lived registry** — Cap devices, locators, and origins and
  reject conflicting DeviceID ownership until reassignment is independently
  confirmed.

### Privacy

- **Local and explicit discovery only** — Use local-interface discovery or
  targets and private ranges supplied directly by the user; do not contact a
  cloud discovery service, analytics service, or guide scraper.
- **Credential-safe diagnostics** — Hide advertised URL values, never
  deserialize, persist, or print `DeviceAuth`, and wipe the bounded metadata
  response buffer after use.
