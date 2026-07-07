# Security Policy

## Supported versions

The latest released version of CursorDump is supported with security fixes.

## Reporting a vulnerability

Please report vulnerabilities privately to
**contact@abstractframework.ai** rather than opening a public issue. Include
steps to reproduce and the impact you believe the issue has. You should
receive an acknowledgement within a few days.

## Scope and threat model

CursorDump is a local tool that reads your own Cursor data and serves a UI on
loopback. Its defenses, in scope for reports:

- **Loopback-only binding** — the server listens on `127.0.0.1` and rejects
  requests whose `Host` header is not a loopback name (DNS-rebinding defense).
- **Per-run API token** — every `/api/*` request requires a random token
  generated at startup and delivered to the browser through the opened URL.
  Other local processes cannot call the API without it.
- **Bounded file serving** — `/api/media` serves only files that resolve
  inside the scanned projects root / `~/.cursor/projects`, or external
  attachment paths actually referenced by an indexed transcript message. It
  is not a general file server.
- **Read-only source** — nothing writes, locks, or opens-for-write under
  `~/.cursor`; exports and backups refuse destinations inside it, with
  containment checks canonicalizing both sides (symlink-safe).
- **Output rendering** — transcript text renders in the browser via
  `textContent` (no HTML execution); served SVG/markup gets a restrictive
  `Content-Security-Policy` and a non-executable MIME type.

Out of scope: attacks requiring an attacker who already has local code
execution as your user (they can read `~/.cursor` directly), and the
sensitivity of dataset contents you choose to export or share — see the
privacy guidance in [docs/faq.md](docs/faq.md#privacy).

## Data handling

CursorDump makes no network requests. All data stays on your machine unless
you share an export or backup yourself. Exports scan their output for common
credential shapes and report them in the manifest (`secrets_detected`);
redaction is available with `--redact-secrets`.
