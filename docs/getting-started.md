# Getting Started

This guide takes you from a clean machine to your first exported dataset.
For an overview of what CursorDump does, see the [README](../README.md).

## Prerequisites

A machine where the Cursor IDE has been used (transcripts live under
`~/.cursor/projects/`). Without Cursor data you can still run CursorDump
against a backup folder or a custom root.

## Install

**Prebuilt binary** (macOS arm64/x86_64, Linux x86_64/arm64 — no Rust
toolchain needed):

```bash
curl -fsSL https://raw.githubusercontent.com/lpalbou/cursordump/main/install.sh | sh
```

The script verifies the release's sha256 and installs to `~/.local/bin`
(override with `CURSORDUMP_BIN_DIR`, pin a version with
`CURSORDUMP_VERSION=vX.Y.Z`). Linux binaries are static (musl) and run on
any distribution. Archives are also downloadable from the
[releases page](https://github.com/lpalbou/cursordump/releases).

**Homebrew** (macOS and Linux):

```bash
brew install lpalbou/tap/cursordump
```

**Cargo** (crates.io — Rust stable, MSRV 1.75, install via
[rustup](https://rustup.rs)):

```bash
cargo install cursordump
```

**From source**:

```bash
git clone https://github.com/lpalbou/cursordump
cd cursordump
cargo install --path .
```

## Launch

```bash
cursordump
```

The server starts on `http://127.0.0.1:7070` and opens your browser with a
per-run access URL. Variants:

```bash
cursordump --port 7075          # custom port
cursordump --no-open            # print the URL, don't open a browser
cursordump /path/to/projects    # explore a custom root (e.g. a backup)
```

CursorDump reads your Cursor data strictly read-only: running agent sessions
are never modified, locked, or disturbed.

## The UI at a glance

The page has three panels plus a top bar:

- **Top bar** — the unified finder (keyword, media chips, tools dropdown),
  the **Viewing** source chip (live Cursor data or an opened backup), the
  live "N selected" count, **⬇ Export for training…**, **🗄 Create backup…**, and a
  "🔒 read-only" reminder.
- **Projects** (left) — every project, most recently active first. Each row
  shows two chips: blue = main sessions, amber = subagent transcripts. Click
  a project to open it; click it again to deselect.
- **Sessions** (middle) — the selected project's sessions, each with an
  export checkbox. Subagent transcripts nest under the master session that
  spawned them; click "▸ N subagents" to expand.
- **Viewer / results** (right) — the open session's messages, or your finder
  results.

Inside a session: user and assistant messages are color-coded, assistant
reasoning sits behind a violet **💭 thinking** toggle, each tool call is a
chip you click to expand into its full input, and attachments render inline
(images with a click-to-zoom lightbox, audio/video players, file chips).

## Finding messages

The top bar is one combined finder. You can mix:

- a **keyword** (press `/` to focus the box),
- **media chips** — 🖼 image, 🔊 audio, 🎬 video, 📄 document, 📎 readable,
- a **Tools** dropdown (filter by tool the assistant used),
- an optional **project scope** (set by selecting a project).

Results are the exact messages matching every active criterion — for
example, toggle 🖼 image and you see only messages that actually carry an
image, shown as thumbnails. Results group by session; click one to jump to
that message. **Clear all** resets the finder.

## Your first export

1. Tick the checkbox on one or more sessions — or **Select all** for the
   whole project. The top bar shows the selection count.
2. Click **⬇ Export for training…** and pick a preset (Chat SFT, Agentic SFT, CPT corpus,
   or Everything). Presets set sensible option combinations; you can adjust
   anything.
3. Confirm the output folder (prefilled under `~/Downloads/`, must be
   outside `~/.cursor`) and press **⬇ Export**. Results — record counts,
   media copied, and any warnings (including detected secrets) — appear in
   the dialog.

The output folder contains ready-to-train JSONL files, a `manifest.json`,
and a generated dataset card. [exporting.md](exporting.md) explains every
format and option, and shows how to load the data in Unsloth Studio and
ForgeLLM.

The same export works headlessly:

```bash
cursordump export --project <project-slug> --out ./my-dataset --all-formats
```

See [api.md](api.md) for the full CLI reference.

## Backing up your sessions

Cursor can clear old agent data. A backup is a verbatim copy of everything —
distinct from a dataset export:

```bash
cursordump backup --out ~/Documents/cursordump-backup
```

or **🗄 Create backup…** in the UI. Re-running into the same folder is
incremental; `cursordump verify <backup>` checks integrity and
`cursordump restore --from <backup>` copies it back. To browse a backup
later, use the **Viewing ▾** menu (top left) → **Open backup…** — or launch
directly on it with `cursordump /path/to/backup`. See
[backup.md](backup.md) for details.

## Next steps

- [exporting.md](exporting.md) — take control of the training data.
- [architecture.md](architecture.md) — how the system works.
- [troubleshooting.md](troubleshooting.md) — if the first run didn't go as
  planned.
