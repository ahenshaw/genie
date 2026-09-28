//! Application shell: state, menus, file handling, dialogs, and the editor.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use egui::{Align, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, Ui};
use elegance::{
    Accent, BadgeTone, BuiltInTheme, Button, ButtonSize, Drawer, DrawerSide, FileDropZone, MenuBar, MenuItem,
    MenuSection, Modal, SegmentedControl, Select, SegmentedSize, SubMenuItem, Switch, TabBar, TextArea, TextInput, Theme,
    Toast, Toasts, glyphs,
};

use crate::graph::{FamilyGraph, LinkMode};
use crate::model::{self, CitationForm, CiteSource, Document, FactKey, PersonForm, Relation, Sex};
use crate::mediaview::MediaUi;
use crate::platform::{self, ServerEvent};
use crate::relation::RelationView;
use crate::remote::{self, Busy, Remote};
use crate::remoteview::{self, RemoteUi, Request};
use crate::reports::ReportsView;
use crate::tools::ToolsUi;
use crate::tree::TreeView;
use crate::views;
use crate::widgets::{self, section_label};

const SAMPLE: &[u8] = genie_core::SAMPLE_GED;
const MAX_RECENT: usize = 10;
pub const TABS: [&str; 8] = ["Profile", "Tree", "Relationship", "Graph", "Media", "Reports", "Overview", "GEDCOM"];
pub const TAB_PROFILE: usize = 0;
pub const TAB_TREE: usize = 1;
pub const TAB_RELATION: usize = 2;
pub const TAB_GRAPH: usize = 3;
pub const TAB_MEDIA: usize = 4;
pub const TAB_REPORTS: usize = 5;
pub const TAB_OVERVIEW: usize = 6;

/// Something a view asks the app to do once the frame's UI is drawn.
#[derive(Clone, Debug)]
pub enum Action {
    Select(String),
    Edit(String),
    /// Open the editor on a person with a new, empty citation ready.
    AddCitation(String),
    NewPerson,
    AddRelative(String, Relation),
    Delete(String),
    Unlink { fam: String, person: String },
    SetTab(usize),
    /// Switch to the Tree section showing ancestors and descendants together.
    OpenTreeBoth,
    /// Show how `subject` is related to `reference`.
    OpenRelationship { subject: String, reference: String },
    SetHome(Option<String>),
    /// Select the home person.
    GoHome,
    ViewMedia { media: String, person: Option<String> },
    EditMedia(String),
    /// Choose files to add, attached to a person or (with `None`) not yet.
    PickMedia(Option<String>),
    AddMedia { person: Option<String>, files: Vec<PathBuf> },
    SetPrimaryPhoto { person: String, media: Option<String> },
    LocateMedia(String),
    /// Choose a file for a document, copied into the media folder.
    ReplaceMediaFile(String),
    /// Search a folder for files for documents that are missing one.
    FindMissingMedia,
    FindDuplicatePeople,
    FindDuplicateSources,
    WikiTreePhotos,
    /// A merge removed a person; move the selection to who they became.
    MergedPerson { kept: String, removed: String },
    Open(Option<PathBuf>),
    New,
    Sample,
    Save,
    SaveAs,
    /// Write the tree and its documents to a `.gdz` bundle.
    ExportBundle,
    /// Sign in to a Genie server's shared tree.
    Connect,
    /// Exchange changes with the server.
    Sync,
    /// End this account's sign-ins everywhere and close the shared tree.
    SignOut,
    ChangePassword,
    Accounts,
    /// Put a tree on the server (an administrator, when it has none).
    UploadTree,
    Close,
    Undo,
    Redo,
    Back,
    Forward,
    ForgetRecent(PathBuf),
}

impl Action {
    /// Whether this changes the tree, which read-only accounts can't.
    fn edits(&self) -> bool {
        matches!(
            self,
            Action::Edit(_)
                | Action::AddCitation(_)
                | Action::NewPerson
                | Action::AddRelative(..)
                | Action::Delete(_)
                | Action::Unlink { .. }
                | Action::EditMedia(_)
                | Action::PickMedia(_)
                | Action::AddMedia { .. }
                | Action::SetPrimaryPhoto { .. }
                | Action::LocateMedia(_)
                | Action::ReplaceMediaFile(_)
                | Action::FindMissingMedia
                | Action::FindDuplicatePeople
                | Action::FindDuplicateSources
                | Action::WikiTreePhotos
                | Action::Undo
                | Action::Redo
        )
    }
}

#[derive(Clone, Debug)]
enum Pending {
    Open(Option<PathBuf>),
    Connect,
    New,
    Sample,
    Close,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
enum EditMode {
    Existing(String),
    New(Option<(String, Relation)>),
}

struct Editor {
    open: bool,
    mode: EditMode,
    form: PersonForm,
    original: PersonForm,
    /// 0 = create a new person, 1 = link someone already in the tree.
    source: usize,
    pick_search: String,
    picked: Option<String>,
    focus_first: bool,
    error: Option<String>,
    /// Known places, most used first, for suggestions.
    places: Vec<(String, usize)>,
    /// Source records to cite, as (xref, title).
    sources: Vec<(String, String)>,
    /// Facts a citation can support.
    facts: Vec<(FactKey, String)>,
    scroll_to_sources: bool,
}

pub struct GenieApp {
    doc: Option<Document>,
    theme: BuiltInTheme,
    recent: Vec<PathBuf>,
    pub selected: Option<String>,
    history: Vec<String>,
    history_pos: usize,
    tab: usize,
    pub search: String,
    pub sex_filter: usize,
    focus_search: bool,
    tree: TreeView,
    graph: FamilyGraph,
    relation: RelationView,
    reports: ReportsView,
    media_ui: MediaUi,
    tools: ToolsUi,
    /// Pairs marked "not duplicates", per file, remembered between runs.
    not_dupes: HashMap<String, HashSet<String>>,
    /// The person relationships are described from, for this file.
    home: Option<String>,
    /// Home person per file, remembered between runs.
    homes: HashMap<String, String>,
    editor: Option<Editor>,
    confirm_delete: Option<String>,
    pending: Option<Pending>,
    unsaved_open: bool,
    about_open: bool,
    shortcuts_open: bool,
    load_notes: Vec<String>,
    allow_close: bool,
    title: String,
    actions: Vec<Action>,
    /// In the browser: where the tree comes from and is saved to.
    server: platform::Server,
    /// Waiting for the server to send the tree, or why it couldn't.
    server_load: Option<Result<(), String>>,
    /// The revision being saved to the server.
    saving: Option<u64>,
    /// The shared tree on a Genie server this document is a working copy of.
    remote: Option<Remote>,
    remote_ui: RemoteUi,
    remote_requests: Vec<Request>,
    /// Filled in on the sign-in form next time.
    last_server: String,
    last_username: String,
    /// What was open before signing in, offered for an empty server.
    before_connect: Option<PathBuf>,
}

impl GenieApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let mut last_open = None;
        let mut app = Self {
            doc: None,
            theme: BuiltInTheme::Slate,
            recent: Vec::new(),
            selected: None,
            history: Vec::new(),
            history_pos: 0,
            tab: TAB_PROFILE,
            search: String::new(),
            sex_filter: 0,
            focus_search: false,
            tree: TreeView::default(),
            graph: FamilyGraph::new(),
            relation: RelationView::default(),
            reports: ReportsView::default(),
            media_ui: MediaUi::default(),
            tools: ToolsUi::default(),
            not_dupes: HashMap::new(),
            home: None,
            homes: HashMap::new(),
            editor: None,
            confirm_delete: None,
            pending: None,
            unsaved_open: false,
            about_open: false,
            shortcuts_open: false,
            load_notes: Vec::new(),
            allow_close: false,
            title: String::new(),
            actions: Vec::new(),
            server: platform::Server::default(),
            server_load: None,
            saving: None,
            remote: None,
            remote_ui: RemoteUi::default(),
            remote_requests: Vec::new(),
            last_server: String::new(),
            last_username: String::new(),
            before_connect: None,
        };
        if let Some(storage) = cc.storage {
            if let Some(t) = storage.get_string("theme")
                && let Some(found) = BuiltInTheme::all().into_iter().find(|b| b.label() == t) {
                    app.theme = found;
                }
            if storage.get_string("graph_links").as_deref() == Some(LinkMode::Direct.label()) {
                app.graph.set_mode(LinkMode::Direct);
            }
            if let Some(n) = storage.get_string("not_dupes") {
                for (path, pair) in n.lines().filter_map(|l| l.split_once('\t')) {
                    app.not_dupes.entry(path.to_string()).or_default().insert(pair.to_string());
                }
            }
            if let Some(h) = storage.get_string("homes") {
                app.homes = h.lines().filter_map(|l| l.split_once('\t')).map(|(p, x)| (p.to_string(), x.to_string())).collect();
            }
            if let Some(r) = storage.get_string("recent") {
                app.recent = r.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
            }
            last_open = storage.get_string("last_open").filter(|p| !p.is_empty()).map(PathBuf::from);
            app.last_server = storage.get_string("last_server").unwrap_or_default();
            app.last_username = storage.get_string("last_username").unwrap_or_default();
        }
        app.theme.theme().install(&cc.egui_ctx);
        crate::fonts::install(&cc.egui_ctx);
        egui_extras::install_image_loaders(&cc.egui_ctx);
        // Reopen whatever file was open when Genie last quit, unless one was
        // named on the command line or the file has since gone away.
        if platform::WEB {
            app.load_from_server(&cc.egui_ctx);
        } else if let Some(path) = open.or(last_open.filter(|p| p.is_file())) {
            app.load_path(&cc.egui_ctx, &path);
        }
        app
    }

    // ---- documents ------------------------------------------------------------

    fn set_doc(&mut self, doc: Document) {
        let first = doc
            .records()
            .iter()
            .find(|r| r.tag == "INDI")
            .and_then(|r| r.xref.clone());
        self.home = doc.path.as_deref().and_then(|p| self.homes.get(&path_key(p))).cloned();
        // Start on the home person when the file has one, else the first person.
        let first = self.home.clone().filter(|h| doc.person(h).is_some()).or(first);
        self.relation = RelationView::default();
        self.media_ui = MediaUi::default();
        self.tools = ToolsUi::default();
        // Opening a server's working copy (from the recent list, or on
        // start-up) reconnects to it; opening anything else leaves it.
        self.remote = match (self.remote.take(), doc.path.as_deref()) {
            (Some(r), Some(p)) if r.tree == p => Some(r),
            (_, Some(p)) => Remote::open_for(p),
            _ => None,
        };
        self.doc = Some(doc);
        self.history.clear();
        self.history_pos = 0;
        self.selected = None;
        self.editor = None;
        self.tree = TreeView::default();
        self.graph.reset();
        self.search.clear();
        if let Some(x) = first {
            self.select(x);
        }
    }

    fn load_path(&mut self, ctx: &egui::Context, path: &Path) {
        if is_bundle(path) {
            self.open_bundle(ctx, path);
            return;
        }
        match std::fs::read(path) {
            Ok(bytes) => {
                self.open_bytes(ctx, path, &bytes);
                self.remember_recent(path);
            }
            Err(e) => {
                self.recent.retain(|p| p != path);
                Toast::new("Couldn't open file")
                    .tone(BadgeTone::Danger)
                    .description(format!("{}: {e}", path.display()))
                    .show(ctx);
            }
        }
    }

    /// Unpacks a bundle into a folder the user picks, then opens the tree.
    fn open_bundle(&mut self, ctx: &egui::Context, bundle: &Path) {
        let Some(parent) = platform::pick_folder("Where should the tree and its documents go?") else { return };
        match crate::bundle::import(bundle, &parent) {
            Ok(tree) => {
                self.load_path(ctx, &tree);
                let folder = tree.parent().map(|d| d.display().to_string()).unwrap_or_default();
                Toast::new(format!("Unpacked {}", file_label(bundle))).tone(BadgeTone::Ok).description(format!("Into {folder}")).show(ctx);
            }
            Err(e) => {
                Toast::new("Couldn't open the bundle").tone(BadgeTone::Danger).description(e).show(ctx);
            }
        }
    }

    fn export_bundle(&mut self, ctx: &egui::Context) {
        let Some(doc) = self.doc.as_ref() else { return };
        if platform::WEB {
            // The server bundles the tree as saved.
            if doc.dirty {
                Toast::new("Bundling the last saved version").tone(BadgeTone::Warning).description("Save first to include your latest changes.").show(ctx);
            }
            platform::open_url(ctx, "/api/bundle");
            return;
        }
        let stem = Path::new(&doc.file_name()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Tree".into());
        let dir = doc.path.as_ref().and_then(|p| p.parent());
        let Some(mut dest) = platform::save_bundle(dir, &format!("{stem}.{}", crate::bundle::EXTENSION)) else { return };
        if dest.extension().is_none() {
            dest.set_extension(crate::bundle::EXTENSION);
        }
        match crate::bundle::export(doc, &dest) {
            Ok(s) => {
                let files = format!("{} document{}", s.files, if s.files == 1 { "" } else { "s" });
                Toast::new(format!("Exported {}", file_label(&dest))).tone(BadgeTone::Ok).description(format!("The tree and {files}.")).show(ctx);
                if !s.missing.is_empty() {
                    let n = s.missing.len();
                    Toast::new(format!("{n} document{} left out", if n == 1 { "" } else { "s" }))
                        .tone(BadgeTone::Warning)
                        .description(format!("Their files weren't found, starting with {}.", s.missing[0]))
                        .show(ctx);
                }
            }
            Err(e) => {
                Toast::new("Export failed").tone(BadgeTone::Danger).description(e.to_string()).show(ctx);
            }
        }
    }

    fn open_bytes(&mut self, ctx: &egui::Context, path: &Path, bytes: &[u8]) {
        let (mut doc, notes) = Document::from_bytes(bytes);
        doc.path = Some(path.to_path_buf());
        let (people, fams) = (doc.people().len(), doc.family_count());
        self.set_doc(doc);
        self.tab = TAB_PROFILE;
        Toast::new(format!("Opened {}", file_label(path)))
            .tone(BadgeTone::Ok)
            .description(format!("{people} people · {fams} families"))
            .show(ctx);
        if !notes.is_empty() {
            Toast::new(format!("{} import note{}", notes.len(), if notes.len() == 1 { "" } else { "s" }))
                .tone(BadgeTone::Warning)
                .description(format!("{} — see the Overview tab.", notes[0]))
                .show(ctx);
        }
        self.load_notes = notes;
    }

    /// In the browser: (re)load the tree the server was started with.
    fn load_from_server(&mut self, ctx: &egui::Context) {
        self.server_load = Some(Ok(()));
        self.server.load(ctx);
    }

    fn server_events(&mut self, ctx: &egui::Context) {
        while let Some(event) = self.server.poll() {
            match event {
                ServerEvent::Loaded { path, bytes } => {
                    self.server_load = None;
                    self.open_bytes(ctx, &path, &bytes);
                }
                ServerEvent::LoadFailed(e) => {
                    self.server_load = Some(Err(e.clone()));
                    Toast::new("Couldn't load the tree").tone(BadgeTone::Danger).description(e).show(ctx);
                }
                ServerEvent::Saved(result) => {
                    let revision = self.saving.take();
                    match result {
                        Ok(()) => {
                            // Unless it was edited while saving.
                            if let Some(doc) = self.doc.as_mut().filter(|d| Some(d.revision) == revision) {
                                doc.dirty = false;
                            }
                            let name = self.doc.as_ref().map(|d| d.file_name()).unwrap_or_default();
                            Toast::new(format!("Saved {name}")).tone(BadgeTone::Ok).show(ctx);
                        }
                        Err(e) => {
                            Toast::new("Save failed").tone(BadgeTone::Danger).description(e).show(ctx);
                        }
                    }
                }
            }
        }
    }

    fn remember_recent(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }

    /// Saves the tree, returning whether it's done. In the browser it's
    /// sent to the server, which answers later, so that returns false.
    fn save(&mut self, ctx: &egui::Context, force_dialog: bool) -> bool {
        let Some(doc) = self.doc.as_mut() else { return false };
        if platform::WEB {
            if self.saving.is_none() {
                self.saving = Some(doc.revision);
                self.server.save(ctx, doc.to_gedcom().into_bytes());
            }
            return false;
        }
        let path = match (&doc.path, force_dialog) {
            (Some(p), false) => p.clone(),
            _ => {
                let dir = doc.path.as_ref().and_then(|p| p.parent());
                let Some(mut p) = platform::save_ged(dir, &doc.file_name()) else { return false };
                if p.extension().is_none() {
                    p.set_extension("ged");
                }
                p
            }
        };
        // Moving the tree to another folder: keep media paths pointing at the same files.
        let old_dir = doc.path.as_ref().and_then(|p| p.parent()).map(Path::to_path_buf);
        if let (Some(old), Some(new)) = (old_dir, path.parent())
            && old != new
        {
            doc.rebase_media(&old, new);
        }
        match std::fs::write(&path, doc.to_gedcom()) {
            Ok(()) => {
                doc.path = Some(path.clone());
                doc.dirty = false;
                if let Some(h) = &self.home {
                    self.homes.insert(path_key(&path), h.clone());
                }
                self.remember_recent(&path);
                Toast::new(format!("Saved {}", file_label(&path))).tone(BadgeTone::Ok).show(ctx);
                true
            }
            Err(e) => {
                Toast::new("Save failed").tone(BadgeTone::Danger).description(e.to_string()).show(ctx);
                false
            }
        }
    }

    fn request(&mut self, ctx: &egui::Context, pending: Pending) {
        if self.doc.as_ref().is_some_and(|d| d.dirty) {
            self.pending = Some(pending);
            self.unsaved_open = true;
        } else {
            self.perform(ctx, pending);
        }
    }

    fn perform(&mut self, ctx: &egui::Context, pending: Pending) {
        match pending {
            Pending::Open(Some(p)) => self.load_path(ctx, &p),
            Pending::Connect => {
                let (server, user) = match &self.remote {
                    Some(r) => (r.state.server.clone(), r.state.user.username.clone()),
                    None => (self.last_server.clone(), self.last_username.clone()),
                };
                self.remote_ui.open_connect(&server, &user, None);
            }
            Pending::Open(None) if platform::WEB => self.load_from_server(ctx),
            Pending::Open(None) => {
                if let Some(p) = platform::pick_ged(self.recent.first().and_then(|p| p.parent())) {
                    self.load_path(ctx, &p);
                }
            }
            Pending::New => {
                self.set_doc(Document::new_empty());
                self.load_notes.clear();
                self.tab = TAB_PROFILE;
                self.open_new_person(None);
            }
            Pending::Sample => {
                let (doc, _) = Document::from_bytes(SAMPLE);
                self.set_doc(doc);
                self.load_notes.clear();
                if let Some(x) = self.doc.as_ref().and_then(|d| d.people().iter().find(|p| p.given == "Arthur")).map(|p| p.xref.clone()) {
                    self.select(x);
                }
                self.tab = TAB_TREE;
            }
            Pending::Close => {
                self.doc = None;
                self.selected = None;
                self.editor = None;
                self.load_notes.clear();
            }
            Pending::Quit => {
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// Copies files into the tree's media folder and adds them as documents,
    /// attached to `person` when given.
    fn add_media(&mut self, ctx: &egui::Context, person: Option<String>, files: Vec<PathBuf>) {
        if files.is_empty() || self.doc.is_none() {
            return;
        }
        if self.doc.as_ref().is_some_and(|d| d.path.is_none()) {
            Toast::new("Save the tree first").description("Documents are copied into a media folder next to the tree file.").show(ctx);
            if !self.save(ctx, true) {
                return;
            }
        }
        let Some(doc) = self.doc.as_mut() else { return };
        let Some(tree) = doc.path.clone() else { return };
        let (created, failed) = crate::media::add_files(doc, &tree, &files, person.as_deref());
        for (f, e) in failed {
            Toast::new(format!("Couldn't add {}", f.display())).tone(BadgeTone::Danger).description(e).show(ctx);
        }
        if created.is_empty() {
            return;
        }
        let n = created.len();
        let to = person.as_deref().and_then(|p| doc.person(p)).map(|p| format!(" to {}", p.display)).unwrap_or_default();
        Toast::new(format!("Added {n} document{}{to}", if n == 1 { "" } else { "s" })).tone(BadgeTone::Ok).show(ctx);
        if let [one] = created.as_slice() {
            self.media_ui.edit(doc, one);
        }
    }

    // ---- the shared tree on a server -------------------------------------------------

    /// Signed in with an account that can't change the tree.
    fn read_only(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| !r.role().can_edit())
    }

    fn explain_read_only(&self, ctx: &egui::Context) {
        if let Some(r) = &self.remote {
            Toast::new("This account can view the tree but not change it")
                .description(format!("{} accounts on {} are read-only. An administrator can change that.", r.role().label(), r.host()))
                .show(ctx);
        }
    }

    /// Saves the working copy, then exchanges changes with the server.
    fn sync(&mut self, ctx: &egui::Context, resolve: HashMap<String, &'static str>) {
        if self.remote.as_ref().is_none_or(|r| r.busy.is_some()) {
            return;
        }
        if !self.read_only() && self.doc.as_ref().is_some_and(|d| d.dirty) && !self.save(ctx, false) {
            return;
        }
        if let Some(r) = self.remote.as_mut() {
            r.sync(ctx, resolve);
        }
    }

    /// Opens the working copy from disk again, after a sync changed it,
    /// staying on the same person and section.
    fn reload_working_copy(&mut self) {
        let Some(tree) = self.remote.as_ref().map(|r| r.tree.clone()) else { return };
        let Ok(bytes) = std::fs::read(&tree) else { return };
        let (mut doc, notes) = Document::from_bytes(&bytes);
        doc.path = Some(tree);
        if self.doc.is_none() {
            self.set_doc(doc);
        } else {
            self.doc = Some(doc);
            self.editor = None;
            self.fix_selection();
        }
        self.load_notes = notes;
    }

    fn connected(&mut self, ctx: &egui::Context, remote: Remote) {
        self.last_server = remote.state.server.clone();
        self.last_username = remote.state.user.username.clone();
        self.before_connect = self.doc.as_ref().and_then(|d| d.path.clone()).filter(|p| *p != remote.tree);
        Toast::new(format!("Signed in to {}", remote.host()))
            .tone(BadgeTone::Ok)
            .description(format!("As {} · {}", remote.state.user.shown_name(), remote.role().label()))
            .show(ctx);
        let tree = remote.tree.clone();
        self.remote = Some(remote);
        if tree.is_file() {
            self.load_path(ctx, &tree);
        } else {
            let mut doc = Document::new_empty();
            doc.path = Some(tree);
            self.set_doc(doc);
            self.load_notes.clear();
        }
        self.tab = TAB_PROFILE;
        if let Some(r) = self.remote.as_mut() {
            r.sync(ctx, HashMap::new());
        }
    }

    /// Ends this account's sign-ins everywhere, and closes the shared tree.
    fn sign_out(&mut self, ctx: &egui::Context) {
        let Some(mut r) = self.remote.take() else { return };
        let client = r.client();
        std::thread::spawn(move || {
            let _ = client.sign_out_everywhere();
        });
        r.state.token.clear();
        let _ = r.save_state();
        Toast::new(format!("Signed out of {}", r.host())).description("Your copy stays on this computer; sign in again to sync it.").show(ctx);
        self.doc = None;
        self.selected = None;
        self.editor = None;
    }

    /// Puts a tree on the server: a `.gdz` as it is, a `.ged` bundled with
    /// the documents it links to.
    fn seed(&mut self, ctx: &egui::Context, path: PathBuf, replace: bool) {
        let Some(r) = self.remote.as_mut() else { return };
        r.request(ctx, move |c| {
            let bundle = if is_bundle(&path) {
                path.clone()
            } else {
                let Ok(bytes) = std::fs::read(&path) else { return remote::Event::Failed(format!("Couldn't read {}", path.display())) };
                let (mut doc, _) = Document::from_bytes(&bytes);
                doc.path = Some(path.clone());
                let out = std::env::temp_dir().join(format!("genie-upload-{}.gdz", std::process::id()));
                if let Err(e) = crate::bundle::export(&doc, &out) {
                    return remote::Event::Failed(e.to_string());
                }
                out
            };
            let result = c.import(&bundle, replace);
            if !is_bundle(&path) {
                let _ = std::fs::remove_file(&bundle);
            }
            match result {
                Ok(()) => remote::Event::Imported,
                Err(e) => remote::Event::Failed(e),
            }
        });
    }

    /// What finished in the background.
    fn remote_events(&mut self, ctx: &egui::Context) {
        let Some(r) = self.remote.as_mut() else { return };
        let host = r.host();
        while let Some(event) = r.poll() {
            match event {
                remote::Event::TreeSynced { base_revision, reload, merged, sent, renamed } => {
                    r.state.base_revision = base_revision;
                    let _ = r.save_state();
                    r.forget_history();
                    r.history.clear();
                    let msg = match (sent, merged, reload) {
                        (true, true, _) => "Synced, with others' changes merged in".to_string(),
                        (true, false, _) => "Synced".to_string(),
                        (false, _, true) => format!("Updated from {host}"),
                        (false, _, false) => "Up to date".to_string(),
                    };
                    let mut toast = Toast::new(msg).tone(BadgeTone::Ok);
                    if renamed > 0 {
                        toast = toast.description(format!("{renamed} new record{} renumbered, as someone else had used the number", if renamed == 1 { "" } else { "s" }));
                    }
                    toast.show(ctx);
                    let empty_server = base_revision.is_none() && r.role() == remote::Role::Admin;
                    r.sync_media(ctx);
                    if reload {
                        self.reload_working_copy();
                    }
                    if empty_server {
                        self.remote_ui.offer_seed(self.before_connect.clone(), false);
                    }
                    return self.remote_events(ctx);
                }
                remote::Event::Conflicts(list) => self.remote_ui.show_conflicts(list),
                remote::Event::MediaSynced { media, downloaded, uploaded, missing } => {
                    r.state.media = media;
                    let _ = r.save_state();
                    if downloaded + uploaded > 0 {
                        ctx.forget_all_images();
                        let mut parts = Vec::new();
                        if downloaded > 0 {
                            parts.push(format!("{downloaded} downloaded"));
                        }
                        if uploaded > 0 {
                            parts.push(format!("{uploaded} uploaded"));
                        }
                        Toast::new(format!("Documents: {}", parts.join(", "))).tone(BadgeTone::Ok).show(ctx);
                    }
                    if missing > 0 {
                        Toast::new(format!("{missing} document{} not on this computer or the server", if missing == 1 { "" } else { "s" }))
                            .tone(BadgeTone::Warning)
                            .show(ctx);
                    }
                }
                remote::Event::News(rows) => r.news = rows,
                remote::Event::History { xref, rows } => {
                    r.history.insert(xref, rows);
                }
                remote::Event::Accounts(list) => self.remote_ui.set_accounts(list),
                remote::Event::Done(msg) => {
                    Toast::new(msg).tone(BadgeTone::Ok).show(ctx);
                    if self.remote_ui.accounts_open() {
                        remoteview::refresh_accounts(r, ctx);
                    }
                }
                remote::Event::PasswordChanged { token, expires_at } => {
                    r.state.token = token;
                    r.state.expires_at = expires_at;
                    let _ = r.save_state();
                    Toast::new("Password changed").tone(BadgeTone::Ok).description("Other computers will need the new password.").show(ctx);
                }
                remote::Event::Imported => {
                    Toast::new(format!("Tree uploaded to {host}")).tone(BadgeTone::Ok).show(ctx);
                    r.sync(ctx, HashMap::new());
                }
                remote::Event::Failed(e) if e == remote::SIGNED_OUT => {
                    let (server, user) = (r.state.server.clone(), r.state.user.username.clone());
                    self.remote_ui.open_connect(&server, &user, Some(e));
                }
                remote::Event::Failed(e) => remoteview::failed(ctx, &host, &e),
            }
        }
    }

    fn remote_requests(&mut self, ctx: &egui::Context) {
        for req in std::mem::take(&mut self.remote_requests) {
            match req {
                Request::Connected(r) => self.connected(ctx, r),
                Request::Resolve(resolve) => self.sync(ctx, resolve),
                Request::Seed { path, replace } => self.seed(ctx, path, replace),
            }
        }
    }

    // ---- navigation -------------------------------------------------------------

    fn select(&mut self, xref: String) {
        if self.selected.as_deref() == Some(xref.as_str()) {
            return;
        }
        if self.history_pos < self.history.len() {
            self.history.truncate(self.history_pos + 1);
        }
        if self.history.last() != Some(&xref) {
            self.history.push(xref.clone());
        }
        self.history_pos = self.history.len() - 1;
        self.selected = Some(xref);
    }

    /// The home person, if set and still in the tree.
    fn live_home(&self) -> Option<String> {
        self.home.clone().filter(|h| self.doc.as_ref().is_some_and(|d| d.person(h).is_some()))
    }

    /// The history position a step of `delta` would land on, skipping
    /// people who have since been deleted.
    fn history_target(&self, delta: isize) -> Option<usize> {
        let doc = self.doc.as_ref()?;
        let mut pos = self.history_pos as isize + delta;
        while pos >= 0 && (pos as usize) < self.history.len() {
            if doc.person(&self.history[pos as usize]).is_some() {
                return Some(pos as usize);
            }
            pos += delta;
        }
        None
    }

    fn go_history(&mut self, delta: isize) {
        if let Some(pos) = self.history_target(delta) {
            self.history_pos = pos;
            self.selected = Some(self.history[pos].clone());
        }
    }

    fn can_back(&self) -> bool {
        self.history_target(-1).is_some()
    }
    fn can_forward(&self) -> bool {
        self.history_target(1).is_some()
    }

    /// "Back to Thomas Hartwell (Alt ←)" style hint for a history button.
    fn history_hint(&self, delta: isize) -> String {
        let (verb, keys) = if delta < 0 { ("Back", "Alt ← or mouse back") } else { ("Forward", "Alt → or mouse forward") };
        let name = self
            .history_target(delta)
            .and_then(|pos| self.doc.as_ref()?.person(&self.history[pos]).map(|p| p.display.clone()));
        match name {
            Some(n) => format!("{verb} to {n}  ({keys})"),
            None => format!("Nothing to go {} to", verb.to_lowercase()),
        }
    }

    // ---- editor -----------------------------------------------------------------

    fn open_edit(&mut self, xref: String) {
        let Some(doc) = &self.doc else { return };
        let mut form = doc.form_for(&xref);
        form.birth_date = model::pretty_date(&form.birth_date);
        form.death_date = model::pretty_date(&form.death_date);
        self.editor = Some(Editor {
            open: true,
            mode: EditMode::Existing(xref.clone()),
            original: form.clone(),
            form,
            source: 0,
            pick_search: String::new(),
            picked: None,
            focus_first: true,
            error: None,
            places: doc.places(),
            sources: doc.sources(),
            facts: doc.facts(&xref),
            scroll_to_sources: false,
        });
    }

    fn open_new_person(&mut self, rel: Option<(String, Relation)>) {
        let mut form = PersonForm::default();
        let places = self.doc.as_ref().map(|d| d.places()).unwrap_or_default();
        if let (Some((anchor, relation)), Some(doc)) = (&rel, &self.doc) {
            if let Some(sex) = relation.implied_sex() {
                form.sex = sex;
            }
            let a = doc.person(anchor);
            match relation {
                Relation::Father | Relation::Sibling => {
                    form.surname = a.map(|p| p.surname.clone()).unwrap_or_default();
                }
                Relation::Spouse => {
                    form.sex = match a.map(|p| p.sex) {
                        Some(Sex::Male) => Sex::Female,
                        Some(Sex::Female) => Sex::Male,
                        _ => Sex::Unknown,
                    };
                }
                Relation::Child(fam) => {
                    // Children take the father's surname by default.
                    let fam = fam.clone().or_else(|| doc.spouse_families(anchor).into_iter().next());
                    let father = fam.and_then(|f| doc.husband(&f));
                    let from = match father {
                        Some(f) => doc.person(&f),
                        None if a.is_some_and(|p| p.sex != Sex::Female) => a,
                        None => None,
                    };
                    form.surname = from.map(|p| p.surname.clone()).unwrap_or_default();
                }
                Relation::Mother => {}
            }
        }
        self.editor = Some(Editor {
            open: true,
            mode: EditMode::New(rel),
            original: form.clone(),
            form,
            source: 0,
            pick_search: String::new(),
            picked: None,
            focus_first: true,
            error: None,
            places,
            sources: self.doc.as_ref().map(|d| d.sources()).unwrap_or_default(),
            facts: default_facts(),
            scroll_to_sources: false,
        });
    }

    fn commit_editor(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.editor.as_mut() else { return };
        let Some(doc) = self.doc.as_mut() else { return };
        let mut form = ed.form.clone();
        form.birth_date = model::normalize_date(&form.birth_date);
        form.death_date = model::normalize_date(&form.death_date);
        if !form.deceased {
            form.death_date.clear();
            form.death_place.clear();
        }
        let mode = ed.mode.clone();
        let creating_or_editing = !(matches!(mode, EditMode::New(Some(_))) && ed.source == 1);
        if creating_or_editing
            && form.citations.iter().any(|c| matches!(&c.source, CiteSource::New { title, .. } if title.trim().is_empty()))
        {
            ed.error = Some("Give each new source a title.".into());
            return;
        }
        let result: Result<Option<String>, String> = match (&mode, ed.source) {
            (EditMode::Existing(x), _) => {
                if form != ed.original || ed.form != ed.original {
                    doc.mutate(|d| d.apply_form(x, &form));
                }
                Ok(Some(x.clone()))
            }
            (EditMode::New(Some((anchor, rel))), 1) => match ed.picked.clone() {
                None => Err("Pick someone from the list first.".into()),
                Some(other) => match doc.mutate(|d| d.link(anchor, rel, &other)) {
                    Ok(()) => Ok(None),
                    Err(e) => {
                        doc.rollback();
                        Err(e)
                    }
                },
            },
            (EditMode::New(rel), _) => {
                if form.given.trim().is_empty() && form.surname.trim().is_empty() {
                    Err("Enter at least a given name or a surname.".into())
                } else {
                    let r = doc.mutate(|d| {
                        let x = d.create_person(&form);
                        if let Some((anchor, rel)) = rel {
                            d.link(anchor, rel, &x).map(|_| x)
                        } else {
                            Ok(x)
                        }
                    });
                    match r {
                        // A new relative leaves the focus on the person being
                        // built out, so parents, siblings... can follow quickly.
                        Ok(_) if rel.is_some() => Ok(None),
                        Ok(x) => Ok(Some(x)),
                        Err(e) => {
                            doc.rollback();
                            Err(e)
                        }
                    }
                }
            }
        };
        match result {
            Ok(sel) => {
                let name = if form.given.is_empty() && form.surname.is_empty() { String::new() } else { format!("{} {}", form.given.trim(), form.surname.trim()).trim().to_string() };
                let (msg, desc) = match &mode {
                    EditMode::Existing(_) => ("Changes saved".to_string(), String::new()),
                    EditMode::New(Some((_, rel))) if ed.source == 1 => (format!("{} linked", rel.label()), String::new()),
                    EditMode::New(Some((_, rel))) => (format!("{} added", rel.label()), name),
                    EditMode::New(None) => ("Person added".to_string(), name),
                };
                let mut toast = Toast::new(msg).tone(BadgeTone::Ok);
                if !desc.is_empty() {
                    toast = toast.description(desc);
                }
                toast.show(ctx);
                self.editor = None;
                if let Some(x) = sel {
                    self.select(x);
                }
            }
            Err(e) => ed.error = Some(e),
        }
    }

    // ---- per-frame ----------------------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        let typing = ctx.egui_wants_keyboard_input();
        let consume = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        if consume(shift_cmd, Key::S) {
            self.actions.push(Action::SaveAs);
        }
        if consume(cmd, Key::S) {
            self.actions.push(Action::Save);
        }
        if consume(cmd, Key::O) {
            self.actions.push(Action::Open(None));
        }
        if consume(cmd, Key::N) {
            self.actions.push(Action::New);
        }
        if self.doc.is_none() {
            return;
        }
        if consume(cmd, Key::F) {
            self.focus_search = true;
        }
        if consume(cmd, Key::P) {
            self.actions.push(Action::NewPerson);
        }
        if !typing && self.editor.is_none() {
            if consume(shift_cmd, Key::Z) || consume(cmd, Key::Y) {
                self.actions.push(Action::Redo);
            }
            if consume(cmd, Key::Z) {
                self.actions.push(Action::Undo);
            }
            let (mouse_back, mouse_fwd) = ctx.input(|i| {
                (i.pointer.button_pressed(egui::PointerButton::Extra1), i.pointer.button_pressed(egui::PointerButton::Extra2))
            });
            if mouse_back {
                self.actions.push(Action::Back);
            }
            if mouse_fwd {
                self.actions.push(Action::Forward);
            }
            if consume(Modifiers::ALT, Key::ArrowLeft) {
                self.actions.push(Action::Back);
            }
            if consume(Modifiers::ALT, Key::ArrowRight) {
                self.actions.push(Action::Forward);
            }
            if consume(Modifiers::ALT, Key::Home) {
                self.actions.push(Action::GoHome);
            }
            if consume(cmd, Key::E)
                && let Some(x) = &self.selected {
                    self.actions.push(Action::Edit(x.clone()));
                }
            for (i, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8].into_iter().enumerate() {
                if consume(cmd, key) {
                    self.actions.push(Action::SetTab(i));
                }
            }
        }
    }

    fn process_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.actions) {
            if action.edits() && self.read_only() {
                self.explain_read_only(ctx);
                continue;
            }
            match action {
                Action::Select(x) => self.select(x),
                Action::Edit(x) => self.open_edit(x),
                Action::AddCitation(x) => {
                    self.open_edit(x);
                    if let (Some(ed), Some(doc)) = (self.editor.as_mut(), self.doc.as_ref()) {
                        ed.form.citations.push(new_citation(doc));
                        ed.focus_first = false;
                        ed.scroll_to_sources = true;
                    }
                }
                Action::NewPerson => self.open_new_person(None),
                Action::AddRelative(anchor, rel) => self.open_new_person(Some((anchor, rel))),
                Action::Delete(x) => self.confirm_delete = Some(x),
                Action::Unlink { fam, person } => {
                    if let Some(doc) = self.doc.as_mut() {
                        doc.mutate(|d| d.unlink(&fam, &person));
                        Toast::new("Relationship removed").description("Undo with Ctrl+Z.").show(ctx);
                    }
                }
                Action::SetTab(t) => self.tab = t,
                Action::OpenRelationship { subject, reference } => {
                    self.select(subject);
                    self.relation.reference = Some(reference);
                    self.tab = TAB_RELATION;
                }
                Action::ViewMedia { media, person } => self.media_ui.view(media, person),
                Action::EditMedia(m) => {
                    if let Some(doc) = &self.doc {
                        self.media_ui.edit(doc, &m);
                    }
                }
                Action::PickMedia(_) | Action::AddMedia { .. } | Action::LocateMedia(_) | Action::ReplaceMediaFile(_) if platform::WEB => {
                    platform::desktop_only(ctx, "Adding and moving files");
                }
                Action::FindMissingMedia if platform::WEB => platform::desktop_only(ctx, "Finding media files"),
                Action::WikiTreePhotos if platform::WEB => platform::desktop_only(ctx, "Getting photos from WikiTree"),
                // The browser edits the tree the server was started with.
                Action::New | Action::Sample | Action::Close | Action::SaveAs | Action::Open(Some(_)) if platform::WEB => {}
                Action::PickMedia(person) => {
                    if let Some(files) = platform::pick_media_files() {
                        self.add_media(ctx, person, files);
                    }
                }
                Action::AddMedia { person, files } => self.add_media(ctx, person, files),
                Action::SetPrimaryPhoto { person, media } => {
                    if let Some(doc) = self.doc.as_mut() {
                        match media {
                            Some(m) => doc.mutate(|d| d.set_primary_photo(&person, &m)),
                            None => doc.mutate(|d| d.clear_primary_photo(&person)),
                        }
                    }
                }
                Action::LocateMedia(m) => {
                    let Some(doc) = self.doc.as_mut() else { continue };
                    if let Some(found) = platform::pick_file(Some("Where is this file now?")) {
                        let file = match doc.path.as_deref().and_then(|t| t.parent()) {
                            Some(base) => pathdiff::diff_paths(&found, base).unwrap_or(found.clone()),
                            None => found.clone(),
                        };
                        let file = crate::media::path_string(&file);
                        doc.mutate(|d| d.relink_media(&m, &file));
                        Toast::new("File located").tone(BadgeTone::Ok).show(ctx);
                    }
                }
                Action::FindMissingMedia => {
                    let Some(doc) = self.doc.as_mut() else { continue };
                    let Some(folder) = platform::pick_folder("Search this folder for files") else { continue };
                    let items = doc.media_items();
                    let wanted = items.iter().filter(|m| crate::media::needs_file(doc.path.as_deref(), m)).count();
                    let found = crate::media::find_files(doc.path.as_deref(), &items, &folder);
                    // Files that moved are linked where they are now; files for
                    // records that never had one are copied into the media folder.
                    let mut links = Vec::new();
                    for f in &found {
                        let link = match (&doc.path, f.was_empty) {
                            (Some(tree), true) => crate::media::import_file(tree, &f.path).ok(),
                            (Some(tree), false) => tree.parent().and_then(|b| pathdiff::diff_paths(&f.path, b)).map(|p| crate::media::path_string(&p)),
                            (None, _) => Some(crate::media::path_string(&f.path)),
                        };
                        if let Some(link) = link {
                            links.push((f.media.clone(), link));
                        }
                    }
                    if !links.is_empty() {
                        doc.mutate(|d| {
                            for (m, file) in &links {
                                d.relink_media(m, file);
                            }
                        });
                    }
                    let tone = if links.len() == wanted { BadgeTone::Ok } else { BadgeTone::Warning };
                    Toast::new(format!("Found {} of {wanted} files", links.len())).tone(tone).show(ctx);
                }
                Action::ReplaceMediaFile(m) => {
                    let Some(doc) = self.doc.as_mut() else { continue };
                    let Some(tree) = doc.path.clone() else {
                        Toast::new("Save the tree first").show(ctx);
                        continue;
                    };
                    if let Some(src) = platform::pick_file(None) {
                        match crate::media::import_file(&tree, &src) {
                            Ok(rel) => {
                                doc.mutate(|d| d.relink_media(&m, &rel));
                                Toast::new("File added").tone(BadgeTone::Ok).show(ctx);
                            }
                            Err(e) => Toast::new("Couldn't copy the file").tone(BadgeTone::Danger).description(e.to_string()).show(ctx),
                        }
                    }
                }
                Action::FindDuplicatePeople => {
                    if let Some(doc) = &self.doc {
                        let key = doc.path.as_deref().map(path_key).unwrap_or_default();
                        let dismissed = self.not_dupes.get(&key).cloned().unwrap_or_default();
                        self.tools.open_people(doc, &dismissed);
                    }
                }
                Action::WikiTreePhotos => {
                    if let Some(doc) = &self.doc {
                        self.tools.open_wikitree(doc);
                    }
                }
                Action::FindDuplicateSources => {
                    if let Some(doc) = &self.doc {
                        self.tools.open_sources(doc);
                    }
                }
                Action::MergedPerson { kept, removed } => {
                    if self.selected.as_deref() == Some(removed.as_str()) {
                        self.selected = None;
                        self.select(kept);
                    }
                }
                Action::GoHome => match self.live_home() {
                    Some(h) => self.select(h),
                    None => {
                        Toast::new("No home person set").description("Right-click someone and choose Set as home person.").show(ctx);
                    }
                },
                Action::SetHome(x) => {
                    let name = x.as_deref().and_then(|x| self.doc.as_ref()?.person(x)).map(|p| p.display.clone());
                    if let Some(path) = self.doc.as_ref().and_then(|d| d.path.as_deref()) {
                        match &x {
                            Some(x) => self.homes.insert(path_key(path), x.clone()),
                            None => self.homes.remove(&path_key(path)),
                        };
                    }
                    self.home = x;
                    let msg = match name {
                        Some(n) => format!("{n} is now the home person"),
                        None => "Home person cleared".into(),
                    };
                    Toast::new(msg).show(ctx);
                }
                Action::OpenTreeBoth => {
                    self.tree.show_both();
                    self.tab = TAB_TREE;
                }
                Action::Open(p) => self.request(ctx, Pending::Open(p)),
                Action::New => self.request(ctx, Pending::New),
                Action::Sample => self.request(ctx, Pending::Sample),
                Action::Close => self.request(ctx, Pending::Close),
                Action::Save if self.remote.is_some() => self.sync(ctx, HashMap::new()),
                Action::Save => {
                    self.save(ctx, false);
                }
                Action::SaveAs if self.remote.is_some() => {
                    Toast::new("This is the shared tree's working copy").description("To keep a copy of your own, use File → Export bundle.").show(ctx);
                }
                Action::SaveAs => {
                    self.save(ctx, true);
                }
                Action::ExportBundle => self.export_bundle(ctx),
                Action::Connect => self.request(ctx, Pending::Connect),
                Action::Sync => self.sync(ctx, HashMap::new()),
                Action::SignOut => self.sign_out(ctx),
                Action::ChangePassword => self.remote_ui.open_password(),
                Action::Accounts => {
                    if let Some(r) = self.remote.as_mut() {
                        self.remote_ui.open_accounts(r, ctx);
                    }
                }
                Action::UploadTree => {
                    let replace = self.remote.as_ref().is_some_and(|r| r.state.base_revision.is_some());
                    self.remote_ui.offer_seed(self.before_connect.clone(), replace);
                }
                Action::Undo => {
                    if self.doc.as_mut().is_some_and(|d| d.undo()) {
                        self.fix_selection();
                    }
                }
                Action::Redo => {
                    if self.doc.as_mut().is_some_and(|d| d.redo()) {
                        self.fix_selection();
                    }
                }
                Action::Back => self.go_history(-1),
                Action::Forward => self.go_history(1),
                Action::ForgetRecent(p) => self.recent.retain(|r| r != &p),
            }
        }
    }

    fn fix_selection(&mut self) {
        let Some(doc) = &self.doc else { return };
        if self.selected.as_ref().is_none_or(|x| doc.person(x).is_none()) {
            self.selected = doc.people().first().map(|p| p.xref.clone());
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match (&self.doc, &self.remote) {
            (Some(d), Some(r)) => format!("{}{} — Genie", r.host(), if d.dirty { " •" } else { "" }),
            (Some(d), None) => format!("{}{} — Genie", d.file_name(), if d.dirty { " •" } else { "" }),
            (None, _) => "Genie".into(),
        };
        if title != self.title {
            platform::set_title(ctx, &title);
            self.title = title;
        }
        platform::set_unsaved(self.doc.as_ref().is_some_and(|d| d.dirty));
    }

    fn menubar(&mut self, ui: &mut Ui) {
        let p = Theme::current(ui.ctx()).palette;
        let has_doc = self.doc.is_some();
        let dirty = self.doc.as_ref().is_some_and(|d| d.dirty);
        let can_undo = self.doc.as_ref().is_some_and(|d| d.can_undo());
        let can_redo = self.doc.as_ref().is_some_and(|d| d.can_redo());
        let mut bar = MenuBar::new("menubar").brand("Genie");
        if let Some(d) = &self.doc {
            let n = d.people().len();
            let people = format!("{n} {}", if n == 1 { "person" } else { "people" });
            let (status, color) = match &self.remote {
                Some(r) => {
                    let unsynced = dirty || r.has_local_changes() && r.role().can_edit();
                    let state = if r.busy.is_some() { "syncing…" } else if unsynced { "not synced" } else { "synced" };
                    (format!("{} · {} ({}) · {people} · {state}", r.host(), r.state.user.shown_name(), r.role().label()), if unsynced { p.amber } else { p.green })
                }
                None => (format!("{} · {people}{}", d.file_name(), if dirty { " · unsaved" } else { "" }), if dirty { p.amber } else { p.green }),
            };
            bar = bar.status_with_dot(status, color);
        }
        let connected = self.remote.as_ref().map(|r| (r.role(), r.busy.is_some()));
        let read_only = self.read_only();
        let (tab, can_back, can_forward) = (self.tab, self.can_back(), self.can_forward());
        let home = self.live_home();
        let acts = &mut self.actions;
        let recent = self.recent.clone();
        let selected = self.selected.clone();
        let mut theme = self.theme;
        let (mut focus_search, mut shortcuts_open, mut about_open) = (false, false, false);
        bar.show(ui, |bar| {
            bar.menu("File", |ui| {
                if platform::WEB {
                    // The browser edits the one tree the server was started with.
                    if ui.add(MenuItem::new("Reload").icon(glyphs::FOLDER_OPEN.to_string()).shortcut("Ctrl O")).on_hover_text("Load the tree from the server again").clicked() {
                        acts.push(Action::Open(None));
                    }
                    if ui.add(MenuItem::new("Save").icon(glyphs::SAVE.to_string()).shortcut("Ctrl S").enabled(has_doc)).clicked() {
                        acts.push(Action::Save);
                    }
                    ui.separator();
                    let item = ui.add(MenuItem::new("Download bundle").icon(glyphs::DOWNLOAD.to_string()).enabled(has_doc));
                    if item.on_hover_text("The saved tree and its documents in one .gdz file").clicked() {
                        acts.push(Action::ExportBundle);
                    }
                    return;
                }
                if ui.add(MenuItem::new("New tree").icon(glyphs::PLUS.to_string()).shortcut("Ctrl N")).clicked() {
                    acts.push(Action::New);
                }
                if ui.add(MenuItem::new("Open…").icon(glyphs::FOLDER_OPEN.to_string()).shortcut("Ctrl O")).clicked() {
                    acts.push(Action::Open(None));
                }
                SubMenuItem::new("Open recent").enabled(!recent.is_empty()).show(ui, |ui| {
                    for r in &recent {
                        if ui.add(MenuItem::new(file_label(r))).on_hover_text(r.display().to_string()).clicked() {
                            acts.push(Action::Open(Some(r.clone())));
                        }
                    }
                });
                if ui.add(MenuItem::new("Open sample tree")).clicked() {
                    acts.push(Action::Sample);
                }
                ui.separator();
                match connected {
                    None => {
                        if ui.add(MenuItem::new("Connect to a shared tree…").icon(glyphs::HOME.to_string())).on_hover_text("Sign in to a Genie server, such as genie.henshaw.us").clicked() {
                            acts.push(Action::Connect);
                        }
                    }
                    Some((_, busy)) => {
                        if ui.add(MenuItem::new("Sync").icon(glyphs::SAVE.to_string()).shortcut("Ctrl S").enabled(!busy)).clicked() {
                            acts.push(Action::Sync);
                        }
                        if ui.add(MenuItem::new("Change password…")).clicked() {
                            acts.push(Action::ChangePassword);
                        }
                        if ui.add(MenuItem::new("Sign in again…")).clicked() {
                            acts.push(Action::Connect);
                        }
                        if ui.add(MenuItem::new("Sign out")).on_hover_text("Ends your sign-in on every computer; your copy stays here").clicked() {
                            acts.push(Action::SignOut);
                        }
                    }
                }
                ui.separator();
                if connected.is_none() && ui.add(MenuItem::new("Save").icon(glyphs::SAVE.to_string()).shortcut("Ctrl S").enabled(has_doc)).clicked() {
                    acts.push(Action::Save);
                }
                if ui.add(MenuItem::new("Save as…").shortcut("Ctrl Shift S").enabled(has_doc && connected.is_none())).clicked() {
                    acts.push(Action::SaveAs);
                }
                let item = ui.add(MenuItem::new("Export bundle…").icon(glyphs::DOWNLOAD.to_string()).enabled(has_doc));
                if item.on_hover_text("The tree and its documents in one .gdz file, to open on another computer").clicked() {
                    acts.push(Action::ExportBundle);
                }
                ui.separator();
                if ui.add(MenuItem::new("Close").enabled(has_doc)).clicked() {
                    acts.push(Action::Close);
                }
            });
            bar.menu("Edit", |ui| {
                if ui.add(MenuItem::new("Undo").shortcut("Ctrl Z").enabled(can_undo && !read_only)).clicked() {
                    acts.push(Action::Undo);
                }
                if ui.add(MenuItem::new("Redo").shortcut("Ctrl Shift Z").enabled(can_redo && !read_only)).clicked() {
                    acts.push(Action::Redo);
                }
                ui.separator();
                if ui.add(MenuItem::new("Add person…").icon(glyphs::PLUS.to_string()).shortcut("Ctrl P").enabled(has_doc && !read_only)).clicked() {
                    acts.push(Action::NewPerson);
                }
                if let Some(x) = selected.as_ref().filter(|_| !read_only) {
                    if ui.add(MenuItem::new("Edit person…").icon(glyphs::PENCIL.to_string()).shortcut("Ctrl E")).clicked() {
                        acts.push(Action::Edit(x.clone()));
                    }
                    SubMenuItem::new("Add relative").show(ui, |ui| {
                        for rel in [Relation::Father, Relation::Mother, Relation::Spouse, Relation::Child(None), Relation::Sibling] {
                            if ui.add(MenuItem::new(rel.label())).clicked() {
                                acts.push(Action::AddRelative(x.clone(), rel));
                            }
                        }
                    });
                    ui.separator();
                    if ui.add(MenuItem::new("Delete person…").icon(glyphs::TRASH.to_string()).danger()).clicked() {
                        acts.push(Action::Delete(x.clone()));
                    }
                }
            });
            bar.menu_keep_open("View", |ui| {
                ui.add(MenuSection::new("Section"));
                for (i, t) in TABS.iter().enumerate() {
                    if ui.add(MenuItem::new(*t).radio(tab == i).shortcut(format!("Ctrl {}", i + 1)).enabled(has_doc)).clicked() {
                        acts.push(Action::SetTab(i));
                    }
                }
                ui.separator();
                ui.add(MenuSection::new("Theme"));
                for t in BuiltInTheme::all() {
                    if ui.add(MenuItem::new(t.label()).radio(theme == t)).clicked() {
                        theme = t;
                    }
                }
            });
            bar.menu("Go", |ui| {
                if ui.add(MenuItem::new("Back").icon(glyphs::ARROW_LEFT.to_string()).shortcut("Alt ←").enabled(can_back)).clicked() {
                    acts.push(Action::Back);
                }
                if ui.add(MenuItem::new("Forward").icon(glyphs::ARROW_RIGHT.to_string()).shortcut("Alt →").enabled(can_forward)).clicked() {
                    acts.push(Action::Forward);
                }
                let label = match home.as_deref().and_then(|h| self.doc.as_ref()?.person(h)) {
                    Some(p) => format!("Home person ({})", p.display),
                    None => "Home person".to_string(),
                };
                let item = ui.add(MenuItem::new(label).icon(glyphs::HOME.to_string()).shortcut("Alt Home").enabled(home.is_some()));
                if home.is_none() {
                    item.on_disabled_hover_text("Right-click someone and choose Set as home person");
                } else if item.clicked() {
                    acts.push(Action::GoHome);
                }
                ui.separator();
                if ui.add(MenuItem::new("Find person").icon(glyphs::SEARCH.to_string()).shortcut("Ctrl F").enabled(has_doc)).clicked() {
                    focus_search = true;
                }
            });
            bar.menu("Tools", |ui| {
                if ui.add(MenuItem::new("Find duplicate people…").icon(glyphs::SEARCH.to_string()).enabled(has_doc)).clicked() {
                    acts.push(Action::FindDuplicatePeople);
                }
                if ui.add(MenuItem::new("Find duplicate sources…").enabled(has_doc)).clicked() {
                    acts.push(Action::FindDuplicateSources);
                }
                if platform::WEB {
                    return;
                }
                if let Some((remote::Role::Admin, _)) = connected {
                    ui.separator();
                    if ui.add(MenuItem::new("Accounts…").icon(glyphs::KEY.to_string())).on_hover_text("Who can see and edit the shared tree").clicked() {
                        acts.push(Action::Accounts);
                    }
                    if ui.add(MenuItem::new("Upload a tree to the server…")).on_hover_text("Replace the shared tree with one from a file (kept in its history)").clicked() {
                        acts.push(Action::UploadTree);
                    }
                }
                ui.separator();
                if ui.add(MenuItem::new("Get photos from WikiTree…").icon(glyphs::DOWNLOAD.to_string()).enabled(has_doc)).clicked() {
                    acts.push(Action::WikiTreePhotos);
                }
                if ui.add(MenuItem::new("Find media files…").icon(glyphs::FOLDER_OPEN.to_string()).enabled(has_doc)).on_hover_text("Search a folder for missing files, and for files belonging to records that have none").clicked() {
                    acts.push(Action::FindMissingMedia);
                }
            });
            bar.menu("Help", |ui| {
                if ui.add(MenuItem::new("Keyboard shortcuts")).clicked() {
                    shortcuts_open = true;
                }
                if ui.add(MenuItem::new("About Genie").icon(glyphs::INFO.to_string())).clicked() {
                    about_open = true;
                }
            });
        });
        self.theme = theme;
        self.focus_search |= focus_search;
        self.shortcuts_open |= shortcuts_open;
        self.about_open |= about_open;
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (back, fwd) = widgets::history_buttons(ui, self.can_back(), self.can_forward());
            if back.on_hover_text(self.history_hint(-1)).clicked() {
                self.actions.push(Action::Back);
            }
            if fwd.on_hover_text(self.history_hint(1)).clicked() {
                self.actions.push(Action::Forward);
            }
            ui.add_space(16.0);
            ui.add(TabBar::new(&mut self.tab, TABS));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let dirty = self.doc.as_ref().is_some_and(|d| d.dirty);
                match &self.remote {
                    Some(r) => {
                        let unsynced = dirty || r.has_local_changes() && r.role().can_edit();
                        let b = Button::new(format!("{}  Sync", glyphs::SAVE)).size(ButtonSize::Small).enabled(r.busy.is_none());
                        let b = if unsynced || !r.news.is_empty() { b.accent(Accent::Green) } else { b.outline() };
                        let hint = if unsynced { format!("Send your changes to {} and get everyone else's (Ctrl S)", r.host()) } else { format!("Get the latest from {} (Ctrl S)", r.host()) };
                        if ui.add(b).on_hover_text(hint).clicked() {
                            self.actions.push(Action::Sync);
                        }
                        if !r.news.is_empty() {
                            let mut who: Vec<&str> = r.news.iter().filter_map(|c| c.user.as_deref()).collect();
                            who.dedup();
                            let n = r.news.len();
                            let text = format!("{n} new change{} by {}", if n == 1 { "" } else { "s" }, who.join(", "));
                            ui.label(RichText::new(text).size(12.5).color(Theme::current(ui.ctx()).palette.amber));
                        }
                    }
                    None => {
                        if ui
                            .add(Button::new(format!("{}  Save", glyphs::SAVE)).accent(Accent::Green).size(ButtonSize::Small).enabled(dirty))
                            .clicked()
                        {
                            self.actions.push(Action::Save);
                        }
                    }
                }
                if !self.read_only() && ui.add(Button::new(format!("{}  Add person", glyphs::PLUS)).size(ButtonSize::Small)).clicked() {
                    self.actions.push(Action::NewPerson);
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut Ui) {
        let p = Theme::current(ui.ctx()).palette;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width().min(620.0);
            let pad = ((ui.available_width() - width) / 2.0).max(0.0);
            ui.horizontal(|ui| {
                ui.add_space(pad);
                ui.vertical(|ui| {
                    ui.set_width(width);
                    ui.add_space((ui.available_height() * 0.12).clamp(16.0, 90.0));
                    ui.label(RichText::new("Genie").size(44.0).family(crate::fonts::semibold()).color(p.focus));
                    ui.label(RichText::new("Build, explore, and share family trees — GEDCOM in, GEDCOM out.").size(16.0).color(p.text_muted));
                    ui.add_space(24.0);
                    if platform::WEB {
                        // The tree comes from the server.
                        match self.server_load.clone() {
                            Some(Err(e)) => {
                                ui.label(RichText::new("Couldn't load the tree").size(16.0).color(p.red));
                                widgets::muted(ui, e);
                                ui.add_space(12.0);
                                if ui.add(Button::new("Try again").size(ButtonSize::Large)).clicked() {
                                    self.load_from_server(ui.ctx());
                                }
                            }
                            _ => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    widgets::muted(ui, "Loading the tree…");
                                });
                            }
                        }
                        return;
                    }
                    let drop = FileDropZone::new()
                        .prompt("Drop a GEDCOM file here")
                        .action_word("browse")
                        .hint(".ged files from Ancestry, FamilySearch, Gramps, RootsMagic, … or a .gdz bundle")
                        .min_height(150.0)
                        .show(ui);
                    if drop.response.clicked() {
                        self.actions.push(Action::Open(None));
                    }
                    if let Some(f) = drop.dropped_files.first() {
                        self.actions.push(Action::Open(Some(f.path().to_path_buf())));
                    }
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if ui.add(Button::new(format!("{}  Open file…", glyphs::FOLDER_OPEN)).size(ButtonSize::Large)).clicked() {
                            self.actions.push(Action::Open(None));
                        }
                        if ui.add(Button::new(format!("{}  Start a new tree", glyphs::PLUS)).accent(Accent::Green).size(ButtonSize::Large)).clicked() {
                            self.actions.push(Action::New);
                        }
                        if ui.add(Button::new("Explore the sample").outline().size(ButtonSize::Large)).clicked() {
                            self.actions.push(Action::Sample);
                        }
                    });
                    if !self.recent.is_empty() {
                        ui.add_space(24.0);
                        section_label(ui, "Recent");
                        for r in self.recent.clone() {
                            let exists = r.exists();
                            ui.horizontal(|ui| {
                                let resp = ui.add(
                                    egui::Button::new(RichText::new(format!("{}  {}", glyphs::FOLDER, file_label(&r))).size(14.0).color(if exists { p.text } else { p.text_faint }))
                                        .frame(false),
                                );
                                ui.label(RichText::new(r.parent().map(|d| d.display().to_string()).unwrap_or_default()).size(12.0).color(p.text_faint));
                                if resp.clicked() {
                                    self.actions.push(Action::Open(Some(r.clone())));
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(egui::Button::new(RichText::new(glyphs::X.to_string()).color(p.text_faint)).frame(false)).on_hover_text("Remove from list").clicked() {
                                        self.actions.push(Action::ForgetRecent(r.clone()));
                                    }
                                });
                            });
                        }
                    }
                });
            });
        });
    }

    fn modals(&mut self, ctx: &egui::Context) {
        // Delete confirmation.
        if let Some(x) = self.confirm_delete.clone() {
            let name = self.doc.as_ref().and_then(|d| d.person(&x)).map(|p| p.display.clone()).unwrap_or_default();
            let mut open = true;
            let mut confirmed = false;
            Modal::new("confirm_delete", &mut open)
                .heading(format!("Delete {name}?"))
                .header_icon(glyphs::TRASH.to_string())
                .header_accent(Accent::Red)
                .alert(true)
                .max_width(420.0)
                .show(ctx, |ui| {
                    widgets::muted(ui, "This removes the person and their links to parents, partners and children. You can undo it with Ctrl+Z.");
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new("Delete").accent(Accent::Red)).clicked() {
                            confirmed = true;
                        }
                        if ui.add(Button::new("Cancel").outline()).clicked() {
                            self.confirm_delete = None;
                        }
                    });
                });
            if confirmed {
                if let Some(doc) = self.doc.as_mut() {
                    doc.mutate(|d| d.delete_person(&x));
                    Toast::new(format!("Deleted {name}")).description("Undo with Ctrl+Z.").show(ctx);
                }
                self.confirm_delete = None;
                self.fix_selection();
            } else if !open {
                self.confirm_delete = None;
            }
        }

        // Unsaved changes guard.
        if self.unsaved_open {
            let name = self.doc.as_ref().map(|d| d.file_name()).unwrap_or_default();
            let mut choice = None;
            Modal::new("unsaved", &mut self.unsaved_open)
                .heading("Save changes?")
                .subtitle(format!("{name} has unsaved changes."))
                .header_icon(glyphs::TRIANGLE_ALERT.to_string())
                .header_accent(Accent::Amber)
                .max_width(440.0)
                .show(ctx, |ui| {
                    widgets::muted(ui, "If you don't save, your changes will be lost.");
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new("Save").accent(Accent::Green)).clicked() {
                            choice = Some(true);
                        }
                        if ui.add(Button::new("Don't save").accent(Accent::Red).outline()).clicked() {
                            choice = Some(false);
                        }
                    });
                });
            match choice {
                Some(save) => {
                    self.unsaved_open = false;
                    let ok = !save || self.save(ctx, false);
                    if save && platform::WEB {
                        // Saved (once the server answers): nothing to reload.
                        self.pending = None;
                    }
                    if ok
                        && let Some(p) = self.pending.take() {
                            if let Some(d) = self.doc.as_mut() {
                                d.dirty = false;
                            }
                            self.perform(ctx, p);
                        }
                }
                None if !self.unsaved_open => self.pending = None,
                None => {}
            }
        }

        Modal::new("about", &mut self.about_open).heading("Genie").subtitle("Genealogy for GEDCOM files").max_width(420.0).show(ctx, |ui| {
            widgets::muted(ui, "Load, build, explore and save family trees as GEDCOM 5.5.1. Unknown tags are preserved when you save.");
            ui.add_space(6.0);
            widgets::muted(ui, "Built with egui, egui-elegance and nodez.");
        });

        Modal::new("shortcuts", &mut self.shortcuts_open).heading("Keyboard shortcuts").max_width(460.0).show(ctx, |ui| {
            let p = Theme::current(ui.ctx()).palette;
            egui::Grid::new("keys").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
                for (k, v) in [
                    ("Ctrl N", "New tree"),
                    ("Ctrl O", "Open"),
                    ("Ctrl S / Ctrl Shift S", "Save / Save as"),
                    ("Ctrl Z / Ctrl Shift Z", "Undo / Redo"),
                    ("Ctrl F", "Find person"),
                    ("Ctrl P", "Add person"),
                    ("Ctrl E", "Edit selected person"),
                    ("Ctrl Enter", "Save in the editor"),
                    ("Alt ← / Alt →", "Back / Forward"),
                    ("Alt Home", "Go to the home person"),
                    ("Ctrl 1 … 7", "Switch section"),
                    ("↑ / ↓ in search", "Move through people"),
                ] {
                    ui.label(RichText::new(k).monospace().color(p.focus));
                    ui.label(v);
                    ui.end_row();
                }
            });
        });
    }

    fn editor_drawer(&mut self, ctx: &egui::Context) {
        let Some(ed) = self.editor.as_mut() else { return };
        let Some(doc) = self.doc.as_ref() else { return };
        let (title, subtitle) = match &ed.mode {
            EditMode::Existing(x) => (
                "Edit person".to_string(),
                doc.person(x).map(|p| p.display.clone()).unwrap_or_default(),
            ),
            EditMode::New(None) => ("New person".to_string(), "Add someone to the tree".to_string()),
            EditMode::New(Some((a, rel))) => (
                format!("Add {}", rel.label().to_lowercase()),
                format!("of {}", doc.person(a).map(|p| p.display.as_str()).unwrap_or("?")),
            ),
        };
        let can_link = matches!(ed.mode, EditMode::New(Some(_)));
        let anchor = match &ed.mode {
            EditMode::New(Some((a, _))) | EditMode::Existing(a) => Some(a.clone()),
            _ => None,
        };
        let places = std::mem::take(&mut ed.places);
        let mut save = false;
        let mut cancel = false;
        Drawer::new("person_editor", &mut ed.open)
            .side(DrawerSide::Right)
            .width(440.0)
            .title(title)
            .subtitle(subtitle)
            .show(ctx, |ui| {
                let footer_h = 60.0;
                let body_h = (ui.available_height() - footer_h).max(0.0);
                // Wide form rows can stretch the drawer's layout; pin the footer
                // to the drawer's own width so its buttons stay on screen.
                let width = ui.available_width();
                ui.allocate_ui_with_layout(egui::vec2(width, body_h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        if can_link {
                            ui.add(SegmentedControl::new(&mut ed.source, ["Create new person", "Choose existing"]).fill());
                            ui.add_space(6.0);
                        }
                        if can_link && ed.source == 1 {
                            let r = widgets::search_input(ui, &mut ed.pick_search, "Search people…", "pick_search", None);
                            if ed.focus_first {
                                if r.has_focus() {
                                    ed.focus_first = false;
                                } else {
                                    r.request_focus();
                                }
                            }
                            let needle = ed.pick_search.to_lowercase();
                            let mut shown = 0;
                            for person in doc.people() {
                                if Some(&person.xref) == anchor.as_ref() || !person.matches(&needle) {
                                    continue;
                                }
                                shown += 1;
                                if shown > 60 {
                                    widgets::muted(ui, "Keep typing to narrow the list…");
                                    break;
                                }
                                let picked = ed.picked.as_deref() == Some(&person.xref);
                                let caption = if picked { Some("selected") } else { None };
                                let resp = widgets::person_chip(ui, doc, &person.xref, caption);
                                if picked {
                                    let p = Theme::current(ui.ctx()).palette;
                                    ui.painter().rect_stroke(resp.rect, 8, egui::Stroke::new(2.0, p.focus), egui::StrokeKind::Inside);
                                }
                                if resp.clicked() {
                                    ed.picked = Some(person.xref.clone());
                                }
                                if resp.double_clicked() {
                                    save = true;
                                }
                            }
                            if shown == 0 {
                                widgets::muted(ui, "No one matches.");
                            }
                        } else {
                            person_form(ui, &mut ed.form, &mut ed.focus_first, &places);
                            ui.add_space(6.0);
                            citations_editor(ui, &mut ed.form.citations, &ed.sources, &ed.facts, &mut ed.scroll_to_sources);
                        }
                        if let Some(err) = &ed.error {
                            ui.add_space(6.0);
                            elegance::Callout::new(elegance::CalloutTone::Danger).title(err.clone()).show(ui, |_| {});
                        }
                    });
                });
                ui.separator();
                ui.add_space(4.0);
                ui.allocate_ui_with_layout(egui::vec2(width, 36.0), Layout::right_to_left(Align::Center), |ui| {
                    let label = match (&ed.mode, ed.source) {
                        (EditMode::Existing(_), _) => "Save changes".to_string(),
                        (EditMode::New(Some((_, rel))), 1) => format!("Link as {}", rel.label().to_lowercase()),
                        (EditMode::New(Some((_, rel))), _) => format!("Add {}", rel.label().to_lowercase()),
                        (EditMode::New(None), _) => "Add person".to_string(),
                    };
                    if ui.add(Button::new(label).accent(Accent::Green)).on_hover_text("Ctrl+Enter").clicked() {
                        save = true;
                    }
                    if ui.add(Button::new("Cancel").outline()).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Enter))) {
            save = true;
        }
        ed.places = places;
        if cancel || !ed.open {
            self.editor = None;
        } else if save {
            self.commit_editor(ctx);
        }
    }
}

fn person_form(ui: &mut Ui, form: &mut PersonForm, focus: &mut bool, places: &[(String, usize)]) {
    let p = Theme::current(ui.ctx()).palette;
    ui.columns(2, |cols| {
        let r = cols[0].add(TextInput::new(&mut form.given).label("Given names").hint("e.g. Mary Ann").id_salt("f_given"));
        // The drawer claims focus as it opens; keep asking until we have it.
        if *focus {
            if r.has_focus() {
                *focus = false;
            } else {
                r.request_focus();
            }
        }
        cols[1].add(TextInput::new(&mut form.surname).label("Surname").hint("Family name").id_salt("f_surname"));
    });
    ui.columns(2, |cols| {
        cols[0].add(TextInput::new(&mut form.suffix).label("Suffix").hint("e.g. Jr, Sr, III").id_salt("f_suffix"));
    });
    ui.label(RichText::new("Sex").size(12.0).color(p.text_muted));
    let mut sex_idx = match form.sex {
        Sex::Male => 0,
        Sex::Female => 1,
        Sex::Unknown => 2,
    };
    ui.add(SegmentedControl::new(&mut sex_idx, ["Male", "Female", "Unknown"]).size(SegmentedSize::Small).id_salt("f_sex"));
    form.sex = [Sex::Male, Sex::Female, Sex::Unknown][sex_idx];

    ui.add_space(6.0);
    section_label(ui, "Birth");
    date_input(ui, &mut form.birth_date, "f_bdate");
    place_input(ui, &mut form.birth_place, "f_bplace", places);

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        section_label(ui, "Death");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add(Switch::new(&mut form.deceased, "Deceased"));
        });
    });
    if !form.deceased && (!form.death_date.is_empty() || !form.death_place.is_empty()) {
        form.deceased = true;
    }
    if form.deceased {
        date_input(ui, &mut form.death_date, "f_ddate");
        place_input(ui, &mut form.death_place, "f_dplace", places);
    }

    ui.add_space(6.0);
    section_label(ui, "More");
    ui.add(TextInput::new(&mut form.occupation).label("Occupation").id_salt("f_occu"));
    ui.add(TextArea::new(&mut form.note).label("Notes").rows(4).id_salt("f_note"));
}

fn date_input(ui: &mut Ui, value: &mut String, id: &str) {
    let p = Theme::current(ui.ctx()).palette;
    ui.add(TextInput::new(value).label("Date").hint("12 Mar 1850 · about 1850 · 1850-03-12").id_salt(id));
    if !value.trim().is_empty() {
        let norm = model::normalize_date(value);
        let pretty = model::pretty_date(&norm);
        let (text, color) = match model::year_of(&norm) {
            Some(_) => (format!("{}  {pretty}", glyphs::CHECK), p.text_faint),
            None => (format!("{}  Not a date Genie understands; it will be kept as text", glyphs::CIRCLE_ALERT), p.amber),
        };
        ui.label(RichText::new(text).size(11.0).color(color));
    }
}

fn place_input(ui: &mut Ui, value: &mut String, id: &str, places: &[(String, usize)]) {
    let resp = ui.add(TextInput::new(value).label("Place").hint("Town, County, Country").id_salt(id));
    let needle = value.trim().to_lowercase();
    if resp.has_focus() && needle.len() >= 2 {
        let matches: Vec<&String> = places
            .iter()
            .map(|(p, _)| p)
            .filter(|p| p.to_lowercase().contains(&needle) && p.to_lowercase() != needle)
            .take(5)
            .collect();
        if !matches.is_empty() {
            let p = Theme::current(ui.ctx()).palette;
            Frame::new().fill(p.input_bg).stroke(egui::Stroke::new(1.0, p.border)).corner_radius(6).inner_margin(Margin::same(4)).show(ui, |ui| {
                for m in matches {
                    // Pointer-down fires before the text box loses focus.
                    let r = ui.add(egui::Button::new(RichText::new(m).size(13.0)).frame(false).sense(egui::Sense::click()));
                    if r.is_pointer_button_down_on() || r.clicked() {
                        *value = m.clone();
                    }
                }
            });
        }
    }
}

/// Identifies a file across runs, however it was opened.
fn path_key(p: &Path) -> String {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf()).display().to_string()
}

fn is_bundle(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(crate::bundle::EXTENSION))
}

fn file_label(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

impl eframe::App for GenieApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.theme.theme().install(&ctx);
        crate::fonts::install(&ctx);
        let p = Theme::current(&ctx).palette;

        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close && self.doc.as_ref().is_some_and(|d| d.dirty) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending = Some(Pending::Quit);
            self.unsaved_open = true;
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if self.doc.is_some() && !dropped.is_empty() {
            let is_tree = |p: &PathBuf| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ged")) || is_bundle(p);
            let (trees, files): (Vec<PathBuf>, Vec<PathBuf>) = dropped.into_iter().partition(is_tree);
            if let Some(path) = trees.into_iter().next() {
                self.actions.push(Action::Open(Some(path)));
            } else {
                // Documents: onto the selected person, or into the library from the Media section.
                let person = if self.tab == TAB_MEDIA { None } else { self.selected.clone() };
                self.actions.push(Action::AddMedia { person, files });
            }
        }
        self.server_events(&ctx);
        self.remote_events(&ctx);
        if let Some(r) = self.remote.as_mut() {
            r.maybe_poll(&ctx);
            if let Some(x) = &self.selected {
                r.want_history(&ctx, x);
            }
        }
        // Views show who last changed the selected person, and hide editing
        // for read-only accounts.
        let history = self.remote.as_ref().zip(self.selected.as_ref()).and_then(|(r, x)| r.history.get(x).cloned());
        let read_only = self.read_only();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::new("genie_history"), history);
            d.insert_temp(egui::Id::new("genie_read_only"), read_only);
        });
        self.handle_shortcuts(&ctx);

        egui::Panel::top("menubar")
            .frame(Frame::new().fill(widgets::mix(p.bg, p.card, 0.45)))
            .show(ui, |ui| self.menubar(ui));

        if self.doc.is_none() {
            egui::CentralPanel::default().frame(Frame::new().fill(p.bg).inner_margin(Margin::same(24))).show(ui, |ui| self.welcome(ui));
        } else {
            egui::Panel::top("toolbar")
                .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 12, right: 12, top: 10, bottom: 0 }))
                .show(ui, |ui| self.toolbar(ui));

            let mut doc = self.doc.take().expect("checked above");
            // Menus anywhere offer relationships to these two.
            let home = self.home.clone().filter(|h| doc.person(h).is_some());
            let selected = self.selected.clone();
            let tree = doc.path.clone();
            ctx.data_mut(|d| {
                d.insert_temp(egui::Id::new("genie_selected"), selected);
                d.insert_temp(egui::Id::new("genie_home"), home.clone());
                d.insert_temp(egui::Id::new("genie_tree_path"), tree);
            });
            egui::Panel::left("people")
                .resizable(true)
                .default_size(300.0)
                .size_range(230.0..=480.0)
                .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 12, right: 8, top: 12, bottom: 8 }))
                .show(ui, |ui| {
                    views::people_panel(ui, &doc, &mut self.search, &mut self.sex_filter, &mut self.focus_search, self.selected.as_deref(), &mut self.actions);
                });
            egui::CentralPanel::default()
                .frame(Frame::new().fill(p.bg).inner_margin(Margin { left: 12, right: 16, top: 12, bottom: 12 }))
                .show(ui, |ui| {
                    if doc.people().is_empty() && !matches!(self.tab, TAB_GRAPH | TAB_MEDIA | TAB_OVERVIEW) {
                        views::empty_tree(ui, &mut self.actions);
                    } else {
                        match self.tab {
                            TAB_PROFILE => views::profile(ui, &doc, self.selected.as_deref(), &mut self.actions),
                            TAB_TREE => self.tree.show(ui, &doc, self.selected.as_deref(), &mut self.actions),
                            TAB_RELATION => self.relation.show(ui, &doc, self.selected.as_deref(), home.as_deref(), &mut self.actions),
                            TAB_GRAPH => self.graph.show(ui, &mut doc, self.selected.as_deref(), &mut self.actions),
                            TAB_MEDIA => self.media_ui.section(ui, &doc, &mut self.actions),
                            TAB_REPORTS => self.reports.show(ui, &doc, self.selected.as_deref(), &mut self.actions),
                            TAB_OVERVIEW => views::overview(ui, &doc, &self.load_notes, &mut self.actions),
                            _ => views::source(ui, &doc, self.selected.as_deref(), &mut self.actions),
                        }
                    }
                });
            self.doc = Some(doc);
            if self.selected.is_none() || self.selected.as_ref().is_some_and(|x| self.doc.as_ref().unwrap().person(x).is_none()) {
                self.fix_selection();
            }
        }

        self.editor_drawer(&ctx);
        if let Some(doc) = self.doc.as_mut() {
            self.media_ui.overlays(&ctx, doc, &mut self.actions);
            let key = doc.path.as_deref().map(path_key).unwrap_or_default();
            let dismissed = self.not_dupes.entry(key).or_default();
            self.tools.overlays(&ctx, doc, dismissed, &mut self.actions);
        }
        self.modals(&ctx);
        self.remote_ui.show(&ctx, self.remote.as_mut(), &mut self.remote_requests);
        if let Some(r) = self.remote.as_ref().filter(|r| r.busy == Some(Busy::Tree)) {
            remoteview::syncing_overlay(&ctx, &r.host());
        }
        Toasts::new().render(&ctx);
        self.process_actions(&ctx);
        self.remote_requests(&ctx);
        // Anything that slipped past the read-only guards is put back.
        if self.read_only() && self.doc.as_ref().is_some_and(|d| d.dirty) {
            self.reload_working_copy();
            self.explain_read_only(&ctx);
            // If the copy couldn't be read back, don't ask again every frame;
            // a read-only account's changes are never sent anyway.
            if let Some(d) = self.doc.as_mut() {
                d.dirty = false;
            }
        }
        self.update_title(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.label().to_string());
        let not_dupes: Vec<String> = self.not_dupes.iter().flat_map(|(p, set)| set.iter().map(move |k| format!("{p}\t{k}"))).collect();
        storage.set_string("not_dupes", not_dupes.join("\n"));
        let homes: Vec<String> = self.homes.iter().map(|(p, x)| format!("{p}\t{x}")).collect();
        storage.set_string("homes", homes.join("\n"));
        storage.set_string("graph_links", self.graph.mode().label().to_string());
        let recent: Vec<String> = self.recent.iter().map(|p| p.display().to_string()).collect();
        storage.set_string("recent", recent.join("\n"));
        let last_open = self.doc.as_ref().and_then(|d| d.path.as_ref());
        storage.set_string("last_open", last_open.map(|p| p.display().to_string()).unwrap_or_default());
        storage.set_string("last_server", self.last_server.clone());
        storage.set_string("last_username", self.last_username.clone());
    }
}

fn default_facts() -> Vec<(FactKey, String)> {
    [("", "Person (general)"), ("NAME", "Name"), ("BIRT", "Birth"), ("DEAT", "Death")]
        .into_iter()
        .map(|(t, l)| ((t.to_string(), 0), l.to_string()))
        .collect()
}

/// A fresh citation: the whole person, citing the first known source (or a
/// new one when the file has none).
fn new_citation(doc: &Document) -> CitationForm {
    let source = match doc.sources().first() {
        Some((x, _)) => CiteSource::Record(x.clone()),
        None => CiteSource::New { title: String::new(), author: String::new(), publication: String::new() },
    };
    CitationForm::new((model::GENERAL_FACT.to_string(), 0), source)
}

#[derive(Clone, PartialEq)]
enum SourcePick {
    Record(String),
    New,
    Text,
}

fn citations_editor(
    ui: &mut Ui,
    cites: &mut Vec<CitationForm>,
    sources: &[(String, String)],
    facts: &[(FactKey, String)],
    scroll_to: &mut bool,
) {
    let p = Theme::current(ui.ctx()).palette;
    let header = ui.horizontal(|ui| {
        section_label(ui, "Sources");
        if !cites.is_empty() {
            ui.label(RichText::new(format!("· {}", cites.len())).size(11.0).color(p.text_faint));
        }
    });
    if *scroll_to {
        header.response.scroll_to_me(Some(Align::Min));
        *scroll_to = false;
    }
    if cites.is_empty() {
        ui.label(RichText::new("Where did these facts come from? Cite a record, book or website.").size(12.5).color(p.text_muted));
    }
    let mut remove = None;
    for (i, c) in cites.iter_mut().enumerate() {
        Frame::new()
            .fill(p.input_bg)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(8)
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.horizontal(|ui| {
                    // Fact: keep a stale one selectable rather than silently moving it.
                    let mut options: Vec<(FactKey, String)> = facts.to_vec();
                    if !options.iter().any(|(k, _)| *k == c.fact) {
                        options.push((c.fact.clone(), model::event_label(&c.fact.0).unwrap_or(&c.fact.0).to_string()));
                    }
                    ui.label(RichText::new("Supports").size(12.0).color(p.text_muted));
                    ui.add(Select::new(("cite_fact", i), &mut c.fact).options(options).width(170.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let del = ui.add(Button::new(glyphs::TRASH.to_string()).outline().size(ButtonSize::Small));
                        if del.on_hover_text("Remove this citation").clicked() {
                            remove = Some(i);
                        }
                    });
                });

                let mut pick = match &c.source {
                    CiteSource::Record(x) => SourcePick::Record(x.clone()),
                    CiteSource::New { .. } => SourcePick::New,
                    CiteSource::Text(_) => SourcePick::Text,
                };
                let mut options: Vec<(SourcePick, String)> =
                    sources.iter().map(|(x, t)| (SourcePick::Record(x.clone()), t.clone())).collect();
                if let CiteSource::Record(x) = &c.source
                    && !sources.iter().any(|(s, _)| s == x) {
                        options.push((SourcePick::Record(x.clone()), format!("Missing source {x}")));
                    }
                if matches!(c.source, CiteSource::Text(_)) {
                    options.push((SourcePick::Text, "Text-only citation".into()));
                }
                options.push((SourcePick::New, format!("{}  New source…", glyphs::PLUS)));
                let before = pick.clone();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Source").size(12.0).color(p.text_muted));
                    ui.add_space(12.0);
                    let w = ui.available_width();
                    ui.add(Select::new(("cite_src", i), &mut pick).options(options).width(w));
                });
                if pick != before {
                    c.source = match pick {
                        SourcePick::Record(x) => CiteSource::Record(x),
                        SourcePick::New => CiteSource::New { title: String::new(), author: String::new(), publication: String::new() },
                        SourcePick::Text => CiteSource::Text(String::new()),
                    };
                }
                match &mut c.source {
                    CiteSource::New { title, author, publication } => {
                        ui.add(TextInput::new(title).label("New source title").hint("e.g. 1900 US Census, Gloucester, MA").id_salt(("cite_title", i)));
                        ui.columns(2, |cols| {
                            cols[0].add(TextInput::new(author).label("Author / agency").id_salt(("cite_auth", i)));
                            cols[1].add(TextInput::new(publication).label("Publication / archive").id_salt(("cite_publ", i)));
                        });
                    }
                    CiteSource::Text(text) => {
                        ui.add(TextInput::new(text).label("Citation text").id_salt(("cite_text", i)));
                    }
                    CiteSource::Record(_) => {}
                }
                ui.add(TextInput::new(&mut c.page).label("Where in the source").hint("Page, entry, film number, URL…").id_salt(("cite_page", i)));
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Reliability").size(12.0).color(p.text_muted));
                    let w = ui.available_width();
                    ui.add(
                        Select::new(("cite_quay", i), &mut c.quality)
                            .options([
                                (None, "Unrated"),
                                (Some(3), "Primary — original record, made at the time"),
                                (Some(2), "Secondary — made later, or copied"),
                                (Some(1), "Questionable — conflicting or indirect"),
                                (Some(0), "Unreliable — estimate or hearsay"),
                            ])
                            .width(w),
                    );
                });
            });
    }
    if let Some(i) = remove {
        cites.remove(i);
    }
    let add = ui.add(Button::new(format!("{}  Add citation", glyphs::PLUS)).outline().size(ButtonSize::Small));
    if add.clicked() {
        let source = match sources.first() {
            Some((x, _)) => CiteSource::Record(x.clone()),
            None => CiteSource::New { title: String::new(), author: String::new(), publication: String::new() },
        };
        cites.push(CitationForm::new((model::GENERAL_FACT.to_string(), 0), source));
    }
}
