# Troubleshooting

Symptom-oriented fixes. If your question is conceptual, try the
[FAQ](faq.md) first.

## Server and UI

### The browser shows 401 / "unauthorized" on API calls

Every `/api/*` request needs the per-run token, delivered through the URL
CursorDump opens (or prints with `--no-open`). Reload the app through that
tokenized URL (`http://127.0.0.1:<port>/?token=…`) rather than a plain
bookmark — the frontend stores the token for the session and strips it from
the address bar. Restarting the server generates a new token, so old tabs
need the new URL.

### `Error: Address already in use`

Another process holds the port. Start with a different one:

```bash
cursordump --port 7075
```

### The browser doesn't open / I'm on a headless machine

Run with `--no-open` and open the printed URL yourself. The server only
listens on `127.0.0.1`; to use it from another machine, tunnel the port
(e.g. `ssh -L 7070:127.0.0.1:7070 host`) and open the tokenized URL locally.

### No projects are listed

CursorDump scans `~/.cursor/projects` by default. If your data lives
elsewhere (or you are exploring a backup), pass the root explicitly:

```bash
cursordump /path/to/projects
```

Press **↻ Rescan** after new sessions appear; the project list is cached.

### An attachment shows as "missing"

The transcript references a file that no longer exists at its original path
(workspace files move or get deleted). Backups capture still-existing
external attachments precisely to avoid this — see
[backup.md](backup.md#what-a-backup-contains).

## Export

### "refusing to export into …" / "exists and is not empty"

Export destinations must be outside `~/.cursor`, and an existing non-empty
directory is only accepted when it is a previous CursorDump dump. Choose a
fresh folder or a prior dump directory.

### The export reports detected secrets

The written files contain strings matching credential patterns. Re-run with
`--redact-secrets` (or tick *redact secrets* in the dialog) and check
`manifest.json` → `secrets_detected` is empty. Review the data regardless —
detection is pattern-based. See
[exporting.md](exporting.md#secret-scanning-and-redaction).

### A session I selected produced no records

Turns are exported only when both the rendered user text and assistant
answer are non-empty; sessions below `--min-turns` are skipped. The export
summary lists skipped sessions, and `manifest.json` records per-session
`turns_trainable`.

### `load_dataset` fails on the output

Point `data_dir` at one format subdirectory (`sft_chatml/`, `cpt/`), not at
the dump root — each subdirectory holds exactly one schema.

### Records are larger than my context window

Lower *max record size* (`max_record_chars`): sessions split at turn
boundaries. Single oversized turns are kept whole and flagged with
`metadata.oversize: true`; filter those downstream if needed. See
[exporting.md](exporting.md#record-length-and-chunking).

## Backup

### "exists and is not empty — choose a new folder …"

Backup destinations must be empty, or a previous CursorDump backup
(recognized by `cursordump-backup.json`). This prevents merging into an
arbitrary directory.

### A warning: "copy … No such file or directory"

A file disappeared between scan and copy — typically Cursor regenerating a
runtime file mid-backup. The rest of the backup is unaffected; re-run to
pick up the file if it exists again.

### A warning: "prior manifest was unreadable"

The existing `cursordump-backup.json` could not be parsed; it was preserved
as `cursordump-backup.json.corrupt` and a fresh manifest was written for
this run. Integrity records from the corrupt file are not merged — re-run a
full backup to regenerate complete records.

### The bundled `cursordump` won't run on macOS

Clear the quarantine attribute:

```bash
xattr -d com.apple.quarantine cursordump
```

If the binary was built for a different OS/architecture, build CursorDump
from source and run `cursordump /path/to/backup/projects`.

## Build and test

### `cargo build` fails on an old Rust

The MSRV is 1.75. Update with `rustup update stable`.

### Integration tests fail or skip on CI

Tests that need real Cursor data skip gracefully when `~/.cursor/projects`
is absent. Ensure the CI gate matches the project's:

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

## Still stuck?

Open a GitHub issue with the command, the full output, and (redacted)
transcript snippets if relevant. For suspected security problems, follow
[SECURITY.md](../SECURITY.md) instead.
