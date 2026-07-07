# CursorDump — Knowledge Base

Accumulated insights about Cursor transcripts and turning them into training
data. Never delete an insight; deprecate with a reason instead.

## Cursor on-disk transcript format (verified July 2026)

- Sessions: `~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`.
- Subagent (Task tool) sessions: `.../<uuid>/subagents/<sub-uuid>.jsonl`.
  On this machine subagents outnumber main transcripts ~5:1.
- Records are one JSON object per line:
  - `{"role":"user"|"assistant","message":{"content":[block,...]}}`
  - `{"type":"turn_ended","status":"success"|...,"error"?:...}`
- Blocks: `{"type":"text","text":...}`, `{"type":"tool_use","name":...,"input":{...}}`.
- Media lives in per-project `assets/` and `uploads/`; referenced by absolute
  path inside user text (tags like `<image_files>`, `<attached_files>`,
  `<uploaded_documents>`). Asset filenames are underscore-sanitized (no spaces).
- No workspace-path metadata is stored; project name is a best-effort slug decode.
- Session titles must come from the first `<user_query>`; dates from file mtime.

## Load-bearing insights for dataset quality

1. **`turn_ended` is unreliable** — present for only ~20% of turns. Segment
   turns on the next REAL user record instead.
2. **Transcripts contain tool CALLS but never tool RESULTS.** Rendering tool
   calls into SFT teaches call syntax, not grounded tool use, and produces
   assistant messages that reference invisible outputs. Exclude by default.
3. **Not every `<user_query>` is human input.** The harness injects records
   with `<user_query>` tags for subagent-result and background-shell
   notifications ("The beginning of the above subagent result…", "Briefly
   inform the user about the task result…"). These must (a) not split a turn
   and (b) be excluded from clean user content. Maintain a prefix blocklist.
4. **Assistant text embeds "thinking summary" narration**: runs of
   `**Title Case Header**` followed by first-person deliberation, often glued
   to the previous sentence by a single newline. ~34% of assistant messages
   on real data. It is NOT a separate content type — detect heuristically
   (normalize header line breaks, then classify header + deliberation
   paragraph). Rather than always stripping, `split_thinking` separates it from
   the answer so exports can (a) tag it `<think>…</think>` for reasoning SFT,
   (b) keep it verbatim for CPT, or (c) strip it. Detection is deliberately
   conservative (header-gated + first-person markers) because once we KEEP
   thinking, a false positive mislabels a real answer as reasoning — worse than
   the old strip-only false positive that merely lost a little text. These are
   Cursor's *summarized* reasoning, i.e. a distillation, not raw CoT — state
   this in the dataset card.
5. **Cursor chat links `[label](<uuid>)`** appear in both assistant and
   user-pasted text; the UUIDs are dead outside the IDE. Rewrite to `label`.
6. **Merged assistant turns end with dangling intent** ("Now I'll run the
   tests.") when tool calls are stripped, teaching announce-then-stop. Trim
   trailing intent sentences.
7. **Sessions are heavy-tailed**: a few sessions hold ~90% of the characters
   (single turns can reach ~450k tokens). Chunk records at turn boundaries
   (~100k chars ≈ 25k tokens) so ordinary records fit a 32k window; a single
   oversized turn is kept whole (splitting a turn breaks the example) and
   flagged via `metadata.chunk`/`chunks`.
8. **Session ids are NOT unique** within a project: a self-forked subagent
   reuses the parent id. Use the transcript PATH as identity everywhere
   (basket, viewer, lookup).
9. **Master ↔ subagent (Task tool) linkage.** A master invokes subagents via a
   `tool_use` named "Task" (`input: {subagent_type, description, prompt,
   run_in_background}`) with NO subagent id. Subagent transcripts live at
   `<master_dir>/subagents/<uuid>.jsonl`; the ONLY reliable link is the
   subagent's first `<user_query>` == the Task `prompt` (norm+exact, then
   substring, then resume pass — ~94% match on real data). Transcripts contain
   no tool RESULTS, so a subagent's output is only in its own transcript. Rule:
   **inline a FOREGROUND subagent's final answer as the Task result; NEVER
   inline a BACKGROUND result** (the master continued result-blind; splicing a
   result teaches ignoring tool output). ~50% of Task calls are background.
   Export as Inline (recover the delegate→receive→synthesize loop), Separate
   (subagent = own `agent_brief` record; do not also inline, or tokens
   double-count), or Drop. Never mix Inline masters and Separate subagents in
   one run without dedup by `task_prompt_hash`.

## Target formats (verified against Unsloth docs + HF `datasets`)

- SFT ChatML: `{"messages":[{"role","content"}]}` — Unsloth/Studio direct.
- SFT ShareGPT: `{"conversations":[{"from":"human"|"gpt","value"}]}` — run
  `standardize_sharegpt` first.
- CPT: `{"text":...}` raw corpus; no chat template / EOS baked in (trainer
  adds model EOS). Plain `.txt` files for ForgeLLM's `dataset/`.
- One schema per output subdirectory: `load_dataset("json", data_dir=...)`
  requires identical columns across every line.

## Headerless thinking (v0.3)

Not all models emit bold `**Header**` thinking. Some summaries are pure
first-person planning paragraphs with no header ("Now I'm creating an
end-to-end example that…"). Header-gated detection misses these entirely (the
`a2a` project showed 0 thinking with 33/60 first-person assistant messages).
`is_headerless_deliberation` catches them, but with a STRICTER bar than the
header path: the paragraph must OPEN with a first-person planning phrase
(`DELIBERATION_OPENERS`) AND pass the marker-density + structural vetoes. This
asymmetry matters because capturing (not stripping) makes a false positive
poisonous — a real answer hidden in `<think>`.

## Web UI + local-server security (v0.3)

The UI is a local axum server + browser frontend (egui removed). Load-bearing
security facts, verified by adversarial review:
- Bind `127.0.0.1` AND validate the `Host` header against a loopback allowlist.
  Without the Host check, DNS-rebinding lets a remote page reach the API and
  both read transcripts and write files via `/api/export`.
- `/api/session` must resolve client paths against scanner-produced paths only
  (in-memory equality before any FS access) — never open a client path directly.
- Frontend renders ALL transcript text via `textContent`/`createTextNode`/
  `pre.textContent`, never `innerHTML` — transcripts contain arbitrary markup.
- axum's `Json` extractor requires `application/json`, which blocks classic
  HTML-form CSRF (form posts are `text/plain`/`urlencoded` → 415).
- Export must refuse populated non-dump directories, not just `~/.cursor`.

## Safety invariants

- `~/.cursor` is read-only; exports refuse to write inside it (canonicalized
  containment check defeats `..` and symlinks).
- Media copy restricted to files canonicalizing inside `<cursor>/projects`;
  arbitrary referenced paths (e.g. `/etc/passwd`) are manifest-listed only.
- `manifest.json` is written LAST and records per-file line counts, so a
  truncated export (disk full / crash) is detectable.
- Parser is snapshot-tolerant: torn trailing lines, invalid UTF-8 (recovered
  lossily), BOM, unknown record/block types — counted, never fatal.

## Validated by adversarial review

Two rounds of 5 + 3 adversarial subagents (design, Rust, UX, robustness,
data-forensics, then Rust-review, data-quality-audit, parser-fuzz) drove the
insights above. Final data-quality audit on real sessions: thinking
narration 34%→0%, harness boilerplate 24%→0%, dangling intents 25%→~1%,
oversized multi-turn records eliminated. 50 automated tests pass.
