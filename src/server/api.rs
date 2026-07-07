//! JSON API handlers.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backup::{self, BackupOptions};
use crate::export::{self, clean, ExportOptions, SubagentMode, ThinkingMode, UserContent};
use crate::model::{Block, Role, SessionMeta};
use crate::{parser, scanner};

use super::{MediaItem, MsgEntry, SessionFacet, SharedState};

fn unix(t: Option<SystemTime>) -> u64 {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn session_json(s: &SessionMeta) -> Value {
    json!({
        "id": s.id,
        "path": s.path.display().to_string(),
        "title": s.title,
        "modified_unix": unix(s.modified),
        "size_bytes": s.size_bytes,
        "is_subagent": s.is_subagent,
        "parent_id": s.parent_id,
    })
}

// ---------------------------------------------------------------- projects

/// Descriptor of the currently explored source (for the UI's source chip).
fn source_descriptor(src: &crate::server::Source) -> Value {
    json!({
        "kind": src.kind.label(),
        "root": src.root.display().to_string(),
        "label": src.label,
        "created_unix": src.created_unix,
    })
}

pub async fn projects(State(state): State<SharedState>) -> Json<Value> {
    let src = state.source();
    let projects = src.projects_snapshot();
    let list: Vec<Value> = projects
        .iter()
        .map(|p| {
            json!({
                "slug": p.slug,
                "display_name": p.display_name,
                "workspace_hint": p.workspace_hint,
                "main_sessions": p.main_sessions(),
                "subagent_sessions": p.total_sessions() - p.main_sessions(),
                "last_activity_unix": unix(p.last_activity),
            })
        })
        .collect();
    Json(json!({
        "projects": list,
        "root": src.root.display().to_string(),
        "source": source_descriptor(&src),
    }))
}

pub async fn rescan(State(state): State<SharedState>) -> Json<Value> {
    let src = state.source();
    let root = src.root.clone();
    let fresh = tokio::task::spawn_blocking(move || scanner::scan_projects(&root))
        .await
        .unwrap_or_default();
    let n = fresh.len();
    src.set_projects(fresh);
    Json(json!({ "ok": true, "projects": n }))
}

// ---------------------------------------------------------------- sessions

#[derive(Deserialize)]
pub struct SessionsQuery {
    pub project: String,
}

pub async fn sessions(
    State(state): State<SharedState>,
    Query(q): Query<SessionsQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let projects = state.source().projects_snapshot();
    let project = projects
        .iter()
        .find(|p| p.slug == q.project)
        .ok_or((StatusCode::NOT_FOUND, "project not found".into()))?;
    let list: Vec<Value> = project.sessions.iter().map(session_json).collect();
    Ok(Json(json!({ "sessions": list })))
}

// ------------------------------------------------------------------- facets

#[derive(Deserialize)]
pub struct FacetsQuery {
    /// Project slug; absent = facets across ALL projects (for search filters).
    pub project: Option<String>,
}

/// Tools used and media kinds attached, per session. Computed by parsing the
/// transcripts (cached per project — and under "*" for the global set).
pub async fn facets(
    State(state): State<SharedState>,
    Query(q): Query<FacetsQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let src = state.source();
    let key = q.project.clone().unwrap_or_else(|| "*".to_string());
    if let Some(cached) = src.cached_facets(&key) {
        return Ok(Json(facets_json(&cached)));
    }
    let projects = src.projects_snapshot();
    let sessions: Vec<SessionMeta> = match &q.project {
        Some(slug) => projects
            .iter()
            .find(|p| &p.slug == slug)
            .ok_or((StatusCode::NOT_FOUND, "project not found".into()))?
            .sessions
            .clone(),
        None => projects
            .iter()
            .flat_map(|p| p.sessions.iter().cloned())
            .collect(),
    };
    let boundary = crate::export::media_boundary(&src.cursor_root);

    let computed = tokio::task::spawn_blocking(move || {
        sessions
            .iter()
            .map(|meta| {
                let parsed = parser::parse_session(meta);
                let mut tools = std::collections::BTreeSet::new();
                for m in &parsed.messages {
                    for b in &m.blocks {
                        if let Block::ToolUse { name, .. } = b {
                            tools.insert(name.clone());
                        }
                    }
                }
                let mut media = std::collections::BTreeSet::new();
                for r in crate::media::extract_media_refs(&parsed, &boundary) {
                    media.insert(r.kind.label().to_string());
                }
                SessionFacet {
                    path: meta.path.display().to_string(),
                    tools: tools.into_iter().collect(),
                    media: media.into_iter().collect(),
                }
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    src.store_facets(&key, computed.clone());
    Ok(Json(facets_json(&computed)))
}

fn facets_json(facets: &[SessionFacet]) -> Value {
    let mut all_tools = std::collections::BTreeSet::new();
    let mut all_media = std::collections::BTreeSet::new();
    let per: Vec<Value> = facets
        .iter()
        .map(|f| {
            for t in &f.tools {
                all_tools.insert(t.clone());
            }
            for m in &f.media {
                all_media.insert(m.clone());
            }
            json!({ "path": f.path, "tools": f.tools, "media": f.media })
        })
        .collect();
    json!({
        "sessions": per,
        "tools": all_tools.into_iter().collect::<Vec<_>>(),
        "media": all_media.into_iter().collect::<Vec<_>>(),
    })
}

// ------------------------------------------------------------------ session

#[derive(Deserialize)]
pub struct SessionQuery {
    pub path: String,
}

/// Resolve a client-supplied transcript path to a known scanned session.
/// Only paths the scanner itself produced are served — the browser can never
/// use this endpoint to read arbitrary files. The path must additionally
/// canonicalize INSIDE the source root: a symlinked `.jsonl` planted in a
/// hostile backup must not read files outside it.
fn find_session(src: &crate::server::Source, path: &str) -> Option<SessionMeta> {
    let path = PathBuf::from(path);
    let meta = src
        .projects_snapshot()
        .iter()
        .flat_map(|p| p.sessions.iter())
        .find(|s| s.path == path)
        .cloned()?;
    let root = src.root.canonicalize().ok()?;
    let real = meta.path.canonicalize().ok()?;
    real.starts_with(&root).then_some(meta)
}

pub async fn session(
    State(state): State<SharedState>,
    Query(q): Query<SessionQuery>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let src = state.source();
    let meta =
        find_session(&src, &q.path).ok_or((StatusCode::NOT_FOUND, "unknown session".into()))?;
    let parsed = tokio::task::spawn_blocking(move || parser::parse_session(&meta))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let boundary = crate::export::media_boundary(&src.cursor_root);
    let messages: Vec<Value> = parsed
        .messages
        .iter()
        .map(|m| {
            let full = m.full_text();
            let (thinking, answer) = match m.role {
                Role::Assistant => clean::split_thinking(&full),
                Role::User => (String::new(), full.clone()),
            };
            let tools: Vec<Value> = m
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::ToolUse { name, input } => Some(json!({
                        "name": name,
                        "input": input,
                    })),
                    _ => None,
                })
                .collect();
            // Attachments referenced in this (user) message, resolved so the
            // frontend can render them (works live AND from a backup).
            let media: Vec<Value> = if m.role == Role::User {
                crate::media::extract_refs_from_text(&full, &boundary)
                    .into_iter()
                    .map(|r| {
                        let resolved = resolve_media_path(&src, &r.path).is_some();
                        json!({
                            "path": r.path.display().to_string(),
                            "name": r.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                            "kind": r.kind.label(),
                            "available": resolved,
                        })
                    })
                    .collect()
            } else {
                Vec::new()
            };
            json!({
                "role": match m.role { Role::User => "user", Role::Assistant => "assistant" },
                "injected": m.is_injected,
                "query": m.user_query,
                "thinking": thinking,
                "answer": answer,
                "raw": full,
                "tools": tools,
                "media": media,
                "line_index": m.line_index,
                "timestamp": m.timestamp,
            })
        })
        .collect();

    Ok(Json(json!({
        "meta": session_json(&parsed.meta),
        "turns": parsed.turns.len(),
        "skipped_lines": parsed.skipped_lines,
        "messages": messages,
    })))
}

// -------------------------------------------------------------------- media

/// Resolve a media path referenced in a transcript to a servable file.
///
/// Two cases:
/// 1. The original absolute path still exists and canonicalizes inside the
///    scanned root or the standard cursor projects boundary → serve it.
/// 2. We are exploring a BACKUP (or the file moved): remap by locating a known
///    project slug inside the path and re-rooting the remainder onto the
///    scanned root (`<root>/<slug>/<rest>`), e.g.
///    `/Users/x/.cursor/projects/<slug>/assets/a.png` → `<backup>/projects/<slug>/assets/a.png`.
///
/// The resolved path MUST canonicalize inside the scanned root or the cursor
/// projects boundary — this endpoint can never serve arbitrary files.
fn resolve_media_path(src: &crate::server::Source, requested: &Path) -> Option<PathBuf> {
    resolve_media_path_ctx(&MediaCtx::new(src), requested)
}

/// Precomputed resolution context so bulk callers (the finder maps hundreds of
/// results) don't re-snapshot projects and re-canonicalize roots per item.
struct MediaCtx {
    /// Canonicalized scan root (None if it doesn't exist).
    root: Option<PathBuf>,
    /// Original (non-canonical) scan root, used for slug re-rooting.
    scan_root: PathBuf,
    boundary: PathBuf,
    slugs: Vec<String>,
    index: std::sync::Arc<Vec<MsgEntry>>,
    /// Whether transcript-referenced EXTERNAL paths may be served (case 1b).
    /// Only true for the live local source: a hostile backup could plant
    /// references to arbitrary local files in its transcripts, so in Backup/
    /// PlainDir mode only the mirror and captured attachments are served.
    allow_external_refs: bool,
}

impl MediaCtx {
    fn new(src: &crate::server::Source) -> Self {
        let boundary = crate::export::media_boundary(&src.cursor_root);
        Self {
            root: src.root.canonicalize().ok(),
            scan_root: src.root.clone(),
            boundary: boundary.canonicalize().unwrap_or(boundary),
            slugs: src
                .projects_snapshot()
                .iter()
                .map(|p| p.slug.clone())
                .collect(),
            index: message_index(src),
            allow_external_refs: src.kind == crate::server::SourceKind::Local,
        }
    }
}

fn resolve_media_path_ctx(ctx: &MediaCtx, requested: &Path) -> Option<PathBuf> {
    let root = ctx.root.as_ref()?;
    let within = |p: &Path| -> Option<PathBuf> {
        let c = p.canonicalize().ok()?;
        (c.starts_with(root) || c.starts_with(&ctx.boundary)).then_some(c)
    };
    // Case 1: original path inside an allowed boundary (cursor assets/uploads).
    if requested.is_file() {
        if let Some(c) = within(requested) {
            return Some(c);
        }
    }
    // Case 1b: the original path still exists at its real workspace location
    // (a user `@file` reference outside ~/.cursor). Only serve it if some
    // indexed message ACTUALLY references this exact path — this bounds the
    // endpoint to attachments the user's own sessions mention, so it can never
    // be used to read arbitrary media-extension files elsewhere on disk.
    // DISABLED for backup/plain-dir sources: their transcripts are untrusted
    // input and could reference any local file.
    if ctx.allow_external_refs && requested.is_file() && path_is_referenced(ctx, requested) {
        if let Ok(c) = requested.canonicalize() {
            return Some(c);
        }
    }
    // Case 2: re-root cursor-internal paths onto the scanned root via slug
    // (browsing a backup: /…/.cursor/projects/<slug>/assets/x → <root>/<slug>/assets/x).
    let req = requested.to_string_lossy();
    for slug in &ctx.slugs {
        let needle = format!("/{slug}/");
        if let Some(idx) = req.find(&needle) {
            let rest = &req[idx + needle.len()..];
            let candidate = ctx.scan_root.join(slug).join(rest);
            if candidate.is_file() {
                if let Some(c) = within(&candidate) {
                    return Some(c);
                }
            }
        }
    }
    // Case 2b: a backup captured this external attachment into
    // `<backup>/attachments/<hash-name>`. When exploring a backup, `root` is
    // `<backup>/projects`, so its parent holds the attachments dir.
    if let Some(backup_root) = root.parent() {
        let cand = backup_root
            .join("attachments")
            .join(crate::media::attachment_filename(requested));
        if cand.is_file() {
            if let Ok(c) = cand.canonicalize() {
                return Some(c);
            }
        }
    }
    None
}

/// True if some indexed message references this exact absolute path as media.
fn path_is_referenced(ctx: &MediaCtx, path: &Path) -> bool {
    let p = path.display().to_string();
    ctx.index
        .iter()
        .any(|e| e.media.iter().any(|m| m.path == p))
}

fn mime_for(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "heic" => "image/heic",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",
        "aiff" => "audio/aiff",
        "caf" => "audio/x-caf",
        "pdf" => "application/pdf",
        "md" | "markdown" | "txt" | "log" => "text/plain; charset=utf-8",
        "csv" | "tsv" => "text/plain; charset=utf-8",
        "json" | "jsonl" => "application/json",
        "html" => "text/plain; charset=utf-8", // never render foreign HTML
        _ => "application/octet-stream",
    }
}

#[derive(Deserialize)]
pub struct MediaQuery {
    pub path: String,
}

/// Serve a referenced attachment (image/audio/video/readable). Only files
/// that `resolve_media_path` accepts are ever read.
pub async fn media(
    State(state): State<SharedState>,
    Query(q): Query<MediaQuery>,
) -> Result<axum::response::Response, (StatusCode, String)> {
    let requested = PathBuf::from(&q.path);
    // Only media-classified extensions are servable at all.
    if matches!(
        crate::media::classify(&requested),
        crate::model::MediaKind::Other
    ) {
        return Err((StatusCode::FORBIDDEN, "not a media file".into()));
    }
    let resolved = resolve_media_path(&state.source(), &requested)
        .ok_or((StatusCode::NOT_FOUND, "attachment not found".into()))?;
    // Stream instead of buffering: session videos can be hundreds of MB and a
    // few parallel <video> tags must not hold whole files in memory.
    let file = tokio::fs::File::open(&resolved)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let len = file
        .metadata()
        .await
        .ok()
        .map(|m| m.len().to_string())
        .unwrap_or_default();
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    let mime = mime_for(&requested);
    let mut resp = axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, mime)
        // Inline display, but never HTML execution (mime above is text/plain
        // for markup) — plus a conservative CSP for SVG.
        .header(
            axum::http::header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; style-src 'unsafe-inline'",
        );
    if !len.is_empty() {
        resp = resp.header(axum::http::header::CONTENT_LENGTH, len);
    }
    resp.body(body)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

// -------------------------------------------------------------- unified find

/// Public entry point to warm the index at startup.
pub fn build_message_index(state: &SharedState) {
    let _ = message_index(&state.source());
}

/// Build (or reuse) the message-level index: one entry per message with its
/// tools, attached media and a short snippet — but no full text (kept small).
fn message_index(src: &crate::server::Source) -> std::sync::Arc<Vec<MsgEntry>> {
    if let Some(idx) = src.cached_index() {
        return idx;
    }
    // Serialize builds: a second caller waits here, then finds the cache warm.
    let _build = src.index_build.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(idx) = src.cached_index() {
        return idx;
    }
    let gen = src.current_index_gen();
    let boundary = crate::export::media_boundary(&src.cursor_root);
    let projects = src.projects_snapshot();
    let mut entries: Vec<MsgEntry> = Vec::new();
    for p in &projects {
        for s in &p.sessions {
            let parsed = parser::parse_session(s);
            let modified = unix(s.modified);
            for m in &parsed.messages {
                let full = m.full_text();
                let mut tools = Vec::new();
                for b in &m.blocks {
                    if let Block::ToolUse { name, .. } = b {
                        if !tools.contains(name) {
                            tools.push(name.clone());
                        }
                    }
                }
                let media: Vec<MediaItem> = if m.role == Role::User {
                    crate::media::extract_refs_from_text(&full, &boundary)
                        .into_iter()
                        .map(|r| MediaItem {
                            name: r
                                .path
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            kind: r.kind.label().to_string(),
                            path: r.path.display().to_string(),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                // Skip messages with nothing to index/show.
                if tools.is_empty() && media.is_empty() && full.trim().is_empty() {
                    continue;
                }
                let snippet_src = match m.role {
                    Role::User => m.user_query.clone().unwrap_or(full),
                    Role::Assistant => full,
                };
                let snippet: String = snippet_src
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                entries.push(MsgEntry {
                    project_slug: p.slug.clone(),
                    session_path: s.path.display().to_string(),
                    session_title: s.title.clone(),
                    is_subagent: s.is_subagent,
                    line_index: m.line_index,
                    role: match m.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    tools,
                    media,
                    snippet,
                    modified_unix: modified,
                });
            }
        }
    }
    let arc = std::sync::Arc::new(entries);
    // Only cache if no rescan invalidated this build while it ran.
    src.store_index_if_current(arc.clone(), gen);
    arc
}

#[derive(Deserialize)]
pub struct FindBody {
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub media: Vec<String>,
    #[serde(default)]
    pub tools: Vec<String>,
    /// Optional project slug to scope to.
    pub project: Option<String>,
}

const FIND_CAP: usize = 500;

/// Unified message-level finder: keyword AND media AND tools, returning the
/// specific messages that match every active criterion.
pub async fn find(
    State(state): State<SharedState>,
    Json(body): Json<FindBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let query = body.query.trim().to_string();
    let has_kw = query.len() >= 2;
    let media: std::collections::HashSet<String> = body.media.into_iter().collect();
    let tools: std::collections::HashSet<String> = body.tools.into_iter().collect();
    if !has_kw && media.is_empty() && tools.is_empty() {
        return Ok(Json(
            json!({ "results": [], "total": 0, "truncated": false }),
        ));
    }
    let project = body.project.clone();

    let (results, total) = tokio::task::spawn_blocking(move || {
        // One atomic source snapshot for the whole request.
        let src = state.source();
        // Keyword hit set: ALL (session_path, line_index) matches, uncapped so
        // combining keyword with media/tool filters isn't truncated early.
        let kw_hits: Option<std::collections::HashSet<(String, usize)>> = if has_kw {
            let projects = src.projects_snapshot();
            let set = crate::search::collect_keyword_hits(&projects, &query)
                .into_iter()
                .map(|(p, i)| (p.display().to_string(), i))
                .collect();
            Some(set)
        } else {
            None
        };

        let index = message_index(&src);
        let mut matched: Vec<&MsgEntry> = index
            .iter()
            .filter(|e| {
                if let Some(ref slug) = project {
                    if &e.project_slug != slug {
                        return false;
                    }
                }
                if !media.is_empty() && !e.media.iter().any(|m| media.contains(&m.kind)) {
                    return false;
                }
                if !tools.is_empty() && !e.tools.iter().any(|t| tools.contains(t)) {
                    return false;
                }
                if let Some(ref hits) = kw_hits {
                    if !hits.contains(&(e.session_path.clone(), e.line_index)) {
                        return false;
                    }
                }
                true
            })
            .collect();
        matched.sort_by_key(|e| std::cmp::Reverse(e.modified_unix));
        let total = matched.len();
        // One snapshot + one canonicalization for the whole result page.
        let ctx = MediaCtx::new(&src);
        let names: std::collections::HashMap<String, String> = src
            .projects_snapshot()
            .into_iter()
            .map(|p| (p.slug, p.display_name))
            .collect();
        let display_map = |e: &MsgEntry| {
            json!({
                "project": e.project_slug,
                "project_name": names.get(&e.project_slug).cloned().unwrap_or_else(|| e.project_slug.clone()),
                "session_path": e.session_path,
                "session_title": e.session_title,
                "is_subagent": e.is_subagent,
                "line_index": e.line_index,
                "role": e.role,
                "snippet": e.snippet,
                "tools": e.tools,
                "media": e.media.iter().map(|m| json!({
                    "name": m.name, "kind": m.kind, "path": m.path,
                    "available": resolve_media_path_ctx(&ctx, &PathBuf::from(&m.path)).is_some(),
                })).collect::<Vec<_>>(),
            })
        };
        let results: Vec<Value> = matched
            .iter()
            .take(FIND_CAP)
            .map(|e| display_map(e))
            .collect();
        (results, total)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(json!({
        "results": results,
        "total": total,
        "truncated": total > FIND_CAP,
    })))
}

// ------------------------------------------------------------------- export

#[derive(Deserialize)]
pub struct ExportBody {
    pub paths: Vec<String>,
    pub out_dir: String,
    #[serde(default)]
    pub options: ExportOptionsBody,
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ExportOptionsBody {
    pub sft_chatml: Option<bool>,
    pub sft_sharegpt: Option<bool>,
    pub cpt_jsonl: Option<bool>,
    pub cpt_txt: Option<bool>,
    pub include_tool_calls: Option<bool>,
    pub user_content: Option<String>,  // clean | raw
    pub thinking: Option<String>,      // tagged | verbatim | strip
    pub subagent_mode: Option<String>, // inline | separate | drop
    pub clean_assistant: Option<bool>,
    pub final_response_only: Option<bool>,
    pub copy_media: Option<bool>,
    pub inline_readable_attachments: Option<bool>,
    pub min_turns: Option<usize>,
    pub val_fraction: Option<f32>,
    pub with_metadata: Option<bool>,
    pub max_record_chars: Option<usize>,
    pub redact_secrets: Option<bool>,
}

impl ExportOptionsBody {
    fn to_options(&self) -> ExportOptions {
        let mut o = ExportOptions::default();
        macro_rules! set {
            ($($f:ident),*) => { $( if let Some(v) = self.$f { o.$f = v; } )* };
        }
        set!(
            sft_chatml,
            sft_sharegpt,
            cpt_jsonl,
            cpt_txt,
            include_tool_calls,
            clean_assistant,
            final_response_only,
            copy_media,
            inline_readable_attachments,
            min_turns,
            val_fraction,
            with_metadata,
            max_record_chars,
            redact_secrets
        );
        if let Some(u) = self.user_content.as_deref() {
            o.user_content = if u == "raw" {
                UserContent::RawFull
            } else {
                UserContent::CleanQuery
            };
        }
        if let Some(t) = self.thinking.as_deref() {
            o.thinking = match t {
                "strip" => ThinkingMode::Strip,
                "verbatim" => ThinkingMode::Verbatim,
                _ => ThinkingMode::Tagged,
            };
        }
        if let Some(s) = self.subagent_mode.as_deref() {
            o.subagent_mode = match s {
                "separate" => SubagentMode::Separate,
                "drop" => SubagentMode::Drop,
                _ => SubagentMode::Inline,
            };
        }
        // Separate mode must include subagent transcripts in the record set.
        o.include_subagent_sessions = o.subagent_mode == SubagentMode::Separate;
        o
    }
}

pub async fn export(
    State(state): State<SharedState>,
    Json(body): Json<ExportBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let src = state.source();
    let sessions: Vec<SessionMeta> = body
        .paths
        .iter()
        .filter_map(|p| find_session(&src, p))
        .collect();
    if sessions.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "no valid sessions selected".into()));
    }
    let selected_total = sessions.len();
    let selected_subagents = sessions.iter().filter(|s| s.is_subagent).count();
    let out_dir = PathBuf::from(shellexpand_home(&body.out_dir));
    // The write-refusal must ALWAYS protect the real ~/.cursor of this
    // machine, even while exploring a backup (whose cursor_root differs).
    export::validate_out_dir(&out_dir, &state.real_cursor_root)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let mut options = body.options.to_options();
    // If the user explicitly ticked subagent transcripts, honor that: keep
    // them in the record set even in Inline mode (otherwise selecting only a
    // subagent would silently export nothing).
    if sessions.iter().any(|s| s.is_subagent) {
        options.include_subagent_sessions = true;
    }
    let cursor_root = src.cursor_root.clone();

    let result = tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        export::run_export(sessions, out_dir, options, cursor_root, tx, || {});
        let mut summary = None;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                export::ExportEvent::Done(s) => summary = Some(s),
                export::ExportEvent::Failed(e) => error = Some(e),
                export::ExportEvent::Progress { .. } => {}
            }
        }
        (summary, error)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match result {
        (Some(s), _) => Ok(Json(json!({
            "ok": true,
            "out_dir": s.out_dir.display().to_string(),
            "selected_total": selected_total,
            "selected_subagents": selected_subagents,
            "sessions_exported": s.sessions_exported,
            "sessions_skipped": s.sessions_skipped,
            "sft_records": s.sft_records,
            "cpt_records": s.cpt_records,
            "media_copied": s.media_copied,
            "media_referenced": s.media_referenced,
            "warnings": s.warnings,
        }))),
        (None, Some(e)) => Err((StatusCode::BAD_REQUEST, e)),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "export produced no result".into(),
        )),
    }
}

// -------------------------------------------------------------------- backup

#[derive(Deserialize)]
pub struct BackupBody {
    /// Project slugs to back up; empty/absent = all projects.
    #[serde(default)]
    pub projects: Vec<String>,
    pub out_dir: String,
    #[serde(default)]
    pub skip_runtime: bool,
    #[serde(default = "default_true")]
    pub verify_transcripts: bool,
    #[serde(default = "default_true")]
    pub include_app: bool,
    #[serde(default = "default_true")]
    pub include_external_attachments: bool,
}

fn default_true() -> bool {
    true
}

pub async fn backup(
    State(state): State<SharedState>,
    Json(body): Json<BackupBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    let src = state.source();
    // Backups are made from LIVE Cursor data; backing up a backup only
    // produces a confusing nested copy (the UI disables the button too).
    if src.kind != crate::server::SourceKind::Local {
        return Err((
            StatusCode::BAD_REQUEST,
            "Backups are made from live Cursor data — switch to Local Cursor first.".into(),
        ));
    }
    let out_dir = PathBuf::from(shellexpand_home(&body.out_dir));
    backup::validate_backup_dir(&out_dir, &state.real_cursor_root)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let options = BackupOptions {
        projects: if body.projects.is_empty() {
            None
        } else {
            Some(body.projects)
        },
        skip_runtime: body.skip_runtime,
        verify_transcripts: body.verify_transcripts,
        include_app: body.include_app,
        include_external_attachments: body.include_external_attachments,
    };
    let root = src.root.clone();
    let cursor_root = src.cursor_root.clone();

    let result = tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        backup::run_backup(root, out_dir, options, cursor_root, tx, || {});
        let mut summary = None;
        let mut error = None;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                backup::BackupEvent::Done(s) => summary = Some(s),
                backup::BackupEvent::Failed(e) => error = Some(e),
                backup::BackupEvent::Progress { .. } => {}
            }
        }
        (summary, error)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match result {
        (Some(s), _) => Ok(Json(json!({
            "ok": true,
            "out_dir": s.out_dir.display().to_string(),
            "projects": s.projects,
            "files_copied": s.files_copied,
            "files_unchanged": s.files_unchanged,
            "bytes_copied": s.bytes_copied,
            "bytes_total": s.bytes_total,
            "warnings": s.warnings,
        }))),
        (None, Some(e)) => Err((StatusCode::BAD_REQUEST, e)),
        _ => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "backup produced no result".into(),
        )),
    }
}

pub async fn default_backup_dir() -> Json<Value> {
    let base = dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    Json(json!({
        "path": base.join("cursordump-backup").display().to_string()
    }))
}

pub async fn default_out_dir() -> Json<Value> {
    let base = dirs::download_dir()
        .or_else(dirs::desktop_dir)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."));
    let stamp = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Json(json!({
        "path": base.join(format!("cursordump-{stamp}")).display().to_string()
    }))
}

/// Expand a leading `~/` to the home directory.
fn shellexpand_home(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).display().to_string();
        }
    }
    p.to_string()
}

// ------------------------------------------------------------------- source

#[derive(Deserialize, Default)]
pub struct SourcesBody {
    /// Recent backup paths remembered by the frontend (localStorage); the
    /// server validates each and returns descriptors for the menu.
    #[serde(default)]
    pub recents: Vec<String>,
}

/// Current source + local availability + validated recents, for the source
/// switcher menu.
pub async fn sources(
    State(state): State<SharedState>,
    Json(body): Json<SourcesBody>,
) -> Json<Value> {
    let src = state.source();
    let local = state.local_root.as_ref().map(|r| {
        json!({
            "path": r.display().to_string(),
            "available": r.is_dir(),
        })
    });
    let recents: Vec<Value> = body
        .recents
        .iter()
        .take(8)
        .map(
            |p| match crate::backup::resolve_source_path(Path::new(p), false) {
                Ok(r) => json!({
                    "path": p,
                    "label": r.label,
                    "created_unix": r.created_unix,
                    "available": true,
                }),
                Err(_) => json!({ "path": p, "label": Path::new(p).file_name()
                    .map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone()),
                    "created_unix": null, "available": false }),
            },
        )
        .collect();
    Json(json!({
        "current": source_descriptor(&src),
        "local": local,
        "recents": recents,
    }))
}

#[derive(Deserialize)]
pub struct SetSourceBody {
    /// "local" to return to the live Cursor data; otherwise `path` is used.
    pub kind: Option<String>,
    pub path: Option<String>,
}

/// Switch the explored source. Only two kinds of root are accepted:
/// the boot-detected local projects root, and marker-verified CursorDump
/// backups — never arbitrary directories (this endpoint must not become a
/// generic filesystem browser).
pub async fn set_source(
    State(state): State<SharedState>,
    Json(body): Json<SetSourceBody>,
) -> Result<Json<Value>, (StatusCode, String)> {
    use crate::server::{Source, SourceKind};
    let new_source = if body.kind.as_deref() == Some("local") {
        let root = state.local_root.clone().ok_or((
            StatusCode::BAD_REQUEST,
            "No Cursor data found at ~/.cursor/projects on this machine.".to_string(),
        ))?;
        tokio::task::spawn_blocking(move || {
            Source::create(SourceKind::Local, root, "Local Cursor".into(), None)
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        let path = body.path.as_deref().unwrap_or("").trim().to_string();
        if path.is_empty() {
            return Err((StatusCode::BAD_REQUEST, "missing path".into()));
        }
        let resolved =
            crate::backup::resolve_source_path(Path::new(&shellexpand_home(&path)), false)
                .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
        tokio::task::spawn_blocking(move || {
            Source::create(
                SourceKind::Backup,
                resolved.projects_root,
                resolved.label,
                resolved.created_unix,
            )
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };
    let descriptor = {
        let src = std::sync::Arc::new(new_source);
        state.swap_source(src.clone());
        // Warm the finder index for the new source in the background.
        let st = state.clone();
        tokio::task::spawn_blocking(move || {
            let _ = message_index(&st.source());
        });
        source_descriptor(&src)
    };
    Ok(Json(json!({ "ok": true, "current": descriptor })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Source, SourceKind};

    fn write(p: &Path, content: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// A hostile backup can reference ANY local file in its transcripts; the
    /// media resolver must refuse to serve such external paths in Backup mode
    /// while still serving them for the live Local source.
    #[test]
    fn external_referenced_media_gated_by_source_kind() {
        let base = std::env::temp_dir().join("cursordump-hostile-backup-test");
        let _ = std::fs::remove_dir_all(&base);

        // A "secret" file elsewhere on disk that the hostile transcript
        // references as if it were a user attachment.
        let secret = base.join("elsewhere/secret.png");
        write(&secret, "SECRET-PNG");

        // Projects tree whose transcript references the secret.
        let root = base.join("projects");
        let rec = serde_json::json!({"role":"user","message":{"content":[
            {"type":"text","text": format!("<user_query>see {}</user_query>", secret.display())}
        ]}})
        .to_string();
        write(
            &root.join("p/agent-transcripts/s/s.jsonl"),
            &format!("{rec}\n"),
        );

        // Same tree explored as a BACKUP: external reference must NOT resolve.
        let backup_src = Source::create(SourceKind::Backup, root.clone(), "bk".into(), None);
        assert!(
            resolve_media_path(&backup_src, &secret).is_none(),
            "hostile backup must not serve external files"
        );

        // Explored as the LOCAL source: the user's own referenced attachment
        // resolves (existing behavior for workspace @files).
        let local_src = Source::create(SourceKind::Local, root, "Local Cursor".into(), None);
        assert_eq!(
            resolve_media_path(&local_src, &secret),
            secret.canonicalize().ok(),
            "local source serves message-referenced attachments"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A symlinked transcript planted in a backup must not be readable via
    /// /api/session: the scanner lists it, but resolution requires the real
    /// file to live inside the source root.
    #[cfg(unix)]
    #[test]
    fn session_lookup_rejects_symlink_escape() {
        let base = std::env::temp_dir().join("cursordump-symlink-session-test");
        let _ = std::fs::remove_dir_all(&base);
        let outside = base.join("outside/real.jsonl");
        write(&outside, "{\"role\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"<user_query>x</user_query>\"}]}}\n");
        let root = base.join("projects");
        let dir = root.join("p/agent-transcripts/s");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join("s.jsonl")).unwrap();

        let src = Source::create(SourceKind::Backup, root, "bk".into(), None);
        // The scanner may list the symlinked transcript…
        if let Some(meta) = src
            .projects_snapshot()
            .iter()
            .flat_map(|p| p.sessions.iter())
            .next()
        {
            // …but find_session must refuse it (canonicalizes outside root).
            assert!(
                find_session(&src, &meta.path.display().to_string()).is_none(),
                "symlink escape must be rejected"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
