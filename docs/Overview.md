# CursorDump — Overview

## Goal

CursorDump is a local web application (Rust + axum server + embedded
browser UI) that explores Cursor agent sessions stored on the local machine and
exports them as training-ready datasets for supervised fine-tuning (SFT) and
continued pre-training (CPT). Exports target Unsloth Studio, ForgeLLM, and any
HuggingFace `datasets`-compatible pipeline.

> The UI moved from native egui to a local web frontend in v0.3 for full
> control over aesthetics and interaction (visible thinking, click-to-expand
> tool calls, an obvious selection→export flow). The Rust core (scanner,
> parser, export, search) is unchanged and shared by both the server and the
> headless CLI.

## Core requirements

1. List all Cursor projects found under `~/.cursor/projects/`.
2. List all agent sessions (transcripts) per project.
3. Explore sessions: browse messages, tool calls, turns.
4. Keyword search across projects/sessions/messages.
5. Export one or more sessions (or whole projects) into:
   - SFT ChatML: `{"messages": [{"role": "...", "content": "..."}]}` JSONL
   - SFT ShareGPT: `{"conversations": [{"from": "...", "value": "..."}]}` JSONL
   - CPT raw corpus: `{"text": "..."}` JSONL (+ optional plain `.txt` per session for ForgeLLM)
6. Media handling: detect media referenced in sessions, classify as
   readable (txt/md/csv/docx/json...), image (png/jpg/...), video (mov/mp4/...),
   audio (wav/m4a/...). Optionally copy into the dump under `media/` and record a
   `manifest.json`. Readable files can optionally be inlined as CPT documents.
7. STRICTLY READ-ONLY access to `~/.cursor/projects` — never modify or lock the
   source data; live sessions must be unaffected.

## Source data format (observed on-disk, Cursor July 2026)

- Root: `~/.cursor/projects/<project-slug>/`
  - `agent-transcripts/<session-uuid>/<session-uuid>.jsonl` — session transcript
  - `assets/` — user-provided media (screenshots etc.), absolute paths referenced in messages
  - `terminals/`, `mcps/`, `canvases/`, `agent-tools/` — not exported (runtime state)
- Transcript JSONL records:
  - `{"role": "user"|"assistant", "message": {"content": [block, ...]}}`
  - `{"type": "turn_ended", "status": "success"|..., "error"?: ...}` — turn boundary
- Blocks: `{"type":"text","text":...}` and `{"type":"tool_use","name":...,"input":{...}}`
- User text wraps the real query in `<user_query>...</user_query>`; surrounding text is
  system-injected context (`<image_files>`, `<attached_files>`, `<external_links>`,
  `<timestamp>`, rules...). Images show as `[Image]` placeholder blocks.
- Some project dirs are numeric IDs (no workspace path); slug dirs encode the
  workspace path (`Users-albou-projects-foo` ⇒ `/Users/albou/projects/foo`, ambiguous
  because `-` can be part of a name; display slug as-is plus best-effort path).

### Corrections from adversarial design review (verified on real data)

- `turn_ended` markers exist for only ~20% of turns. Turn segmentation MUST split
  on the next user record, not on `turn_ended`.
- Session dirs may contain `subagents/<uuid>.jsonl` (5x more transcripts than main
  ones on this machine). They are listed and viewable; export includes them only
  when the "include subagent transcripts" option is on (default off).
- Transcripts contain NO tool results, only tool calls. Rendering tool calls into
  SFT data without results risks teaching hallucinated tool outcomes: default SFT
  preset excludes tool calls; the "agentic" preset renders them with an explicit
  caveat in the dataset card.
- Some user records are system-injected (no `<user_query>` tag: subagent
  notifications, system notifications). Default: treated as context and excluded
  from "clean" user content; toggle to keep raw.
- Consecutive user records occur (resumed sessions, queued messages): merged.
- Media dirs: per-project `assets/` and `uploads/`. `docx`/`pdf` are readable
  documents but NOT inlineable plain text; only txt/md/csv/json/code files may be
  inlined into CPT.
- No `Workspace Path` metadata exists in transcripts; project display name is a
  best-effort decode of the dir slug, and numeric-ID dirs display as-is.
- Session title: first `<user_query>` (truncated); date: file mtime; size: bytes.

### Safety invariants (from red-team review)

1. Never write, create, lock, or open-for-write anything under `~/.cursor`.
2. Refuse to export INTO `~/.cursor` (path containment check on canonicalized paths).
3. Parser is snapshot-tolerant: a torn/partial trailing line (live session being
   appended) is skipped and counted, never fatal.
4. Media copy: only copy files that resolve (after symlink canonicalization) inside
   `~/.cursor/projects`; anything else is listed in the manifest as
   `referenced-not-copied`. No symlink following outside the boundary.
5. Copied media names are `<sha256-prefix>-<sanitized-basename>` to avoid collisions.
6. Exports write into the final directory but produce `manifest.json` LAST, and the
   manifest records line counts per file, so a truncated export (disk full, crash)
   is detectable; errors surface in the GUI with partial-output warnings.
7. All records in one JSONL file share the exact same top-level key set
   (HF `load_dataset("json")` requires column consistency).

## Export design

A dump is a directory:

```
<dump-name>/
├── sft_chatml.jsonl      # if SFT selected
├── sft_sharegpt.jsonl    # optional variant
├── cpt.jsonl             # if CPT selected  {"text": ...}
├── cpt_txt/              # optional per-session .txt (ForgeLLM dataset/ style)
├── media/                # optional copies of referenced media
├── manifest.json         # sessions, sources, media classification, options used
└── README.md             # dataset card (HF-style): provenance, format, counts
```

### SFT mapping

- One JSONL record per session (multi-turn conversation).
- A "turn" = one user record + all assistant records until `turn_ended`.
- User content: extracted `<user_query>` by default (toggle: keep full raw text).
- Assistant content: text blocks joined; tool calls optionally rendered as
  fenced blocks (`` ```tool_call {name, input}``` ``) or dropped (toggle).
  Rationale: Unsloth text SFT cannot use structured tool_call fields; rendering
  keeps agentic behaviour learnable, dropping yields clean chat data.
- Consecutive same-role messages merged (ChatML requires alternation).
- Metadata per record: project, session id, timestamp, turn count (in a
  `metadata` object; ignored by trainers, useful for filtering).

### CPT mapping

- One `{"text": ...}` record per session: transcript rendered as readable
  markdown (`## User` / `## Assistant` sections, tool calls summarized).
- Optional: inline readable attachments as extra `{"text": ...}` records.
- No chat template; natural flowing text per CPT best practices.

### Media

- Extract `/…/assets/…` paths and workspace file references from user messages.
- Classify by extension: readable / image / video / audio / other.
- `manifest.json` records every reference: original path, classification,
  exists-on-disk, sha256 (if copied), copied relative path.
- Toggle: copy media into dump; toggle: inline readable files into CPT.

## Architecture (modules)

```
src/
├── main.rs           # CLI: web server (default) or headless `export`
├── model.rs          # domain types: Project, Session, Turn, Message, Block, MediaRef
├── scanner.rs        # discover projects/sessions (read-only), file metadata
├── parser.rs         # tolerant JSONL parsing -> model types; user_query extraction
├── media.rs          # media reference extraction + classification
├── search.rs         # keyword search (substring, cancellable)
├── backup.rs         # full verbatim project backup (incremental, integrity)
├── server/
│   ├── mod.rs        # axum router, 127.0.0.1 bind, Host-header guard, state
│   ├── api.rs        # JSON API handlers (projects/sessions/session/search/export)
│   └── ui/           # embedded frontend: index.html, app.css, app.js
├── export/
│   ├── mod.rs        # ExportOptions, orchestration, turn rendering + chunking
│   ├── clean.rs      # thinking split (header + headerless), link/intent cleaning
│   ├── subagent.rs   # Task↔subagent linkage, inline/separate rendering
│   ├── sft.rs        # ChatML + ShareGPT writers
│   ├── cpt.rs        # raw corpus writer + txt files
│   └── manifest.rs   # media copy, manifest + dataset card writer
```

Threading: parse/search/export run on Tokio `spawn_blocking`; the async
runtime never blocks. Parser is tolerant: unknown record types and malformed
lines (and invalid UTF-8 / BOM) are skipped and counted, never fatal (sessions
may be actively written by running agents; we read a snapshot).

Security: the server binds `127.0.0.1`, rejects non-loopback `Host` headers
(DNS-rebinding defense), serves only scanner-produced session paths (no
traversal), renders all transcript text in the browser via `textContent` (no
XSS), and refuses exports into `~/.cursor` or any populated non-dump directory.

## Non-goals

- No writes into `~/.cursor` (only reads).
- No tokenization/training; we produce data, trainers consume it.
- No parsing of Cursor's internal SQLite chat DBs (transcripts are the
  authoritative export surface).
