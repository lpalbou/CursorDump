# Exporting Datasets

This page covers everything about turning Cursor sessions into training
data: the output layout, every export option, and step-by-step usage with
Unsloth Studio and ForgeLLM. For a first export, see
[getting-started.md](getting-started.md); for the rationale behind these
policies, see [knowledge-base.md](knowledge-base.md).

## Output layout

```text
my-dataset/
├── sft_chatml/train.jsonl      # {"messages":[{"role","content"}]}   → Unsloth / HF
├── sft_sharegpt/train.jsonl    # {"conversations":[{"from","value"}]}
├── cpt/train.jsonl             # {"text": ...}                       → Unsloth CPT / HF
├── cpt_txt/*.txt               # one plain-text file per session     → ForgeLLM dataset/
├── media/                      # copied attachments (images etc.)
├── manifest.json               # provenance, options, media index, secret scan
└── README.md                   # generated dataset card
```

Each format lives in its own subdirectory so schemas never mix, and all
records in one file share the same top-level keys — both requirements of
HuggingFace `load_dataset("json", data_dir=…)`:

```python
from datasets import load_dataset
sft = load_dataset("json", data_dir="my-dataset/sft_chatml")
cpt = load_dataset("json", data_dir="my-dataset/cpt")
```

With a validation split > 0, `val.jsonl` appears next to each `train.jsonl`.
Splits are made at the session level (whole sessions, no turn leakage
between splits).

## CLI

```bash
cursordump export --project <project-slug> --out <dir> [options]

# Options:
#   --all-formats                        sft_chatml + sft_sharegpt + cpt + cpt_txt
#   --subagent-mode inline|separate|drop (default inline)
#   --include-subagents                  shorthand for --subagent-mode separate
#   --thinking tagged|verbatim|strip     (default tagged)
#   --val <fraction>                     validation split, e.g. 0.1
#   --min-turns N                        skip sessions with fewer trainable turns
#   --tool-calls  --raw-user  --no-clean --no-media  --no-metadata  --final-only
#   --redact-secrets                     replace detected credentials with [REDACTED_…]
```

## Options reference

| Option | Default | Notes |
|---|---|---|
| Thinking | capture as `<think>…</think>` | reasoning-model convention; also `verbatim` or `strip` |
| User content | clean query | extracts `<user_query>`; "raw" keeps injected system context |
| Clean assistant | on | strips IDE `[label](uuid)` chat links; trims dangling "I'll now…" intents |
| Final response only | off | keeps just the last assistant message per turn |
| Tool calls | excluded | can be rendered as ```tool_call``` blocks; transcripts hold calls but **no results** |
| Subagents | inline | `inline` foreground results into master, `separate` records, or `drop` |
| Copy media | on | copies attachments referenced by user messages: files inside `~/.cursor/projects` and external workspace `@file`s that still exist |
| Inline readable attachments | off | adds txt/md/csv/… contents as extra CPT records |
| Min turns | 1 | skips empty/degenerate sessions |
| Validation split | 0 | fraction of sessions routed to `val.jsonl` |
| Max record size | 100 000 chars | splits long sessions at turn boundaries (0 = unlimited) |
| Metadata column | on | project/session/timestamps/chunk/task-links per record; ignored by trainers |
| Redact secrets | off | replaces detected credentials with `[REDACTED_…]` markers |

## Thinking (reasoning traces)

Cursor records the assistant's summarized reasoning alongside its answers.
You choose how it appears in the dataset:

- **Capture as `<think>…</think>` (default)** — reasoning is wrapped in
  `<think>` tags before the answer, the convention used by reasoning models
  (DeepSeek-R1, Qwen3). Use a reasoning-capable base model and chat template
  when training on this.
- **Keep verbatim inline** — the original text with thinking left in place
  (natural for CPT corpora).
- **Strip** — answer only, for a clean non-reasoning assistant.

Detection separates bold-header reasoning blocks and headerless first-person
planning prose from user-facing answers. It is deliberately conservative:
structured content (code, lists, tables) is never classified as thinking.
These traces are Cursor's *summarized* reasoning — a distillation, not raw
chain-of-thought — and the generated dataset card states this.

## User content

- **Clean query (default)** — just what you typed (the `<user_query>` part);
  harness-injected notifications (subagent results, background-task
  messages) are excluded.
- **Raw records** — everything, including injected context and rules.
  Rarely what you want for training.

## Assistant cleaning

On by default:

- IDE-only `[label](uuid)` chat links are rewritten to plain text (the UUIDs
  are dead outside Cursor).
- Dangling "I'll now run the tests." intents at the end of a turn are
  trimmed when tool calls were stripped (otherwise the model learns to
  announce and stop).

**Tool calls** are excluded by default: transcripts contain tool *calls* but
never tool *results*, so rendering calls without results teaches call syntax
against invisible outputs. Enable `--tool-calls` only when you specifically
want that syntax in the data.

## Subagents (Task tool)

When the agent delegates work to subagents, choose how that appears:

- **Inline (default)** — a foreground subagent's final answer is spliced
  into the master conversation as the Task tool's result, preserving the
  delegate → receive → synthesize loop. Background tasks are marked
  `spawned_in_background` with no result (the master continued without
  waiting). Resumed tasks are marked `resumed` without repeating output that
  is already present.
- **Separate** — every subagent becomes its own conversation record, tagged
  `metadata.user_kind: "agent_brief"` (useful for training worker agents).
  Self-forked sessions that exist as both a main and a subagent transcript
  are exported once.
- **Drop** — subagents excluded.

Task calls are linked to their subagent transcripts by matching the Task
prompt to the subagent's first user query; per-record metadata carries the
linkage (`task_prompt_hash`, `child_transcript`, match kind).

## Record length and chunking

Long sessions are split into multiple records at turn boundaries (default
100 000 UTF-8 characters, roughly 25k tokens — chars/4 is a reasonable token
estimate). `metadata.chunk` / `metadata.chunks` identify the pieces. A
single turn larger than the limit cannot be split and is kept whole with
`metadata.oversize: true`, so you can filter or truncate it knowingly.

## Secret scanning and redaction

Every export scans the final written files for common credential shapes
(HuggingFace/OpenAI/GitHub/AWS/Google/Slack tokens, bearer tokens, private
keys) and reports counts in `manifest.json` under `secrets_detected`, in the
dataset card, and as a warning in the CLI/UI.

Redaction is opt-in: `--redact-secrets` (CLI) or *redact secrets* (UI)
replaces detected secrets with `[REDACTED_…]` markers; a redacted export
reports zero remaining detections. Detection is pattern-based and not
exhaustive — review a dump before publishing it.

## Using the dataset in Unsloth Studio

[Unsloth Studio](https://unsloth.ai/docs/new/studio) is a local no-code UI
for training models.

1. Launch Studio (`unsloth studio` after installing).
2. Import your dataset:
   - **SFT** — point it at `my-dataset/sft_chatml` (ChatML `messages`);
     Studio applies the target model's chat template automatically. For
     ShareGPT exports, `standardize_sharegpt` handles the conversion.
   - **CPT** — point it at `my-dataset/cpt`; the `text` column is exactly
     what continued pre-training expects (no template, no EOS baked in;
     the trainer adds the model's EOS per sample).
3. Pick a base or instruct model, choose LoRA/full fine-tune, and train.
4. If you exported thinking as `<think>` tags, use a reasoning-capable base
   model and its reasoning chat template.

In code (Unsloth notebooks) the same data works directly:

```python
from datasets import load_dataset
ds = load_dataset("json", data_dir="my-dataset/sft_chatml", split="train")
# ds[i]["messages"] is a ChatML conversation; apply your tokenizer's
# chat template, then train with SFTTrainer as in the Unsloth guide.
```

## Using the dataset in ForgeLLM

[ForgeLLM](https://github.com/lpalbou/ForgeLLM) does continued pre-training
and fine-tuning with MLX on Apple Silicon.

1. ForgeLLM reads a `dataset/` directory of text files for CPT:

   ```bash
   cp my-dataset/cpt_txt/*.txt /path/to/forgellm/dataset/
   ```

   (or point ForgeLLM's dataset path at `my-dataset/cpt_txt`).
2. Start ForgeLLM (`forgellm start`), open the Training tab, pick a base
   model, and run continued pre-training on your session corpus.
3. The `cpt/train.jsonl` and SFT JSONL files are standard HuggingFace shapes
   that ForgeLLM's data pipeline can consume as it adds instruction
   fine-tuning support.

The formats are portable HuggingFace conventions, not tool-specific — any
`datasets`-compatible trainer can use them.

## Quick reference

| I want… | Do this |
|---|---|
| Clean chat data to teach answers | Preset **Chat SFT (clean)** |
| Agentic data with tool use + subagents | Preset **Agentic SFT (tools + subagents)** |
| A raw corpus for continued pretraining | Preset **CPT corpus** |
| Reasoning traces in the data | Thinking = **Capture as `<think>`** (default) |
| No reasoning, answers only | Thinking = **Strip** |
| Subagent work folded into the master | Subagents = **Inline** (default) |
| Subagents as their own examples | Subagents = **Separate** |
| Everything, for later filtering | Preset **Everything** + metadata on |
| A dataset safe to share | `--redact-secrets`, then review the manifest |
