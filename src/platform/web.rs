//! The browser: the tree and its media come from the Genie server that
//! served the page (see `server.rs`).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::ServerEvent;
use crate::media::path_string;

thread_local! {
    /// Media files that exist on the server, as it reported them.
    static FILES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    static UNSAVED: Cell<bool> = const { Cell::new(false) };
    static UNLOAD_GUARD: Cell<bool> = const { Cell::new(false) };
}

fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

pub fn exists(p: &Path) -> bool {
    FILES.with_borrow(|f| f.contains(&path_string(p)))
}

/// The server's URL for one of its files, as `/media/<path>`.
pub fn image_uri(p: &Path) -> String {
    let path = path_string(p);
    let mut out = format!("{}/media/", origin());
    for b in path.trim_start_matches('/').bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Opens a file in a new browser tab; folders can't be shown.
pub fn open_path(ctx: &egui::Context, p: &Path) -> Result<(), String> {
    if !exists(p) {
        return Err("The browser can only open documents, not folders.".into());
    }
    ctx.open_url(egui::OpenUrl::new_tab(image_uri(p)));
    Ok(())
}

pub fn open_url(ctx: &egui::Context, url: &str) {
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

pub fn set_title(_ctx: &egui::Context, title: &str) {
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        doc.set_title(title);
    }
}

/// Asks the browser to warn before leaving the page with unsaved changes.
pub fn set_unsaved(unsaved: bool) {
    UNSAVED.set(unsaved);
    if UNLOAD_GUARD.replace(true) {
        return;
    }
    let Some(window) = web_sys::window() else { return };
    let guard = Closure::<dyn FnMut(web_sys::BeforeUnloadEvent)>::new(|e: web_sys::BeforeUnloadEvent| {
        if UNSAVED.get() {
            e.prevent_default();
            e.set_return_value("You have unsaved changes.");
        }
    });
    window.set_onbeforeunload(Some(guard.as_ref().unchecked_ref()));
    guard.forget();
}

// ---- dialogs: the browser can't pick files on the server's disk -------------------------------

pub fn pick_ged(_dir: Option<&Path>) -> Option<PathBuf> {
    None
}

pub fn save_ged(_dir: Option<&Path>, _name: &str) -> Option<PathBuf> {
    None
}

pub fn save_bundle(_dir: Option<&Path>, _name: &str) -> Option<PathBuf> {
    None
}

pub fn pick_media_files() -> Option<Vec<PathBuf>> {
    None
}

pub fn pick_file(_title: Option<&str>) -> Option<PathBuf> {
    None
}

pub fn pick_folder(_title: &str) -> Option<PathBuf> {
    None
}

// ---- the server ------------------------------------------------------------------------------

/// Requests to the server, answered on a channel polled each frame.
pub struct Server {
    tx: Sender<ServerEvent>,
    rx: Receiver<ServerEvent>,
}

impl Default for Server {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { tx, rx }
    }
}

fn failure(r: &ehttp::Result<ehttp::Response>) -> Option<String> {
    match r {
        Err(e) => Some(format!("Couldn't reach the Genie server: {e}")),
        Ok(resp) if !resp.ok => {
            let text = resp.text().unwrap_or_default().trim().to_string();
            Some(if text.is_empty() { format!("The server replied {} {}", resp.status, resp.status_text) } else { text })
        }
        Ok(_) => None,
    }
}

impl Server {
    /// Fetches which media files exist, then the tree itself.
    pub fn load(&mut self, ctx: &egui::Context) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let base = origin();
        ehttp::fetch(ehttp::Request::get(format!("{base}/api/files")), move |r| {
            if let Some(e) = failure(&r) {
                let _ = tx.send(ServerEvent::LoadFailed(e));
                ctx.request_repaint();
                return;
            }
            let files: Vec<String> = r.ok().and_then(|r| serde_json::from_slice(&r.bytes).ok()).unwrap_or_default();
            FILES.set(files.into_iter().collect());
            ehttp::fetch(ehttp::Request::get(format!("{base}/api/tree")), move |r| {
                let event = match failure(&r) {
                    Some(e) => ServerEvent::LoadFailed(e),
                    None => {
                        let r = r.expect("checked by failure()");
                        let path = r.headers.get("x-genie-path").unwrap_or("tree.ged").to_string();
                        ServerEvent::Loaded { path: PathBuf::from(path), bytes: r.bytes }
                    }
                };
                let _ = tx.send(event);
                ctx.request_repaint();
            });
        });
    }

    pub fn save(&mut self, ctx: &egui::Context, bytes: Vec<u8>) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        let mut req = ehttp::Request::put(format!("{}/api/tree", origin()), bytes);
        req.headers.insert("Content-Type", "text/plain; charset=utf-8");
        ehttp::fetch(req, move |r| {
            let _ = tx.send(ServerEvent::Saved(failure(&r).map_or(Ok(()), Err)));
            ctx.request_repaint();
        });
    }

    pub fn poll(&mut self) -> Option<ServerEvent> {
        self.rx.try_recv().ok()
    }
}
