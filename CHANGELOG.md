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
  inconsistent discovery replies; keep advertised URLs untrusted for later
  source validation.

### Privacy

- **Local and explicit discovery only** — Use local-interface discovery or
  targets and private ranges supplied directly by the user; do not contact a
  cloud discovery service, analytics service, or guide scraper.
