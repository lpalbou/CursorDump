//! SFT writers: ChatML (`messages`) and ShareGPT (`conversations`) JSONL.
//!
//! One record per session chunk (sessions exceeding `max_record_chars` are
//! split at turn boundaries). Roles strictly alternate user/assistant.
//! Assistant content is composed per `ThinkingMode` (thinking as a leading
//! `<think>…</think>` block by default).

use std::fs;
use std::io::{BufWriter, Write};
use std::path::Path;

use serde_json::json;

use super::{chunk_turns, record_metadata, render_turns, ExportOptions, Prepared, RenderedTurn};

pub fn write_chatml(
    dir: &Path,
    train: &[Prepared],
    val: &[Prepared],
    options: &ExportOptions,
) -> Result<usize, String> {
    let mode = options.thinking;
    write_split(dir, train, val, options, move |turns| {
        let mut messages = Vec::new();
        for t in turns {
            messages.push(json!({"role": "user", "content": t.user}));
            messages.push(json!({"role": "assistant", "content": t.sft_assistant(mode)}));
        }
        json!({ "messages": messages })
    })
}

pub fn write_sharegpt(
    dir: &Path,
    train: &[Prepared],
    val: &[Prepared],
    options: &ExportOptions,
) -> Result<usize, String> {
    let mode = options.thinking;
    write_split(dir, train, val, options, move |turns| {
        let mut conversations = Vec::new();
        for t in turns {
            conversations.push(json!({"from": "human", "value": t.user}));
            conversations.push(json!({"from": "gpt", "value": t.sft_assistant(mode)}));
        }
        json!({ "conversations": conversations })
    })
}

/// Write train.jsonl (and val.jsonl when non-empty).
fn write_split(
    dir: &Path,
    train: &[Prepared],
    val: &[Prepared],
    options: &ExportOptions,
    to_record: impl Fn(&[RenderedTurn]) -> serde_json::Value,
) -> Result<usize, String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut written = 0usize;
    for (name, sessions) in [("train.jsonl", train), ("val.jsonl", val)] {
        if sessions.is_empty() {
            continue;
        }
        let path = dir.join(name);
        let file =
            fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
        let mut writer = BufWriter::new(file);
        for prep in sessions {
            let session = &prep.session;
            let turns = render_turns(session, options, prep.index());
            let chunks = chunk_turns(&turns, options.max_record_chars);
            let n_chunks = chunks.len();
            for (i, chunk) in chunks.iter().enumerate() {
                if chunk.is_empty() {
                    continue;
                }
                let mut record = to_record(chunk);
                if options.with_metadata {
                    let mut md = record_metadata(session, chunk.len(), i, n_chunks, prep.index());
                    // Consistent key on every record: subagent briefs are
                    // machine-authored, master queries are human-authored.
                    md["user_kind"] = if session.meta.is_subagent {
                        json!("agent_brief")
                    } else {
                        json!("human")
                    };
                    // Audit trail: detection-failure records (all-think or
                    // zero-think on long content) are grep-able downstream.
                    md["think_chars"] =
                        json!(chunk.iter().map(|t| t.thinking.len()).sum::<usize>());
                    md["answer_chars"] = json!(chunk.iter().map(|t| t.answer.len()).sum::<usize>());
                    record["metadata"] = md;
                }
                let line =
                    serde_json::to_string(&record).map_err(|e| format!("serialize record: {e}"))?;
                writeln!(writer, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
                written += 1;
            }
        }
        writer
            .flush()
            .map_err(|e| format!("flush {}: {e}", path.display()))?;
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::ThinkingMode;
    use crate::model::SessionMeta;
    use crate::parser::parse_content;
    use std::path::PathBuf;

    fn prepared(content: &str) -> Prepared {
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
        Prepared {
            session: parse_content(meta, content),
            index: None,
        }
    }

    fn sample() -> Prepared {
        prepared(concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>fix the bug</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Fixed it."},{"type":"tool_use","name":"Read","input":{"path":"/x"}}]}}"#,
        ))
    }

    #[test]
    fn chatml_roles_alternate_and_schema_is_stable() {
        let dir = std::env::temp_dir().join("cursordump-sft-test");
        let _ = fs::remove_dir_all(&dir);
        let n = write_chatml(&dir, &[sample()], &[], &ExportOptions::default()).unwrap();
        assert_eq!(n, 1);
        let content = fs::read_to_string(dir.join("train.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        let messages = record["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "fix the bug");
        assert_eq!(messages[1]["role"], "assistant");
        assert!(!messages[1]["content"]
            .as_str()
            .unwrap()
            .contains("tool_call"));
        assert!(record.get("metadata").is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn thinking_tagged_wraps_reasoning() {
        let dir = std::env::temp_dir().join("cursordump-sft-think");
        let _ = fs::remove_dir_all(&dir);
        let prep = prepared(concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>why does it fail?</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"**Investigating**\n\nI need to check the stack trace. I'll look at the log first.\n\nThe failure is a null pointer in parse()."}]}}"#,
        ));
        write_chatml(&dir, &[prep], &[], &ExportOptions::default()).unwrap();
        let content = fs::read_to_string(dir.join("train.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        let asst = record["messages"][1]["content"].as_str().unwrap();
        assert!(asst.starts_with("<think>"), "got: {asst}");
        assert!(asst.contains("</think>"));
        let think_part = &asst[..asst.find("</think>").unwrap()];
        let answer_part = &asst[asst.find("</think>").unwrap()..];
        // Headers stay INSIDE <think> for auditability, never in the answer.
        assert!(think_part.contains("**Investigating**"));
        assert!(think_part.contains("I need to check the stack trace"));
        assert!(answer_part.contains("The failure is a null pointer"));
        assert!(!answer_part.contains("Investigating"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn thinking_strip_removes_reasoning() {
        let dir = std::env::temp_dir().join("cursordump-sft-strip");
        let _ = fs::remove_dir_all(&dir);
        let prep = prepared(concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>why?</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"**Thinking**\n\nI need to inspect the parser carefully. I'll check the failing assertion now and trace it back.\n\nThe answer is 42."}]}}"#,
        ));
        let opts = ExportOptions {
            thinking: ThinkingMode::Strip,
            ..Default::default()
        };
        write_chatml(&dir, &[prep], &[], &opts).unwrap();
        let content = fs::read_to_string(dir.join("train.jsonl")).unwrap();
        let record: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        let asst = record["messages"][1]["content"].as_str().unwrap();
        assert_eq!(asst, "The answer is 42.");
        let _ = fs::remove_dir_all(&dir);
    }
}
