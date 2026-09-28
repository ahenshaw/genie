//! Working on the shared tree kept by a Genie server (genie.henshaw.us).
//!
//! Signing in makes a working copy on this computer, one per server and
//! account, in Genie's data folder:
//!
//! ```text
//! servers/<host>/<username>/
//!     <host>.ged     the tree as edited here; opened like any other file
//!     base.ged       the server's tree as of the last sync
//!     sync.json      server, sign-in token, base revision, documents' hashes
//!     …              documents, at the paths the tree gives them
//! ```
//!
//! Editing works offline: the working copy is an ordinary tree file. Sync
//! sends what changed since `base.ged` along with the revision it was based
//! on, and the server merges in everyone else's changes; records changed on
//! both sides come back as conflicts to settle. Documents are then brought
//! up to date both ways, by SHA-256.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde::{Deserialize, Serialize};

use crate::model::Document;

pub const DEFAULT_SERVER: &str = "https://genie.henshaw.us";
const STATE_FILE: &str = "sync.json";
const BASE_FILE: &str = "base.ged";
/// How often to ask the server whether others have changed the tree.
pub const POLL_SECONDS: f64 = 60.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Guest,
    Family,
    Editor,
    Admin,
}

impl Role {
    pub const ALL: [Role; 4] = [Role::Admin, Role::Editor, Role::Family, Role::Guest];

    pub fn can_edit(self) -> bool {
        self >= Role::Editor
    }

    pub fn label(self) -> &'static str {
        match self {
            Role::Guest => "Guest",
            Role::Family => "Family",
            Role::Editor => "Editor",
            Role::Admin => "Administrator",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Role::Guest => "sees people who have died; living people appear as Private",
            Role::Family => "sees everyone; can't change anything",
            Role::Editor => "sees and edits everything",
            Role::Admin => "edits, and manages accounts",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|r| format!("{r:?}").eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct User {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub role: Role,
}

impl User {
    pub fn shown_name(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
}

/// What `sync.json` holds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SyncState {
    pub server: String,
    pub token: String,
    pub expires_at: u64,
    pub user: User,
    /// The server revision `base.ged` is.
    pub base_revision: Option<i64>,
    /// Document path → SHA-256, as of the last sync.
    #[serde(default)]
    pub media: BTreeMap<String, String>,
}

/// One change in the tree's history.
#[derive(Clone, Debug, Deserialize)]
pub struct ChangeRow {
    pub action: String,
    pub label: String,
    pub user: Option<String>,
    /// UTC, `2026-09-28T14:12:00Z`.
    pub at: String,
}

impl ChangeRow {
    pub fn day(&self) -> String {
        crate::model::pretty_date(&self.at.get(..10).map(iso_to_gedcom).unwrap_or_default())
    }
}

fn iso_to_gedcom(d: &str) -> String {
    const M: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    let mut p = d.split('-');
    match (p.next(), p.next().and_then(|m| m.parse::<usize>().ok()), p.next()) {
        (Some(y), Some(m), Some(day)) if (1..=12).contains(&m) => format!("{} {} {y}", day.trim_start_matches('0'), M[m - 1]),
        _ => d.to_string(),
    }
}

/// A record both this computer and someone else changed.
#[derive(Clone, Debug, Deserialize)]
pub struct Conflict {
    pub xref: String,
    /// `both_changed`, `changed_and_deleted` or `dangling_link`.
    pub kind: String,
    pub label: String,
    pub changed_by: Vec<String>,
}

impl Conflict {
    pub fn describe(&self) -> String {
        let who = if self.changed_by.is_empty() { "someone else".to_string() } else { self.changed_by.join(" and ") };
        match self.kind.as_str() {
            "changed_and_deleted" => format!("Changed here and deleted by {who}, or the other way round"),
            "dangling_link" => format!("Linked here to someone {who} deleted"),
            _ => format!("Changed here and by {who}"),
        }
    }
}

/// An account, as the administrator's list shows it.
#[derive(Clone, Debug, Deserialize)]
pub struct Account {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub disabled: bool,
    pub last_login_at: Option<String>,
}

#[derive(Deserialize)]
struct Session {
    token: String,
    expires_at: u64,
    user: User,
}

#[derive(Deserialize)]
struct MediaFile {
    path: String,
    sha256: String,
}

// ---- HTTP -------------------------------------------------------------------------------------

#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
enum Payload<'a> {
    None,
    Json(serde_json::Value),
    File(&'a Path),
}

struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Reply {
    /// The server's `{"error": …}` message, or a general one.
    fn error(&self) -> String {
        serde_json::from_slice::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v.get("error")?.as_str().map(str::to_string))
            .unwrap_or_else(|| format!("The server answered {}.", self.status))
    }

    fn json<T: for<'de> Deserialize<'de>>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("Unexpected answer from the server: {e}"))
    }
}

/// Sign-in expired or was ended: the token is no good any more.
pub const SIGNED_OUT: &str = "Your sign-in has ended. Sign in again.";

#[derive(Clone)]
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
pub struct Client {
    pub server: String,
    token: String,
}

impl Client {
    pub fn new(server: &str, token: &str) -> Self {
        Self { server: server.trim_end_matches('/').to_string(), token: token.to_string() }
    }

    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    fn host(&self) -> String {
        host_of(&self.server)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn send(&self, method: &str, path: &str, payload: Payload, save_to: Option<&Path>) -> Result<Reply, String> {
        use std::time::Duration;
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(30 * 60)))
            .timeout_connect(Some(Duration::from_secs(15)))
            .build()
            .into();
        let url = format!("{}{path}", self.server);
        let auth = format!("Bearer {}", self.token);
        let unreachable = |e: ureq::Error| format!("Couldn't reach {}: {e}", self.host());
        let mut resp = match (method, payload) {
            ("GET", _) => agent.get(&url).header("Authorization", &auth).call(),
            (m, Payload::Json(v)) => {
                let body = v.to_string();
                let req = match m {
                    "POST" => agent.post(&url),
                    "PATCH" => agent.patch(&url),
                    _ => agent.put(&url),
                };
                req.header("Authorization", &auth).header("Content-Type", "application/json").send(body)
            }
            (m, Payload::File(p)) => {
                let file = std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let req = if m == "POST" { agent.post(&url) } else { agent.put(&url) };
                req.header("Authorization", &auth).header("Content-Type", "application/octet-stream").send(file)
            }
            (m, Payload::None) => {
                let req = if m == "POST" { agent.post(&url) } else { agent.put(&url) };
                req.header("Authorization", &auth).send_empty()
            }
        }
        .map_err(unreachable)?;
        let status = resp.status().as_u16();
        if status == 401 && path != "/api/login" {
            return Err(SIGNED_OUT.into());
        }
        if let (Some(dest), 200) = (save_to, status) {
            let tmp = dest.with_extension("part");
            let mut out = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::io::copy(&mut resp.body_mut().as_reader(), &mut out).map_err(|e| format!("Download interrupted: {e}"))?;
            out.sync_all().map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, dest).map_err(|e| e.to_string())?;
            return Ok(Reply { status, body: Vec::new() });
        }
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut resp.body_mut().as_reader(), &mut body).map_err(|e| format!("Couldn't read the answer: {e}"))?;
        Ok(Reply { status, body })
    }

    #[cfg(target_arch = "wasm32")]
    fn send(&self, _: &str, _: &str, _: Payload, _: Option<&Path>) -> Result<Reply, String> {
        Err("Signing in to a server is only available in the desktop app.".into())
    }

    fn ok(&self, method: &str, path: &str, payload: Payload) -> Result<Reply, String> {
        let r = self.send(method, path, payload, None)?;
        if (200..300).contains(&r.status) { Ok(r) } else { Err(r.error()) }
    }

    pub fn changes(&self, since: Option<i64>, xref: Option<&str>) -> Result<Vec<ChangeRow>, String> {
        let mut q = format!("/api/changes?since={}", since.unwrap_or(0));
        if let Some(x) = xref {
            q.push_str(&format!("&xref={}", url_escape(x)));
        }
        self.ok("GET", &q, Payload::None)?.json()
    }

    pub fn accounts(&self) -> Result<Vec<Account>, String> {
        self.ok("GET", "/api/users", Payload::None)?.json()
    }

    pub fn create_account(&self, username: &str, display_name: &str, password: &str, role: Role) -> Result<(), String> {
        self.ok("POST", "/api/users", Payload::Json(serde_json::json!({"username": username, "display_name": display_name, "password": password, "role": role})))
            .map(|_| ())
    }

    pub fn update_account(&self, id: i32, change: serde_json::Value) -> Result<(), String> {
        self.ok("PATCH", &format!("/api/users/{id}"), Payload::Json(change)).map(|_| ())
    }

    /// A new token for this account; its other sign-ins end.
    pub fn change_password(&self, current: &str, new: &str) -> Result<(String, u64), String> {
        let s: Session = self.ok("POST", "/api/password", Payload::Json(serde_json::json!({"current": current, "new": new})))?.json()?;
        Ok((s.token, s.expires_at))
    }

    /// Ends every sign-in of this account, everywhere.
    pub fn sign_out_everywhere(&self) -> Result<(), String> {
        self.ok("POST", "/api/logout", Payload::None).map(|_| ())
    }

    /// Replaces the server's tree, and adds its documents, from a `.gdz`.
    pub fn import(&self, bundle: &Path, replace: bool) -> Result<(), String> {
        let path = if replace { "/api/import?replace=true" } else { "/api/import" };
        self.ok("POST", path, Payload::File(bundle)).map(|_| ())
    }
}

fn url_escape(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn host_of(server: &str) -> String {
    let s = server.trim().trim_end_matches('/');
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    s.split('/').next().unwrap_or(s).to_string()
}

/// A server address as typed, with `https://` assumed.
pub fn normalize_server(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    if s.contains("://") { s.to_string() } else { format!("https://{s}") }
}

// ---- the working copy ------------------------------------------------------------------------------

/// Where working copies are kept (`GENIE_SERVERS_DIR` overrides it, for tests).
pub fn servers_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("GENIE_SERVERS_DIR") {
        return Some(PathBuf::from(d));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        eframe::storage_dir("genie").map(|d| d.join("servers"))
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

fn file_safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect()
}

fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)
}

/// A signed-in working copy.
pub struct Remote {
    pub state: SyncState,
    pub dir: PathBuf,
    pub tree: PathBuf,
    tx: Sender<Event>,
    rx: Receiver<Event>,
    /// What's running, for the status line.
    pub busy: Option<Busy>,
    /// Others' changes the server has that this copy doesn't.
    pub news: Vec<ChangeRow>,
    pub last_poll: Option<f64>,
    polling: bool,
    /// History per record, fetched as people are looked at.
    pub history: HashMap<String, Vec<ChangeRow>>,
    history_asked: HashMap<String, Option<i64>>,
    /// [`Remote::has_local_changes`], kept until either file changes.
    local_changes: std::cell::Cell<Option<(Stamp, bool)>>,
}

type Stamp = (Option<std::time::SystemTime>, Option<std::time::SystemTime>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Exchanging the tree: editing waits for this.
    Tree,
    /// Bringing documents up to date: editing carries on.
    Media,
    Other,
}

/// What a background request finished with.
pub enum Event {
    /// The tree was exchanged. `reload`: the working copy changed on disk.
    TreeSynced { base_revision: Option<i64>, reload: bool, merged: bool, sent: bool, renamed: usize },
    Conflicts(Vec<Conflict>),
    MediaSynced { media: BTreeMap<String, String>, downloaded: usize, uploaded: usize, missing: usize },
    News(Vec<ChangeRow>),
    History { xref: String, rows: Vec<ChangeRow> },
    Accounts(Vec<Account>),
    /// Something the account dialog asked for is done.
    Done(String),
    PasswordChanged { token: String, expires_at: u64 },
    Imported,
    Failed(String),
}

impl Remote {
    fn new(state: SyncState, dir: PathBuf) -> Self {
        let tree = dir.join(format!("{}.ged", file_safe(&host_of(&state.server))));
        let (tx, rx) = channel();
        Self {
            state,
            dir,
            tree,
            tx,
            rx,
            busy: None,
            news: Vec::new(),
            last_poll: None,
            polling: false,
            history: HashMap::new(),
            history_asked: HashMap::new(),
            local_changes: std::cell::Cell::new(None),
        }
    }

    /// The working copy `tree` belongs to, if it's one.
    pub fn open_for(tree: &Path) -> Option<Self> {
        let dir = tree.parent()?;
        let text = std::fs::read_to_string(dir.join(STATE_FILE)).ok()?;
        let state: SyncState = serde_json::from_str(&text).ok()?;
        let r = Self::new(state, dir.to_path_buf());
        (r.tree == tree).then_some(r)
    }

    pub fn client(&self) -> Client {
        Client::new(&self.state.server, &self.state.token)
    }

    pub fn host(&self) -> String {
        host_of(&self.state.server)
    }

    pub fn role(&self) -> Role {
        self.state.user.role
    }

    pub fn save_state(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.state).map_err(|e| e.to_string())?;
        write_private(&self.dir.join(STATE_FILE), &text).map_err(|e| e.to_string())
    }

    /// Whether the working copy differs from what the server had at the
    /// last sync. Cheap to ask every frame: the files are only compared
    /// again when one of them has changed.
    pub fn has_local_changes(&self) -> bool {
        let base_path = self.dir.join(BASE_FILE);
        let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let stamp = (modified(&self.tree), modified(&base_path));
        if let Some((s, answer)) = self.local_changes.get()
            && s == stamp
        {
            return answer;
        }
        let local = std::fs::read(&self.tree).ok();
        let answer = local.is_some() && local != std::fs::read(&base_path).ok();
        self.local_changes.set(Some((stamp, answer)));
        answer
    }

    pub fn poll(&mut self) -> Option<Event> {
        let e = self.rx.try_recv().ok()?;
        match &e {
            Event::TreeSynced { .. } => self.busy = None,
            Event::MediaSynced { .. } | Event::Conflicts(_) | Event::Failed(_) | Event::Imported | Event::Done(_) | Event::PasswordChanged { .. } | Event::Accounts(_) => {
                self.busy = None;
            }
            Event::News(_) => self.polling = false,
            _ => {}
        }
        Some(e)
    }

    fn spawn(&self, ctx: &egui::Context, job: impl FnOnce() -> Event + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(job());
            ctx.request_repaint();
        });
    }

    /// Exchanges the tree with the server, then its documents. The working
    /// copy must already be saved to disk.
    pub fn sync(&mut self, ctx: &egui::Context, resolve: HashMap<String, &'static str>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(Busy::Tree);
        self.news.clear();
        let (client, dir, tree, base, role) = (self.client(), self.dir.clone(), self.tree.clone(), self.state.base_revision, self.role());
        self.spawn(ctx, move || match sync_tree(&client, &dir, &tree, base, role, &resolve) {
            Ok(e) => e,
            Err(e) => Event::Failed(e),
        });
    }

    /// After the tree: documents both ways.
    pub fn sync_media(&mut self, ctx: &egui::Context) {
        self.busy = Some(Busy::Media);
        let (client, dir, tree, role, known) = (self.client(), self.dir.clone(), self.tree.clone(), self.role(), self.state.media.clone());
        self.spawn(ctx, move || match sync_media(&client, &dir, &tree, role, known) {
            Ok(e) => e,
            Err(e) => Event::Failed(e),
        });
    }

    /// Asks, every so often, whether others have changed the tree.
    pub fn maybe_poll(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.polling || self.busy.is_some() || self.last_poll.is_some_and(|t| now - t < POLL_SECONDS) {
            if let Some(t) = self.last_poll {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64((POLL_SECONDS - (now - t)).max(1.0)));
            }
            return;
        }
        self.last_poll = Some(now);
        self.polling = true;
        // Everything after the base revision is someone else's: our own
        // syncs move the base forward.
        let (client, since) = (self.client(), self.state.base_revision);
        self.spawn(ctx, move || match client.changes(since, None) {
            Ok(rows) => Event::News(rows),
            Err(e) if e == SIGNED_OUT => Event::Failed(e),
            Err(_) => Event::News(Vec::new()),
        });
    }

    /// Fetches the history of `xref`, once per base revision.
    pub fn want_history(&mut self, ctx: &egui::Context, xref: &str) {
        if self.history_asked.get(xref) == Some(&self.state.base_revision) {
            return;
        }
        self.history_asked.insert(xref.to_string(), self.state.base_revision);
        let (client, x) = (self.client(), xref.to_string());
        self.spawn(ctx, move || match client.changes(None, Some(&x)) {
            Ok(rows) => Event::History { xref: x, rows },
            Err(e) if e == SIGNED_OUT => Event::Failed(e),
            Err(_) => Event::History { xref: x, rows: Vec::new() },
        });
    }

    pub fn forget_history(&mut self) {
        self.history_asked.clear();
    }

    /// Runs an account request in the background.
    pub fn request(&mut self, ctx: &egui::Context, job: impl FnOnce(&Client) -> Event + Send + 'static) {
        self.busy = Some(Busy::Other);
        let client = self.client();
        self.spawn(ctx, move || job(&client));
    }
}

/// Signs in, and makes (or reopens) the working copy for this server and account.
pub fn sign_in(server: &str, username: &str, password: &str) -> Result<Remote, String> {
    let server = normalize_server(server);
    let anon = Client::new(&server, "");
    let r = anon.send("POST", "/api/login", Payload::Json(serde_json::json!({"username": username.trim(), "password": password})), None)?;
    if r.status != 200 {
        return Err(r.error());
    }
    let s: Session = r.json()?;
    let root = servers_dir().ok_or("There's nowhere to keep a working copy on this computer.")?;
    let dir = root.join(file_safe(&host_of(&server))).join(file_safe(&s.user.username));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // Keep what an earlier sign-in synced, if it's the same account.
    let previous: Option<SyncState> = std::fs::read_to_string(dir.join(STATE_FILE)).ok().and_then(|t| serde_json::from_str(&t).ok());
    let (base_revision, media) = match previous {
        Some(p) if p.user.id == s.user.id => (p.base_revision, p.media),
        _ => (None, BTreeMap::new()),
    };
    let remote = Remote::new(SyncState { server, token: s.token, expires_at: s.expires_at, user: s.user, base_revision, media }, dir);
    remote.save_state()?;
    Ok(remote)
}

#[derive(Deserialize)]
struct TreeReply {
    revision: Option<i64>,
    gedcom: String,
}

#[derive(Deserialize)]
struct SaveReply {
    revision: i64,
    merged: bool,
    #[serde(default)]
    renamed: Vec<(String, String)>,
    gedcom: Option<String>,
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("ged.tmp");
    std::fs::write(&tmp, text).and_then(|_| std::fs::rename(&tmp, path)).map_err(|e| format!("{}: {e}", path.display()))
}

/// The tree half of a sync (see the module docs).
fn sync_tree(client: &Client, dir: &Path, tree: &Path, base_revision: Option<i64>, role: Role, resolve: &HashMap<String, &'static str>) -> Result<Event, String> {
    let base_path = dir.join(BASE_FILE);
    let local = std::fs::read_to_string(tree).ok();
    let base = std::fs::read_to_string(&base_path).ok();
    let changed_here = local.is_some() && local != base && role.can_edit();

    if let (true, Some(text)) = (changed_here, &local) {
        let body = serde_json::json!({ "base_revision": base_revision, "gedcom": text, "resolve": resolve });
        let r = client.send("POST", "/api/tree", Payload::Json(body), None)?;
        if r.status == 409 {
            let v: serde_json::Value = r.json()?;
            if let Some(list) = v.get("conflicts") {
                let conflicts: Vec<Conflict> = serde_json::from_value(list.clone()).map_err(|e| e.to_string())?;
                return Ok(Event::Conflicts(conflicts));
            }
            return Err(r.error());
        }
        if r.status != 200 {
            return Err(r.error());
        }
        let saved: SaveReply = r.json()?;
        let result = saved.gedcom.as_deref().unwrap_or(text);
        write(&base_path, result)?;
        if saved.gedcom.is_some() {
            write(tree, result)?;
        }
        return Ok(Event::TreeSynced { base_revision: Some(saved.revision), reload: saved.gedcom.is_some(), merged: saved.merged, sent: true, renamed: saved.renamed.len() });
    }

    let r = client.send("GET", "/api/tree", Payload::None, None)?;
    if r.status != 200 {
        return Err(r.error());
    }
    let t: TreeReply = r.json()?;
    let unchanged = t.revision.is_some() && t.revision == base_revision && local.is_some() && local == base;
    if !unchanged {
        write(&base_path, &t.gedcom)?;
        write(tree, &t.gedcom)?;
    }
    Ok(Event::TreeSynced { base_revision: t.revision, reload: !unchanged, merged: false, sent: false, renamed: 0 })
}

fn sha256_of(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The document half of a sync: download what's new or missing here, upload
/// (editors) what changed here or the server lacks.
fn sync_media(client: &Client, dir: &Path, tree: &Path, role: Role, mut known: BTreeMap<String, String>) -> Result<Event, String> {
    let manifest: Vec<MediaFile> = client.ok("GET", "/api/media", Payload::None)?.json()?;
    let (mut downloaded, mut uploaded, mut missing) = (0, 0, 0);
    let mut on_server = std::collections::HashSet::new();
    for m in &manifest {
        // Never write outside the working copy, whatever the server says.
        let Some(rel) = crate::bundle::safe_relative(&m.path) else { continue };
        on_server.insert(rel.clone());
        let local = dir.join(&rel);
        let here = sha256_of(&local);
        if here.as_deref() == Some(m.sha256.as_str()) {
            known.insert(rel, m.sha256.clone());
            continue;
        }
        let changed_here = here.is_some() && here.as_deref() != known.get(&rel).map(String::as_str);
        if changed_here && role.can_edit() {
            client.ok("PUT", &format!("/api/media?path={}", url_escape(&rel)), Payload::File(&local))?;
            known.insert(rel, here.unwrap_or_default());
            uploaded += 1;
        } else {
            if let Some(parent) = local.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let r = client.send("GET", &format!("/api/media/{}", m.sha256), Payload::None, Some(&local))?;
            if r.status != 200 {
                return Err(r.error());
            }
            known.insert(rel, m.sha256.clone());
            downloaded += 1;
        }
    }
    // Documents this copy links to that the server doesn't have yet.
    if role.can_edit() {
        let (doc, _) = Document::from_bytes(&std::fs::read(tree).map_err(|e| e.to_string())?);
        for item in doc.media_items().iter().filter(|m| m.has_file()) {
            let Some(rel) = crate::bundle::safe_relative(&item.file) else { continue };
            if on_server.contains(&rel) {
                continue;
            }
            let local = dir.join(&rel);
            match sha256_of(&local) {
                Some(sha) => {
                    client.ok("PUT", &format!("/api/media?path={}", url_escape(&rel)), Payload::File(&local))?;
                    known.insert(rel.clone(), sha);
                    on_server.insert(rel);
                    uploaded += 1;
                }
                None => missing += 1,
            }
        }
    }
    Ok(Event::MediaSynced { media: known, downloaded, uploaded, missing })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_servers() {
        assert_eq!(host_of("https://genie.henshaw.us/"), "genie.henshaw.us");
        assert_eq!(host_of("http://127.0.0.1:3100"), "127.0.0.1:3100");
        assert_eq!(normalize_server("genie.henshaw.us"), "https://genie.henshaw.us");
        assert_eq!(normalize_server(" http://localhost:3100/ "), "http://localhost:3100");
        assert_eq!(file_safe("127.0.0.1:3100"), "127.0.0.1_3100");
    }

    #[test]
    fn roles_parse_and_rank() {
        assert_eq!(Role::parse("editor"), Some(Role::Editor));
        assert!(Role::Editor.can_edit() && !Role::Family.can_edit());
        let u: User = serde_json::from_str(r#"{"id":1,"username":"ann","display_name":"","role":"family"}"#).unwrap();
        assert_eq!((u.role, u.shown_name()), (Role::Family, "ann"));
    }

    /// Several people syncing through a real server. Set GENIE_TEST_SERVER
    /// (e.g. http://127.0.0.1:13101) and GENIE_TEST_ADMIN_PASSWORD for an
    /// administrator called `admin` there; the tree on it is replaced.
    #[test]
    fn people_sync_through_a_real_server() {
        let (Ok(server), Ok(admin_pw)) = (std::env::var("GENIE_TEST_SERVER"), std::env::var("GENIE_TEST_ADMIN_PASSWORD")) else {
            eprintln!("GENIE_TEST_SERVER not set; skipping");
            return;
        };
        let scratch = std::env::temp_dir().join(format!("genie-remote-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        // SAFETY: set before any other thread reads the environment.
        unsafe { std::env::set_var("GENIE_SERVERS_DIR", scratch.join("servers")) };
        let pw = "test-password-1";

        let admin = sign_in(&server, "admin", &admin_pw).expect("admin signs in");
        for (u, n, role) in [("alice", "Alice", Role::Editor), ("bob", "Bob", Role::Editor), ("gus", "Gus", Role::Guest)] {
            let _ = admin.client().create_account(u, n, pw, role);
        }

        // Seed: the sample, with one document, as a bundle.
        let src = scratch.join("src");
        std::fs::create_dir_all(src.join("Family media")).unwrap();
        std::fs::write(src.join("Family media/thomas.jpg"), b"thomas").unwrap();
        let text = String::from_utf8(genie_core::SAMPLE_GED.to_vec()).unwrap()
            .replace("0 @I1@ INDI\n", "0 @I1@ INDI\n1 OBJE @M1@\n")
            .replace("0 TRLR", "0 @M1@ OBJE\n1 FILE Family media/thomas.jpg\n0 TRLR");
        let (mut doc, _) = Document::from_bytes(text.as_bytes());
        doc.path = Some(src.join("Family.ged"));
        crate::bundle::export(&doc, &src.join("Family.gdz")).unwrap();
        admin.client().import(&src.join("Family.gdz"), true).expect("import");

        // Runs a whole sync the way the app does, returning what the tree step said.
        fn sync(r: &mut Remote, resolve: &[(&str, &'static str)]) -> Event {
            let resolve: HashMap<String, &'static str> = resolve.iter().map(|(x, s)| (x.to_string(), *s)).collect();
            let e = sync_tree(&r.client(), &r.dir, &r.tree, r.state.base_revision, r.role(), &resolve).expect("tree syncs");
            if let Event::TreeSynced { base_revision, .. } = &e {
                r.state.base_revision = *base_revision;
                match sync_media(&r.client(), &r.dir, &r.tree, r.role(), r.state.media.clone()).expect("media syncs") {
                    Event::MediaSynced { media, .. } => r.state.media = media,
                    _ => unreachable!(),
                }
            }
            e
        }
        let edit = |r: &Remote, from: &str, to: &str| {
            let t = std::fs::read_to_string(&r.tree).unwrap();
            assert!(t.contains(from), "{from} not in {}", r.state.user.username);
            std::fs::write(&r.tree, t.replacen(from, to, 1)).unwrap();
        };
        let has = |r: &Remote, s: &str| std::fs::read_to_string(&r.tree).unwrap().contains(s);

        let mut alice = sign_in(&server, "alice", pw).unwrap();
        let mut bob = sign_in(&server, "bob", pw).unwrap();
        assert!(matches!(sync(&mut alice, &[]), Event::TreeSynced { reload: true, .. }));
        assert!(has(&alice, "Thomas /Hartwell/"));
        assert_eq!(std::fs::read(alice.dir.join("Family media/thomas.jpg")).unwrap(), b"thomas");
        sync(&mut bob, &[]);

        // Different people: merged.
        edit(&alice, "NAME Thomas /Hartwell/", "NAME Tom /Hartwell/");
        assert!(matches!(sync(&mut alice, &[]), Event::TreeSynced { sent: true, merged: false, .. }));
        edit(&bob, "NAME Margaret /Doyle/", "NAME Maggie /Doyle/");
        assert!(matches!(sync(&mut bob, &[]), Event::TreeSynced { sent: true, merged: true, reload: true, .. }));
        assert!(has(&bob, "Tom /Hartwell/") && has(&bob, "Maggie /Doyle/"));
        assert!(!alice.has_local_changes() && !bob.has_local_changes());

        // The same person: a conflict naming Alice, settled by keeping Bob's.
        edit(&alice, "NAME Tom /Hartwell/", "NAME Thos /Hartwell/");
        sync(&mut alice, &[]);
        edit(&bob, "NAME Tom /Hartwell/", "NAME Tommy /Hartwell/");
        match sync(&mut bob, &[]) {
            Event::Conflicts(c) => {
                assert_eq!(c[0].xref, "I1");
                assert_eq!(c[0].changed_by, ["Alice"]);
            }
            _ => panic!("expected a conflict"),
        }
        sync(&mut bob, &[("I1", "mine")]);
        sync(&mut alice, &[]);
        assert!(has(&alice, "Tommy /Hartwell/"));

        // A new document from Alice reaches Bob.
        std::fs::write(alice.dir.join("Family media/new.jpg"), b"new").unwrap();
        edit(&alice, "0 TRLR", "0 @M2@ OBJE\r\n1 FILE Family media/new.jpg\r\n0 TRLR");
        sync(&mut alice, &[]);
        sync(&mut bob, &[]);
        assert_eq!(std::fs::read(bob.dir.join("Family media/new.jpg")).unwrap(), b"new");

        // Who changed Thomas, newest first.
        let hist = bob.client().changes(None, Some("I1")).unwrap();
        assert_eq!(hist.iter().take(3).map(|h| h.user.clone().unwrap()).collect::<Vec<_>>(), ["Bob", "Alice", "Alice"]);

        // A guest's copy has no living people, and no photos of them.
        let mut gus = sign_in(&server, "gus", pw).unwrap();
        sync(&mut gus, &[]);
        assert!(!has(&gus, "Ferris") && has(&gus, "Tommy /Hartwell/") && has(&gus, "NAME Private"));
        // A guest's local edit is never sent.
        edit(&gus, "NAME Tommy /Hartwell/", "NAME Guest /Edit/");
        assert!(matches!(sync(&mut gus, &[]), Event::TreeSynced { sent: false, .. }));
        assert!(has(&gus, "Tommy /Hartwell/"));

        let _ = std::fs::remove_dir_all(&scratch);
    }

    #[test]
    fn change_dates_read_nicely() {
        let row = ChangeRow { action: "modify".into(), label: String::new(), user: None, at: "2026-10-03T14:00:00Z".into() };
        assert_eq!(row.day(), "3 Oct 2026");
    }
}
