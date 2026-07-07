# CursorDump — User Guide

A step-by-step guide to turning your Cursor agent sessions into training
datasets and using them in **Unsloth Studio** and **ForgeLLM** (soon
**AbstractForge**).

---

## 1. What CursorDump is for

Every conversation you have with the Cursor agent is stored on your machine as
a transcript. CursorDump lets you browse those transcripts and export the ones
you choose as a clean dataset you can fine-tune a model on — either as
chat/instruction data (SFT) or as a raw text corpus (CPT).

It only ever **reads** your Cursor data. Nothing under `~/.cursor` is modified,
and running agent sessions are never disturbed.

---

## 2. Install & launch

```bash
cargo run --release              # serves http://127.0.0.1:7070 and opens your browser
cargo run --release -- --port 7075
cargo run --release -- --no-open /path/to/.cursor/projects
```

CursorDump runs as a small local web server (127.0.0.1 only, `Host`-checked,
read-only on `~/.cursor`) and opens in your browser. The page has three areas
plus a top bar:

- **Top bar** — global search, the live "N selected" count, the **⬇ Export…**
  button, and a "🔒 read-only" reminder.
- **Projects** (left) — every project, most recently active first. Each row
  shows two count chips: blue = normal sessions, amber = subagent transcripts.
- **Sessions** (middle) — the sessions of the selected project, each with a
  selection checkbox. Subagent transcripts nest under the master that spawned
  them; click "▸ N subagents" to expand.
- **Viewer / Search results** (right) — the open session's messages, or your
  search hits.

---

## 3. Finding sessions

- **Browse**: click a project, then a session. User and assistant messages are
  colour-coded; assistant reasoning is behind a violet **💭 thinking** toggle;
  each tool call is a chip you **click to expand** into its full input;
  **attachments render inline** (images, audio/video players, file chips).
  Click the active project again to deselect it.
- **Find** (the top bar is one unified finder): combine a **keyword**, the
  **media chips** (🖼 image / 🔊 audio / 🎬 video / 📄 document / 📎 readable)
  and a **Tools** dropdown, optionally scoped to the selected project. Results
  are the **exact messages that match every active criterion** — e.g. toggle
  🖼 image and you get only the messages that actually carry an image, with the
  images shown as thumbnails right in the results (click to zoom). Add a
  keyword to narrow further. Results group by session; click any result to jump
  to that message. "Clear all" resets the finder. `/` focuses the keyword box.

---

## 4. Building an export

1. Tick the checkbox on any session — or **Select all** for the whole project.
2. The top bar shows how many sessions are selected.
3. Click **⬇ Export…**, pick a preset (or set options), confirm the output
   folder (prefilled to a fresh `~/Downloads/cursordump-…`, must be **outside
   `~/.cursor`**), and press **⬇ Export**. Results appear inline in the dialog.

You can also export from the command line:

```bash
cursordump export --project <project-slug> --out ./my-dataset --all-formats
# flags: --subagent-mode inline|separate|drop   --include-subagents (= separate)
#        --tool-calls   --raw-user   --no-clean
```

---

## 5. What you get

The output folder is self-describing:

```
my-dataset/
├── sft_chatml/train.jsonl      # {"messages":[{role,content}]}      → Unsloth / HF
├── sft_sharegpt/train.jsonl    # {"conversations":[{from,value}]}
├── cpt/train.jsonl             # {"text": ...}                      → Unsloth CPT / HF
├── cpt_txt/*.txt               # one plain-text file per session    → ForgeLLM
├── media/                      # copied attachments (images, etc.)
├── manifest.json               # provenance, options, media, counts
└── README.md                   # dataset card (auto-generated)
```

If you set a validation split, a `val.jsonl` appears next to each `train.jsonl`.

Load any format with HuggingFace `datasets`:

```python
from datasets import load_dataset
sft = load_dataset("json", data_dir="my-dataset/sft_chatml")
cpt = load_dataset("json", data_dir="my-dataset/cpt")
```

---

## 6. The options that matter

### Thinking (reasoning traces)

Cursor records the assistant's summarized *thinking* alongside its answers.
You choose how it appears:

- **Capture as `<think>…</think>` (default)** — reasoning is wrapped in
  `<think>` tags before the answer, the convention used by reasoning models
  (DeepSeek-R1, Qwen3). Best if you want to train reasoning behaviour.
- **Keep verbatim inline** — the original text, thinking left in place.
- **Strip** — answer only, for a clean non-reasoning assistant.

### User content

- **Clean query (default)** — just what *you* typed (`<user_query>`); harness
  notifications (subagent/background messages) are excluded.
- **Raw records** — everything, including injected context. Rarely what you want.

### Assistant

- **Strip IDE chat links + dangling intents (default)** — removes
  `[label](uuid)` links that only work inside Cursor, and trailing
  "I'll now run the tests." lines that go nowhere.
- **Keep only final response per turn** — drops intermediate working messages.
- **Render tool calls** — writes tool *calls* into the text. Transcripts hold
  no tool *results*, so leave this off unless you know why you want it.

### Subagents (Task tool)

When the agent delegates to subagents, you decide how that appears:

- **Inline (default)** — for a foreground task, the subagent's final answer is
  spliced into the master conversation as the tool's result, so the
  delegate → receive → synthesize loop is preserved. Background tasks are
  marked `spawned_in_background` with no result (the master continued without
  waiting, so inventing a result would teach the wrong thing).
- **Separate** — every subagent becomes its own conversation record, tagged
  `user_kind: agent_brief` (useful for training worker agents).
- **Drop** — subagents excluded.

### Length

Long sessions are split into multiple records at turn boundaries
(default ~100 000 characters ≈ 25k tokens) so records fit a 32k context window.
`metadata.chunk` / `chunks` tells you which piece is which.

---

## 7. Using the dataset in Unsloth Studio

[Unsloth Studio](https://unsloth.ai/docs/new/studio) is a local no-code UI for
training models on Apple Silicon, NVIDIA and more.

1. Launch Studio (`unsloth studio` after installing).
2. In the training flow, import your dataset:
   - For **SFT**, point it at `my-dataset/sft_chatml` (ChatML `messages`).
     Studio applies the target model's chat template automatically. If you
     exported ShareGPT instead, Studio/Unsloth's `standardize_sharegpt` handles
     the `from`/`value` → `role`/`content` conversion.
   - For **CPT / continued pretraining**, point it at `my-dataset/cpt`
     (the `text` column is exactly what CPT expects — no template, no EOS baked
     in; Studio adds the model's EOS per sample).
3. Pick a base or instruct model, choose LoRA/full-finetune, and train.
4. If you exported with **thinking as `<think>` tags**, use a reasoning-capable
   base model and its reasoning chat template so the tags are handled correctly.

In code (Unsloth notebooks) the same data works directly:

```python
from datasets import load_dataset
ds = load_dataset("json", data_dir="my-dataset/sft_chatml", split="train")
# ds[i]["messages"] is a ChatML conversation; apply your tokenizer's
# chat template, then train with SFTTrainer as in the Unsloth guide.
```

---

## 8. Using the dataset in ForgeLLM (→ AbstractForge)

[ForgeLLM](https://github.com/lpalbou/ForgeLLM) does continued pre-training and
fine-tuning with MLX on Apple Silicon.

1. ForgeLLM reads a **`dataset/`** directory of text files for CPT.
2. Copy the exported plain-text files in:

   ```bash
   cp my-dataset/cpt_txt/*.txt /path/to/forgellm/dataset/
   ```

   (or point ForgeLLM's dataset path at `my-dataset/cpt_txt`.)
3. Start ForgeLLM (`forgellm start`), open the Training tab, pick a base model,
   and run continued pre-training on your session corpus.
4. For instruction fine-tuning, the `cpt/train.jsonl` (`{"text"}`) and the
   SFT JSONL files are standard HuggingFace shapes that ForgeLLM's data
   pipeline can consume as it adds IFT support.

When ForgeLLM becomes **AbstractForge**, the same files apply — the formats
here are the portable, tool-agnostic HuggingFace conventions, not tool-specific.

---

## 8b. Full backup (don't lose your sessions)

Cursor may clear old agent data. A **backup** is different from a dataset
export: it is a verbatim, complete copy of your projects — every transcript,
subagent, asset, upload, canvas and terminal — that you can restore later.

In the UI: click **🗄 Backup…** (top bar), choose *all projects* or *only the
selected project*, pick a folder outside `~/.cursor`, and press **Back up**.

From the CLI:

```bash
cursordump backup --out ~/Documents/cursordump-backup
cursordump backup --out ~/Documents/cursordump-backup --project <slug> --skip-runtime
```

- **Self-contained**: the backup bundles the `cursordump` app, so you can
  **re-explore it without Cursor** — inside the backup run `./cursordump
  projects` and the full explorer opens (sessions, thinking, attachments,
  search, export). Skip bundling with `--no-app`.
- **Captures attachments**: pasted images/uploads (inside `~/.cursor`) are
  copied verbatim, and workspace `@file` attachments that live outside
  `~/.cursor` (and still exist) are copied into `<backup>/attachments/` so they
  survive workspace changes. Skip with `--no-attachments`. Files Cursor already
  deleted can't be recovered — back up before they're gone.
- **Incremental**: point at the same folder again and only changed files are
  copied — cheap to run on a schedule (e.g. a cron job or a Cursor hook).
- **Integrity**: `cursordump-backup.json` lists a sha256 for every `.jsonl`
  transcript.
- **Restore into Cursor**: `cp -a <backup>/projects/* ~/.cursor/projects/`
- `--skip-runtime` omits regenerable `terminals/` and `agent-tools/` caches;
  `node_modules`/`.git` are always skipped.

---

## 9. Privacy — read before sharing

Transcripts can contain file contents, shell output, file paths, and
occasionally secrets that appeared during your sessions. CursorDump surfaces
this in the generated dataset card, but **review a dump before publishing or
sharing it**. Images/videos/audio are listed in `manifest.json` and copied into
`media/` when enabled, but they are never woven into the text records — text-only
SFT/CPT cannot use them.

---

## 10. Quick reference

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
