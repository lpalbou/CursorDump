//! Keyword search across all transcripts.
//!
//! Runs on a background thread; results stream back over a channel so the GUI
//! never blocks. Search is a case-insensitive substring match over raw file
//! content (fast prefilter), then matched lines are parsed to produce
//! human-readable snippets.

use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use crate::model::{Project, SessionMeta};

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub project_slug: String,
    pub session: SessionMeta,
    /// 0-based record index within the transcript (line number of the match).
    pub record_index: usize,
    pub snippet: String,
}

#[derive(Debug)]
pub enum SearchEvent {
    Hit(SearchHit),
    Finished { files_scanned: usize },
}

/// Handle used by the GUI to cancel an in-flight search.
#[derive(Clone)]
pub struct SearchCancel(Arc<AtomicBool>);

impl Default for SearchCancel {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchCancel {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

const MAX_HITS: usize = 500;
const SNIPPET_CONTEXT: usize = 80;

/// Collect ALL (session_path, line_index) keyword hits, uncapped and without
/// building snippets. Used by the message-level finder so a keyword combined
/// with media/tool filters isn't silently truncated by the display cap.
pub fn collect_keyword_hits(
    projects: &[Project],
    query: &str,
) -> std::collections::HashSet<(std::path::PathBuf, usize)> {
    let needle = query.to_ascii_lowercase();
    let mut hits = std::collections::HashSet::new();
    for project in projects {
        for session in &project.sessions {
            let Ok(bytes) = fs::read(&session.path) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            if !content.to_ascii_lowercase().contains(&needle) {
                continue;
            }
            for (idx, line) in content.lines().enumerate() {
                if line.to_ascii_lowercase().contains(&needle) {
                    hits.insert((session.path.clone(), idx));
                }
            }
        }
    }
    hits
}

/// Blocking search, intended to run on a background thread.
/// `repaint` is called after each event so the GUI wakes up.
pub fn run_search(
    projects: &[Project],
    query: &str,
    tx: Sender<SearchEvent>,
    cancel: SearchCancel,
    repaint: impl Fn(),
) {
    // ASCII case folding preserves byte offsets (unlike Unicode lowercasing,
    // where e.g. 'İ' changes length and would desync snippet offsets).
    let needle = query.to_ascii_lowercase();
    let mut files_scanned = 0usize;
    let mut hits = 0usize;

    'outer: for project in projects {
        for session in &project.sessions {
            if cancel.is_cancelled() || hits >= MAX_HITS {
                break 'outer;
            }
            let Ok(bytes) = fs::read(&session.path) else {
                continue;
            };
            let content = String::from_utf8_lossy(&bytes);
            files_scanned += 1;
            if !content.to_ascii_lowercase().contains(&needle) {
                continue;
            }
            for (idx, line) in content.lines().enumerate() {
                let lower = line.to_ascii_lowercase();
                let Some(pos) = lower.find(&needle) else {
                    continue;
                };
                let snippet = make_snippet(line, pos, needle.len());
                let _ = tx.send(SearchEvent::Hit(SearchHit {
                    project_slug: project.slug.clone(),
                    session: session.clone(),
                    record_index: idx,
                    snippet,
                }));
                repaint();
                hits += 1;
                if hits >= MAX_HITS {
                    break;
                }
            }
        }
    }
    let _ = tx.send(SearchEvent::Finished { files_scanned });
    repaint();
}

/// Extract a readable snippet around the match. The raw line is JSON, so we
/// unescape the most common sequences for display.
fn make_snippet(line: &str, byte_pos: usize, needle_len: usize) -> String {
    let start = floor_char_boundary(line, byte_pos.saturating_sub(SNIPPET_CONTEXT));
    let end = ceil_char_boundary(
        line,
        (byte_pos + needle_len + SNIPPET_CONTEXT).min(line.len()),
    );
    let raw = &line[start..end];
    let cleaned = raw
        .replace("\\n", " ")
        .replace("\\t", " ")
        .replace("\\\"", "\"");
    format!("…{}…", cleaned.trim())
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_respects_utf8_boundaries() {
        let line = "ααααααα needle βββββββ";
        let pos = line.find("needle").unwrap();
        let s = make_snippet(line, pos, 6);
        assert!(s.contains("needle"));
    }
}
