# FAQ

Common questions about CursorDump. For symptom-driven fixes, see
[troubleshooting.md](troubleshooting.md).

## General

### Does CursorDump modify my Cursor data?

No. Access to `~/.cursor` is strictly read-only — nothing is written,
locked, or opened for write, and running agent sessions are unaffected.
Exports and backups refuse to write inside `~/.cursor`.

### Does CursorDump send my data anywhere?

No. It makes no network requests. The server listens on `127.0.0.1` only and
requires a per-run token, so other machines — and other local processes that
don't have the token — cannot reach the API. Data leaves your machine only
if you share an export or backup yourself.

### Can I use it without the web UI?

Yes. `cursordump export …` and `cursordump backup …` run headlessly; see
[api.md](api.md) for all flags.

### Can I explore a machine that never had Cursor installed?

Yes, via a backup: backups bundle the `cursordump` binary and mirror the
original layout, so `./cursordump projects` inside a backup opens the full
explorer. See [backup.md](backup.md#exploring-a-backup-without-cursor).

## Privacy

### What ends up in an exported dataset?

Whatever your sessions contain: your queries, assistant answers (optionally
with reasoning traces), and — depending on options — tool calls and inlined
readable attachments. Transcripts routinely embed file contents, shell
output, and paths from your work.

### How are secrets handled?

Every export scans its written files for common credential shapes
(HuggingFace/OpenAI/GitHub/AWS/Google/Slack tokens, bearer tokens, private
keys) and reports counts in `manifest.json` (`secrets_detected`), the
dataset card, and the CLI/UI output. Redaction is opt-in via
`--redact-secrets`. Detection is pattern-based and not exhaustive — always
review before publishing. See
[exporting.md](exporting.md#secret-scanning-and-redaction).

## Datasets

### Which format should I train on?

- Chat/instruction fine-tuning: `sft_chatml` (works directly with Unsloth
  and HF TRL).
- Tools that expect ShareGPT: `sft_sharegpt` (run `standardize_sharegpt`).
- Continued pre-training: `cpt` (JSONL) or `cpt_txt` (plain text, ForgeLLM).

See [exporting.md](exporting.md) for the full decision guide.

### What are the `<think>` tags in assistant content?

Cursor records summarized assistant reasoning. The default export wraps it
in `<think>…</think>` before the answer — the convention reasoning models
(DeepSeek-R1, Qwen3) use. Choose `verbatim` to leave it inline or `strip`
for answers only. These traces are distilled summaries, not raw
chain-of-thought.

### Why are tool calls excluded by default?

Cursor transcripts contain tool *calls* but never tool *results*. Training
on calls without results teaches call syntax against invisible outputs.
Enable `--tool-calls` only when that is what you want.

### How do subagents appear in the data?

Your choice: inlined into the master conversation (foreground results
spliced in as tool results), as separate `agent_brief` records, or dropped.
See [exporting.md](exporting.md#subagents-task-tool).

### Why is a record split into chunks / marked oversize?

Sessions longer than `max_record_chars` (default 100k characters ≈ 25k
tokens) split at turn boundaries into `metadata.chunk`-numbered records so
they fit common context windows. A single turn larger than the limit cannot
be split and is kept whole with `metadata.oversize: true`.

### Are images and other media part of the training data?

No. Media references are listed in `manifest.json` and optionally copied to
`media/`, but text-only SFT/CPT cannot consume them, so they are never woven
into the records. Readable attachments (txt/md/csv/…) can optionally be
inlined as extra CPT documents.

## Limitations

- **Text-only exports** — no vision-dataset output; media is preserved but
  not wired into records.
- **Distilled reasoning** — thinking traces are Cursor's summaries, not raw
  chain-of-thought.
- **No tool results** — the transcripts do not contain them, so no export
  mode can include them (foreground subagent results are the exception,
  recovered from the subagent's own transcript).
- **Heuristic thinking detection** — deliberately conservative; some
  narration can remain in answers. `metadata.think_chars` /
  `answer_chars` support downstream auditing.
- **Secret detection is pattern-based** — not a guarantee; review before
  sharing.
