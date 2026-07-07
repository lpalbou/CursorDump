# Full Backups

A backup is a verbatim, complete copy of your Cursor projects — every
transcript, subagent, asset, upload, canvas, terminal, and tool cache — that
survives independently of Cursor. It is distinct from a
[dataset export](exporting.md), which transforms transcripts for training.

## Why back up

Cursor can clear old agent data. A backup preserves your full agent history
in the original layout, with integrity records, and can be explored or
restored at any time.

## Creating a backup

In the UI: **🗄 Backup…** (top bar) → all projects or the selected one →
choose a folder outside `~/.cursor` → **Back up**.

From the CLI:

```bash
cursordump backup --out ~/Documents/cursordump-backup                    # all projects
cursordump backup --out ~/Documents/cursordump-backup --project <slug>   # subset (repeatable)

# Options:
#   --skip-runtime      omit regenerable terminals/ and agent-tools/ caches
#   --no-verify         skip per-transcript sha256 hashing
#   --no-app            don't bundle the cursordump binary
#   --no-attachments    don't capture external workspace attachments
```

## What a backup contains

```text
cursordump-backup/
├── projects/<slug>/…           # verbatim mirror of ~/.cursor/projects/<slug>/
├── attachments/                # external workspace @files referenced by sessions
├── cursordump                  # the bundled app (self-contained exploration)
├── cursordump-backup.json      # manifest: projects, sha256 per transcript, attachments
└── README.md                   # restore and exploration instructions
```

- **Verbatim mirror** — original layout and modification times preserved;
  `node_modules/` and `.git/` are always skipped (regenerable, never Cursor
  data).
- **External attachments** — workspace `@file` references that live outside
  `~/.cursor` (and still exist) are copied into `attachments/` with sha256
  hashes, so they survive workspace changes. Files already deleted cannot be
  recovered — back up before they are gone.
- **Integrity** — the manifest records a sha256 for every `.jsonl`
  transcript (disable with `--no-verify`) and for every captured attachment.

## Incremental behavior

Re-running a backup into the same folder copies only files whose size or
modification time changed — cheap enough to run on a schedule (cron,
launchd, or a Cursor hook). Subset re-runs (`--project X`) merge into the
manifest: records for projects outside the run are preserved. Changed
external attachments are re-copied and re-hashed.

## Exploring a backup without Cursor

The backup bundles the `cursordump` binary, so it is self-contained:

```bash
cd ~/Documents/cursordump-backup
./cursordump projects
```

This opens the full explorer — sessions, thinking traces, attachments,
finder, and dataset export — directly on the backup, no Cursor installation
required. On macOS, a first run may need
`xattr -d com.apple.quarantine cursordump`. If the bundled binary does not
match your OS/architecture, build CursorDump from source and run
`cursordump /path/to/backup/projects`.

Attachment paths inside transcripts are re-rooted automatically: media that
lived under `~/.cursor/projects/` resolves to the backup's `projects/`
mirror, and external attachments resolve to the backup's `attachments/`
copies.

## Verifying a backup

```bash
cursordump verify ~/Documents/cursordump-backup
```

Recomputes the sha256 of every transcript and attachment recorded in the
manifest and reports ok / mismatched / missing counts, exiting non-zero on
any failure. It also flags *unlisted* transcripts — `.jsonl` files present
in the backup tree but absent from the manifest, whose integrity is
therefore unknown. Verification is read-only. Backups made with
`--no-verify` have no transcript hashes; `verify` reports those entries as
unhashed.

## Restoring into Cursor

```bash
cursordump restore --from ~/Documents/cursordump-backup --dry-run   # preview
cursordump restore --from ~/Documents/cursordump-backup             # restore
```

Restore is deliberately conservative:

- by default it copies only files **missing** at the destination — existing
  files are never touched;
- `--overwrite` additionally replaces destination files whose size or
  modification time differ from the backup;
- nothing is ever deleted;
- `--project <slug>` (repeatable) restores a subset;
- `--dry-run` prints what would be copied without writing.

Because the backup mirrors the original layout, a manual copy works too:

```bash
cp -a ~/Documents/cursordump-backup/projects/* ~/.cursor/projects/
```

Run `cursordump verify` first if you need certainty about integrity.

## See also

- [getting-started.md](getting-started.md) — the UI backup dialog.
- [api.md](api.md#backup) — CLI flags and the backup manifest schema.
- [troubleshooting.md](troubleshooting.md) — common backup issues.
