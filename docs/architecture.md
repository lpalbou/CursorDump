# Architecture

CursorDump is a single Rust binary with two frontends over one core library:
a local web server (axum) with an embedded browser UI, and a headless CLI.
The core does all the work — scanning, parsing, cleaning, exporting, backup —
and both frontends share it.

## Source data

Cursor stores agent transcripts on disk under `~/.cursor/projects/`:

```text
~/.cursor/projects/<project-slug>/
├── agent-transcripts/<session-uuid>/
│   ├── <session-uuid>.jsonl        # main session transcript
│   └── subagents/<uuid>.jsonl      # transcripts of spawned subagents
├── assets/ , uploads/              # user-provided media (screenshots, files)
└── terminals/ , mcps/ , canvases/ , agent-tools/   # runtime state
```

Transcript records are JSONL:

- `{"role": "user"|"assistant", "message": {"content": [block, …]}}` with
  blocks `{"type":"text","text":…}` and
  `{"type":"tool_use","name":…,"input":{…}}`.
- User text wraps the human query in `<user_query>…</user_query>`;
  surrounding text is system-injected context (attached files, rules,
  timestamps). Some user records are entirely harness-injected (subagent and
  background-task notifications).
- Transcripts contain tool *calls* but never tool *results*; a subagent's
  output exists only in its own transcript.

The full observed format and the dataset-quality rules derived from it are
documented in [knowledge-base.md](knowledge-base.md).

## Module map

```text
src/
├── main.rs           # CLI: web server (default), headless `export`, `backup`
├── model.rs          # domain types: Project, SessionMeta, Message, Block, MediaRef
├── scanner.rs        # project/session discovery (read-only, metadata scan)
├── parser.rs         # tolerant JSONL parsing; user_query extraction; turn segmentation
├── media.rs          # media reference extraction + classification
├── search.rs         # keyword search across transcripts
├── backup.rs         # verbatim incremental backup with integrity manifest
├── server/
│   ├── mod.rs        # axum router, loopback bind, Host guard, token auth, state
│   ├── api.rs        # JSON API handlers + message-level finder index
│   └── ui/           # embedded frontend: index.html, app.css, app.js
└── export/
    ├── mod.rs        # ExportOptions, orchestration, turn rendering, chunking
    ├── clean.rs      # thinking/answer separation, link and intent cleaning
    ├── subagent.rs   # Task-call ↔ subagent-transcript linkage
    ├── secrets.rs    # credential detection and redaction
    ├── sft.rs        # ChatML + ShareGPT writers
    ├── cpt.rs        # JSONL corpus + plain-text writers
    └── manifest.rs   # media copy, manifest.json, dataset card
```

## Data flow

```text
~/.cursor/projects/                        (read-only source)
        │
        ▼
scanner.rs ──► Vec<Project{ SessionMeta… }>       metadata-only scan
        │
        ▼
server/ (axum, 127.0.0.1, Host + token guarded) ──► browser UI
   /api/projects /sessions /session /find /media /export /backup /rescan
        │
        ├── /api/session ──► parser.rs ──► split_thinking ──► JSON
        ├── /api/find    ──► cached message index (keyword ∩ media ∩ tools)
        └── /api/export  ──► export/mod.rs (spawn_blocking)
                           ├─ parser.rs           full parse per selected session
                           ├─ export/subagent.rs  link Task calls ↔ subagents
                           ├─ export/clean.rs     thinking split, link/intent cleanup
                           ├─ export/sft.rs       sft_chatml/, sft_sharegpt/
                           ├─ export/cpt.rs       cpt/, cpt_txt/
                           ├─ media.rs            reference extraction + classification
                           └─ export/manifest.rs  media copy, secret scan, manifest LAST
```

IO-heavy work (parsing, search, export, backup) runs on Tokio
`spawn_blocking`; the async runtime never blocks. The project scan is cached
in the server state (`/api/rescan` refreshes it), and the message-level
finder index is built once in the background at startup, invalidated on
rescan with a generation guard so a stale build is never cached.

## Turn model

A turn is one or more consecutive user records plus the assistant records
that follow, ended by the next *real* user record. The `turn_ended` markers
present in transcripts are unreliable (they exist for a minority of turns)
and are used only for error counting, never segmentation. Harness-injected
user records never start a new turn. A turn is exported only when both its
rendered user text and rendered assistant answer are non-empty.

`render_assistant` produces three views per turn:

- `thinking` — the reasoning narration (from `clean::split_thinking`),
- `answer` — user-facing text (links stripped, dangling intents trimmed),
- `native` — the full original text with thinking left inline.

Writers compose from these per `ThinkingMode`: SFT emits
`<think>…</think>` + answer when tagged; CPT uses the native text (or
answer-only when stripping). Chunking bounds records by the largest view so
`max_record_chars` holds in every mode.

## Tolerant parsing

Transcripts may be actively appended by running agents, so the parser reads
a snapshot and never fails hard: malformed lines, torn trailing lines,
BOMs, and invalid UTF-8 are skipped and counted (`skipped_lines`); unknown
record and block types are preserved as opaque values and excluded from
exports.

## Security model

The threat model and reporting process live in [SECURITY.md](../SECURITY.md).
Mechanisms:

- **Loopback + Host guard** — binds `127.0.0.1`; non-loopback `Host` headers
  are rejected (DNS-rebinding defense).
- **Per-run API token** — required on every `/api/*` request; generated at
  startup and delivered via the opened URL (`X-CursorDump-Token` header, or
  `?token=` for `<img>`/`<video>` requests that cannot set headers).
- **Bounded media serving** — `/api/media` serves only files canonicalizing
  inside the scanned root or `~/.cursor/projects`, plus external attachment
  paths actually referenced by an indexed message; files stream rather than
  buffer. When exploring a backup, paths re-root onto the backup's mirror.
- **XSS-safe rendering** — all transcript text renders via `textContent`;
  served SVG/markup gets a restrictive CSP and non-executable MIME types.

## Safety invariants

1. Nothing writes, locks, or opens-for-write under `~/.cursor`.
2. Exports and backups refuse destinations inside `~/.cursor`, and refuse
   populated directories that are not a prior dump/backup. Containment
   checks canonicalize both sides (symlink-safe).
3. Media copies are driven exclusively by attachments referenced in USER
   messages: files canonicalizing inside `~/.cursor/projects`, plus external
   workspace attachments that still exist at their referenced path.
   Incidental paths in assistant or tool output never trigger a copy, and
   nonexistent references are manifest-listed only.
4. `manifest.json` is written last and records per-file line counts, so a
   truncated export (disk full, crash) is detectable.
5. All records in one JSONL file share the same top-level key set
   (HuggingFace column consistency).

## Non-goals

- No writes into `~/.cursor` (read-only by design).
- No tokenization or training; CursorDump produces data, trainers consume it.
- No parsing of Cursor's internal SQLite databases — the on-disk JSONL
  transcripts are the authoritative surface.

## See also

- [api.md](api.md) — CLI, JSON API, and output schemas.
- [knowledge-base.md](knowledge-base.md) — transcript format details and
  dataset-quality rules.
- [exporting.md](exporting.md) — export behavior from a user's perspective.
