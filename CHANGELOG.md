# Changelog

All notable changes to CursorDump are documented here.

## [0.6.2] — 2026-07-07

Adversarial release verification: exports certified 8/8 (all format/mode
combinations, HF `datasets` loading, content audits, determinism, failure
modes) and backups certified 6/6 (135/135 transcript hashes, 38/38 attachment
hashes, self-healing incremental, bundled-app re-exploration proven via
attachments re-rooting). Two manifest bugs found and fixed:

### Fixed
- **Backup manifest merge**: a subset re-run (`--project X` into an existing
  multi-project backup) previously REPLACED the manifest, silently losing the
  integrity records of every other project. Manifests now merge: records for
  projects outside the run are preserved (projects, transcript hashes,
  attachments), with recomputed totals and a separate `last_run` block.
- **Attachment records**: external attachments are now listed in the manifest
  with sha256 hashes whenever PRESENT (hash reused from the prior manifest on
  incremental runs, avoiding re-hashing large media). Previously only newly
  copied files were counted, so incremental runs reported 0 and attachments
  had no hash entries at all.

### Added
- CLI: `--help`; export gains `--thinking tagged|verbatim|strip`,
  `--val <fraction>`, `--min-turns N`, `--no-media`, `--no-metadata`,
  `--final-only` (previously API/UI-only).

## [0.6.1] — 2026-07-07

### Fixed
- **Tools dropdown was open by default and wouldn't close** — `.tools-menu`'s
  `display:flex` overrode the `hidden` attribute; added `.tools-menu[hidden]{
  display:none}`. Verified: hidden by default, opens on click, closes on
  outside-click and on toggle.
- **Workspace `@file` attachments showed as "missing" and couldn't be opened.**
  Only pasted images/uploads live inside `~/.cursor`; files referenced by their
  workspace path (e.g. `/Users/…/docs/memory.md`) live outside it and the media
  resolver refused them. `resolve_media_path` now also serves a referenced file
  at its real path when it still exists (local, loopback-only, Host-guarded,
  media-extensions only). The exact `memory.md` from the report now resolves.

### Added
- **Backups capture external attachments.** Referenced workspace files that
  live outside `~/.cursor` (and still exist) are copied into
  `<backup>/attachments/<sha8>-<name>`, and the resolver re-roots to them, so a
  backup — and its bundled app — can open those attachments even after the
  original workspace changes. Toggle: UI "capture referenced workspace
  attachments"; CLI `--no-attachments`. (Note: files already deleted from disk
  can't be recovered — run a backup before they're gone.)

## [0.6.0] — 2026-07-07

### Changed — Unified message-level Finder (replaces the split filters)
- One filter surface in the top bar: **keyword + media chips (🖼🔊🎬📄📎) +
  Tools dropdown + scope**. Results are the **exact messages that match every
  active criterion**, not whole sessions. Selecting "image" now returns only
  the messages that actually carry an image attachment (verified: 303 messages,
  all user turns), with **thumbnails rendered right in the result**; adding a
  keyword narrows further (e.g. image + "roadmap" → 4). Results group by
  session with "show N more"; clicking a result jumps to that message.
- Removed the duplicate, session-level facet chips (sessions pane + search
  results) and the separate Search button — the finder is live/debounced and is
  the single place for keyword + attachment + tool filtering.
- New backend: `POST /api/find` over a message-level index (tools, media,
  snippet per message; ~15 MB) built once at startup in the background and
  invalidated on rescan. Keyword hits are collected uncapped
  (`search::collect_keyword_hits`) so combining keyword AND filters isn't
  truncated. Join key `(session_path, line_index)` verified consistent between
  the raw search and the parser.

### Added — attachment polish
- Uniform image thumbnails (180×135, captioned), a **lightbox** (click image →
  full size, Esc/click to close, "open raw"), fixed video sizing, `🚫` missing
  markers, a `📎 N` count on messages, `preload="none"` for videos, and
  `content-visibility` on message cards for smooth long sessions.

## [0.5.0] — 2026-07-06

### Added — Attachments in the viewer
- Messages now **render their attachments**: images inline (click to open full
  size), inline **video** and **audio** players, and chips linking to
  readable/document files. Missing files show a dimmed "(missing)" chip.
- New path-safe `GET /api/media?path=` serves referenced attachments with the
  right MIME type. It only serves media-classified files that resolve inside
  the scanned root / cursor projects boundary (verified: `/etc/passwd` → 403,
  outside paths → 404) and never serves HTML as executable.

### Added — Self-contained, Cursor-independent backups
- `cursordump backup` now **bundles the CursorDump binary** into the backup and
  writes a self-contained README. You can re-explore a backup with
  `./cursordump projects` from inside it — full explorer, thinking,
  **attachments**, search and export — with **no Cursor installation**.
- `/api/media` remaps original transcript paths onto the backup layout, so
  attachments render when browsing a backup too (verified: a 114 KB PNG served
  from a backup with Cursor absent).
- Options: UI checkbox "bundle the CursorDump app"; CLI `--no-app` to skip it.

## [0.4.1] — 2026-07-06

### Added
- **Filter sessions by tool used and by media kind attached.** Selecting a
  project shows tool chips (Shell, Read, Task, …) and media chips
  (image, readable, document, video, audio); toggling them narrows the session
  list to sessions that used those tools / attached those media types. Backed
  by a new cached `GET /api/facets?project=` (parses the project's transcripts
  once).
- **The same filters on SEARCH results**: a filter bar (media + tools, across
  all projects) appears above search hits; each hit also shows its session's
  media chips. `GET /api/facets` without a project computes the global set
  (~0.5 s for ~900 sessions, cached).

### Fixed — UX
- **Deselect a project**: clicking the active project again returns to the
  welcome screen and clears the session pane and viewer (previously stuck).
- **Sessions "Clear"** now deselects ALL of the project's sessions (not just
  the currently-visible rows), so it reliably empties the export selection.
- **Search clear (✕)** button resets the query and results, returning to the
  current session/project or the welcome screen.

## [0.4.0] — 2026-07-06

### Added — Full backup
- **Full, faithful backup of Cursor projects** (`src/backup.rs`) — a verbatim
  recursive copy of `~/.cursor/projects/<slug>/…` (transcripts, subagents,
  assets, uploads, canvases, terminals, tool files) mirrored under
  `<out>/projects/<slug>/…`, so nothing is lost if Cursor flushes its data.
  - **Incremental & idempotent**: re-running into the same folder copies only
    files whose size or mtime changed.
  - Preserves modification times (faithful restore, correct incremental).
  - Records a sha256 per `.jsonl` transcript in `cursordump-backup.json` for
    integrity; writes a `README.md` with restore instructions
    (`cp -a projects/* ~/.cursor/projects/`).
  - Skips regenerable caches (`node_modules`, `.git`) always; optional
    `--skip-runtime` also skips `terminals/` and `agent-tools/`.
  - Read-only on source; refuses destinations inside `~/.cursor` or populated
    non-backup folders.
- Web UI **🗄 Backup…** dialog (all projects or the selected one) and CLI:
  `cursordump backup --out <dir> [--project <slug>]… [--skip-runtime] [--no-verify]`.
- API: `POST /api/backup`, `GET /api/default_backup_dir`.

## [0.3.1] — 2026-07-06

Fixes from a second adversarial review round (UX, security, data-quality
subagents).

### Fixed — data quality
- **Curly-apostrophe blindness**: deliberation detection now normalizes
  U+2019/U+2018 before matching, so GPT-family transcripts ("I’ll", "I’m") are
  handled. Residual thinking leaked into answers dropped from ~691 to ~4 on a
  real project; captured think ratio rose from ~6% to ~28%.
- **Think-only turns** (assistant text that is all reasoning) are dropped
  instead of emitting `<think>…</think>` with an empty answer.
- **Literal `<think>` tokens** inside content are neutralized so the exporter's
  wrapper is the only real tag pair (no unbalanced tags).
- **Headerless detection hardened**: strong vs weak first-person openers +
  deliverable-structure vetoes prevent answer paragraphs ("Now I'm setting up
  the HTML structure…") from being misfiled as thinking.
- **`task_calls` metadata** is emitted only on chunk 0 (no ~7× inflation when
  summing across chunked records).
- **Resume Task calls** are matched (pass 3) and unmatched foreground calls
  render a `{"status":"unresolved"}` result instead of dangling.
- **Dataset card** text is now conditional on the actual thinking mode and
  subagent mode (previously claimed "tool calls excluded / narration stripped"
  even when tagged thinking and inlined Task results were present).

### Fixed — CLI
- Added `--subagent-mode inline|separate|drop`; `--include-subagents` now
  implies Separate (no accidental inline+separate double-export).

### Fixed — UX (web UI)
- "Select all" selects only currently-visible rows (respects filter/collapse),
  no longer silently selecting hidden subagents.
- Selection summary and export result break down main vs subagent counts and
  explain when subagents were inlined rather than written separately.
- URL hash state: deep links, Back button, and recoverable search results.
- Search hits highlight the term (`<mark>`), auto-expand the containing
  thinking/tool block, and flash the target message.
- "Expand/collapse all thinking" control; tool chips show ▸/▾ + active state
  and a labelled input panel ("outputs are not recorded in transcripts").
- Per-project ⬇ export shortcut; preset buttons show active state; Export
  button disabled during export; empty states and `/`-to-search keyboard hint.

## [0.3.0] — 2026-07-06

### Changed
- **UI rewritten with web technologies.** The native egui GUI is replaced by a
  local axum web server (`src/server/`) with an embedded vanilla-JS frontend
  (`src/server/ui/`). The Rust core library is unchanged. Launch opens the
  browser at `http://127.0.0.1:7070`.
  - Assistant **thinking is visible** in a collapsible 💭 section (previously no
    access at all).
  - **Tool calls expand on click** to their full pretty-printed input
    (previously hover-only, undiscoverable).
  - **Explicit selection → Export flow**: per-session checkboxes, "Select all",
    a live "N selected" count and a prominent **⬇ Export…** button (previously
    an obscure basket/bottom-bar).

### Added
- **Headerless thinking detection**: first-person deliberation paragraphs with
  NO bold header (e.g. "Now I'm creating an end-to-end example that…") are now
  detected, with a stricter opener+density heuristic to avoid false positives.
  Fixes sessions (e.g. `a2a`) that showed zero thinking before.
- JSON API (`/api/projects|sessions|session|search|export|rescan`) and a
  headless-safe frontend (all transcript text rendered via `textContent`).

### Security
- Server binds `127.0.0.1` only and validates the `Host` header (loopback
  allowlist) to defeat DNS-rebinding attacks that could otherwise read
  transcripts or write files via `/api/export`.
- `/api/session` serves only paths the scanner produced (no path traversal).
- Export refuses to write into a populated non-CursorDump directory (won't
  clobber user files) in addition to refusing anything inside `~/.cursor`.
- `RwLock` access recovers from poisoning instead of bricking endpoints.

## [0.2.0] — 2026-07-06

### Added
- **Thinking capture**: assistant reasoning is no longer only strippable. New
  `ThinkingMode` (Tagged / Verbatim / Strip, default Tagged) captures thinking
  as a leading `<think>…</think>` block in SFT (reasoning-model convention) and
  verbatim in CPT. `clean::split_thinking` separates reasoning from answer.
- **Subagent linkage & export**: Task-tool calls are matched to their subagent
  transcripts (prompt ↔ first user query, ~94% on real data). New
  `SubagentMode` (Inline / Separate / Drop, default Inline): Inline splices a
  foreground subagent's final answer into the master turn as a tool result and
  marks background tasks `spawned_in_background`; Separate exports subagents as
  `agent_brief` records. Metadata carries `task_calls`, `parent_session_id`,
  `task_prompt_hash`, `match`, `spawn_order`.
- **UI redesign**: cohesive dark theme (`gui/theme.rs`), colored role badges,
  count chips (main vs subagent), collapsible thinking in the viewer, subagent
  transcripts nested under their master in the session list, numbered
  onboarding guide, empty states, icons, read-only indicator.
- **Docs**: new `docs/UserGuide.md` with an end-to-end walkthrough and explicit
  Unsloth Studio and ForgeLLM (→ AbstractForge) instructions.

### Changed
- Assistant cleaning split into thinking handling vs link/intent trimming.
- `record_metadata` gains `parent_session_id`, chunk indices and task links.
- Default `max_record_chars` 120k → 100k (safer 32k-window margin).
- Harness boilerplate blocklist extended ("Briefly inform the user…").
- Thinking-capture fidelity safeguards (from adversarial review): structured
  paragraphs (code fences, tables, bullet/numbered lists, headings) are never
  classified as thinking; `**Header**` lines are kept inside `<think>` blocks
  for auditability; every record carries `think_chars`/`answer_chars` and a
  consistent `user_kind` ("human" vs "agent_brief") for downstream filtering.

## [0.1.0] — 2026-07-06

Initial release.

### Added
- Native GUI (Rust + eframe/egui) to explore local Cursor agent sessions.
  - Project list (recency-sorted, filterable, empty-project toggle).
  - Session list per project with derived titles, dates, sizes, subagent
    transcripts.
  - Message viewer: user/assistant/tool-call rendering, long-text expansion,
    raw-user toggle, tool-call chips with input on hover.
  - Global keyword search across all transcripts (background thread,
    cancellable) with jump-to-message.
  - Export basket (path-based selection) with bottom-bar status.
- Dataset export to a self-describing dump directory:
  - SFT ChatML (`messages`) and ShareGPT (`conversations`) JSONL.
  - CPT raw-corpus (`{"text"}`) JSONL and per-session `.txt` (ForgeLLM).
  - `manifest.json` (provenance, options, media index, per-file line counts)
    and generated `README.md` dataset card.
  - Presets (Chat SFT, Agentic SFT, CPT corpus, Everything) and per-option
    controls; train/val split; record chunking by size.
- Headless `cursordump export …` CLI mode for scripted exports.
- Media handling: reference extraction, classification
  (readable/document/image/video/audio), containment-restricted copying.

### Data-quality pipeline (from adversarial review)
- Turn segmentation on real user records (ignores unreliable `turn_ended`).
- Harness-injected user records (subagent/background notifications) excluded
  from clean content and prevented from splitting turns.
- Assistant cleaning: strips Cursor thinking-summary narration and IDE chat
  links; trims dangling "I'll now…" intents when tool calls are excluded.
- Consistent per-record `metadata` (schema-stable) with chunk indices.

### Safety
- Strictly read-only access to `~/.cursor`; exports refused inside it.
- Snapshot-tolerant parser (torn lines, invalid UTF-8, BOM, unknown types).
- Media copies restricted to the projects root; others manifest-listed only.
- Manifest written last with line counts for truncation detection.

### Tested
- 50 automated tests: unit, integration against real sessions (read-only),
  adversarial fixtures (hostile inputs), turn-semantics regressions.
