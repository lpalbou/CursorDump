# CursorDump Documentation

CursorDump explores Cursor IDE agent sessions, exports them as SFT/CPT
training datasets, and makes complete Cursor-independent backups. This is the
documentation index; start with the project
[README](../README.md) for an overview and quick start.

## Core guides

- **[getting-started.md](getting-started.md)** — install, first launch, a
  tour of the UI, and your first export.
- **[architecture.md](architecture.md)** — system design: modules, data flow,
  the turn model, threading, and the security/safety invariants.
- **[api.md](api.md)** — the complete reference: CLI commands and flags, the
  local JSON API, and the dataset output schemas.
- **[faq.md](faq.md)** — common questions: privacy, thinking traces, subagent
  handling, format choices, limitations.
- **[troubleshooting.md](troubleshooting.md)** — symptom-oriented fixes for
  setup, UI, export, and backup problems.

## Topic deep dives

- **[exporting.md](exporting.md)** — everything about dataset exports: the
  output layout, every option explained (thinking, subagents, cleaning,
  chunking, splits, secret redaction), and step-by-step usage with Unsloth
  Studio and ForgeLLM. Read this after
  [getting-started.md](getting-started.md) when you want control over the
  training data.
- **[backup.md](backup.md)** — full verbatim backups: incremental behavior,
  integrity records, external attachment capture, the bundled explorer, and
  restore procedures. Complements the dataset export (a backup preserves
  everything; an export transforms for training).
- **[knowledge-base.md](knowledge-base.md)** — the observed Cursor transcript
  format and the dataset-quality rules derived from it. This is the rationale
  behind the export behavior described in [exporting.md](exporting.md);
  contributors should read it before changing export or cleaning logic.

## Related root documents

- [CHANGELOG.md](../CHANGELOG.md) — release history.
- [CONTRIBUTING.md](../CONTRIBUTING.md) — development workflow and quality gates.
- [SECURITY.md](../SECURITY.md) — vulnerability reporting and threat model.
- [ACKNOWLEDGEMENTS.md](../ACKNOWLEDGEMENTS.md) — upstream projects and credits.
