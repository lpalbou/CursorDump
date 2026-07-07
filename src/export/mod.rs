//! Dataset export: orchestration, options and shared rendering.
//!
//! A dump is a directory with one subdirectory per format so that
//! `load_dataset("json", data_dir=...)` never mixes schemas:
//!
//! ```text
//! <dump>/
//! ├── sft_chatml/train.jsonl [val.jsonl]
//! ├── sft_sharegpt/train.jsonl [val.jsonl]
//! ├── cpt/train.jsonl [val.jsonl]
//! ├── cpt_txt/<session>.txt          # ForgeLLM-style raw text files
//! ├── media/<sha8>-<name>            # optional copies of attachments
//! ├── manifest.json                  # written LAST: sources, counts, media
//! └── README.md                      # dataset card
//! ```

pub mod clean;
pub mod cpt;
pub mod manifest;
pub mod secrets;
pub mod sft;
pub mod subagent;

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use crate::model::{Block, ParsedSession, SessionMeta, Turn};
use crate::parser;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserContent {
    /// Extracted `<user_query>` only, harness-injected records dropped
    /// (recommended for training).
    CleanQuery,
    /// Full raw user record text, including system-injected context.
    RawFull,
}

/// How assistant "thinking" narration is represented in exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    /// Drop thinking entirely; keep only the user-facing answer.
    Strip,
    /// SFT: emit `<think>reasoning</think>\n\nanswer` (reasoning-model
    /// convention). CPT: keep thinking verbatim in its native form.
    Tagged,
    /// Keep the original assistant text verbatim (thinking inline, unreordered).
    Verbatim,
}

/// How subagent (Task tool) transcripts are represented in SFT exports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentMode {
    /// Subagents excluded from export entirely.
    Drop,
    /// Foreground Task results inlined into the master turn as a tool result;
    /// subagent transcripts are NOT emitted as separate records.
    Inline,
    /// Each subagent transcript emitted as its own conversation record
    /// (tagged `user_kind: agent_brief`); masters do not inline results.
    Separate,
}

#[derive(Debug, Clone)]
pub struct ExportOptions {
    pub sft_chatml: bool,
    pub sft_sharegpt: bool,
    pub cpt_jsonl: bool,
    pub cpt_txt: bool,
    /// Render tool calls into assistant text as fenced blocks. Transcripts do
    /// not contain tool RESULTS, so this is off by default (see dataset card).
    pub include_tool_calls: bool,
    pub user_content: UserContent,
    /// How assistant thinking narration is represented.
    pub thinking: ThinkingMode,
    /// Strip IDE chat links and trailing "I'll now…" intents from assistant
    /// text (recommended: these poison training targets).
    pub clean_assistant: bool,
    /// Keep only the LAST assistant text per turn (the user-facing response)
    /// instead of the full merged working narration.
    pub final_response_only: bool,
    /// How subagent (Task) transcripts are represented in SFT.
    pub subagent_mode: SubagentMode,
    pub include_subagent_sessions: bool,
    /// Copy referenced media (resolving inside the projects root) into media/.
    pub copy_media: bool,
    /// Inline readable attachments (txt/md/csv/...) as extra CPT text records.
    pub inline_readable_attachments: bool,
    /// Skip sessions with fewer trainable turns than this.
    pub min_turns: usize,
    /// 0.0 = everything in train.jsonl; e.g. 0.1 = last 10% of sessions to val.
    pub val_fraction: f32,
    /// Emit a `metadata` object per record (project, session, ...).
    pub with_metadata: bool,
    /// Replace detected secrets (API tokens, keys, bearer tokens) with
    /// `[REDACTED_…]` markers in exported text. Off by default; the manifest
    /// always REPORTS how many secrets remain regardless of this flag.
    pub redact_secrets: bool,
    /// Split sessions whose rendered content exceeds this many characters
    /// into multiple records at turn boundaries (0 = never split).
    /// Default 100_000 chars ≈ 25k tokens: keeps records inside a 32k
    /// context window with margin for chat-template overhead.
    pub max_record_chars: usize,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            sft_chatml: true,
            sft_sharegpt: false,
            cpt_jsonl: false,
            cpt_txt: false,
            include_tool_calls: false,
            user_content: UserContent::CleanQuery,
            thinking: ThinkingMode::Tagged,
            clean_assistant: true,
            final_response_only: false,
            subagent_mode: SubagentMode::Inline,
            include_subagent_sessions: false,
            copy_media: true,
            inline_readable_attachments: false,
            min_turns: 1,
            val_fraction: 0.0,
            with_metadata: true,
            redact_secrets: false,
            max_record_chars: 100_000,
        }
    }
}

#[derive(Debug)]
pub enum ExportEvent {
    Progress {
        done: usize,
        total: usize,
        stage: String,
    },
    Done(ExportSummary),
    Failed(String),
}

/// A parsed session together with its subagent index (Inline masters only).
#[derive(Debug, Clone)]
pub struct Prepared {
    pub session: ParsedSession,
    pub index: Option<subagent::SubagentIndex>,
}

impl Prepared {
    pub fn index(&self) -> Option<&subagent::SubagentIndex> {
        self.index.as_ref()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExportSummary {
    pub out_dir: PathBuf,
    pub sessions_exported: usize,
    pub sessions_skipped: usize,
    pub sft_records: usize,
    pub cpt_records: usize,
    pub media_copied: usize,
    pub media_referenced: usize,
    /// Secrets detected in the FINAL written files, tallied by kind (0 if
    /// redaction removed them).
    pub secrets_detected: std::collections::BTreeMap<String, usize>,
    pub warnings: Vec<String>,
}

/// Validate the output directory. Refuses anything inside `cursor_root`
/// (typically `~/.cursor`) — the source tree must never be written to.
pub fn validate_out_dir(out_dir: &Path, cursor_root: &Path) -> Result<(), String> {
    // Canonicalize the deepest existing ancestor to defeat `..` and symlinks.
    let mut probe = out_dir.to_path_buf();
    let canonical = loop {
        match probe.canonicalize() {
            Ok(c) => break c,
            Err(_) => match probe.parent() {
                Some(p) => probe = p.to_path_buf(),
                None => return Err("output path has no existing ancestor".into()),
            },
        }
    };
    // Canonicalize the boundary too, so a symlink component in cursor_root
    // can't make the containment check fail open.
    let cursor_canon = cursor_root
        .canonicalize()
        .unwrap_or_else(|_| cursor_root.to_path_buf());
    if canonical.starts_with(&cursor_canon) {
        return Err(format!(
            "refusing to export inside {} — pick a directory outside ~/.cursor",
            cursor_root.display()
        ));
    }
    // Refuse to scribble into an existing NON-empty directory unless it is
    // itself a previous CursorDump dump (has manifest.json). This prevents an
    // export from overwriting files in an arbitrary populated folder.
    if out_dir.is_dir() {
        let non_empty = std::fs::read_dir(out_dir)
            .map(|mut it| it.next().is_some())
            .unwrap_or(false);
        let is_prior_dump = out_dir.join("manifest.json").is_file();
        if non_empty && !is_prior_dump {
            return Err(format!(
                "{} exists and is not empty — choose a new folder or an existing CursorDump dump",
                out_dir.display()
            ));
        }
    }
    Ok(())
}

/// Media-copy containment boundary: `<cursor_root>/projects` when it exists
/// (the standard layout), otherwise `cursor_root` itself (custom roots).
pub fn media_boundary(cursor_root: &Path) -> PathBuf {
    let projects = cursor_root.join("projects");
    if projects.is_dir() {
        projects
    } else {
        cursor_root.to_path_buf()
    }
}

/// Blocking export intended for a background thread. `repaint` wakes the GUI.
pub fn run_export(
    sessions: Vec<SessionMeta>,
    out_dir: PathBuf,
    options: ExportOptions,
    cursor_root: PathBuf,
    tx: Sender<ExportEvent>,
    repaint: impl Fn(),
) {
    let send = |ev: ExportEvent| {
        let _ = tx.send(ev);
        repaint();
    };
    if let Err(e) = validate_out_dir(&out_dir, &cursor_root) {
        send(ExportEvent::Failed(e));
        return;
    }
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        send(ExportEvent::Failed(format!(
            "cannot create {}: {e}",
            out_dir.display()
        )));
        return;
    }

    // In Separate mode, subagent transcripts are exported as their own
    // records; otherwise (Drop/Inline) they are excluded from the record set
    // (Inline pulls their result into the master via the SubagentIndex).
    let keep_subagents =
        options.include_subagent_sessions || options.subagent_mode == SubagentMode::Separate;
    let mut seen_paths = std::collections::HashSet::new();
    let mut selected: Vec<SessionMeta> = sessions
        .into_iter()
        .filter(|s| keep_subagents || !s.is_subagent)
        // Dedupe by transcript path: a repeated selection must not double the
        // exported records.
        .filter(|s| seen_paths.insert(s.path.clone()))
        .collect();
    // Self-forked subagents (`resume: "self"`) are written to disk BOTH as a
    // main transcript and as `subagents/<same-id>.jsonl` with identical
    // content. Drop the subagent copy when a main transcript with the same
    // session id is also selected, so Separate mode doesn't emit near-dupes.
    let main_ids: std::collections::HashSet<String> = selected
        .iter()
        .filter(|s| !s.is_subagent)
        .map(|s| s.id.clone())
        .collect();
    selected.retain(|s| !(s.is_subagent && main_ids.contains(&s.id)));
    let total = selected.len();
    let mut summary = ExportSummary {
        out_dir: out_dir.clone(),
        ..Default::default()
    };

    // Parse and filter sessions; build subagent index for each master.
    let mut parsed: Vec<Prepared> = Vec::new();
    for (i, meta) in selected.iter().enumerate() {
        send(ExportEvent::Progress {
            done: i,
            total,
            stage: format!("parsing {}", meta.title),
        });
        let session = parser::parse_session(meta);
        // Build the subagent index only for masters in Inline mode.
        let index = if !meta.is_subagent && options.subagent_mode == SubagentMode::Inline {
            Some(subagent::index_for_master(&session, &cursor_root))
        } else {
            None
        };
        let trainable = render_turns(&session, &options, index.as_ref()).len();
        if trainable < options.min_turns.max(1) {
            summary.sessions_skipped += 1;
            continue;
        }
        if session.skipped_lines > 0 {
            summary.warnings.push(format!(
                "{}: {} unparseable line(s) skipped (session may be live)",
                meta.id, session.skipped_lines
            ));
        }
        parsed.push(Prepared { session, index });
    }
    summary.sessions_exported = parsed.len();

    // Deterministic order, then split train/val by session. Never let the
    // validation split consume the whole training set.
    parsed.sort_by(|a, b| a.session.meta.modified.cmp(&b.session.meta.modified));
    let mut val_count =
        ((parsed.len() as f32) * options.val_fraction.clamp(0.0, 0.5)).round() as usize;
    if val_count >= parsed.len() && !parsed.is_empty() {
        val_count = parsed.len() - 1;
    }
    let split_at = parsed.len().saturating_sub(val_count);
    let (train, val) = parsed.split_at(split_at);

    let result = (|| -> Result<(), String> {
        if options.sft_chatml {
            summary.sft_records +=
                sft::write_chatml(&out_dir.join("sft_chatml"), train, val, &options)?;
        }
        if options.sft_sharegpt {
            summary.sft_records +=
                sft::write_sharegpt(&out_dir.join("sft_sharegpt"), train, val, &options)?;
        }
        if options.cpt_jsonl || options.cpt_txt {
            let counts = cpt::write_cpt(&out_dir, train, val, &options, &cursor_root)?;
            summary.cpt_records += counts;
        }
        send(ExportEvent::Progress {
            done: total,
            total,
            stage: "media + manifest".into(),
        });
        manifest::finalize(&out_dir, &parsed, &options, &cursor_root, &mut summary)?;
        Ok(())
    })();

    match result {
        Ok(()) => send(ExportEvent::Done(summary)),
        Err(e) => send(ExportEvent::Failed(e)),
    }
}

/// A rendered, trainable turn. `thinking` and `answer` are the split assistant
/// content; `native` is the assistant content with thinking left inline (used
/// by CPT and by Verbatim SFT). `has_tool_content` records whether tool
/// calls/results were rendered (so trailing-intent trimming is skipped).
#[derive(Debug, Clone)]
pub struct RenderedTurn {
    pub user: String,
    pub thinking: String,
    pub answer: String,
    pub native: String,
}

impl RenderedTurn {
    /// Size used for chunking. Counts the LARGEST rendering this turn can
    /// produce (native includes inlined Task results + tool calls, which
    /// exceed thinking+answer), so `max_record_chars` bounds every mode.
    pub fn chars(&self) -> usize {
        self.user.len() + self.thinking.len() + self.answer.len().max(self.native.len())
    }

    /// Assistant string for SFT, composed per thinking mode.
    pub fn sft_assistant(&self, mode: ThinkingMode) -> String {
        match mode {
            ThinkingMode::Strip => self.answer.clone(),
            ThinkingMode::Verbatim => self.native.clone(),
            ThinkingMode::Tagged => {
                if self.thinking.trim().is_empty() {
                    self.answer.clone()
                } else {
                    // Neutralize any LITERAL <think>/</think> tokens inside the
                    // content so our wrapper stays the only real tag pair
                    // (a transcript may quote the tag in prose/docs).
                    format!(
                        "<think>\n{}\n</think>\n\n{}",
                        neutralize_think_tags(self.thinking.trim()),
                        neutralize_think_tags(&self.answer)
                    )
                }
            }
        }
    }

    /// Assistant string for CPT. Tagged/Verbatim keep thinking inline in its
    /// native `**Header**` form (raw-corpus convention); Strip drops it.
    pub fn cpt_assistant(&self, mode: ThinkingMode) -> String {
        match mode {
            ThinkingMode::Strip => self.answer.clone(),
            _ => self.native.clone(),
        }
    }
}

/// Break literal `<think>`/`</think>` tokens (a zero-width space after `<`)
/// so downstream tag-splitters see only our own wrapper pair.
fn neutralize_think_tags(s: &str) -> String {
    s.replace("<think>", "<\u{200b}think>")
        .replace("</think>", "<\u{200b}/think>")
}

/// Render all trainable turns of a session. `index` (when Inline mode) supplies
/// foreground subagent results to splice into Task calls. Single source of
/// truth used by every writer.
pub fn render_turns(
    session: &ParsedSession,
    options: &ExportOptions,
    index: Option<&subagent::SubagentIndex>,
) -> Vec<RenderedTurn> {
    session
        .turns
        .iter()
        .filter_map(|turn| {
            let user = render_user(turn, options);
            let (thinking, answer, native) = render_assistant(turn, options, index);
            // A turn needs BOTH a real user side and a non-empty ANSWER. A
            // think-only assistant turn (all reasoning, no user-facing answer)
            // would otherwise emit "<think>…</think>\n\n" with nothing after
            // it — training the model to reason and stop. Drop it.
            if user.trim().is_empty() || answer.trim().is_empty() {
                None
            } else {
                Some(RenderedTurn {
                    user,
                    thinking,
                    answer,
                    native,
                })
            }
        })
        .collect()
}

/// Retained for tests and callers that need a yes/no per turn.
pub fn turn_is_trainable(turn: &Turn, options: &ExportOptions) -> bool {
    let user = render_user(turn, options);
    let (_, answer, _) = render_assistant(turn, options, None);
    !user.trim().is_empty() && !answer.trim().is_empty()
}

/// Render the user side of a turn.
fn render_user(turn: &Turn, options: &ExportOptions) -> String {
    let mut parts = Vec::new();
    for msg in &turn.user {
        let mut text = match options.user_content {
            UserContent::CleanQuery => {
                if msg.is_injected {
                    continue; // harness boilerplate is not human input
                }
                msg.user_query.clone().unwrap_or_default()
            }
            UserContent::RawFull => msg.full_text(),
        };
        if options.clean_assistant {
            text = clean::strip_chat_links(&text);
        }
        if options.redact_secrets {
            text = secrets::redact(&text);
        }
        let text = text.trim();
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
    parts.join("\n\n")
}

/// Render the assistant side of a turn, returning (thinking, answer, native).
fn render_assistant(
    turn: &Turn,
    options: &ExportOptions,
    index: Option<&subagent::SubagentIndex>,
) -> (String, String, String) {
    let messages: Vec<&crate::model::Message> = if options.final_response_only {
        turn.assistant.last().into_iter().collect()
    } else {
        turn.assistant.iter().collect()
    };

    let mut thinking_parts: Vec<String> = Vec::new();
    let mut answer_parts: Vec<String> = Vec::new();
    let mut native_parts: Vec<String> = Vec::new();
    let mut has_tool_content = false;

    for msg in messages {
        for block in &msg.blocks {
            match block {
                Block::Text(t) => {
                    let cleaned = if options.clean_assistant {
                        clean::strip_chat_links(t)
                    } else {
                        t.clone()
                    };
                    let cleaned = cleaned.trim();
                    if cleaned.is_empty() {
                        continue;
                    }
                    native_parts.push(cleaned.to_string());
                    let (th, ans) = clean::split_thinking(cleaned);
                    if !th.trim().is_empty() {
                        thinking_parts.push(th);
                    }
                    if !ans.trim().is_empty() {
                        answer_parts.push(ans);
                    }
                }
                Block::ToolUse { name, input } => {
                    let is_task = name == "Task";
                    let inline_task =
                        is_task && options.subagent_mode == SubagentMode::Inline && index.is_some();
                    if inline_task {
                        let rendered = render_task_inline(input, index.unwrap(), options);
                        if !rendered.is_empty() {
                            has_tool_content = true;
                            native_parts.push(rendered.clone());
                            answer_parts.push(rendered);
                        }
                    } else if options.include_tool_calls {
                        has_tool_content = true;
                        let call = render_tool_call(name, input);
                        native_parts.push(call.clone());
                        answer_parts.push(call);
                    }
                }
                Block::Other(_) => {}
            }
        }
    }

    let mut answer = answer_parts.join("\n\n");
    // Drop a dangling "now I'll do X" tail only when no tool/result content
    // follows (otherwise the intent is legitimately fulfilled by the result).
    if options.clean_assistant && !has_tool_content {
        answer = clean::strip_trailing_intent(&answer);
    }
    let mut thinking = thinking_parts.join("\n\n").trim().to_string();
    let mut answer = answer.trim().to_string();
    let mut native = native_parts.join("\n\n").trim().to_string();
    if options.redact_secrets {
        thinking = secrets::redact(&thinking);
        answer = secrets::redact(&answer);
        native = secrets::redact(&native);
    }
    (thinking, answer, native)
}

fn render_tool_call(name: &str, input: &serde_json::Value) -> String {
    let args = serde_json::to_string(input).unwrap_or_else(|_| "{}".into());
    format!("```tool_call\n{{\"name\": {name:?}, \"input\": {args}}}\n```")
}

/// Max chars of an inlined subagent result before truncation.
const MAX_INLINE_RESULT: usize = 16_000;

/// Render a Task call plus (for foreground, matched) its subagent result.
fn render_task_inline(
    input: &serde_json::Value,
    index: &subagent::SubagentIndex,
    options: &ExportOptions,
) -> String {
    let prompt = input
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let hash = subagent::prompt_hash(prompt);
    let call = render_tool_call("Task", input);
    let Some(task) = index.lookup(&hash) else {
        // Unknown call (shouldn't happen): mark unresolved, never dangle.
        return format!(
            "{call}\n\n```tool_result\n{{\"name\": \"Task\", \"status\": \"unresolved\"}}\n```"
        );
    };
    if task.background {
        return format!(
            "{call}\n\n```tool_result\n{{\"name\": \"Task\", \"status\": \"spawned_in_background\"}}\n```"
        );
    }
    // A resumed Task re-prompts a child whose FINAL answer was already spliced
    // into the original call. Repeating it here would duplicate the same
    // (often large) output within one record, so emit a status instead.
    if task.match_kind == subagent::MatchKind::Resume {
        return format!(
            "{call}\n\n```tool_result\n{{\"name\": \"Task\", \"status\": \"resumed\"}}\n```"
        );
    }
    let Some(result) = &task.result else {
        // Foreground call we could not link to a subagent transcript: render a
        // result-bearing block so the assistant never "announces then stops".
        return format!(
            "{call}\n\n```tool_result\n{{\"name\": \"Task\", \"status\": \"unresolved\"}}\n```"
        );
    };
    // Clean the subagent's final answer the same way as assistant text.
    let (_, answer) = clean::split_thinking(&clean::strip_chat_links(result));
    let answer = if answer.trim().is_empty() {
        clean::strip_chat_links(result)
    } else {
        answer
    };
    let mut out = answer.trim().to_string();
    let truncated = out.chars().count() > MAX_INLINE_RESULT;
    if truncated {
        out = out.chars().take(MAX_INLINE_RESULT).collect::<String>();
        out.push_str(" [truncated]");
    }
    let _ = options;
    let result_json = serde_json::json!({"name": "Task", "status": "completed", "output": out});
    format!(
        "{call}\n\n```tool_result\n{}\n```",
        serde_json::to_string(&result_json).unwrap_or_default()
    )
}

/// Pack rendered turns into chunks bounded by `max_record_chars`
/// (0 = single chunk). Splits only at turn boundaries; a single oversized
/// turn becomes its own chunk rather than being truncated.
pub fn chunk_turns(turns: &[RenderedTurn], max_chars: usize) -> Vec<Vec<RenderedTurn>> {
    if max_chars == 0 || turns.is_empty() {
        return if turns.is_empty() {
            Vec::new()
        } else {
            vec![turns.to_vec()]
        };
    }
    let mut chunks: Vec<Vec<RenderedTurn>> = Vec::new();
    let mut current: Vec<RenderedTurn> = Vec::new();
    let mut size = 0usize;
    for turn in turns {
        let t = turn.chars();
        if !current.is_empty() && size + t > max_chars {
            chunks.push(std::mem::take(&mut current));
            size = 0;
        }
        size += t;
        current.push(turn.clone());
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Consistent per-record metadata (same keys on every record of a file).
pub fn record_metadata(
    session: &ParsedSession,
    emitted_turns: usize,
    chunk: usize,
    chunks: usize,
    index: Option<&subagent::SubagentIndex>,
) -> serde_json::Value {
    let modified = session
        .meta
        .modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut md = serde_json::json!({
        "source": "cursor-agent-transcript",
        "project": session.meta.project_slug,
        "session_id": session.meta.id,
        "is_subagent": session.meta.is_subagent,
        "parent_session_id": session.meta.parent_id,
        "modified_unix": modified,
        "turns": emitted_turns,
        "chunk": chunk,
        "chunks": chunks,
    });
    // The task list is session-level; attach it only to the FIRST chunk so
    // summing metadata across records does not multiply-count Task calls.
    if let Some(index) = index {
        if chunk == 0 {
            let calls: Vec<serde_json::Value> = index
                .tasks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "spawn_order": t.spawn_order,
                        "subagent_type": t.subagent_type,
                        "description": t.description,
                        "background": t.background,
                        "task_prompt_hash": t.prompt_hash,
                        "child_transcript": t.child_relpath,
                        "match": t.match_kind.label(),
                    })
                })
                .collect();
            md["task_calls"] = serde_json::Value::Array(calls);
            md["unmatched_task_calls"] = serde_json::json!(index.unmatched());
        } else {
            // Keep the key present (schema consistency) but empty on later chunks.
            md["task_calls"] = serde_json::Value::Array(vec![]);
            md["unmatched_task_calls"] = serde_json::json!(0);
        }
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_dir_inside_cursor_is_rejected() {
        let home = dirs::home_dir().unwrap();
        let cursor = home.join(".cursor");
        // The containment check canonicalizes the deepest EXISTING ancestor,
        // so it only applies on machines where ~/.cursor exists (CI has none).
        if cursor.is_dir() {
            let bad = cursor.join("projects").join("x").join("dump");
            assert!(validate_out_dir(&bad, &cursor).is_err());
        }
        let tmp = std::env::temp_dir().join("cursordump-test-out");
        assert!(validate_out_dir(&tmp, &cursor).is_ok());
    }

    #[test]
    fn rejects_populated_non_dump_directory() {
        let home = dirs::home_dir().unwrap();
        let cursor = home.join(".cursor");
        let dir = std::env::temp_dir().join("cursordump-populated-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("important.txt"), "do not clobber").unwrap();
        // Non-empty, no manifest.json -> refused.
        assert!(validate_out_dir(&dir, &cursor).is_err());
        // A prior dump (has manifest.json) is allowed to be re-exported into.
        std::fs::write(dir.join("manifest.json"), "{}").unwrap();
        assert!(validate_out_dir(&dir, &cursor).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn chunking_splits_at_turn_boundaries() {
        let turn = |n: usize| RenderedTurn {
            user: "u".repeat(n),
            thinking: String::new(),
            answer: "a".repeat(n),
            native: "a".repeat(n),
        };
        // Three turns of 200 chars each, limit 450 -> [t1,t2], [t3]
        let turns = vec![turn(100), turn(100), turn(100)];
        let chunks = chunk_turns(&turns, 450);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2);
        assert_eq!(chunks[1].len(), 1);
        // Oversized single turn still emitted whole.
        let big = vec![turn(1000)];
        let chunks = chunk_turns(&big, 100);
        assert_eq!(chunks.len(), 1);
        // 0 = no split
        assert_eq!(chunk_turns(&turns, 0).len(), 1);
    }
}
