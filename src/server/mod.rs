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

/// Shared application state.
pub struct AppState {
    pub root: PathBuf,
    pub cursor_root: PathBuf,
    /// Random per-run token required on every `/api/*` request. The browser
    /// receives it via the opened URL; other local processes that cannot read
    /// that URL cannot call the API (defends the "local, read-only" posture
    /// beyond the loopback + Host guard).
    pub token: String,
    /// Cached scan; refreshed via /api/rescan.
    pub projects: RwLock<Vec<Project>>,
    /// Lazily computed per-project facets (tools/media per session), keyed by
    /// project slug. Cleared on rescan.
    pub facets: RwLock<std::collections::HashMap<String, Vec<SessionFacet>>>,
    /// Lazily built message-level index for the unified finder. Cleared on
    /// rescan; built on first find.
    pub message_index: RwLock<Option<Arc<Vec<MsgEntry>>>>,
    /// Bumped on every rescan. A build that started before a rescan must not
    /// store its (stale) result over the cleared slot.
    pub index_gen: std::sync::atomic::AtomicU64,
    /// Serializes index builds so concurrent first-finds don't each pay the
    /// full parse cost.
    pub index_build: std::sync::Mutex<()>,
}

impl AppState {
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
pub fn run(root: PathBuf, port: u16, open_browser: bool) -> anyhow::Result<()> {
    let cursor_root = root
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| root.clone());

    let state: SharedState = Arc::new(AppState {
        projects: RwLock::new(crate::scanner::scan_projects(&root)),
        facets: RwLock::new(std::collections::HashMap::new()),
        message_index: RwLock::new(None),
        index_gen: std::sync::atomic::AtomicU64::new(0),
        index_build: std::sync::Mutex::new(()),
        token: generate_token(),
        root,
        cursor_root,
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
