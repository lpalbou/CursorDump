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
    /// Cached scan; refreshed via /api/rescan.
    pub projects: RwLock<Vec<Project>>,
    /// Lazily computed per-project facets (tools/media per session), keyed by
    /// project slug. Cleared on rescan.
    pub facets: RwLock<std::collections::HashMap<String, Vec<SessionFacet>>>,
    /// Lazily built message-level index for the unified finder. Cleared on
    /// rescan; built on first find.
    pub message_index: RwLock<Option<Arc<Vec<MsgEntry>>>>,
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

    pub fn store_index(&self, index: Arc<Vec<MsgEntry>>) {
        *self
            .message_index
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(index);
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

/// Reject requests whose Host header is not a loopback name. This defeats
/// DNS-rebinding: a malicious `evil.com` page cannot become same-origin with
/// the local server (which would otherwise let it read transcripts and write
/// files via /api/export). Only literal loopback hosts are accepted.
async fn guard_host(req: Request, next: Next) -> Result<Response, StatusCode> {
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    // Strip an optional port; accept the host portion only.
    let name = host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host);
    let ok = matches!(name, "localhost" | "127.0.0.1" | "::1" | "[::1]") || name.is_empty();
    if ok {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
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
        .route("/api/search", post(api::search))
        .route("/api/find", post(api::find))
        .route("/api/export", post(api::export))
        .route("/api/backup", post(api::backup))
        .route("/api/default_out_dir", get(api::default_out_dir))
        .route("/api/default_backup_dir", get(api::default_backup_dir))
        .layer(middleware::from_fn(guard_host))
        .with_state(state.clone());

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let url = format!("http://{}", listener.local_addr()?);
        eprintln!("CursorDump running at {url} (read-only on ~/.cursor)");
        if open_browser {
            let _ = open::that(&url);
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
