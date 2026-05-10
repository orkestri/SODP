# Security Policy

## Supported Versions

| Component | Supported |
|---|---|
| Rust server (latest release) | yes |
| Go server (latest release) | yes |
| `@sodp/client` (latest) | yes |
| `@sodp/react` (latest) | yes |
| `sodp` Python (latest) | yes |
| `io.sodp:sodp-client` (latest) | yes |
| Any previous release | no |

Only the latest released version of each component receives security fixes. We recommend always running the latest release.

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

Report vulnerabilities privately via [GitHub Security Advisories](https://github.com/orkestri/SODP/security/advisories/new). You will receive a response within **72 hours** acknowledging the report.

Please include:

- A description of the vulnerability and its potential impact
- Steps to reproduce or a proof-of-concept
- The affected component(s) and version(s)
- Any suggested mitigations, if known

## Disclosure Policy

1. The vulnerability is confirmed and a fix is developed privately
2. A patch release is prepared for all affected components
3. The fix is released and a GitHub Security Advisory is published
4. Credit is given to the reporter (unless they prefer to remain anonymous)

We aim to release patches within **14 days** of confirming a vulnerability. Critical issues are prioritized and patched as fast as possible.

## Scope

Issues considered in scope:

- Authentication bypass (JWT validation, ACL enforcement)
- Unauthorized read or write access to state keys
- Denial of service via malformed frames or resource exhaustion
- Memory safety issues in the Rust server
- Dependency vulnerabilities in released packages

Issues out of scope:

- Vulnerabilities in demo applications (`demo-collab/`)
- Issues requiring physical access to the server
- Social engineering attacks
