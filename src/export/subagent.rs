//! Master ↔ subagent (Task tool) linkage.
//!
//! A master session invokes subagents via `tool_use` blocks named "Task"
//! (`input: {subagent_type, description, prompt, run_in_background}`). The
//! subagent's own transcript lives at `<master_dir>/subagents/<uuid>.jsonl`
//! and its FIRST user `<user_query>` equals (or contains) the Task `prompt`.
//! There is no id in the Task input, so linkage is by prompt text.
//!
//! Crucially, transcripts record no tool RESULTS. For a FOREGROUND task we can
//! recover the result as the subagent's final assistant text. For a BACKGROUND
//! task the master's continuation was written result-blind, so we must NOT
//! splice a result in (that would teach ignoring tool output).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model::{Block, ParsedSession, Role};
use crate::parser;

/// How a Task call was matched to a subagent transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    Exact,
    Substring,
    Resume,
    None,
}

impl MatchKind {
    pub fn label(self) -> &'static str {
        match self {
            MatchKind::Exact => "exact",
            MatchKind::Substring => "substring",
            MatchKind::Resume => "resume",
            MatchKind::None => "none",
        }
    }
}

/// One Task call in a master, with its resolved subagent (if any).
#[derive(Debug, Clone)]
pub struct LinkedTask {
    pub spawn_order: usize,
    pub subagent_type: String,
    pub description: String,
    pub prompt: String,
    pub prompt_hash: String,
    pub background: bool,
    pub match_kind: MatchKind,
    pub child_relpath: Option<String>,
    /// The subagent's final user-facing answer (foreground only).
    pub result: Option<String>,
}

/// All Task links for a master session, keyed by prompt hash for lookup
/// during rendering.
#[derive(Debug, Clone, Default)]
pub struct SubagentIndex {
    pub tasks: Vec<LinkedTask>,
    by_hash: BTreeMap<String, usize>,
}

impl SubagentIndex {
    pub fn lookup(&self, prompt_hash: &str) -> Option<&LinkedTask> {
        self.by_hash.get(prompt_hash).map(|&i| &self.tasks[i])
    }
    pub fn unmatched(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| t.match_kind == MatchKind::None)
            .count()
    }
}

/// Normalize prompt text for matching: trim + collapse whitespace runs.
fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn prompt_hash(prompt: &str) -> String {
    let mut h = Sha256::new();
    h.update(norm(prompt).as_bytes());
    format!("{:x}", h.finalize())[..16].to_string()
}

/// Extract Task calls from a master session in document order.
fn task_calls(master: &ParsedSession) -> Vec<LinkedTask> {
    let mut out = Vec::new();
    for msg in &master.messages {
        if msg.role != Role::Assistant {
            continue;
        }
        for block in &msg.blocks {
            if let Block::ToolUse { name, input } = block {
                if name != "Task" {
                    continue;
                }
                let get = |k: &str| {
                    input
                        .get(k)
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                };
                let prompt = get("prompt");
                let background = input
                    .get("run_in_background")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                out.push(LinkedTask {
                    spawn_order: out.len(),
                    subagent_type: get("subagent_type"),
                    description: get("description"),
                    prompt_hash: prompt_hash(&prompt),
                    prompt,
                    background,
                    match_kind: MatchKind::None,
                    child_relpath: None,
                    result: None,
                });
            }
        }
    }
    out
}

/// Final user-facing assistant text of a parsed subagent (thinking left in;
/// the caller cleans it). Empty if none.
fn final_answer(session: &ParsedSession) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Assistant && !m.full_text().trim().is_empty())
        .map(|m| m.full_text())
        .unwrap_or_default()
}

/// Build the subagent index for a master by scanning its `subagents/` dir and
/// matching Task prompts. `cursor_root` is used to compute relative paths.
pub fn index_for_master(master: &ParsedSession, cursor_root: &Path) -> SubagentIndex {
    let mut tasks = task_calls(master);
    let sub_dir = master
        .meta
        .path
        .parent()
        .map(|d| d.join("subagents"))
        .filter(|d| d.is_dir());

    // Parse every subagent transcript once; keep first_query + final answer.
    struct Sub {
        path: PathBuf,
        first_query: String,
        /// Normalized text of ALL user records (for the resume pass: a resumed
        /// Task re-prompts an existing child, so its prompt shows up as a
        /// LATER user record, not the first query).
        all_user: String,
        answer: String,
        used: bool,
    }
    let mut subs: Vec<Sub> = Vec::new();
    if let Some(dir) = &sub_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut paths: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
                .collect();
            paths.sort(); // deterministic
            for p in paths {
                let id = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                let meta = crate::model::SessionMeta {
                    id,
                    project_slug: master.meta.project_slug.clone(),
                    path: p.clone(),
                    title: String::new(),
                    modified: None,
                    size_bytes: 0,
                    is_subagent: true,
                    parent_id: Some(master.meta.id.clone()),
                };
                let parsed = parser::parse_session(&meta);
                let first_query = parsed
                    .messages
                    .iter()
                    .find(|m| m.role == Role::User && m.user_query.is_some())
                    .and_then(|m| m.user_query.clone())
                    .unwrap_or_default();
                let all_user = norm(
                    &parsed
                        .messages
                        .iter()
                        .filter(|m| m.role == Role::User)
                        .filter_map(|m| m.user_query.clone())
                        .collect::<Vec<_>>()
                        .join(" \u{1f}"),
                );
                subs.push(Sub {
                    path: p,
                    first_query: norm(&first_query),
                    all_user,
                    answer: final_answer(&parsed),
                    used: false,
                });
            }
        }
    }

    let relpath = |p: &Path| -> String {
        p.strip_prefix(cursor_root)
            .unwrap_or(p)
            .to_string_lossy()
            .to_string()
    };

    // Pass 1 exact, pass 2 substring (greedy, one-to-one, in spawn order).
    for task in tasks.iter_mut() {
        let np = norm(&task.prompt);
        if np.is_empty() {
            continue;
        }
        let exact = subs.iter().position(|s| !s.used && s.first_query == np);
        let idx = exact.or_else(|| {
            subs.iter()
                .position(|s| !s.used && s.first_query.contains(&np))
        });
        if let Some(i) = idx {
            let s = &mut subs[i];
            s.used = true;
            task.match_kind = if exact.is_some() {
                MatchKind::Exact
            } else {
                MatchKind::Substring
            };
            task.child_relpath = Some(relpath(&s.path));
            // Foreground: recover result. Background: leave None by design.
            if !task.background {
                task.result = Some(s.answer.clone());
            }
        }
    }

    // Pass 3 (resume): an unmatched Task re-prompts an ALREADY-matched child;
    // its prompt appears as a later user record of that child. Match against
    // `all_user` of used children. Resumed calls share the child's transcript
    // and (foreground) its final answer, but are tagged `resume`.
    for task in tasks.iter_mut() {
        if task.match_kind != MatchKind::None {
            continue;
        }
        let np = norm(&task.prompt);
        if np.is_empty() {
            continue;
        }
        if let Some(s) = subs.iter().find(|s| s.used && s.all_user.contains(&np)) {
            task.match_kind = MatchKind::Resume;
            task.child_relpath = Some(relpath(&s.path));
            if !task.background {
                task.result = Some(s.answer.clone());
            }
        }
    }

    let by_hash = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.prompt_hash.clone(), i))
        .collect();
    SubagentIndex { tasks, by_hash }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_hash_is_stable_and_short() {
        let a = prompt_hash("  explore   the  repo\n");
        let b = prompt_hash("explore the repo");
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }
}
