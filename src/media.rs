//! Media reference extraction and classification.
//!
//! Attachments show up in user messages as absolute paths (typically under a
//! project's `assets/` or `uploads/` dir) inside tags like `<image_files>`,
//! `<attached_files>` or `<uploaded_documents>`. We extract every absolute
//! path that looks like a file, classify it by extension, and record whether
//! it resolves inside `~/.cursor/projects` (only those are ever copied).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use regex::Regex;

use crate::model::{MediaKind, MediaRef, ParsedSession, Role};

const READABLE_EXT: &[&str] = &[
    "txt", "md", "markdown", "csv", "tsv", "json", "jsonl", "yaml", "yml", "toml", "xml", "html",
    "css", "js", "ts", "tsx", "jsx", "py", "rs", "go", "java", "c", "h", "cpp", "hpp", "sh", "rb",
    "swift", "kt", "sql", "log", "tex", "rst", "ini", "cfg", "conf",
];
const DOCUMENT_EXT: &[&str] = &[
    "pdf", "docx", "doc", "odt", "rtf", "pptx", "ppt", "xlsx", "xls", "pages", "key", "numbers",
    "epub",
];
const IMAGE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "tif", "svg", "heic", "ico",
];
const VIDEO_EXT: &[&str] = &["mov", "mp4", "avi", "mkv", "webm", "m4v", "mpg", "mpeg"];
const AUDIO_EXT: &[&str] = &[
    "wav", "m4a", "mp3", "aac", "flac", "ogg", "opus", "aiff", "caf",
];

/// Deterministic filename for a captured external attachment, so the backup
/// writer and the media resolver agree without a lookup table:
/// `<sha8(original_abs_path)>-<sanitized_basename>`.
pub fn attachment_filename(original: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(original.to_string_lossy().as_bytes());
    let hash = format!("{:x}", h.finalize());
    let base = original
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
    format!("{}-{}", &hash[..8], safe)
}

pub fn classify(path: &Path) -> MediaKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let e = ext.as_str();
    if READABLE_EXT.contains(&e) {
        MediaKind::Readable
    } else if DOCUMENT_EXT.contains(&e) {
        MediaKind::Document
    } else if IMAGE_EXT.contains(&e) {
        MediaKind::Image
    } else if VIDEO_EXT.contains(&e) {
        MediaKind::Video
    } else if AUDIO_EXT.contains(&e) {
        MediaKind::Audio
    } else {
        MediaKind::Other
    }
}

fn media_regex() -> &'static Regex {
    use std::sync::OnceLock;
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Absolute paths ending with a known media/document extension.
        // Stops at whitespace, quotes and common delimiters.
        Regex::new(
            r#"(?x)
            (/[^\s"'`<>|*?\x00]+
             \.(?i:png|jpe?g|gif|webp|bmp|tiff?|svg|heic|ico|
                mov|mp4|avi|mkv|webm|m4v|mpe?g|
                wav|m4a|mp3|aac|flac|ogg|opus|aiff|caf|
                pdf|docx?|odt|rtf|pptx?|xlsx?|pages|key|numbers|epub|
                txt|md|markdown|csv|tsv))
        "#,
        )
        .expect("valid media path regex")
    })
}

/// Extract media references from a single piece of text (e.g. one user
/// message). Deduplicates within the text.
pub fn extract_refs_from_text(text: &str, cursor_projects_root: &Path) -> Vec<MediaRef> {
    let re = media_regex();
    let mut seen = BTreeSet::new();
    let mut refs = Vec::new();
    for cap in re.captures_iter(text) {
        let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
        let path = PathBuf::from(raw);
        if !seen.insert(path.clone()) {
            continue;
        }
        let exists = path.is_file();
        // Canonicalize BOTH sides so a symlink component in the boundary can't
        // make the containment check fail open.
        let boundary_canon = cursor_projects_root
            .canonicalize()
            .unwrap_or_else(|_| cursor_projects_root.to_path_buf());
        let within_cursor = exists
            && path
                .canonicalize()
                .map(|c| c.starts_with(&boundary_canon))
                .unwrap_or(false);
        refs.push(MediaRef {
            kind: classify(&path),
            path,
            exists,
            within_cursor,
        });
    }
    refs
}

/// Extract all media references from user messages of a session.
/// Only user messages are scanned: assistant/tool text is full of incidental
/// paths (code, shell output) that are not user-provided attachments.
pub fn extract_media_refs(session: &ParsedSession, cursor_projects_root: &Path) -> Vec<MediaRef> {
    let mut seen = BTreeSet::new();
    let mut refs = Vec::new();
    for msg in &session.messages {
        if msg.role != Role::User {
            continue;
        }
        for r in extract_refs_from_text(&msg.full_text(), cursor_projects_root) {
            if seen.insert(r.path.clone()) {
                refs.push(r);
            }
        }
    }
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_extensions() {
        assert_eq!(classify(Path::new("/a/b.md")), MediaKind::Readable);
        assert_eq!(classify(Path::new("/a/b.PNG")), MediaKind::Image);
        assert_eq!(classify(Path::new("/a/b.mov")), MediaKind::Video);
        assert_eq!(classify(Path::new("/a/b.m4a")), MediaKind::Audio);
        assert_eq!(classify(Path::new("/a/b.docx")), MediaKind::Document);
        assert_eq!(classify(Path::new("/a/b.xyz")), MediaKind::Other);
    }
}
