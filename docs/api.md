# Reference: CLI, JSON API, and Schemas

The canonical interface surfaces of CursorDump. For task-oriented guides,
see [exporting.md](exporting.md) and [backup.md](backup.md).

## CLI

```text
cursordump [--port N] [--no-open] [<projects-root>]     start the web UI (default)
cursordump export --project <slug> --out <dir> [options]
cursordump backup --out <dir> [--project <slug>]... [options]
cursordump verify <backup-dir>
cursordump restore --from <backup-dir> [options]
cursordump --help
```

### Server mode

| Flag | Meaning |
|---|---|
| `--port N` | listen port (default 7070); invalid values are rejected |
| `--no-open` | print the tokenized URL instead of opening a browser |
| `<projects-root>` | custom root to explore (default `~/.cursor/projects`); use a backup's `projects/` folder to explore a backup |

The server prints `http://127.0.0.1:<port>/?token=<token>`; the token is
required for all API access (see [JSON API](#json-api)).

### `export`

| Flag | Meaning |
|---|---|
| `--project <slug>` | project to export (required) |
| `--out <dir>` | output directory, must be outside `~/.cursor` (required) |
| `--all-formats` | write sft_chatml + sft_sharegpt + cpt + cpt_txt |
| `--subagent-mode inline\|separate\|drop` | subagent handling (default `inline`) |
| `--include-subagents` | shorthand for `--subagent-mode separate` |
| `--thinking tagged\|verbatim\|strip` | reasoning-trace handling (default `tagged`) |
| `--val <fraction>` | validation split by session, e.g. `0.1` |
| `--min-turns N` | skip sessions with fewer trainable turns |
| `--tool-calls` | render tool calls as ```tool_call``` blocks |
| `--raw-user` | keep full raw user records (default: clean `<user_query>`) |
| `--no-clean` | keep IDE chat links and dangling intents |
| `--no-media` | don't copy referenced media |
| `--no-metadata` | omit the per-record `metadata` object |
| `--final-only` | keep only the last assistant message per turn |
| `--redact-secrets` | replace detected credentials with `[REDACTED_…]` |

### `backup`

| Flag | Meaning |
|---|---|
| `--out <dir>` | backup directory, must be outside `~/.cursor` (required) |
| `--project <slug>` | restrict to this project; repeatable (default: all) |
| `--skip-runtime` | omit `terminals/` and `agent-tools/` caches |
| `--no-verify` | skip per-transcript sha256 hashing |
| `--no-app` | don't bundle the `cursordump` binary |
| `--no-attachments` | don't capture external workspace attachments |

### `verify`

```text
cursordump verify <backup-dir>
```

Recomputes every transcript and attachment sha256 recorded in
`cursordump-backup.json` and reports ok / mismatched / missing counts.
Read-only; exits non-zero if anything fails.

### `restore`

| Flag | Meaning |
|---|---|
| `--from <backup-dir>` | CursorDump backup to restore from (required) |
| `--project <slug>` | restrict to this project; repeatable (default: all in backup) |
| `--dry-run` | print what would be copied, write nothing |
| `--overwrite` | also replace destination files that differ (default: copy missing files only) |

Restores into `~/.cursor/projects`. Never deletes anything; by default it
never overwrites existing files either. This is the only CursorDump command
that writes under `~/.cursor` — that is its explicit purpose.

## JSON API

The web UI drives a local JSON API. All `/api/*` routes require the per-run
token, via the `X-CursorDump-Token` header or a `?token=` query parameter,
and reject non-loopback `Host` headers.

| Route | Method | Purpose |
|---|---|---|
| `/api/projects` | GET | list projects with session/subagent counts |
| `/api/rescan` | POST | re-scan the projects root, invalidate caches |
| `/api/sessions?project=<slug>` | GET | sessions of a project (with subagent nesting info) |
| `/api/session?path=<transcript>` | GET | full parsed session: messages, thinking, tools, media |
| `/api/facets?project=<slug>` | GET | tools used and media kinds present (global or per project) |
| `/api/find` | POST | message-level finder: `{query, media[], tools[], project?}` → matching messages |
| `/api/media?path=<abs-path>` | GET | stream a referenced attachment (bounded resolution) |
| `/api/export` | POST | run an export: `{paths[], out_dir, options{…}}` → summary |
| `/api/backup` | POST | run a backup: `{out_dir, projects?, …options}` → summary |
| `/api/default_out_dir` | GET | suggested fresh export directory |
| `/api/default_backup_dir` | GET | suggested backup directory |

The API serves only transcript paths produced by the scanner and media files
referenced by messages; it is not a general file server. See
[architecture.md](architecture.md#security-model).

## Dataset output schemas

| File | Schema | Consumer |
|---|---|---|
| `sft_chatml/train.jsonl` | `{"messages":[{"role","content"}], "metadata"}` — assistant content may open with `<think>…</think>` | Unsloth, HF TRL |
| `sft_sharegpt/train.jsonl` | `{"conversations":[{"from":"human"\|"gpt","value"}], "metadata"}` | Unsloth (`standardize_sharegpt`) |
| `cpt/train.jsonl` | `{"text", "metadata"}` | Unsloth CPT, HF |
| `cpt_txt/<project>__<session>.txt` | rendered `## User` / `## Assistant` dialogue | ForgeLLM `dataset/` |
| `media/*` | sha-prefixed copies of referenced attachments | manual / vision pipelines |
| `manifest.json` | provenance, options, sessions, media index, line counts, `secrets_detected` | tooling, audits |
| `README.md` | generated dataset card | humans |

With `--val`, each `train.jsonl` gains a sibling `val.jsonl` (same schema).

### Record metadata

When enabled (default), every record carries a `metadata` object that
trainers ignore but filtering pipelines can use:

- `project`, `session_id`, `session_title`, `modified_unix`
- `chunk`, `chunks` — position when a session was split at turn boundaries
- `oversize: true` — a single turn exceeded `max_record_chars` and was kept whole
- `user_kind` — `"human"` (master sessions) or `"agent_brief"` (subagent records)
- `think_chars`, `answer_chars` — audit counters for the thinking split
- `task_calls` and per-task links (`task_prompt_hash`, `child_transcript`,
  match kind) on records whose sessions delegated to subagents

### Backup manifest (`cursordump-backup.json`)

- `projects[]` — slug, source path, file and byte counts
- `transcripts[]` — project, file, sha256, mtime for every `.jsonl`
- `attachments[]` — original path, captured name, kind, sha256
- `totals`, `last_run` — merged totals and the most recent run's activity
- `options`, `warnings`

Subset re-runs merge into the manifest; records for projects outside the run
are preserved.

## See also

- [getting-started.md](getting-started.md) — first run and UI tour.
- [troubleshooting.md](troubleshooting.md) — when a command fails.
