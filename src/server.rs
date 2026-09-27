//! `genie --serve`: shows Genie in a web browser.
//!
//! Serves the browser build of the app (made with `trunk build --release`
//! into `dist/`, and embedded in release builds), the tree it was started
//! with, and the media files that tree refers to. It listens on localhost
//! only and has no login, so it's for use on this computer.

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::media::{self, path_string};
use crate::model::Document;

pub const DEFAULT_PORT: u16 = 8080;

#[derive(rust_embed::RustEmbed)]
#[folder = "dist/"]
#[allow_missing = true]
struct WebApp;

struct Shared {
    tree: PathBuf,
    port: u16,
    /// Media files the tree refers to that exist, the only files served
    /// besides the tree itself. Updated whenever the tree is read or saved.
    media: Mutex<HashSet<String>>,
}

type AppState = Arc<Shared>;

pub fn run(tree: PathBuf, port: u16, open_browser: bool) -> std::io::Result<()> {
    // Not canonicalized: on Windows that gives `\\?\` paths the browser can't use.
    let tree = std::path::absolute(&tree)?;
    if WebApp::get("index.html").is_none() {
        eprintln!("warning: the browser app isn't built; run `trunk build --release` and rebuild Genie");
    }
    let state = Arc::new(Shared { tree, port, media: Mutex::new(HashSet::new()) });
    let app = Router::new()
        .route("/api/tree", get(get_tree).put(put_tree))
        .route("/api/files", get(get_files))
        .route("/media/{*path}", get(get_media))
        .fallback(get(static_file))
        .layer(middleware::from_fn_with_state(state.clone(), local_only))
        .with_state(state.clone());

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(async move {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let url = format!("http://localhost:{port}/");
        println!("Genie is serving {} at {url}", state.tree.display());
        println!("Press Ctrl+C to stop.");
        if open_browser {
            let _ = open::that_detached(&url);
        }
        axum::serve(listener, app).await
    })
}

/// Turns away requests addressed to any other host name, so a web page
/// can't reach the server through DNS rebinding.
async fn local_only(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or("");
    let port = state.port;
    let allowed = [format!("localhost:{port}"), format!("127.0.0.1:{port}")];
    if !allowed.iter().any(|a| a == host) {
        return (StatusCode::FORBIDDEN, "Genie only answers requests to localhost.").into_response();
    }
    next.run(req).await
}

fn error(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, msg.into()).into_response()
}

/// The media files the tree refers to that exist on disk.
fn existing_media(tree: &Path, bytes: &[u8]) -> HashSet<String> {
    let (doc, _) = Document::from_bytes(bytes);
    doc.media_items()
        .iter()
        .filter_map(|m| media::resolve(Some(tree), &m.file))
        .filter(|p| p.is_file())
        .map(|p| path_string(&p))
        .collect()
}

/// The tree as saved, or an empty one when it doesn't exist yet.
async fn read_tree(state: &Shared) -> Result<Vec<u8>, Response> {
    match tokio::fs::read(&state.tree).await {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Document::new_empty().to_gedcom().into_bytes()),
        Err(e) => Err(error(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't read {}: {e}", state.tree.display()))),
    }
}

async fn get_tree(State(state): State<AppState>) -> Response {
    let bytes = match read_tree(&state).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&path_string(&state.tree)) {
        headers.insert("x-genie-path", v);
    }
    resp
}

async fn get_files(State(state): State<AppState>) -> Response {
    let bytes = match read_tree(&state).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let tree = state.tree.clone();
    let files = tokio::task::spawn_blocking(move || existing_media(&tree, &bytes)).await.unwrap_or_default();
    let list: Vec<&String> = files.iter().collect();
    let json = serde_json::to_vec(&list).unwrap_or_default();
    *state.media.lock().unwrap() = files;
    ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], json).into_response()
}

/// Saves the tree: to a temporary file first, so a failed write can't
/// leave it half written.
async fn put_tree(State(state): State<AppState>, body: Bytes) -> Response {
    let tree = state.tree.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<HashSet<String>> {
        let mut tmp = tree.clone().into_os_string();
        tmp.push(".saving");
        std::fs::write(&tmp, &body)?;
        std::fs::rename(&tmp, &tree)?;
        Ok(existing_media(&tree, &body))
    })
    .await;
    match result {
        Ok(Ok(files)) => {
            *state.media.lock().unwrap() = files;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(e)) => error(StatusCode::INTERNAL_SERVER_ERROR, format!("Couldn't save {}: {e}", state.tree.display())),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_media(State(state): State<AppState>, axum::extract::Path(path): axum::extract::Path<String>) -> Response {
    // `/media/home/me/a.jpg` or `/media/C:/Users/me/a.jpg`.
    let path = if path.as_bytes().get(1) == Some(&b':') { path } else { format!("/{path}") };
    if !state.media.lock().unwrap().contains(&path) {
        return error(StatusCode::NOT_FOUND, "Not a file in this tree.");
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.essence_str().to_string())], bytes).into_response()
        }
        Err(e) => error(StatusCode::NOT_FOUND, e.to_string()),
    }
}

async fn static_file(req: Request) -> Response {
    let path = req.uri().path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebApp::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            // Trunk puts a hash in every name but index.html's.
            let cache = if path == "index.html" { "no-cache" } else { "public, max-age=31536000, immutable" };
            ([(header::CONTENT_TYPE, mime.essence_str().to_string()), (header::CACHE_CONTROL, cache.to_string())], file.data.into_owned()).into_response()
        }
        None if path == "index.html" => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "The browser version of Genie hasn't been built.\n\nRun `trunk build --release` in the Genie source folder, then rebuild Genie.",
        )
            .into_response(),
        None => error(StatusCode::NOT_FOUND, "Not found."),
    }
}
