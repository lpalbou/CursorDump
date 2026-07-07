//! Local web server: JSON API over the core library + embedded frontend.
//!
//! Binds 127.0.0.1 only. All session reads are validated to stay inside the
//! scanned projects root, and exports refuse destinations inside `~/.cursor`
//! (enforced by `export::validate_out_dir`).

mod api;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;

use crate::model::Project;

/// Per-session facets: which tools were used and which media kinds attached.
#[derive(Clone)]
pub struct SessionFacet {
    pub path: String,
    pub tools: Vec<String>,
    pub media: Vec<String>,
}

/// One attachment referenced by a message.
#[derive(Clone)]
pub struct MediaItem {
    pub name: String,
    pub kind: String,
    pub path: String,
}

/// A single message, indexed for the message-level finder. Holds no full text
/// (only a short snippet) to keep the whole index small in memory.
#[derive(Clone)]
pub struct MsgEntry {
    pub project_slug: String,
    pub session_path: String,
    pub session_title: String,
    pub is_subagent: bool,
    pub line_index: usize,
    pub role: &'static str,
    pub tools: Vec<String>,
    pub media: Vec<MediaItem>,
    pub snippet: String,
    pub modified_unix: u64,
}

/// What kind of data source is being explored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SourceKind {
    /// The live `~/.cursor/projects` of this machine.
    Local,
    /// A CursorDump backup (directory with a `cursordump-backup.json` marker).
    Backup,
    /// A bare projects directory passed on the command line.
    PlainDir,
}

impl SourceKind {
    pub fn label(self) -> &'static str {
        match self {
            SourceKind::Local => "local",
            SourceKind::Backup => "backup",
            SourceKind::PlainDir => "dir",
        }
    }
}

/// One data source and every cache derived from it. Swapping sources swaps
/// the WHOLE struct behind `AppState.current`, so a request that snapshots
/// its `Source` once can never observe a torn mix of old root + new caches.
pub struct Source {
    pub kind: SourceKind,
    /// The projects root being scanned.
    pub root: PathBuf,
    /// Parent of `root` — media boundary + backup `attachments/` location.
    pub cursor_root: PathBuf,
    /// Display name (folder name for backups, "Local Cursor" for local).
    pub label: String,
    /// Backup creation time from the manifest (backups only).
    pub created_unix: Option<u64>,
    /// Cached scan; refreshed via /api/rescan.
    pub projects: RwLock<Vec<Project>>,
    /// Lazily computed per-project facets, keyed by project slug.
    pub facets: RwLock<std::collections::HashMap<String, Vec<SessionFacet>>>,
    /// Lazily built message-level index for the unified finder.
    pub message_index: RwLock<Option<Arc<Vec<MsgEntry>>>>,
    /// Bumped on every rescan. A build that started before a rescan must not
    /// store its (stale) result over the cleared slot.
    pub index_gen: std::sync::atomic::AtomicU64,
    /// Serializes index builds so concurrent first-finds don't each pay the
    /// full parse cost.
    pub index_build: std::sync::Mutex<()>,
}

impl Source {
    /// Build a source and run its initial scan.
    pub fn create(
        kind: SourceKind,
        root: PathBuf,
        label: String,
        created_unix: Option<u64>,
    ) -> Self {
        let cursor_root = root
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| root.clone());
        Self {
            kind,
            cursor_root,
            label,
            created_unix,
            projects: RwLock::new(crate::scanner::scan_projects(&root)),
            facets: RwLock::new(std::collections::HashMap::new()),
            message_index: RwLock::new(None),
            index_gen: std::sync::atomic::AtomicU64::new(0),
            index_build: std::sync::Mutex::new(()),
            root,
        }
    }

    /// Snapshot the cached projects, recovering from lock poisoning rather
    /// than propagating a panic that would brick every endpoint.
    pub fn projects_snapshot(&self) -> Vec<Project> {
        self.projects
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_projects(&self, projects: Vec<Project>) {
        *self.projects.write().unwrap_or_else(|e| e.into_inner()) = projects;
        self.facets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Invalidate any in-flight index build BEFORE clearing the slot, so a
        // stale build can't repopulate it.
        self.index_gen
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self
            .message_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn cached_index(&self) -> Option<Arc<Vec<MsgEntry>>> {
        self.message_index
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Store a built index, but only if no rescan happened since the build
    /// started (`gen` is the generation observed at build start).
    pub fn store_index_if_current(&self, index: Arc<Vec<MsgEntry>>, gen: u64) {
        if self.index_gen.load(std::sync::atomic::Ordering::SeqCst) == gen {
            *self
                .message_index
                .write()
                .unwrap_or_else(|e| e.into_inner()) = Some(index);
        }
    }

    pub fn current_index_gen(&self) -> u64 {
        self.index_gen.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn cached_facets(&self, slug: &str) -> Option<Vec<SessionFacet>> {
        self.facets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(slug)
            .cloned()
    }

    pub fn store_facets(&self, slug: &str, facets: Vec<SessionFacet>) {
        self.facets
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(slug.to_string(), facets);
    }
}

/// Shared application state.
pub struct AppState {
    /// Random per-run token required on every `/api/*` request. The browser
    /// receives it via the opened URL; other local processes that cannot read
    /// that URL cannot call the API (defends the "local, read-only" posture
    /// beyond the loopback + Host guard).
    pub token: String,
    /// The REAL `~/.cursor` of this machine, pinned at boot. Export and
    /// backup destination guards ALWAYS protect this directory, no matter
    /// which source is currently being explored (switching to a backup must
    /// not silently drop the write-refusal for the live Cursor data).
    pub real_cursor_root: PathBuf,
    /// The local projects root detected at boot (None on machines without
    /// Cursor data). This is the only non-backup root `/api/source` accepts.
    pub local_root: Option<PathBuf>,
    /// The source currently being explored.
    pub current: RwLock<Arc<Source>>,
}

impl AppState {
    /// Atomic snapshot of the current source. Take it ONCE per request; all
    /// reads through the same `Arc<Source>` are mutually consistent.
    pub fn source(&self) -> Arc<Source> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn swap_source(&self, source: Arc<Source>) {
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = source;
    }
}

pub type SharedState = Arc<AppState>;

/// Random hex token generated per run.
fn generate_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(&mut buf).is_ok() {
            return buf.iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{t:x}{:x}", std::process::id())
}

/// Host is a loopback name, and (for `/api/*`) a valid token is present.
///
/// - The Host allowlist defeats DNS-rebinding (a remote page can't become
///   same-origin with the local server).
/// - The token defeats other local processes / stray clients: only the browser
///   tab we opened (which received the token in its URL) can call the API.
///   `/api/media` is reached via `<img src>`/`<video>` which cannot set
///   headers, so the token is also accepted as a `token` query parameter.
async fn guard(
    axum::extract::State(state): axum::extract::State<SharedState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let host_ok = matches!(name, "localhost" | "127.0.0.1" | "::1" | "[::1]") || name.is_empty();
    if !host_ok {
        return Err(StatusCode::FORBIDDEN);
    }

    if req.uri().path().starts_with("/api/") {
        let from_header = req
            .headers()
            .get("x-cursordump-token")
            .and_then(|h| h.to_str().ok())
            .map(str::to_string);
        let from_query = req.uri().query().and_then(|q| {
            q.split('&').find_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                (k == "token").then(|| urldecode(v))
            })
        });
        let provided = from_header.or(from_query);
        if provided.as_deref() != Some(state.token.as_str()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(req).await)
}

/// Minimal percent-decode for the token query value (hex tokens rarely need
/// it, but `%`-escapes are handled for safety).
fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

const INDEX_HTML: &str = include_str!("ui/index.html");
const APP_CSS: &str = include_str!("ui/app.css");
const APP_JS: &str = include_str!("ui/app.js");

/// Start the server (blocking). Opens the browser once the port is bound.
///
/// `root` is the projects root to explore first; when it belongs to a backup
/// (marker present) the source is created in Backup mode with its hardened
/// media rules.
pub fn run(root: PathBuf, port: u16, open_browser: bool) -> anyhow::Result<()> {
    let local_root = crate::scanner::default_root().filter(|r| r.is_dir());
    let real_cursor_root = local_root
        .as_deref()
        .and_then(|r| r.parent())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|h| h.join(".cursor"))
                .unwrap_or_else(|| root.clone())
        });

    // Classify the launch root: local, backup (marker), or plain dir.
    let source = if Some(&root) == local_root.as_ref() {
        Source::create(SourceKind::Local, root, "Local Cursor".into(), None)
    } else {
        match crate::backup::resolve_source_path(&root, true) {
            Ok(r) => Source::create(
                if r.is_backup {
                    SourceKind::Backup
                } else {
                    SourceKind::PlainDir
                },
                r.projects_root,
                r.label,
                r.created_unix,
            ),
            Err(_) => Source::create(
                SourceKind::PlainDir,
                root.clone(),
                root.display().to_string(),
                None,
            ),
        }
    };

    let state: SharedState = Arc::new(AppState {
        token: generate_token(),
        real_cursor_root,
        local_root,
        current: RwLock::new(Arc::new(source)),
    });

    let app = Router::new()
        .route("/", get(|| async { axum::response::Html(INDEX_HTML) }))
        .route(
            "/app.css",
            get(|| async { ([(axum::http::header::CONTENT_TYPE, "text/css")], APP_CSS) }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/javascript")],
                    APP_JS,
                )
            }),
        )
        .route("/api/projects", get(api::projects))
        .route("/api/rescan", post(api::rescan))
        .route("/api/sessions", get(api::sessions))
        .route("/api/facets", get(api::facets))
        .route("/api/session", get(api::session))
        .route("/api/media", get(api::media))
        .route("/api/find", post(api::find))
        .route("/api/export", post(api::export))
        .route("/api/backup", post(api::backup))
        .route("/api/default_out_dir", get(api::default_out_dir))
        .route("/api/default_backup_dir", get(api::default_backup_dir))
        .route("/api/sources", post(api::sources))
        .route("/api/source", post(api::set_source))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state.clone());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let base = format!("http://{}", listener.local_addr()?);
        let url = format!("{base}/?token={}", state.token);
        eprintln!("CursorDump running at {base} (read-only on ~/.cursor)");
        if open_browser {
            let _ = open::that(&url);
        } else {
            // Headless/manual launch: the token is required to reach the API.
            eprintln!("Open: {url}");
        }
        // Warm the message index in the background so the first find is instant.
        {
            let st = state.clone();
            tokio::task::spawn_blocking(move || {
                api::build_message_index(&st);
            });
        }
        axum::serve(listener, app).await?;
        Ok(())
    })
}
