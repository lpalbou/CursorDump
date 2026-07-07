//! Domain types shared across scanner, parser, search, export and GUI.

use std::path::PathBuf;
use std::time::SystemTime;

/// A Cursor project directory under `~/.cursor/projects/`.
#[derive(Debug, Clone)]
pub struct Project {
    /// Directory name, e.g. `Users-albou-projects-foo` or a numeric id.
    pub slug: String,
    /// Best-effort human display name (last path segment of the decoded slug).
    pub display_name: String,
    /// Best-effort decoded workspace path (may be ambiguous, informational only).
    pub workspace_hint: String,
    pub dir: PathBuf,
    pub sessions: Vec<SessionMeta>,
    /// Most recent session mtime, used for sorting.
    pub last_activity: Option<SystemTime>,
}

impl Project {
    pub fn total_sessions(&self) -> usize {
        self.sessions.len()
    }
    pub fn main_sessions(&self) -> usize {
        self.sessions.iter().filter(|s| !s.is_subagent).count()
    }
}

/// Lightweight session descriptor gathered during the scan (no full parse).
#[derive(Debug, Clone)]
pub struct SessionMeta {
    pub id: String,
    pub project_slug: String,
    pub path: PathBuf,
    /// Title derived from the first user query (or first user text).
    pub title: String,
    pub modified: Option<SystemTime>,
    pub size_bytes: u64,
    /// True for transcripts found under `agent-transcripts/<id>/subagents/`.
    pub is_subagent: bool,
    /// Id of the parent session for subagent transcripts.
    pub parent_id: Option<String>,
}

impl SessionMeta {
    /// Unique key for selection/lookup. Session ids are NOT unique across a
    /// project (a self-forked subagent reuses its parent id), so the
    /// transcript path is the only safe identity.
    pub fn key(&self) -> PathBuf {
        self.path.clone()
    }
}

/// A single content block inside a message.
#[derive(Debug, Clone)]
pub enum Block {
    Text(String),
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// Unknown block type, preserved for display.
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// One transcript record with role + content blocks.
#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
    /// True when a user record carries a `<user_query>` tag.
    pub has_user_query: bool,
    /// Extracted `<user_query>` content (user records only).
    pub user_query: Option<String>,
    /// True for user records that are harness-injected rather than typed by
    /// the human: records without a `<user_query>` tag, and records whose
    /// query matches known harness boilerplate (subagent-result
    /// notifications, system notifications). Injected records never start a
    /// new turn and are excluded from "clean" exports.
    pub is_injected: bool,
    /// 0-based line number of this record in the source transcript, used to
    /// map search hits (which index raw lines) to parsed messages.
    pub line_index: usize,
    /// Wall-clock time of the message when Cursor recorded one. Transcript
    /// records carry no timestamp field; the only real time data is the
    /// `<timestamp>` tag the harness injects into some USER records, so this
    /// is present only where that tag exists (never fabricated).
    pub timestamp: Option<String>,
}

impl Message {
    /// Concatenated text of all text blocks.
    pub fn full_text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            if let Block::Text(t) = b {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(t);
            }
        }
        out
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &serde_json::Value)> {
        self.blocks.iter().filter_map(|b| match b {
            Block::ToolUse { name, input } => Some((name.as_str(), input)),
            _ => None,
        })
    }
}

/// A conversational turn: one (merged) user message and the assistant
/// records that follow it, up to the next user message.
#[derive(Debug, Clone)]
pub struct Turn {
    pub user: Vec<Message>,
    pub assistant: Vec<Message>,
}

/// Fully parsed session.
#[derive(Debug, Clone)]
pub struct ParsedSession {
    pub meta: SessionMeta,
    pub messages: Vec<Message>,
    pub turns: Vec<Turn>,
    /// Lines that could not be parsed (torn writes from live sessions, etc.).
    pub skipped_lines: usize,
    /// Number of `turn_ended` markers with an error status.
    pub errored_turns: usize,
}

/// Classification of a media/attachment reference found in a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// Plain-text-ish: can be inlined into CPT corpora (txt, md, csv, code...).
    Readable,
    /// Rich documents readable by humans but not inlineable as-is (pdf, docx...).
    Document,
    Image,
    Video,
    Audio,
    Other,
}

impl MediaKind {
    pub fn label(&self) -> &'static str {
        match self {
            MediaKind::Readable => "readable",
            MediaKind::Document => "document",
            MediaKind::Image => "image",
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Other => "other",
        }
    }
}

/// A media file referenced somewhere in a session transcript.
#[derive(Debug, Clone)]
pub struct MediaRef {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub exists: bool,
    /// True when the canonicalized path stays inside `~/.cursor/projects`
    /// (only such files are ever copied into a dump).
    pub within_cursor: bool,
}
