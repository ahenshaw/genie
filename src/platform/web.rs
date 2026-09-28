//! The browser: the tree and its media come from the Genie server that
//! served the page (see `server.rs`).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use super::ServerEvent;
use crate::media::path_string;

thread_local! {
    /// Media files that exist on the server, as it reported them.
    static FILES: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// On a Genie server with accounts: document path → SHA-256, for the
    /// documents this account may see (see [`set_hosted_media`]).
    static HOSTED: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    static UNSAVED: Cell<bool> = const { Cell::new(false) };
    static UNLOAD_GUARD: Cell<bool> = const { Cell::new(false) };
}

pub fn origin() -> String {
    web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default()
}

pub fn exists(p: &Path) -> bool {
    FILES.with_borrow(|f| f.contains(&path_string(p)))
}

/// The shared tree's documents, as (path, sha256). Their paths are made
/// absolute under `/`, where the tree is taken to live.
pub fn set_hosted_media(files: Vec<(String, String)>) {
    FILES.set(files.iter().map(|(p, _)| format!("/{p}")).collect());
    HOSTED.set(files.into_iter().collect());
}

fn hosted_sha(p: &Path) -> Option<String> {
    let path = path_string(p);
    HOSTED.with_borrow(|h| h.get(path.trim_start_matches('/')).cloned())
}

/// Where to get one of the server's files: a Genie server's documents by
/// hash (through [`MediaLoader`], which sends the sign-in cookie), or
/// `genie --serve`'s as `/media/<path>`.
pub fn image_uri(p: &Path) -> String {
    if let Some(sha) = hosted_sha(p) {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return format!("{MEDIA_SCHEME}{sha}/{name}");
    }
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
    let url = match hosted_sha(p) {
        Some(sha) => format!("{}/api/media/{sha}", origin()),
        None => image_uri(p),
    };
    ctx.open_url(egui::OpenUrl::new_tab(url));
    Ok(())
}

const MEDIA_SCHEME: &str = "genie-media://";

/// Loads a Genie server's documents with the sign-in cookie, which egui's
/// own HTTP loader doesn't send.
#[derive(Default)]
struct MediaLoader {
    cache: std::sync::Arc<std::sync::Mutex<HashMap<String, Option<Result<(egui::load::Bytes, Option<String>), String>>>>>,
}

impl egui::load::BytesLoader for MediaLoader {
    fn id(&self) -> &str {
        egui::generate_loader_id!(MediaLoader)
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> egui::load::BytesLoadResult {
        use egui::load::{BytesPoll, LoadError};
        let Some(rest) = uri.strip_prefix(MEDIA_SCHEME) else { return Err(LoadError::NotSupported) };
        let sha = rest.split('/').next().unwrap_or_default().to_string();
        let mut cache = self.cache.lock().unwrap();
        match cache.get(uri) {
            Some(Some(Ok((bytes, mime)))) => return Ok(BytesPoll::Ready { size: None, bytes: bytes.clone(), mime: mime.clone() }),
            Some(Some(Err(e))) => return Err(LoadError::Loading(e.clone())),
            Some(None) => return Ok(BytesPoll::Pending { size: None }),
            None => {}
        }
        cache.insert(uri.to_string(), None);
        let (cache, uri, ctx) = (self.cache.clone(), uri.to_string(), ctx.clone());
        let mut req = ehttp::Request::get(format!("{}/api/media/{sha}", origin()));
        req.credentials = ehttp::Credentials::SameOrigin;
        ehttp::fetch(req, move |r| {
            let result = match r {
                Ok(resp) if resp.ok => {
                    let mime = resp.content_type().map(str::to_string);
                    Ok((egui::load::Bytes::Shared(resp.bytes.into()), mime))
                }
                Ok(resp) => Err(format!("{} {}", resp.status, resp.status_text)),
                Err(e) => Err(e),
            };
            cache.lock().unwrap().insert(uri, Some(result));
            ctx.request_repaint();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.cache.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().unwrap().clear();
    }

    fn byte_size(&self) -> usize {
        self.cache.lock().unwrap().values().map(|v| match v {
            Some(Ok((b, _))) => b.len(),
            _ => 0,
        }).sum()
    }
}

thread_local! {
    /// The last frame had the pointer over a text field.
    static OVER_TEXT: Cell<bool> = const { Cell::new(false) };
}

/// Notes, at the end of each frame, whether the pointer is over a text
/// field (egui gives it the text cursor), for [`prepare_touch_keyboard`].
pub fn end_frame(ctx: &egui::Context) {
    OVER_TEXT.set(ctx.output(|o| o.cursor_icon == egui::CursorIcon::Text));
}

/// Makes the on-screen keyboard of an iPad or phone work with the app.
///
/// egui types through a hidden `<input>` that eframe puts beside the canvas.
/// iOS only opens the keyboard when that input is focused *during* a tap,
/// but egui gives a text field focus in the frame after the tap ends, so on
/// its own the keyboard needs a second tap. A tap starts with `touchstart`,
/// which egui draws a frame for, hovering the field under the finger; so if
/// that frame had the pointer over a text field, the input is focused here,
/// as the finger lifts, before eframe's own handler.
///
/// Also turns off autocorrect and the like on the input: they rewrite what
/// is typed without showing it, which silently spoils passwords.
pub fn prepare_touch_keyboard(canvas_id: &str) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(Some(input)) = document.query_selector(&format!("#{canvas_id} + input")) else { return };
    for (name, value) in [("autocorrect", "off"), ("autocapitalize", "off"), ("autocomplete", "off"), ("spellcheck", "false")] {
        let _ = input.set_attribute(name, value);
    }
    let Ok(input) = input.dyn_into::<web_sys::HtmlElement>() else { return };
    let focus = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| {
        if OVER_TEXT.get() {
            let _ = input.focus();
        }
    });
    // Capture phase: before eframe's handler, which looks at the focus.
    let _ = document.add_event_listener_with_callback_and_bool("touchend", focus.as_ref().unchecked_ref(), true);
    focus.forget();
}

pub fn install_loaders(ctx: &egui::Context) {
    ctx.add_bytes_loader(std::sync::Arc::new(MediaLoader::default()));
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
