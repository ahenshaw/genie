//! Application shell: state, menus, file handling, dialogs, and the editor.

use std::path::{Path, PathBuf};

use egui::{Align, Frame, Key, KeyboardShortcut, Layout, Margin, Modifiers, RichText, Ui};
use elegance::{
    Accent, BadgeTone, BuiltInTheme, Button, ButtonSize, Drawer, DrawerSide, FileDropZone, MenuBar, MenuItem,
    MenuSection, Modal, SegmentedControl, Select, SegmentedSize, SubMenuItem, Switch, TabBar, TextArea, TextInput, Theme,
    Toast, Toasts, glyphs,
};

use crate::graph::FamilyGraph;
use crate::model::{self, CitationForm, CiteSource, Document, FactKey, PersonForm, Relation, Sex};
use crate::tree::TreeView;
use crate::views;
use crate::widgets::{self, section_label};

const SAMPLE: &[u8] = include_bytes!("sample.ged");
const MAX_RECENT: usize = 10;
pub const TABS: [&str; 5] = ["Profile", "Tree", "Graph", "Overview", "GEDCOM"];
pub const TAB_PROFILE: usize = 0;
pub const TAB_TREE: usize = 1;
pub const TAB_GRAPH: usize = 2;

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
    Open(Option<PathBuf>),
    New,
    Sample,
    Save,
    SaveAs,
    Close,
    Undo,
    Redo,
    Back,
    Forward,
    ForgetRecent(PathBuf),
}

#[derive(Clone, Debug)]
enum Pending {
    Open(Option<PathBuf>),
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
}

impl GenieApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
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
        };
        if let Some(storage) = cc.storage {
            if let Some(t) = storage.get_string("theme")
                && let Some(found) = BuiltInTheme::all().into_iter().find(|b| b.label() == t) {
                    app.theme = found;
                }
            if let Some(r) = storage.get_string("recent") {
                app.recent = r.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect();
            }
        }
        app.theme.theme().install(&cc.egui_ctx);
        crate::fonts::install(&cc.egui_ctx);
        if let Some(path) = open {
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
        match std::fs::read(path) {
            Ok(bytes) => {
                let (mut doc, notes) = Document::from_bytes(&bytes);
                doc.path = Some(path.to_path_buf());
                let (people, fams) = (doc.people().len(), doc.family_count());
                self.set_doc(doc);
                self.remember_recent(path);
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
            Err(e) => {
                self.recent.retain(|p| p != path);
                Toast::new("Couldn't open file")
                    .tone(BadgeTone::Danger)
                    .description(format!("{}: {e}", path.display()))
                    .show(ctx);
            }
        }
    }

    fn remember_recent(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.recent.retain(|p| p != &path);
        self.recent.insert(0, path);
        self.recent.truncate(MAX_RECENT);
    }

    fn save(&mut self, ctx: &egui::Context, force_dialog: bool) -> bool {
        let Some(doc) = self.doc.as_mut() else { return false };
        let path = match (&doc.path, force_dialog) {
            (Some(p), false) => p.clone(),
            _ => {
                let mut dialog = rfd::FileDialog::new()
                    .add_filter("GEDCOM", &["ged", "GED"])
                    .set_file_name(doc.file_name());
                if let Some(dir) = doc.path.as_ref().and_then(|p| p.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                let Some(mut p) = dialog.save_file() else { return false };
                if p.extension().is_none() {
                    p.set_extension("ged");
                }
                p
            }
        };
        match std::fs::write(&path, doc.to_gedcom()) {
            Ok(()) => {
                doc.path = Some(path.clone());
                doc.dirty = false;
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
            Pending::Open(None) => {
                let mut dialog = rfd::FileDialog::new().add_filter("GEDCOM", &["ged", "GED"]);
                if let Some(dir) = self.recent.first().and_then(|p| p.parent()) {
                    dialog = dialog.set_directory(dir);
                }
                if let Some(p) = dialog.pick_file() {
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
            if consume(cmd, Key::E)
                && let Some(x) = &self.selected {
                    self.actions.push(Action::Edit(x.clone()));
                }
            for (i, key) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].into_iter().enumerate() {
                if consume(cmd, key) {
                    self.actions.push(Action::SetTab(i));
                }
            }
        }
    }

    fn process_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.actions) {
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
                Action::OpenTreeBoth => {
                    self.tree.show_both();
                    self.tab = TAB_TREE;
                }
                Action::Open(p) => self.request(ctx, Pending::Open(p)),
                Action::New => self.request(ctx, Pending::New),
                Action::Sample => self.request(ctx, Pending::Sample),
                Action::Close => self.request(ctx, Pending::Close),
                Action::Save => {
                    self.save(ctx, false);
                }
                Action::SaveAs => {
                    self.save(ctx, true);
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
        let title = match &self.doc {
            Some(d) => format!("{}{} — Genie", d.file_name(), if d.dirty { " •" } else { "" }),
            None => "Genie".into(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
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
            let status = format!(
                "{} · {n} {}{}",
                d.file_name(),
                if n == 1 { "person" } else { "people" },
                if dirty { " · unsaved" } else { "" }
            );
            bar = bar.status_with_dot(status, if dirty { p.amber } else { p.green });
        }
        let (tab, can_back, can_forward) = (self.tab, self.can_back(), self.can_forward());
        let acts = &mut self.actions;
        let recent = self.recent.clone();
        let selected = self.selected.clone();
        let mut theme = self.theme;
        let (mut focus_search, mut shortcuts_open, mut about_open) = (false, false, false);
        bar.show(ui, |bar| {
            bar.menu("File", |ui| {
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
                if ui.add(MenuItem::new("Save").icon(glyphs::SAVE.to_string()).shortcut("Ctrl S").enabled(has_doc)).clicked() {
                    acts.push(Action::Save);
                }
                if ui.add(MenuItem::new("Save as…").shortcut("Ctrl Shift S").enabled(has_doc)).clicked() {
                    acts.push(Action::SaveAs);
                }
                ui.separator();
                if ui.add(MenuItem::new("Close").enabled(has_doc)).clicked() {
                    acts.push(Action::Close);
                }
            });
            bar.menu("Edit", |ui| {
                if ui.add(MenuItem::new("Undo").shortcut("Ctrl Z").enabled(can_undo)).clicked() {
                    acts.push(Action::Undo);
                }
                if ui.add(MenuItem::new("Redo").shortcut("Ctrl Shift Z").enabled(can_redo)).clicked() {
                    acts.push(Action::Redo);
                }
                ui.separator();
                if ui.add(MenuItem::new("Add person…").icon(glyphs::PLUS.to_string()).shortcut("Ctrl P").enabled(has_doc)).clicked() {
                    acts.push(Action::NewPerson);
                }
                if let Some(x) = &selected {
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
                ui.separator();
                if ui.add(MenuItem::new("Find person").icon(glyphs::SEARCH.to_string()).shortcut("Ctrl F").enabled(has_doc)).clicked() {
                    focus_search = true;
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
                if ui
                    .add(Button::new(format!("{}  Save", glyphs::SAVE)).accent(Accent::Green).size(ButtonSize::Small).enabled(dirty))
                    .clicked()
                {
                    self.actions.push(Action::Save);
                }
                if ui.add(Button::new(format!("{}  Add person", glyphs::PLUS)).size(ButtonSize::Small)).clicked() {
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
                    let drop = FileDropZone::new()
                        .prompt("Drop a GEDCOM file here")
                        .action_word("browse")
                        .hint(".ged files from Ancestry, FamilySearch, Gramps, RootsMagic, …")
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
                    ("Ctrl 1 … 5", "Switch section"),
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
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), body_h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        if can_link {
                            ui.add(SegmentedControl::new(&mut ed.source, ["Create new person", "Choose existing"]).fill());
                            ui.add_space(6.0);
                        }
                        if can_link && ed.source == 1 {
                            let r = ui.add(TextInput::new(&mut ed.pick_search).hint("Search people…").id_salt("pick_search"));
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
                ui.horizontal(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = match (&ed.mode, ed.source) {
                            (EditMode::Existing(_), _) => "Save changes".to_string(),
                            (EditMode::New(Some((_, rel))), 1) => format!("Link as {}", rel.label().to_lowercase()),
                            (EditMode::New(Some((_, rel))), _) => format!("Add {}", rel.label().to_lowercase()),
                            (EditMode::New(None), _) => "Add person".to_string(),
                        };
                        if ui.add(Button::new(label).accent(Accent::Green)).clicked() {
                            save = true;
                        }
                        if ui.add(Button::new("Cancel").outline()).clicked() {
                            cancel = true;
                        }
                        ui.label(RichText::new("Ctrl+Enter").size(11.0).color(Theme::current(ui.ctx()).palette.text_faint));
                    });
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
        if self.doc.is_some()
            && let Some(path) = dropped.into_iter().next() {
                self.actions.push(Action::Open(Some(path)));
            }
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
                    if doc.people().is_empty() && self.tab != TAB_GRAPH && self.tab != 3 {
                        views::empty_tree(ui, &mut self.actions);
                    } else {
                        match self.tab {
                            TAB_PROFILE => views::profile(ui, &doc, self.selected.as_deref(), &mut self.actions),
                            TAB_TREE => self.tree.show(ui, &doc, self.selected.as_deref(), &mut self.actions),
                            TAB_GRAPH => self.graph.show(ui, &mut doc, self.selected.as_deref(), &mut self.actions),
                            3 => views::overview(ui, &doc, &self.load_notes, &mut self.actions),
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
        self.modals(&ctx);
        Toasts::new().render(&ctx);
        self.process_actions(&ctx);
        self.update_title(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("theme", self.theme.label().to_string());
        let recent: Vec<String> = self.recent.iter().map(|p| p.display().to_string()).collect();
        storage.set_string("recent", recent.join("\n"));
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
