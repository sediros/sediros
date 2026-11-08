# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 1.x (Debian 13 trixie), newest release | yes |
| Release candidates before 1.0 (13.1-rcN) | no: upgrade with `apt` |

Installed systems follow the newest 1.x release through `apt`; there is no separate support for older 1.x releases.

## Reporting a vulnerability

Please report security problems **privately** through GitHub: **[Report a vulnerability](https://github.com/sediros/sediros/security/advisories/new)** (the repository's Security tab). Do not open a public issue for them.

Useful in a report: the SedirOS release and build (`cat /etc/sediros/build-info`), what is affected, how to reproduce it, and what an attacker could do with it.

SedirOS is a volunteer project. You can expect an answer within a week; a fix is published as a new package version through the SedirOS apt repository, which installed systems apply automatically, and credited in the [CHANGELOG](CHANGELOG.md) unless you prefer otherwise.

## What belongs where

- **SedirOS's own code and settings:** the hardening, `sediros-audit`, `sediros-install`, `sediros-firstboot`, the `sediros` CLI, the build and the release process: report here.
- **Debian packages** (the kernel, OpenSSH, OpenSSL, ...): SedirOS uses them unchanged, so report to Debian's security team (<https://www.debian.org/security/>). Their fixes reach SedirOS systems automatically.
- **A SedirOS setting that makes a Debian bug reachable, or a hardening that does not do what it claims:** report here.

How SedirOS protects a server, and what it does not do: see [How SedirOS protects a server](README.md#how-sediros-protects-a-server).
