//! CPT (continued pre-training) writers.
//!
//! - `cpt/train.jsonl` [+ `val.jsonl`]: `{"text": ...}` records, one per
//!   session chunk, rendered as naturally flowing markdown dialogue.
//!   Optionally, extra records for inlined readable attachments.
//! - `cpt_txt/<project>__<session>.txt`: the same rendered text as plain
//!   files, directly usable as a ForgeLLM `dataset/` folder.
//!
//! No chat template and no EOS token are baked in: trainers (Unsloth,
//! ForgeLLM/MLX-LM) append the model-specific EOS themselves.

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde_json::json;

use crate::media;
use crate::model::MediaKind;

use super::{
    chunk_turns, media_boundary, record_metadata, render_turns, ExportOptions, Prepared,
    RenderedTurn, ThinkingMode,
};

/// Max size of a readable attachment inlined into the corpus (2 MB).
const MAX_INLINE_BYTES: u64 = 2 * 1024 * 1024;

pub fn write_cpt(
    out_dir: &Path,
    train: &[Prepared],
    val: &[Prepared],
    options: &ExportOptions,
    cursor_root: &Path,
) -> Result<usize, String> {
    let mut written = 0usize;

    if options.cpt_jsonl {
        let dir = out_dir.join("cpt");
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        for (name, sessions) in [("train.jsonl", train), ("val.jsonl", val)] {
            if sessions.is_empty() {
                continue;
            }
            let path = dir.join(name);
            let file =
                fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
            let mut writer = BufWriter::new(file);
            for prep in sessions {
                written += write_session_records(&mut writer, prep, options, cursor_root)?;
            }
            writer
                .flush()
                .map_err(|e| format!("flush {}: {e}", path.display()))?;
        }
    }

    if options.cpt_txt {
        let dir = out_dir.join("cpt_txt");
        fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        for prep in train.iter().chain(val.iter()) {
            let session = &prep.session;
            let turns = render_turns(session, options, prep.index());
            let text = render_dialogue(&turns, options.thinking, session);
            if text.trim().is_empty() {
                continue;
            }
            let name = format!(
                "{}__{}.txt",
                sanitize(&session.meta.project_slug),
                sanitize(&session.meta.id)
            );
            let path = dir.join(name);
            fs::write(&path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
        }
    }

    Ok(written)
}

fn write_session_records(
    writer: &mut impl Write,
    prep: &Prepared,
    options: &ExportOptions,
    cursor_root: &Path,
) -> Result<usize, String> {
    let session = &prep.session;
    let mut written = 0usize;
    let turns = render_turns(session, options, prep.index());
    let chunks = chunk_turns(&turns, options.max_record_chars);
    let n_chunks = chunks.len();
    for (i, chunk) in chunks.iter().enumerate() {
        let text = render_dialogue(chunk, options.thinking, session);
        if text.trim().is_empty() {
            continue;
        }
        let mut record = json!({ "text": text });
        if options.with_metadata {
            let mut md = record_metadata(session, chunk.len(), i, n_chunks, prep.index());
            md["kind"] = json!("session");
            md["think_chars"] = json!(chunk.iter().map(|t| t.thinking.len()).sum::<usize>());
            md["answer_chars"] = json!(chunk.iter().map(|t| t.answer.len()).sum::<usize>());
            record["metadata"] = md;
        }
        writeln!(
            writer,
            "{}",
            serde_json::to_string(&record).map_err(|e| e.to_string())?
        )
        .map_err(|e| e.to_string())?;
        written += 1;
    }

    // Inline readable attachments as standalone corpus documents.
    if options.inline_readable_attachments {
        let boundary = media_boundary(cursor_root);
        for media_ref in media::extract_media_refs(session, &boundary) {
            if media_ref.kind != MediaKind::Readable || !media_ref.exists {
                continue;
            }
            let Ok(meta) = fs::metadata(&media_ref.path) else {
                continue;
            };
            if meta.len() > MAX_INLINE_BYTES {
                continue;
            }
            let Ok(content) = fs::read_to_string(&media_ref.path) else {
                continue; // not valid UTF-8: skip silently, listed in manifest
            };
            if content.trim().is_empty() {
                continue;
            }
            let mut record = json!({ "text": content });
            if options.with_metadata {
                let mut md = record_metadata(session, 0, 0, 1, None);
                md["kind"] = json!("attachment");
                md["think_chars"] = json!(0);
                md["answer_chars"] = json!(content.len());
                record["metadata"] = md;
            }
            writeln!(
                writer,
                "{}",
                serde_json::to_string(&record).map_err(|e| e.to_string())?
            )
            .map_err(|e| e.to_string())?;
            written += 1;
        }
    }
    Ok(written)
}

/// Render turns as naturally flowing dialogue text. Subagent documents get a
/// one-line provenance header so the master↔subagent tree survives in prose.
fn render_dialogue(
    turns: &[RenderedTurn],
    mode: ThinkingMode,
    session: &crate::model::ParsedSession,
) -> String {
    let mut out = String::new();
    if session.meta.is_subagent {
        if let Some(parent) = &session.meta.parent_id {
            out.push_str(&format!(
                "# Subagent transcript (spawned by session {})\n\n",
                &parent[..8.min(parent.len())]
            ));
        }
    }
    for t in turns {
        out.push_str("## User\n\n");
        out.push_str(t.user.trim());
        out.push_str("\n\n## Assistant\n\n");
        out.push_str(t.cpt_assistant(mode).trim());
        out.push_str("\n\n");
    }
    out.trim_end().to_string()
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionMeta;
    use crate::parser::parse_content;
    use std::path::PathBuf;

    #[test]
    fn renders_session_as_dialogue_text() {
        let meta = SessionMeta {
            id: "s1".into(),
            project_slug: "proj".into(),
            path: PathBuf::from("/dev/null"),
            title: "t".into(),
            modified: None,
            size_bytes: 0,
            is_subagent: false,
            parent_id: None,
        };
        let content = concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>explain X</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"X is..."}]}}"#,
        );
        let session = parse_content(meta, content);
        let turns = render_turns(&session, &ExportOptions::default(), None);
        let text = render_dialogue(&turns, ExportOptions::default().thinking, &session);
        assert!(text.starts_with("## User"));
        assert!(text.contains("explain X"));
        assert!(text.contains("## Assistant"));
        assert!(text.contains("X is..."));
    }
}
