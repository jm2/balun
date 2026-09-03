# Security policy

## Supported versions

Balun is pre-release. No version has been released yet, so only the `main` branch receives fixes.
This table will name the supported releases once `v0.1.0-alpha.1` is published.

| Version | Supported |
| --- | --- |
| `main` | ✅ |
| Released versions | None published yet |

## Reporting a vulnerability

Use GitHub's private vulnerability reporting for
[jm2/balun](https://github.com/jm2/balun/security/advisories/new). Do not open a public issue or
pull request for a security problem. If the reporting form is unavailable, open an issue that
says only that you have a security report and how you can be reached privately, with no detail
of the problem. Include the affected component, a reproduction, and the impact you believe it
has. A sanitized packet capture or fixture is welcome; a real device ID, address, or `DeviceAuth`
value is not.

## What is in scope

- Network admission: the discovery prefix and port checks, the per-operation and routed-scan
  packet budgets, and the approval store that gates route-derived discovery.
- Credential handling: `DeviceAuth` must never be parsed, persisted, or printed, and advertised
  URLs must never be followed as given.
- Persisted state: the contents of `settings.json` and the approval store, which must hold no
  credentials, stream URLs, or raw topology.
- The stream transport: its refusal of proxies, redirects, DNS, and URL credentials, and the
  bounded size and time limits on device HTTP.
- Package contents and the release component policy, once packages exist.
- CI and release workflow permissions and their pinned tools.

## What is out of scope

- DRM and protected channels. Balun lists them and does not play them; bypasses are not wanted.
- HDHomeRun firmware and the device's own HTTP and UDP services; report those to SiliconDust.
- Your network, firewall, or tunnel configuration.
- Vulnerabilities in GTK, GStreamer, or other dependencies Balun does not ship. `cargo audit`
  runs in CI, and a dependency bump is welcome as an ordinary pull request.

## What to expect

- An acknowledgement within a week.
- A fix, or an agreed plan, before any public disclosure; please allow time for it to land.
- Credit in the changelog and release notes if you want it.

Balun is a volunteer project, so these timelines are best effort.
