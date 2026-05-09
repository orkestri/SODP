# Email Draft — URI Scheme Review Request

**To:** uri-review@ietf.org
**Subject:** Provisional URI scheme registration request: `sodp` and `sodps`
**Content-Type:** text/plain

---

Hello,

We would like to request provisional registration of two URI schemes,
"sodp" and "sodps", for the State-Oriented Data Protocol (SODP).

SODP is an open protocol for continuous state synchronization over
persistent bidirectional connections. Instead of request-response, clients
subscribe to named state keys and receive a full snapshot followed by
incremental binary deltas containing only the changed fields. The protocol
uses MessagePack-encoded frames and currently binds to WebSocket (RFC 6455)
as its transport layer.

The "sodp" scheme identifies an unencrypted SODP endpoint; "sodps"
identifies a TLS-secured endpoint, analogous to ws/wss (RFC 6455) and
mqtt/mqtts.

The protocol has a published specification, a reference server
implementation in Rust, and client SDKs in TypeScript, Python, and Java,
all available under the MIT license. There is also an independent
third-party implementation in Go.

## Registration templates

### sodp

  Scheme name:  sodp
  Status:  Provisional
  Applications/protocols that use this scheme name:
    State-Oriented Data Protocol (SODP) — a binary protocol for continuous
    state synchronization over persistent bidirectional connections. Uses
    MessagePack-encoded frames over WebSocket (RFC 6455).
  Contact:  Orkestri <hello@orkestri.site>
  Change controller:  Orkestri <hello@orkestri.site>
  References:
    https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md

### sodps

  Scheme name:  sodps
  Status:  Provisional
  Applications/protocols that use this scheme name:
    State-Oriented Data Protocol (SODP) over TLS — the TLS-secured
    variant of SODP. Requires TLS 1.2 or later. Semantically identical
    to "sodp" but encrypted. Analogous to ws/wss (RFC 6455).
  Contact:  Orkestri <hello@orkestri.site>
  Change controller:  Orkestri <hello@orkestri.site>
  References:
    https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md

## URI Syntax

  sodp-URI  = "sodp://" authority [ "/" ]
  sodps-URI = "sodps://" authority [ "/" ]
  authority = host [ ":" port ]

  Default port: 7777

  Examples:
    sodp://localhost:7777
    sodps://sodp.example.com
    sodps://sodp.example.com:8443

## Running code

The specification and all implementations are publicly available:

  Specification:  https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md
  Repository:     https://github.com/orkestri/SODP
  npm (TS):       https://www.npmjs.com/package/@sodp/client
  PyPI (Python):  https://pypi.org/project/sodp/

We welcome any feedback from the review list.

Best regards,
Abel Eduardo Mondlane
Orkestri
hello@orkestri.site
