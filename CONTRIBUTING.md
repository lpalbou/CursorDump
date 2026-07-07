# Contributing to CursorDump

Thank you for considering a contribution. This document explains how to set
up a development environment, what the quality gates are, and how to submit
changes.

## Development setup

Requires Rust stable (MSRV 1.75).

```bash
git clone https://github.com/lpalbou/cursordump
cd cursordump
cargo build
cargo run                # debug build, serves http://127.0.0.1:7070
```

The web frontend (`src/server/ui/`) is embedded into the binary at compile
time (`include_str!`), so a frontend change requires a rebuild to take effect.

## Quality gates

CI runs on Linux and macOS. All of the following must pass:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Integration tests read `~/.cursor/projects` read-only when it exists and skip
gracefully when it does not, so the suite passes on machines without Cursor.

## Design rules

- **Read-only source data.** Nothing may write, lock, or open-for-write under
  `~/.cursor`. Exports and backups must refuse destinations inside it.
- **Tolerant parsing.** Transcripts may be actively appended by running
  agents; malformed or torn lines are skipped and counted, never fatal.
- **Training-aware exports.** Export logic must be general-purpose: no
  special-casing of specific sessions, and no policies that only work on test
  fixtures. The rationale behind each dataset-quality rule is documented in
  [docs/knowledge-base.md](docs/knowledge-base.md) — read it before changing
  export or cleaning behavior, and update it when you learn something new
  about the transcript format.
- **Schema stability.** All records in one JSONL file must share the same
  top-level key set (HuggingFace `load_dataset("json")` requires column
  consistency).
- **Small, focused modules.** See
  [docs/architecture.md](docs/architecture.md) for the module map and
  boundaries.

## Submitting changes

1. Fork and create a feature branch.
2. Add or update tests for the behavior you change. Tests describe the
   expected behavior; the implementation must work for real-world transcripts
   beyond the fixtures.
3. Update documentation affected by the change ([docs/](docs/README.md)) and
   add a user-visible entry to [CHANGELOG.md](CHANGELOG.md).
4. Ensure the quality gates pass locally.
5. Open a pull request describing what changed and why.

## Reporting issues

- Bugs and feature requests: open a GitHub issue with steps to reproduce
  (transcript snippets help — redact anything sensitive first).
- Security vulnerabilities: follow [SECURITY.md](SECURITY.md) instead of
  opening a public issue.

## Code of conduct

All participation is covered by the
[Code of Conduct](CODE_OF_CONDUCT.md).
