//! Read-only discovery of Cursor projects and sessions.
//!
//! Scanning is metadata-only: we read directory entries, file sizes/mtimes and
//! the first few KB of each transcript to derive a session title. Full parsing
//! happens lazily, per session, in `parser`.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::model::{Project, SessionMeta};
use crate::parser;

/// Default root: `~/.cursor/projects`.
pub fn default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cursor").join("projects"))
}

/// Scan all projects under `root`. Projects without transcripts are included
/// (with zero sessions) so the GUI can optionally show/hide them.
pub fn scan_projects(root: &Path) -> Vec<Project> {
    let mut projects = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return projects;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let slug = entry.file_name().to_string_lossy().to_string();
        let sessions = scan_sessions(&dir, &slug);
        let last_activity = sessions.iter().filter_map(|s| s.modified).max();
        let (display_name, workspace_hint) = decode_slug(&slug);
        projects.push(Project {
            slug,
            display_name,
            workspace_hint,
            dir,
            sessions,
            last_activity,
        });
    }
    // Most recently active first; empty projects last, alphabetical.
    projects.sort_by(|a, b| match (b.last_activity, a.last_activity) {
        (Some(x), Some(y)) => x.cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, Some(_)) => std::cmp::Ordering::Less,
        (None, None) => a.display_name.cmp(&b.display_name),
    });
    projects
}

/// Scan sessions of one project, including subagent transcripts.
fn scan_sessions(project_dir: &Path, slug: &str) -> Vec<SessionMeta> {
    let mut sessions = Vec::new();
    let transcripts = project_dir.join("agent-transcripts");
    let Ok(entries) = fs::read_dir(&transcripts) else {
        return sessions;
    };
    for entry in entries.flatten() {
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let jsonl = session_dir.join(format!("{id}.jsonl"));
        if jsonl.is_file() {
            if let Some(meta) = session_meta(&jsonl, &id, slug, false, None) {
                sessions.push(meta);
            }
        }
        // Subagent transcripts: agent-transcripts/<id>/subagents/<sub-id>.jsonl
        let subdir = session_dir.join("subagents");
        if let Ok(subs) = fs::read_dir(&subdir) {
            for sub in subs.flatten() {
                let p = sub.path();
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    let sub_id = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if let Some(meta) = session_meta(&p, &sub_id, slug, true, Some(id.clone())) {
                        sessions.push(meta);
                    }
                }
            }
        }
    }
    // Newest first.
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
    sessions
}

fn session_meta(
    path: &Path,
    id: &str,
    slug: &str,
    is_subagent: bool,
    parent_id: Option<String>,
) -> Option<SessionMeta> {
    let fs_meta = fs::metadata(path).ok()?;
    let title = derive_title(path);
    Some(SessionMeta {
        id: id.to_string(),
        project_slug: slug.to_string(),
        path: path.to_path_buf(),
        title,
        modified: fs_meta.modified().ok(),
        size_bytes: fs_meta.len(),
        is_subagent,
        parent_id,
    })
}

/// Read only the beginning of the transcript to derive a human title from the
/// first user query. Capped read keeps scanning fast even for multi-MB files.
fn derive_title(path: &Path) -> String {
    const MAX_TITLE: usize = 90;
    const MAX_SCAN_BYTES: u64 = 512 * 1024;
    let Ok(file) = fs::File::open(path) else {
        return "(unreadable)".into();
    };
    let mut reader = BufReader::new(file.take(MAX_SCAN_BYTES));
    let mut line = String::new();
    // Look through the first few records for a user query.
    for _ in 0..8 {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(q) = parser::title_from_record(&line) {
            return truncate_title(&q, MAX_TITLE);
        }
    }
    "(untitled session)".into()
}

fn truncate_title(s: &str, max: usize) -> String {
    let cleaned: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if cleaned.chars().count() <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Best-effort decoding of a project slug into (display name, workspace hint).
/// Slugs replace `/` with `-`, which is ambiguous; this is informational only.
fn decode_slug(slug: &str) -> (String, String) {
    if slug.chars().all(|c| c.is_ascii_digit()) {
        return (format!("project {slug}"), String::new());
    }
    let hint = format!("/{}", slug.replace('-', "/"));
    // Display name: keep the tail after the last common prefix segment.
    let display = slug
        .rsplit('-')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(slug)
        .to_string();
    (display, hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_numeric_slug() {
        let (name, hint) = decode_slug("1775445826829");
        assert_eq!(name, "project 1775445826829");
        assert!(hint.is_empty());
    }

    #[test]
    fn decode_path_slug() {
        let (name, hint) = decode_slug("Users-albou-projects-foo");
        assert_eq!(name, "foo");
        assert_eq!(hint, "/Users/albou/projects/foo");
    }

    #[test]
    fn truncate_short_title() {
        assert_eq!(truncate_title("hello world", 90), "hello world");
    }
}
