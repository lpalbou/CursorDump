//! Tolerant transcript parsing.
//!
//! Transcripts may be actively appended to by running agents, so the parser
//! treats every line independently: malformed or unknown lines are counted and
//! skipped, never fatal. Nothing here ever writes to the source files.

use std::fs;

use serde_json::Value;

use crate::model::{Block, Message, ParsedSession, Role, SessionMeta, Turn};

/// Parse a whole session transcript from disk (snapshot read).
/// Invalid UTF-8 (torn multi-byte write from a live session) is replaced,
/// never dropped: losing a whole session to one bad byte is unacceptable.
pub fn parse_session(meta: &SessionMeta) -> ParsedSession {
    let bytes = fs::read(&meta.path).unwrap_or_default();
    let content = String::from_utf8_lossy(&bytes);
    parse_content(meta.clone(), &content)
}

/// User-record query prefixes that are injected by the agent harness rather
/// than typed by the human (subagent/background notifications). Compared
/// against the extracted `<user_query>` text.
const INJECTED_QUERY_PREFIXES: &[&str] = &[
    "The beginning of the above subagent result",
    "The above subagent result",
    "A subagent has completed",
    "The background shell command",
    "Briefly inform the user about the task result",
];

fn is_injected_query(query: &str) -> bool {
    let q = query.trim_start();
    INJECTED_QUERY_PREFIXES.iter().any(|p| q.starts_with(p))
}

/// Parse transcript content (separated from IO for testability).
pub fn parse_content(meta: SessionMeta, content: &str) -> ParsedSession {
    let mut messages = Vec::new();
    let mut skipped = 0usize;
    let mut errored_turns = 0usize;

    for (line_index, line) in content.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            skipped += 1; // torn write from a live session, or corruption
            continue;
        };
        match record_from_value(&value, line_index) {
            RecordKind::Message(msg) => messages.push(msg),
            RecordKind::TurnEnded { errored } => {
                if errored {
                    errored_turns += 1;
                }
            }
            RecordKind::Unknown => skipped += 1,
        }
    }

    let turns = segment_turns(&messages);
    ParsedSession {
        meta,
        messages,
        turns,
        skipped_lines: skipped,
        errored_turns,
    }
}

enum RecordKind {
    Message(Message),
    TurnEnded { errored: bool },
    Unknown,
}

fn record_from_value(value: &Value, line_index: usize) -> RecordKind {
    // Turn markers: {"type": "turn_ended", "status": ..., "error"?}
    if value.get("type").and_then(Value::as_str) == Some("turn_ended") {
        let errored = value.get("error").is_some()
            || value.get("status").and_then(Value::as_str) != Some("success");
        return RecordKind::TurnEnded { errored };
    }
    let role = match value.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return RecordKind::Unknown,
    };
    let blocks = parse_blocks(value.get("message").and_then(|m| m.get("content")));
    let text = blocks_text(&blocks);
    let user_query = if role == Role::User {
        extract_user_query(&text)
    } else {
        None
    };
    let is_injected = role == Role::User
        && match &user_query {
            Some(q) => is_injected_query(q),
            None => true, // no <user_query> tag => system-injected context
        };
    // The only genuine per-message time data: the harness-injected
    // <timestamp> tag present in some user records.
    let timestamp = if role == Role::User {
        extract_tag(&text, "timestamp")
    } else {
        None
    };
    RecordKind::Message(Message {
        role,
        has_user_query: user_query.is_some(),
        user_query,
        is_injected,
        blocks,
        line_index,
        timestamp,
    })
}

/// Extract the content of the first `<tag>…</tag>` pair in `text`.
fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    let inner = text[start..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

fn parse_blocks(content: Option<&Value>) -> Vec<Block> {
    let mut blocks = Vec::new();
    match content {
        // Observed format: array of typed blocks.
        Some(Value::Array(items)) => {
            for item in items {
                let ty = item.get("type").and_then(Value::as_str).unwrap_or("");
                match ty {
                    "text" => {
                        let t = item.get("text").and_then(Value::as_str).unwrap_or("");
                        blocks.push(Block::Text(t.to_string()));
                    }
                    "tool_use" => {
                        let name = item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("(unnamed tool)")
                            .to_string();
                        let input = item.get("input").cloned().unwrap_or(Value::Null);
                        blocks.push(Block::ToolUse { name, input });
                    }
                    other => blocks.push(Block::Other(other.to_string())),
                }
            }
        }
        // Defensive: plain string content.
        Some(Value::String(s)) => blocks.push(Block::Text(s.clone())),
        _ => {}
    }
    blocks
}

fn blocks_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        if let Block::Text(t) = b {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(t);
        }
    }
    out
}

/// Extract the real user query from a user record's text.
/// Cursor wraps it in `<user_query>...</user_query>`; the rest of the record is
/// system-injected context. Multiple tags are joined (queued messages).
pub fn extract_user_query(text: &str) -> Option<String> {
    let mut queries = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("<user_query>") {
        let after = &rest[start + "<user_query>".len()..];
        if let Some(end) = after.find("</user_query>") {
            queries.push(after[..end].trim().to_string());
            rest = &after[end + "</user_query>".len()..];
        } else {
            // Unclosed tag (torn write): take what we have.
            queries.push(after.trim().to_string());
            break;
        }
    }
    // Empty tags exist in real data; an all-empty extraction means there is
    // no usable query, so report None for consistent downstream handling.
    queries.retain(|q| !q.is_empty());
    if queries.is_empty() {
        None
    } else {
        Some(queries.join("\n\n"))
    }
}

/// Fast path used by the scanner: parse a single JSONL line and return the
/// user query text if this is a user record carrying one.
pub fn title_from_record(line: &str) -> Option<String> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    if value.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let blocks = parse_blocks(value.get("message").and_then(|m| m.get("content")));
    let text = blocks_text(&blocks);
    extract_user_query(&text).filter(|q| !q.is_empty())
}

/// Segment messages into turns. `turn_ended` markers are unreliable (present
/// for ~20% of turns on real data), so a turn boundary is "the next REAL user
/// record". Harness-injected user records (subagent notifications, injected
/// context) never start a new turn: the agent's work continues, so the
/// following assistant records still belong to the human's request.
pub fn segment_turns(messages: &[Message]) -> Vec<Turn> {
    let mut turns: Vec<Turn> = Vec::new();
    for msg in messages {
        match msg.role {
            Role::User => {
                let start_new = !msg.is_injected
                    && match turns.last() {
                        // Merge consecutive user records into the same turn.
                        Some(t) => !t.assistant.is_empty(),
                        None => true,
                    };
                if start_new || turns.is_empty() {
                    turns.push(Turn {
                        user: vec![msg.clone()],
                        assistant: Vec::new(),
                    });
                } else if let Some(t) = turns.last_mut() {
                    t.user.push(msg.clone());
                }
            }
            Role::Assistant => {
                if turns.is_empty() {
                    turns.push(Turn {
                        user: Vec::new(),
                        assistant: Vec::new(),
                    });
                }
                if let Some(t) = turns.last_mut() {
                    t.assistant.push(msg.clone());
                }
            }
        }
    }
    turns
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meta() -> SessionMeta {
        SessionMeta {
            id: "test".into(),
            project_slug: "proj".into(),
            path: PathBuf::from("/dev/null"),
            title: "t".into(),
            modified: None,
            size_bytes: 0,
            is_subagent: false,
            parent_id: None,
        }
    }

    #[test]
    fn extracts_harness_timestamp_from_user_records_only() {
        let content = concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Tuesday, Jul 7, 2026, 2:35 PM (UTC+2)</timestamp>\n<user_query>hi</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"hello <timestamp>fake</timestamp>"}]}}"#,
            "\n",
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>no time here</user_query>"}]}}"#,
            "\n",
        );
        let parsed = parse_content(meta(), content);
        assert_eq!(
            parsed.messages[0].timestamp.as_deref(),
            Some("Tuesday, Jul 7, 2026, 2:35 PM (UTC+2)")
        );
        // Assistant text never carries a real timestamp tag; never extracted.
        assert_eq!(parsed.messages[1].timestamp, None);
        // Absent tag => None (never fabricated).
        assert_eq!(parsed.messages[2].timestamp, None);
    }

    #[test]
    fn parses_basic_records() {
        let content = concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>hi</user_query>"}]}}"#,
            "\n",
            r#"{"role":"assistant","message":{"content":[{"type":"text","text":"hello"},{"type":"tool_use","name":"Read","input":{"path":"/x"}}]}}"#,
            "\n",
            r#"{"type":"turn_ended","status":"success"}"#,
            "\n",
        );
        let parsed = parse_content(meta(), content);
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.skipped_lines, 0);
        assert_eq!(parsed.messages[0].user_query.as_deref(), Some("hi"));
        assert_eq!(parsed.messages[1].tool_calls().count(), 1);
    }

    #[test]
    fn tolerates_torn_lines_and_unknown_records() {
        let content = concat!(
            r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>q</user_query>"}]}}"#,
            "\n",
            r#"{"role":"weird","message":{}}"#,
            "\n",
            r#"{"role":"assistant","mess"#, // torn write
        );
        let parsed = parse_content(meta(), content);
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.skipped_lines, 2);
    }

    #[test]
    fn turn_segmentation_without_markers() {
        // u a a u a  -> 2 turns; markers absent on purpose.
        let mk_user = r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>x</user_query>"}]}}"#;
        let mk_asst = r#"{"role":"assistant","message":{"content":[{"type":"text","text":"y"}]}}"#;
        let content = [mk_user, mk_asst, mk_asst, mk_user, mk_asst].join("\n");
        let parsed = parse_content(meta(), &content);
        assert_eq!(parsed.turns.len(), 2);
        assert_eq!(parsed.turns[0].assistant.len(), 2);
        assert_eq!(parsed.turns[1].assistant.len(), 1);
    }

    #[test]
    fn consecutive_user_records_merge_into_one_turn() {
        let mk_user = r#"{"role":"user","message":{"content":[{"type":"text","text":"<user_query>x</user_query>"}]}}"#;
        let mk_asst = r#"{"role":"assistant","message":{"content":[{"type":"text","text":"y"}]}}"#;
        let content = [mk_user, mk_user, mk_asst].join("\n");
        let parsed = parse_content(meta(), &content);
        assert_eq!(parsed.turns.len(), 1);
        assert_eq!(parsed.turns[0].user.len(), 2);
    }

    #[test]
    fn leading_assistant_forms_userless_turn() {
        let mk_asst = r#"{"role":"assistant","message":{"content":[{"type":"text","text":"y"}]}}"#;
        let parsed = parse_content(meta(), mk_asst);
        assert_eq!(parsed.turns.len(), 1);
        assert!(parsed.turns[0].user.is_empty());
    }

    #[test]
    fn extracts_multiple_queries() {
        let text = "<user_query>a</user_query>junk<user_query>b</user_query>";
        assert_eq!(extract_user_query(text).as_deref(), Some("a\n\nb"));
    }
}
