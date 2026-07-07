# CursorDump — Data Flow

## Components

```
~/.cursor/projects/                       (read-only source)
        │
        ▼
scanner.rs ──► Vec<Project{ SessionMeta… }>      metadata-only scan:
        │                                        dir listing + first KBs for titles
        ▼
server/ (axum, 127.0.0.1, Host-checked)  ──►  browser UI (server/ui/*)
   JSON API: /api/projects /sessions /session /search /export /rescan
        │
        ├── /api/session ──► parser.rs ──► ParsedSession ──► split_thinking ──► JSON
        │
        ├── /api/search ──► search.rs (spawn_blocking)
        │                     └─► SearchHit{ session, record line, snippet }
        │
        └── /api/export ──► export/mod.rs (spawn_blocking)
                          ├─ parser.rs        (full parse per selected session)
                          ├─ export/subagent.rs (link Task calls ↔ subagent transcripts)
                          ├─ export/clean.rs  (split thinking, strip links/intents)
                          ├─ export/sft.rs    (sft_chatml/, sft_sharegpt/)
                          ├─ export/cpt.rs    (cpt/, cpt_txt/)
                          ├─ media.rs         (reference extraction + classification)
                          └─ export/manifest.rs (media copy, manifest.json LAST, README.md)
```

## Threading

IO-heavy work (session parse, search, export) runs on Tokio
`spawn_blocking` tasks so the async runtime is never blocked. The initial scan
runs at startup; `/api/rescan` refreshes the cached `RwLock<Vec<Project>>`
(poison-resilient). The browser drives everything over `fetch`.

## Turn model

A turn = consecutive user records + the assistant records that follow, ended
by the next REAL user record. `turn_ended` markers exist for only ~20% of turns
in real data, so they are used exclusively for error counting, never for
segmentation. Harness-injected user records (subagent/background notifications)
never start a new turn. A turn is exported only when both its rendered user text
and rendered assistant answer are non-empty.

## Assistant rendering (per turn)

`render_assistant` returns `(thinking, answer, native)`:
- `thinking` — concatenated reasoning (from `clean::split_thinking`).
- `answer` — user-facing text (chat links stripped, trailing intents trimmed).
- `native` — full assistant text with thinking left inline (for CPT/Verbatim).

Writers compose from these: SFT via `sft_assistant(ThinkingMode)`
(`<think>…</think>` + answer when Tagged), CPT via `cpt_assistant(ThinkingMode)`
(native, or answer-only when Strip). In Inline subagent mode, a foreground
Task's final answer is spliced in as a ```tool_result``` block; background
Tasks render ```{"status":"spawned_in_background"}``` with no result.

## Input records (observed Cursor formats)

| Record | Handling |
|---|---|
| `{"role":"user","message":{content:[…]}}` | message; `<user_query>` extracted |
| `{"role":"assistant","message":{content:[…]}}` | message; text + tool_use blocks |
| `{"type":"turn_ended","status":…}` | error counter only |
| unparseable / torn line | counted in `skipped_lines`, never fatal |
| unknown block type | preserved as `Block::Other`, excluded from exports |

## Export outputs

| File | Schema | Consumer |
|---|---|---|
| `sft_chatml/train.jsonl` | `{"messages":[{role,content}], "metadata"}` (assistant may carry `<think>…</think>`) | Unsloth, HF TRL |
| `sft_sharegpt/train.jsonl` | `{"conversations":[{from,value}], "metadata"}` | Unsloth (`standardize_sharegpt`) |
| `cpt/train.jsonl` | `{"text", "metadata"}` | Unsloth CPT, HF |
| `cpt_txt/*.txt` | rendered dialogue text | ForgeLLM `dataset/` |
| `media/*` | sha-prefixed copies | manual / vision pipelines |
| `manifest.json` | provenance, options, per-file line counts | tooling / audits |
| `README.md` | dataset card | humans |

## Safety invariants

1. No writes, locks or opens-for-write under `~/.cursor`.
2. Export refuses output directories inside `~/.cursor` (canonicalized check).
3. Media copies restricted to files canonicalizing inside `~/.cursor/projects`;
   everything else is manifest-listed as referenced-only.
4. `manifest.json` is written last and records line counts → truncated exports
   are detectable.
5. One schema per output directory → `load_dataset("json", data_dir=…)` is safe.
