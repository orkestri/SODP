# IANA URI Scheme Registration — Submission Guide

## What We're Registering

Two provisional URI schemes under [RFC 7595](https://www.rfc-editor.org/rfc/rfc7595):

| Scheme | Description | Default Port |
|--------|-------------|-------------|
| `sodp` | SODP over unencrypted transport | 7777 |
| `sodps` | SODP over TLS | 7777 |

## Files in This Directory

| File | Purpose |
|------|---------|
| `uri-scheme-registration.md` | The formal registration templates + specification summary |
| `uri-review-email.md` | Draft email ready to send to `uri-review@ietf.org` |
| `README.md` | This file — step-by-step submission guide |

## Submission Steps

### Step 1 — Pre-flight Checks

Before submitting, verify:

- [ ] `SPECIFICATION.md` is committed and pushed to `master`
- [ ] GitHub repo (`github.com/orkestri/SODP`) is public
- [ ] The spec URL resolves: https://github.com/orkestri/SODP/blob/master/SPECIFICATION.md
- [ ] npm packages are published: `@sodp/client`, `@sodp/react`
- [ ] PyPI package is published: `sodp`
- [ ] Contact email (`hello@orkestri.io`) is monitored

### Step 2 — Community Review (uri-review@ietf.org)

Send the email from `docs/iana/uri-review-email.md` to:

```
To: uri-review@ietf.org
Subject: Provisional URI scheme registration request: sodp and sodps
```

This is a public mailing list. Anyone can comment. The review period
is typically 2-4 weeks. Common feedback:

- Clarify the relationship to existing schemes (ws/wss, mqtt/mqtts)
- Justify why a new scheme is needed vs. a WebSocket sub-protocol
- Confirm the spec is publicly accessible

Be prepared to answer these. The key argument: SODP is transport-
independent — WebSocket is the current binding, but the protocol
anticipates raw TCP, HTTP/2 streams, and WebTransport. A dedicated
scheme signals protocol identity, not just transport.

### Step 3 — IANA Submission

After the review period (and addressing any feedback), submit via the
IANA web form:

**URL:** https://www.iana.org/form/uri-schemes

Fill in the same template fields from `uri-scheme-registration.md`.
Submit once for `sodp`, once for `sodps`.

### Step 4 — Confirmation

IANA will:

1. Review the submission (1-2 weeks).
2. Add the schemes to the provisional registry at:
   https://www.iana.org/assignments/uri-schemes/uri-schemes.xhtml
3. Send a confirmation email.

Once registered, `sodp://` and `sodps://` are officially recognized
URI schemes. Clients and servers can use them in connection strings,
documentation, and tooling.

## What Comes After (Optional)

To upgrade from Provisional to Permanent, you need a published RFC
(or equivalent Standards Track document). The path:

1. Reformat `SPECIFICATION.md` as an Internet-Draft (I-D)
2. Submit to https://datatracker.ietf.org/
3. Go through IETF review (Independent Submission or Working Group)
4. Published RFC permanently registers the schemes

This is a separate, longer process (6-18 months). The provisional
registration is valid indefinitely and sufficient for production use.

## Timeline

| Milestone | Estimated Time |
|-----------|---------------|
| Send review email | Day 1 |
| Review period | 2-4 weeks |
| Address feedback | 1 week |
| Submit IANA form | Day after review closes |
| IANA processes | 1-2 weeks |
| **Schemes registered** | **~6-8 weeks total** |
