# Knowledge Base: the Cursor Transcript Format and Dataset-Quality Rules

This page documents the Cursor on-disk transcript format as observed on real
data, and the rules CursorDump applies to produce high-quality training
data. It is the rationale behind the behavior described in
[exporting.md](exporting.md); contributors should read it before changing
export or cleaning logic (see [CONTRIBUTING.md](../CONTRIBUTING.md)).

Percentages below are measurements from large real-world session corpora and
will vary by machine and usage; the rules are designed to hold regardless.

## On-disk transcript format (observed July 2026)

- Sessions: `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`.
- Subagent (Task tool) sessions: `…/<uuid>/subagents/<sub-uuid>.jsonl`.
  Subagents can heavily outnumber main transcripts (≈5:1 observed).
- Records are one JSON object per line:
  - `{"role":"user"|"assistant","message":{"content":[block,…]}}`
  - `{"type":"turn_ended","status":"success"|…,"error"?:…}`
- Blocks: `{"type":"text","text":…}`,
  `{"type":"tool_use","name":…,"input":{…}}`.
- Media lives in per-project `assets/` and `uploads/`, referenced by
  absolute path inside user text (tags like `<image_files>`,
  `<attached_files>`, `<uploaded_documents>`). Asset filenames are
  underscore-sanitized.
- No workspace-path metadata is stored; a project's display name is a
  best-effort decode of its directory slug.
- Session titles come from the first `<user_query>`; dates from file mtime.

## Dataset-quality rules

1. **`turn_ended` is unreliable** — present for only ~20% of turns. Turns
   are segmented on the next *real* user record instead; `turn_ended` is
   used only for error counting.
2. **Transcripts contain tool CALLS but never tool RESULTS.** Rendering
   tool calls into SFT teaches call syntax against invisible outputs, so
   they are excluded by default and opt-in via `--tool-calls`.
3. **Not every `<user_query>` is human input.** The harness injects records
   with `<user_query>` tags for subagent-result and background-task
   notifications. These never split a turn and are excluded from clean user
   content (a prefix blocklist identifies them).
4. **Assistant text embeds "thinking summary" narration** — runs of
   `**Title Case Header**` followed by first-person deliberation, often
   glued to the previous sentence by a single newline (~34% of assistant
   messages observed). It is not a separate content type, so
   `split_thinking` detects it heuristically and separates it from the
   answer, letting exports tag it `<think>…</think>`, keep it verbatim, or
   strip it. Detection is deliberately conservative (header-gated plus
   first-person markers, with structural vetoes): since thinking is
   *captured* rather than discarded, a false positive would mislabel a real
   answer as reasoning. These traces are Cursor's summarized reasoning — a
   distillation, not raw chain-of-thought — and the dataset card says so.
5. **Headerless thinking exists too.** Some reasoning summaries are plain
   first-person planning paragraphs with no bold header ("Now I'm creating
   an end-to-end example that…"). These are detected with a stricter bar
   than the header path: the paragraph must open with a first-person
   planning phrase and pass marker-density and structural vetoes. Short
   single-sentence narrations ("Let me check the parser.") count as
   thinking; user-facing closings ("Let me know if…") are explicitly
   excluded.
6. **Cursor chat links `[label](<uuid>)`** appear in assistant and
   user-pasted text; the UUIDs are dead outside the IDE. They are rewritten
   to plain `label`.
7. **Merged assistant turns can end with dangling intent** ("Now I'll run
   the tests.") when tool calls are stripped, which would teach
   announce-then-stop. Trailing intent sentences are trimmed when no
   tool/result content follows.
8. **Sessions are heavy-tailed**: a few sessions hold most of the
   characters, and single turns can reach hundreds of thousands of tokens.
   Records are chunked at turn boundaries (default 100k chars ≈ 25k tokens)
   so ordinary records fit common context windows; a single oversized turn
   is kept whole (splitting inside a turn breaks the example) and flagged
   `metadata.oversize: true`.
9. **Session ids are NOT unique** within a project: a self-forked subagent
   reuses the parent id, and the identical content exists as two files
   (main + `subagents/<same-id>.jsonl`). The transcript *path* is the
   identity everywhere, and Separate-mode exports drop the subagent copy
   when the main transcript with the same id is selected (otherwise ~6% of
   records are near-duplicates).
10. **Master ↔ subagent (Task tool) linkage.** A master invokes subagents
    via a `tool_use` named "Task" (`input: {subagent_type, description,
    prompt, run_in_background}`) with no subagent id recorded. The reliable
    link is the subagent's first `<user_query>` matching the Task `prompt`
    (normalized exact, then substring, then a resume pass — ~94% match
    observed). Rule: **inline a FOREGROUND subagent's final answer as the
    Task result; never inline a BACKGROUND result** (the master continued
    result-blind, so splicing one in teaches ignoring tool output; ~50% of
    Task calls are background). Never mix Inline masters and Separate
    subagents in one dataset without deduplicating by `task_prompt_hash`,
    or subagent tokens double-count.
11. **Resumed Task calls must not re-splice output.** A resume re-prompts an
    already-matched child; only the child's final answer is recoverable and
    it is already inlined at the original call. Resumed calls render as
    `{"status": "resumed"}` — repeating the answer duplicates large blocks
    inside one record.
12. **Transcripts carry live secrets.** Shell output and pasted configs
    embed real API tokens. Every export scans its final files with
    conservative pattern matching and reports `secrets_detected` in the
    manifest unconditionally; `--redact-secrets` replaces matches with
    `[REDACTED_…]` markers. Pattern-based detection is not exhaustive, and
    the dataset card states that too.

## Target formats (verified against Unsloth docs and HF `datasets`)

- SFT ChatML: `{"messages":[{"role","content"}]}` — consumed directly by
  Unsloth and Unsloth Studio.
- SFT ShareGPT: `{"conversations":[{"from":"human"|"gpt","value"}]}` — run
  `standardize_sharegpt` first.
- CPT: `{"text":…}` raw corpus with no chat template or EOS baked in (the
  trainer adds the model's EOS). Plain `.txt` files serve ForgeLLM's
  `dataset/` layout.
- One schema per output subdirectory:
  `load_dataset("json", data_dir=…)` requires identical columns on every
  line.

## Local-server security rules

- Bind `127.0.0.1` AND validate the `Host` header against a loopback
  allowlist — without the Host check, DNS rebinding lets a remote page
  reach the API.
- Require the per-run token on every `/api/*` request so other local
  processes cannot call the API.
- `/api/session` resolves client paths against scanner-produced paths only
  (in-memory equality before any filesystem access).
- The frontend renders all transcript text via `textContent`, never
  `innerHTML` — transcripts contain arbitrary markup.
- axum's `Json` extractor requires `application/json`, which blocks
  classic HTML-form CSRF.
- Exports refuse populated non-dump directories, not just `~/.cursor`.

## Safety invariants

- `~/.cursor` is read-only; exports and backups refuse to write inside it
  (canonicalized containment checks defeat `..` and symlinks).
- Media copies are driven by USER-message references only (assistant/tool
  text is full of incidental paths): files inside `~/.cursor/projects` plus
  external workspace attachments that still exist. Nonexistent references
  are manifest-listed only.
- `manifest.json` is written last and records per-file line counts, so a
  truncated export is detectable.
- The parser is snapshot-tolerant: torn trailing lines, invalid UTF-8
  (recovered lossily), BOMs, and unknown record/block types are counted,
  never fatal — sessions may be actively written by running agents.

## See also

- [architecture.md](architecture.md) — where these rules live in the code.
- [exporting.md](exporting.md) — the user-facing behavior they produce.
