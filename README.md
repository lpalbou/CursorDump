# CursorDump

A local web app (Rust server + browser UI) to explore your Cursor IDE agent
sessions and export them as training-ready datasets for supervised
fine-tuning (SFT) and continued pre-training (CPT).

![CursorDump](docs/screenshot.png)

The Rust core does all the work (scanning, parsing, cleaning, export); the
interface is served locally in your browser. The server binds `127.0.0.1`
only, validates the `Host` header (DNS-rebinding defense), and never writes to
`~/.cursor`.

## What it does

- **Browse** every Cursor project on your machine (`~/.cursor/projects/`),
  sorted by recent activity, with per-project session lists (including
  subagent transcripts).
- **Explore** sessions message by message: user queries, assistant text,
  collapsible thinking traces (💭), tool calls (chips that expand to the full
  input on click), turn counts. Subagent transcripts nest under the master that
  spawned them.
- **Find** messages by keyword + attachment media-type + tool used, all in one
  bar — results are the exact messages that match, with image attachments shown
  as thumbnails; click to jump to the message.
- **Export** any selection of sessions into a dataset directory usable by
  [Unsloth](https://unsloth.ai/docs/get-started/fine-tuning-llms-guide/datasets-guide)
  (including Unsloth Studio), [ForgeLLM](https://github.com/lpalbou/ForgeLLM),
  and anything that speaks HuggingFace `datasets`.
- **View attachments** inline — images, audio/video players and file chips for
  every media reference in a message.
- **Back up** all (or selected) projects verbatim — a complete, incremental
  copy of every transcript/asset/subagent so nothing is lost if Cursor flushes
  its data. The backup **bundles the app** so it can be re-explored without
  Cursor; restore with `cp -a projects/* ~/.cursor/projects/`.

Access to `~/.cursor` is strictly read-only: running agent sessions are never
altered, locked or disturbed.

## Install & run

Requires Rust (stable). Then:

```bash
cargo run --release                       # opens http://127.0.0.1:7070 in your browser
cargo run --release -- --port 7075        # custom port
cargo run --release -- --no-open /path    # don't open browser; custom projects root
```

## Exporting a dataset

1. Pick a project (left), open sessions (middle), and tick the checkbox on the
   sessions you want — or **Select all** for the whole project.
2. Click **⬇ Export…** (top right).
3. Pick a preset (or set options), confirm the output folder (must be outside
   `~/.cursor`), and hit **⬇ Export**.

The dump directory looks like:

```
my-dump/
├── sft_chatml/train.jsonl      # {"messages":[{"role","content"}]}   ← Unsloth / HF
├── sft_sharegpt/train.jsonl    # {"conversations":[{"from","value"}]}
├── cpt/train.jsonl             # {"text": ...}                       ← Unsloth CPT / HF
├── cpt_txt/*.txt               # one plain-text file per session     ← ForgeLLM dataset/
├── media/                      # copied attachments (images etc.)
├── manifest.json               # provenance, options, media index, line counts
└── README.md                   # generated dataset card
```

Each format lives in its own subdirectory so schemas never mix:

```python
from datasets import load_dataset
sft = load_dataset("json", data_dir="my-dump/sft_chatml")
cpt = load_dataset("json", data_dir="my-dump/cpt")
```

With a validation split > 0, `val.jsonl` appears next to each `train.jsonl`.

### Unsloth / Unsloth Studio

- `sft_chatml` is the ChatML (`messages`) schema Unsloth consumes directly;
  apply your model's chat template at training time.
- `sft_sharegpt` is the ShareGPT schema; run `standardize_sharegpt` first.
- `cpt` is the raw-corpus `{"text"}` format for continued pre-training.

### ForgeLLM

Point ForgeLLM's `dataset/` directory at `cpt_txt/` (or copy the `.txt`
files into it) and run CPT from the ForgeLLM web UI.

## Export options

| Option | Default | Notes |
|---|---|---|
| Thinking | capture as `<think>…</think>` | reasoning-model convention; also `verbatim` or `strip` |
| User content | clean query | extracts `<user_query>`; "raw" keeps injected system context |
| Clean assistant | on | strips IDE `[label](uuid)` chat links; trims dangling "I'll now…" intents |
| Final response only | off | keeps just the last assistant message per turn |
| Tool calls | excluded | can be rendered as ```tool_call``` blocks; transcripts hold calls but **no results** |
| Subagents | inline | `inline` foreground results into master, `separate` records, or `drop` |
| Copy media | on | only files resolving inside `~/.cursor/projects` are copied |
| Inline readable attachments | off | adds txt/md/csv/... contents as extra CPT records |
| Min turns | 1 | skips empty/degenerate sessions |
| Validation split | 0 | fraction of sessions routed to `val.jsonl` |
| Max record size | 100000 chars | splits long sessions at turn boundaries (0 = unlimited) |
| Metadata column | on | project/session/timestamps/chunk/task-links per record; ignored by trainers |

### Thinking, cleaning and subagents

- **Thinking** — Cursor records summarized assistant reasoning. By default
  CursorDump captures it as a leading `<think>…</think>` block (SFT) or keeps it
  verbatim in the corpus (CPT); you can also strip it. Detection is
  conservative (bold-header + first-person deliberation) to avoid mislabeling
  real answers.
- **Cleaning** — IDE-only `[label](uuid)` chat links are rewritten to plain
  text and dangling "I'll now…" intents (from stripped tool calls) are trimmed.
  Harness-injected `<user_query>` records (subagent/background notifications)
  never split a turn and are excluded from clean user content.
- **Subagents** — the agent's Task-tool delegations are linked to their
  subagent transcripts by matching the Task prompt to the subagent's first
  query (~94% match on real data). Inline mode splices a **foreground**
  subagent's final answer into the master turn as the tool result; background
  tasks are marked `spawned_in_background` (no fabricated result). Separate mode
  exports each subagent as its own `agent_brief` conversation.

Turn segmentation ignores the unreliable `turn_ended` markers and splits on real
user messages instead. See `docs/KnowledgeBase.md` for the full rationale.

## Headless / scripted export

```bash
cursordump export --project <project-slug> --out <dir> \
  [--all-formats] [--include-subagents] [--tool-calls] [--raw-user] [--no-clean]
```

Example:

```bash
cursordump export --project Users-albou-projects-myapp --out ./myapp-dataset --all-formats
```

## Full backup (data preservation)

Distinct from dataset export: a **verbatim, complete** copy of your Cursor
projects, so you never lose sessions to a Cursor data flush.

```bash
cursordump backup --out ~/Documents/cursordump-backup                 # all projects
cursordump backup --out ~/Documents/cursordump-backup --project <slug> [--skip-runtime]
```

Or in the UI: **🗄 Backup…** (top bar) → all projects or the selected one →
choose a folder outside `~/.cursor` → **Back up**. Re-running into the same
folder is incremental (only changed files copied). The backup mirrors the
original layout under `<out>/projects/<slug>/…` and records a sha256 per
transcript in `cursordump-backup.json`.

**Restore:** `cp -a ~/Documents/cursordump-backup/projects/* ~/.cursor/projects/`

## Full walkthrough

See **`docs/UserGuide.md`** for a step-by-step guide, including how to load the
exported datasets in **Unsloth Studio** and **ForgeLLM** (soon AbstractForge).

## Media handling

Attachments referenced in sessions are classified as **readable**
(txt/md/csv/code…), **document** (pdf/docx…), **image** (png/jpg…),
**video** (mov/mp4…) or **audio** (wav/m4a…). All references are listed in
`manifest.json` with existence and copy status. Images/videos/audio are never
inlined into text datasets (text-only SFT/CPT cannot use them); readable
files can optionally be inlined as CPT documents.

## Privacy

Transcripts routinely contain file contents, shell output, paths and
potentially secrets from your sessions. Review a dump (the generated dataset
card repeats this warning) before sharing or publishing it.

## Development

```bash
cargo test          # unit + integration tests (integration tests read
                    # ~/.cursor/projects read-only when present)
cargo run           # debug build
```

See `docs/Overview.md` (architecture and design decisions, including the
adversarial review findings) and `docs/DataFlow.md`.

CI (GitHub Actions) runs `cargo fmt --check`, `cargo clippy -- -D warnings`,
build and tests on Linux and macOS. Integration tests that need real Cursor
data skip gracefully when `~/.cursor/projects` is absent.

## Contributing

Issues and pull requests are welcome. Please run the CI gate locally before
submitting:

```bash
cargo fmt --check && cargo clippy -- -D warnings && cargo test
```

## License

MIT — Copyright (c) 2026 Laurent-Philippe Albou
<[contact@abstractframework.ai](mailto:contact@abstractframework.ai)>.
See [LICENSE](LICENSE).
