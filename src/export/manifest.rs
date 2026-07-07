//! Media copying, manifest and dataset card.
//!
//! The manifest is written LAST so its presence marks a completed export, and
//! it records line counts per output file so truncation (disk full, crash) is
//! detectable afterwards.

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::media;
use crate::model::MediaKind;

use super::{media_boundary, render_turns, ExportOptions, ExportSummary, Prepared};

/// Copy media, then write manifest.json and README.md (dataset card).
pub fn finalize(
    out_dir: &Path,
    prepared: &[Prepared],
    options: &ExportOptions,
    cursor_root: &Path,
    summary: &mut ExportSummary,
) -> Result<(), String> {
    let mut media_entries = Vec::new();
    let boundary = media_boundary(cursor_root);
    // The same file referenced by several sessions is copied (and counted)
    // once; every referencing session still gets its own manifest entry.
    let mut copied_paths: std::collections::HashMap<std::path::PathBuf, (String, String)> =
        std::collections::HashMap::new();

    for prep in prepared {
        let session = &prep.session;
        for media_ref in media::extract_media_refs(session, &boundary) {
            summary.media_referenced += 1;
            let mut entry = json!({
                "session_id": session.meta.id,
                "project": session.meta.project_slug,
                "original_path": media_ref.path.display().to_string(),
                "kind": media_ref.kind.label(),
                "exists": media_ref.exists,
                "copied_to": serde_json::Value::Null,
                "sha256": serde_json::Value::Null,
                "trainable_text": media_ref.kind == MediaKind::Readable,
            });
            // Only copy files that resolve inside the projects root: paths in
            // transcripts are arbitrary strings and must not let a dump
            // exfiltrate unrelated files (e.g. /etc/passwd, ~/.ssh).
            if options.copy_media && media_ref.exists && media_ref.within_cursor {
                if let Some((rel, sha)) = copied_paths.get(&media_ref.path) {
                    entry["copied_to"] = json!(rel);
                    entry["sha256"] = json!(sha);
                } else {
                    match copy_media_file(&media_ref.path, out_dir) {
                        Ok((rel, sha)) => {
                            entry["copied_to"] = json!(rel.clone());
                            entry["sha256"] = json!(sha.clone());
                            copied_paths.insert(media_ref.path.clone(), (rel, sha));
                            summary.media_copied += 1;
                        }
                        Err(e) => summary.warnings.push(format!(
                            "media copy failed for {}: {e}",
                            media_ref.path.display()
                        )),
                    }
                }
            }
            media_entries.push(entry);
        }
    }

    write_dataset_card(out_dir, prepared, options, summary)?;

    // Line counts per emitted jsonl file, for post-hoc truncation detection.
    let mut file_counts = serde_json::Map::new();
    for sub in ["sft_chatml", "sft_sharegpt", "cpt"] {
        for name in ["train.jsonl", "val.jsonl"] {
            let p = out_dir.join(sub).join(name);
            if p.is_file() {
                let lines = fs::read_to_string(&p)
                    .map(|c| c.lines().count())
                    .unwrap_or(0);
                file_counts.insert(format!("{sub}/{name}"), json!(lines));
            }
        }
    }

    let manifest = json!({
        "tool": "CursorDump",
        "version": env!("CARGO_PKG_VERSION"),
        "created_unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        "options": {
            "sft_chatml": options.sft_chatml,
            "sft_sharegpt": options.sft_sharegpt,
            "cpt_jsonl": options.cpt_jsonl,
            "cpt_txt": options.cpt_txt,
            "include_tool_calls": options.include_tool_calls,
            "user_content": match options.user_content {
                super::UserContent::CleanQuery => "clean_query",
                super::UserContent::RawFull => "raw_full",
            },
            "thinking": match options.thinking {
                super::ThinkingMode::Strip => "strip",
                super::ThinkingMode::Tagged => "tagged",
                super::ThinkingMode::Verbatim => "verbatim",
            },
            "subagent_mode": match options.subagent_mode {
                super::SubagentMode::Drop => "drop",
                super::SubagentMode::Inline => "inline",
                super::SubagentMode::Separate => "separate",
            },
            "include_subagent_sessions": options.include_subagent_sessions,
            "copy_media": options.copy_media,
            "inline_readable_attachments": options.inline_readable_attachments,
            "min_turns": options.min_turns,
            "val_fraction": options.val_fraction,
            "with_metadata": options.with_metadata,
            "clean_assistant": options.clean_assistant,
            "final_response_only": options.final_response_only,
            "max_record_chars": options.max_record_chars,
        },
        "sessions": prepared.iter().map(|p| { let s = &p.session; json!({
            "id": s.meta.id,
            "project": s.meta.project_slug,
            "title": s.meta.title,
            "is_subagent": s.meta.is_subagent,
            "parent_session_id": s.meta.parent_id,
            "turns_trainable": render_turns(s, options, p.index()).len(),
            "turns_raw": s.turns.len(),
            "skipped_lines": s.skipped_lines,
            "task_calls": p.index().map(|ix| ix.tasks.len()).unwrap_or(0),
            "source_path": s.meta.path.display().to_string(),
        })}).collect::<Vec<_>>(),
        "file_line_counts": file_counts,
        "media": media_entries,
        "warnings": summary.warnings,
    });

    let path = out_dir.join("manifest.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(())
}

/// Copy one media file into `<out>/media/<sha8>-<basename>`.
/// The hash prefix prevents collisions between same-named files.
fn copy_media_file(src: &Path, out_dir: &Path) -> Result<(String, String), String> {
    let media_dir = out_dir.join("media");
    fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;

    // Stream the hash: media can be large videos.
    let mut file = fs::File::open(src).map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let sha = format!("{:x}", hasher.finalize());

    let base = src
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let safe: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || ".-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let name = format!("{}-{}", &sha[..8], safe);
    let dest = media_dir.join(&name);
    if !dest.exists() {
        fs::copy(src, &dest).map_err(|e| e.to_string())?;
    }
    Ok((format!("media/{name}"), sha))
}

fn write_dataset_card(
    out_dir: &Path,
    prepared: &[Prepared],
    options: &ExportOptions,
    summary: &ExportSummary,
) -> Result<(), String> {
    let sessions_len = prepared.len();
    let total_turns: usize = prepared
        .iter()
        .map(|p| render_turns(&p.session, options, p.index()).len())
        .sum();
    let projects: std::collections::BTreeSet<&str> = prepared
        .iter()
        .map(|p| p.session.meta.project_slug.as_str())
        .collect();

    let mut card = String::new();
    card.push_str("# CursorDump dataset\n\n");
    card.push_str(
        "Agent conversation data exported from Cursor IDE session transcripts by CursorDump.\n\n",
    );
    card.push_str(&format!(
        "- Sessions: {} (from {} project(s)), total turns: {}\n",
        sessions_len,
        projects.len(),
        total_turns
    ));
    card.push_str(&format!(
        "- Media referenced: {}, copied into `media/`: {}\n\n",
        summary.media_referenced, summary.media_copied
    ));

    card.push_str("## Files and how to load them\n\n");
    if options.sft_chatml {
        card.push_str(
            "### `sft_chatml/` — supervised fine-tuning, ChatML schema\n\n\
             One conversation per line: `{\"messages\": [{\"role\": \"user\"|\"assistant\", \"content\": ...}]}`.\n\n\
             ```python\nfrom datasets import load_dataset\nds = load_dataset(\"json\", data_dir=\"sft_chatml\")\n```\n\n\
             Unsloth (Studio or notebooks) consumes this directly; apply the target model's chat template at training time.\n\n",
        );
    }
    if options.sft_sharegpt {
        card.push_str(
            "### `sft_sharegpt/` — supervised fine-tuning, ShareGPT schema\n\n\
             `{\"conversations\": [{\"from\": \"human\"|\"gpt\", \"value\": ...}]}`.\n\
             With Unsloth, run `standardize_sharegpt` before applying a chat template.\n\n",
        );
    }
    if options.cpt_jsonl {
        card.push_str(
            "### `cpt/` — continued pre-training corpus\n\n\
             `{\"text\": ...}` per line (raw corpus format). No chat template or EOS baked in;\n\
             your trainer appends the model-specific EOS token per sample.\n\n",
        );
    }
    if options.cpt_txt {
        card.push_str(
            "### `cpt_txt/` — plain-text corpus (ForgeLLM)\n\n\
             One `.txt` per session. Point ForgeLLM's `dataset/` directory at this folder\n\
             (or copy the files in) for CPT training.\n\n",
        );
    }

    card.push_str("## Provenance and caveats\n\n");
    card.push_str(
        "- Source: local Cursor agent transcripts (`~/.cursor/projects/*/agent-transcripts/`). Read-only export.\n",
    );
    match options.user_content {
        super::UserContent::CleanQuery => card.push_str(
            "- User content: extracted `<user_query>` only; system-injected context removed.\n",
        ),
        super::UserContent::RawFull => card.push_str(
            "- User content: RAW records including system-injected context (rules, attachments, search results).\n",
        ),
    }
    // Thinking representation.
    match options.thinking {
        super::ThinkingMode::Tagged => card.push_str(
            "- Assistant reasoning is captured in leading `<think>…</think>` blocks (SFT) and kept\n  \
             verbatim in the corpus (CPT). Use a reasoning-capable base model and its reasoning\n  \
             chat template. Note: these are Cursor's SUMMARIZED reasoning traces (a distillation),\n  \
             detected heuristically — spot-check before training.\n",
        ),
        super::ThinkingMode::Verbatim => card.push_str(
            "- Assistant reasoning is kept verbatim inline (not tagged).\n",
        ),
        super::ThinkingMode::Strip => card.push_str(
            "- Assistant reasoning (thinking) was stripped; only user-facing answers remain.\n",
        ),
    }
    // Tool calls / subagents actually present in the output.
    let inlines_tasks = options.subagent_mode == super::SubagentMode::Inline;
    if options.include_tool_calls || inlines_tasks {
        card.push_str(
            "- Assistant tool CALLS are rendered as ```tool_call``` blocks. Transcripts contain NO\n  \
             tool RESULTS in general",
        );
        if inlines_tasks {
            card.push_str(
                ", except foreground `Task` (subagent) calls whose result is\n  \
                 recovered from the subagent transcript and inlined as a ```tool_result``` block\n  \
                 (background tasks are marked `spawned_in_background`).\n",
            );
        } else {
            card.push_str(" — training on calls teaches syntax, not grounded tool use.\n");
        }
    } else {
        card.push_str("- Assistant tool calls were excluded; only assistant prose is kept.\n");
    }
    match options.subagent_mode {
        super::SubagentMode::Inline => {}
        super::SubagentMode::Separate => card.push_str(
            "- Subagent (Task) transcripts are exported as separate conversations tagged\n  \
             `metadata.user_kind = \"agent_brief\"` (machine-authored task briefs — weight accordingly).\n",
        ),
        super::SubagentMode::Drop => card.push_str("- Subagent transcripts were dropped.\n"),
    }
    if options.clean_assistant {
        card.push_str(
            "- Cleaned: IDE-only `[label](uuid)` chat links removed; dangling \"I'll now…\" intents\n  \
             trimmed when no tool/result content follows.\n",
        );
    }
    if options.max_record_chars > 0 {
        card.push_str(&format!(
            "- Sessions longer than {} characters were split at turn boundaries\n  \
             (see `metadata.chunk`/`metadata.chunks`).\n",
            options.max_record_chars
        ));
    }
    card.push_str(
        "- PRIVACY: transcripts can embed file contents, paths, shell output and secrets that\n  \
         appeared during your sessions. Review before sharing or publishing this dataset.\n",
    );
    card.push_str(
        "- Images/videos/audio referenced in sessions are listed in `manifest.json` and, when\n  \
         copied, live under `media/`. They are NOT wired into the text records: text-only\n  \
         SFT/CPT cannot consume them (use a vision-dataset pipeline for that).\n",
    );

    fs::write(out_dir.join("README.md"), card).map_err(|e| e.to_string())
}
