//! The Graph section: the family as a nodez node graph.
//!
//! People and families are nodes. A person's output wires into a family's
//! Partners; a family's Children output wires into each child's Parents
//! input. Genealogy is a DAG, so nodez's cycle refusal doubles as a sanity
//! check (nobody can be their own ancestor), and `nodez::layered` lays each
//! generation out in its own column.
//!
//! In the Direct link mode each person instead has Father and Mother
//! inputs wired straight from the parents; the family record behind them
//! is found or created automatically, and Couple nodes appear only for
//! partners without children (or ones added to record a marriage).
//!
//! The document stays the source of truth. After any edit on the canvas the
//! graph is diffed against the document and the difference applied as one
//! undoable step; when the document changes elsewhere the graph is rebuilt,
//! keeping everyone where the user left them.

use std::collections::{HashMap, HashSet, VecDeque};

use egui::{Align, Color32, Layout, Pos2, RichText, Ui, vec2};
use elegance::{Badge, BadgeTone, Button, ButtonSize, Segment, SegmentedControl, SegmentedSize, Theme, Toast, glyphs};
use nodez::{
    DataTypeBuilder, EditorAction, EditorStyle, Graph, LayoutOptions, NodeEditor, NodeId, NodeLibrary, NodeTemplate,
    ParamSpec, SocketShape, SocketSpec, TemplateId, TypeRegistry, Value, Widget,
};

use crate::app::Action;
use crate::model::{self, Document, PersonForm, Sex};
use crate::widgets::mix;

/// Above this many people the graph shows a neighbourhood of the selection.
const FULL_GRAPH_LIMIT: usize = 250;
/// How many people away from the selection a neighbourhood reaches: a
/// parent, child or partner is one away, a grandparent two.
pub const REACH_CHOICES: [usize; 4] = [2, 3, 4, 5];
pub const DEFAULT_REACH: usize = 3;
/// Upper bound on people and families drawn in a neighbourhood: as many as
/// a whole tree the graph would draw.
const NEIGHBOURHOOD_MAX: usize = FULL_GRAPH_LIMIT;
const SEXES: [&str; 3] = ["Male", "Female", "Unknown"];

/// How parents are wired to children on the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkMode {
    /// Parents → Family node → children, mirroring GEDCOM's family records.
    Families,
    /// Parents wired straight into each child's Father and Mother.
    Direct,
}

impl LinkMode {
    fn index(self) -> usize {
        self as usize
    }
    pub fn label(self) -> &'static str {
        match self {
            LinkMode::Families => "Families",
            LinkMode::Direct => "Direct",
        }
    }
}

/// The node palette for one link mode. Each mode has its own, so the
/// add menu never offers a node that belongs to the other.
struct Kit {
    library: NodeLibrary,
    person: TemplateId,
    family: TemplateId,
}

pub struct FamilyGraph {
    editor: NodeEditor,
    mode: LinkMode,
    kits: [Kit; 2],
    graph: Graph,
    /// Families given a Couple node in Direct mode although they have children.
    pinned: HashSet<String>,
    node_of: HashMap<String, NodeId>,
    xref_of: HashMap<NodeId, String>,
    positions: HashMap<String, Pos2>,
    synced_rev: Option<u64>,
    /// The people shown when the tree is too big to show whole.
    scope_root: Option<String>,
    limited: bool,
    /// People away from the selection a neighbourhood reaches.
    reach: usize,
    /// The neighbourhood stopped at NEIGHBOURHOOD_MAX before its reach.
    capped: bool,
    needs_fit: bool,
    styled_for: Option<Color32>,
    last_selected: Option<String>,
}

impl FamilyGraph {
    pub fn new() -> Self {
        Self {
            // Scroll always pans, as in the tree views; Ctrl+scroll or a pinch
            // zooms. nodez's Auto mode guesses trackpad vs. wheel per event, and
            // a wrong guess turned two-finger scrolls into small zooms.
            editor: {
                let mut editor = NodeEditor::new();
                editor.scroll_mode = nodez::ScrollMode::Pan;
                editor
            },
            mode: LinkMode::Families,
            kits: [kit(LinkMode::Families), kit(LinkMode::Direct)],
            graph: Graph::new(),
            pinned: HashSet::new(),
            node_of: HashMap::new(),
            xref_of: HashMap::new(),
            positions: HashMap::new(),
            synced_rev: None,
            scope_root: None,
            limited: false,
            reach: DEFAULT_REACH,
            capped: false,
            needs_fit: true,
            styled_for: None,
            last_selected: None,
        }
    }

    pub fn mode(&self) -> LinkMode {
        self.mode
    }

    pub fn reach(&self) -> usize {
        self.reach
    }

    pub fn set_reach(&mut self, reach: usize) {
        let reach = reach.clamp(REACH_CHOICES[0], REACH_CHOICES[REACH_CHOICES.len() - 1]);
        if reach != self.reach {
            self.reach = reach;
            // Draw the new neighbourhood.
            self.synced_rev = None;
            self.needs_fit = true;
        }
    }

    pub fn set_mode(&mut self, mode: LinkMode) {
        if mode != self.mode {
            self.mode = mode;
            // The two modes lay out differently; start each afresh.
            self.reset();
        }
    }

    /// Forget everything; call when a different document is loaded.
    pub fn reset(&mut self) {
        self.graph.clear();
        self.node_of.clear();
        self.xref_of.clear();
        self.positions.clear();
        self.pinned.clear();
        self.synced_rev = None;
        self.scope_root = None;
        self.needs_fit = true;
        self.last_selected = None;
        self.editor.state = Default::default();
    }

    fn apply_theme(&mut self, ui: &Ui) {
        let p = Theme::current(ui.ctx()).palette;
        if self.styled_for == Some(p.bg) {
            return;
        }
        self.styled_for = Some(p.bg);
        let mut s = if p.is_dark { EditorStyle::default() } else { EditorStyle::light() };
        s.background = mix(p.bg, p.card, 0.5);
        s.grid_minor = mix(s.background, p.border, 0.18);
        s.grid_major = mix(s.background, p.border, 0.45);
        s.node_fill = p.card;
        s.node_outline = p.border;
        s.node_selected_outline = p.focus;
        s.node_active_outline = mix(p.focus, Color32::WHITE, 0.3);
        s.node_corner_radius = 10.0;
        // A wide reach in a big family is tall: let Fit zoom out far enough.
        s.min_zoom = 0.05;
        s.header_text = p.text;
        s.body_text = p.text_muted;
        s.wire_highlight = p.focus;
        s.node_shadow = Color32::from_black_alpha(if p.is_dark { 70 } else { 22 });
        self.editor.style = s;
        for kit in &mut self.kits {
            kit.library.set_category_color("People", mix(p.card, p.blue, 0.45));
            kit.library.set_category_color("Families", mix(p.card, p.amber, 0.45));
        }
    }

    // ---- building the graph from the document -----------------------------------

    /// The people and families to draw, when the tree is too big to draw
    /// whole: out from the selection, `reach` people away (two steps each,
    /// person to family to person), nearest first, up to NEIGHBOURHOOD_MAX.
    fn in_scope(&mut self, doc: &Document, selected: Option<&str>) -> Option<HashSet<String>> {
        self.capped = false;
        if doc.people().len() <= FULL_GRAPH_LIMIT {
            return None;
        }
        let root = selected.map(str::to_string).or_else(|| doc.people().first().map(|p| p.xref.clone()))?;
        let mut seen = HashSet::from([root.clone()]);
        let mut queue = VecDeque::from([(root, 0usize)]);
        while let Some((x, d)) = queue.pop_front() {
            if d >= self.reach * 2 {
                continue;
            }
            let next: Vec<String> = if doc.person(&x).is_some() {
                doc.parent_families(&x).into_iter().chain(doc.spouse_families(&x)).collect()
            } else {
                doc.husband(&x).into_iter().chain(doc.wife(&x)).chain(doc.children(&x)).collect()
            };
            for n in next {
                if seen.len() >= NEIGHBOURHOOD_MAX {
                    self.capped = true;
                    break;
                }
                if seen.insert(n.clone()) {
                    queue.push_back((n, d + 1));
                }
            }
        }
        Some(seen)
    }

    fn remember_positions(&mut self) {
        for node in self.graph.nodes() {
            if let Some(x) = self.xref_of.get(&node.id) {
                self.positions.insert(x.clone(), node.position);
            }
        }
    }

    fn rebuild(&mut self, doc: &Document, selected: Option<&str>) {
        self.remember_positions();
        let selection: Vec<String> = self.editor.state.selection().filter_map(|id| self.xref_of.get(&id).cloned()).collect();
        self.graph.clear();
        self.node_of.clear();
        self.xref_of.clear();
        self.editor.state.selection.clear();
        self.editor.state.active = None;

        let scope = self.in_scope(doc, selected);
        self.limited = scope.is_some();
        self.scope_root = if self.limited { selected.map(str::to_string) } else { None };
        let shown = |x: &str| scope.as_ref().is_none_or(|s| s.contains(x));

        let k = self.mode.index();
        for person in doc.people().iter().filter(|p| shown(&p.xref)) {
            let id = self.graph.add_node(&self.kits[k].library, self.kits[k].person, Pos2::ZERO);
            self.map(id, &person.xref);
            self.write_person_params(doc, id, &person.xref);
        }
        let direct = self.mode == LinkMode::Direct;
        for fam in doc.families().into_iter().filter(|f| shown(f)) {
            // Direct mode only draws couples the parent links can't show.
            if direct && !doc.children(&fam).is_empty() && !self.pinned.contains(&fam) {
                continue;
            }
            let id = self.graph.add_node(&self.kits[k].library, self.kits[k].family, Pos2::ZERO);
            self.map(id, &fam);
            self.write_family_params(doc, id, &fam);
            for partner in doc.husband(&fam).into_iter().chain(doc.wife(&fam)) {
                if let Some(&pid) = self.node_of.get(&partner) {
                    let _ = self.graph.connect(&self.kits[k].library, (pid, "person"), (id, "partners"));
                }
            }
            if direct {
                continue;
            }
            for child in doc.children(&fam) {
                // Parents is single-link: only the first birth family is wired.
                let first = doc.parent_families(&child).into_iter().find(|f| shown(f));
                if first.as_deref() != Some(fam.as_str()) {
                    continue;
                }
                if let Some(&cid) = self.node_of.get(&child) {
                    let _ = self.graph.connect(&self.kits[k].library, (id, "children"), (cid, "parents"));
                }
            }
        }
        if direct {
            let people: Vec<String> = self.node_of.keys().filter(|x| doc.person(x).is_some()).cloned().collect();
            for child in people {
                let Some(fam) = doc.parent_families(&child).into_iter().next() else { continue };
                let cid = self.node_of[&child];
                for (parent, socket) in [(doc.husband(&fam), "father"), (doc.wife(&fam), "mother")] {
                    if let Some(&pid) = parent.as_ref().and_then(|p| self.node_of.get(p)) {
                        let _ = self.graph.connect(&self.kits[k].library, (pid, "person"), (cid, socket));
                    }
                }
            }
        }

        let missing = self.graph.nodes().filter(|n| !self.positions.contains_key(&self.xref_of[&n.id])).count();
        if missing * 2 > self.graph.node_count() {
            self.auto_layout();
            self.needs_fit = true;
        } else {
            let ids: Vec<NodeId> = self.graph.node_ids().collect();
            for id in ids {
                let x = self.xref_of[&id].clone();
                let pos = self.positions.get(&x).copied().unwrap_or_else(|| self.place_near(id));
                if let Some(n) = self.graph.node_mut(id) {
                    n.position = pos;
                }
            }
        }
        for x in selection {
            if let Some(&id) = self.node_of.get(&x) {
                self.editor.state.selection.insert(id);
            }
        }
        self.remember_positions();
        self.synced_rev = Some(doc.revision);
    }

    /// A spot beside something the new node is wired to.
    fn place_near(&self, id: NodeId) -> Pos2 {
        for c in self.graph.connections() {
            let (other, dx) = if c.to.node == id {
                (c.from.node, 260.0)
            } else if c.from.node == id {
                (c.to.node, -260.0)
            } else {
                continue;
            };
            if let Some(x) = self.xref_of.get(&other)
                && let Some(p) = self.positions.get(x) {
                    return *p + vec2(dx, 40.0);
                }
        }
        self.editor.view_center()
    }

    fn auto_layout(&mut self) {
        let (library, style) = (&self.kits[self.mode.index()].library, &self.editor.style);
        let options = LayoutOptions { column_gap: 70.0, row_gap: 22.0, ..Default::default() };
        let _ = nodez::layered(&mut self.graph, &options, |g, n| nodez::node_size(g, library, n, style));
    }

    fn map(&mut self, id: NodeId, xref: &str) {
        self.node_of.insert(xref.to_string(), id);
        self.xref_of.insert(id, xref.to_string());
    }

    fn write_person_params(&mut self, doc: &Document, id: NodeId, xref: &str) {
        let form = doc.form_for(xref);
        let title = doc.person(xref).map(|p| p.display.clone()).unwrap_or_default();
        if let Some(n) = self.graph.node_mut(id) {
            n.title = title;
            n.set_param("given", form.given);
            n.set_param("surname", form.surname);
            n.set_param("sex", Value::Choice(form.sex.label().into()));
            n.set_param("born", model::pretty_date(&form.birth_date));
            n.set_param("died", model::pretty_date(&form.death_date));
        }
    }

    fn write_family_params(&mut self, doc: &Document, id: NodeId, fam: &str) {
        let date = doc.family_event(fam, "MARR").map(|(d, _)| model::pretty_date(&d)).unwrap_or_default();
        let title = family_title(doc, fam);
        if let Some(n) = self.graph.node_mut(id) {
            n.title = title;
            n.set_param("married", date);
        }
    }

    // ---- applying canvas edits to the document ------------------------------------

    fn param(&self, id: NodeId, name: &str) -> String {
        self.graph
            .node(id)
            .and_then(|n| n.param(name).and_then(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default()
    }

    fn form_from_node(&self, doc: &Document, id: NodeId, xref: Option<&str>) -> PersonForm {
        let mut form = xref.map(|x| doc.form_for(x)).unwrap_or_default();
        form.given = self.param(id, "given");
        form.surname = self.param(id, "surname");
        form.sex = match self.param(id, "sex").as_str() {
            "Male" => Sex::Male,
            "Female" => Sex::Female,
            _ => Sex::Unknown,
        };
        let born = model::normalize_date(&self.param(id, "born"));
        if model::normalize_date(&model::pretty_date(&form.birth_date)) != born {
            form.birth_date = born;
        }
        let died = model::normalize_date(&self.param(id, "died"));
        if model::normalize_date(&model::pretty_date(&form.death_date)) != died {
            form.death_date = died;
        }
        form.deceased = form.deceased || !form.death_date.is_empty();
        form
    }

    /// Makes the document match the canvas. Returns problems to report.
    fn sync_to_doc(&mut self, doc: &mut Document, merge_key: Option<String>) -> Vec<String> {
        let (person_t, family_t) = (self.kits[self.mode.index()].person, self.kits[self.mode.index()].family);
        let direct = self.mode == LinkMode::Direct;
        // Only keystrokes in a node's fields carry a merge key.
        let structural = merge_key.is_none();
        let mut problems = Vec::new();
        let mut bad_links = Vec::new();
        let removed: Vec<(NodeId, String)> = self
            .xref_of
            .iter()
            .filter(|(id, _)| !self.graph.contains_node(**id))
            .map(|(id, x)| (*id, x.clone()))
            .collect();
        let added: Vec<NodeId> = self.graph.node_ids().filter(|id| !self.xref_of.contains_key(id)).collect();

        doc.mutate_merge(merge_key, |d| {
            for (id, x) in &removed {
                if d.person(x).is_some() {
                    // Families stay: on the canvas a half-wired family is a
                    // normal step on the way to a finished one.
                    d.delete_person_only(x);
                } else if direct && !d.children(x).is_empty() {
                    // Removing a Couple node only hides a family that still
                    // links children to their parents.
                    self.pinned.remove(x);
                } else {
                    d.delete_family(x);
                }
                self.xref_of.remove(id);
                self.node_of.remove(x);
            }
            for &id in &added {
                let template = self.graph.node(id).map(|n| n.template);
                let x = if template == Some(person_t) {
                    let form = self.form_from_node(d, id, None);
                    d.create_person(&form)
                } else {
                    let f = d.new_family();
                    self.pinned.insert(f.clone());
                    f
                };
                self.map(id, &x);
            }

            let shown: HashSet<String> = self.node_of.keys().cloned().collect();
            let fam_nodes: Vec<(NodeId, String)> = self
                .xref_of
                .iter()
                .filter(|(id, _)| self.graph.node(**id).is_some_and(|n| n.template == family_t))
                .map(|(id, x)| (*id, x.clone()))
                .collect();
            for (fid, fx) in fam_nodes {
                let mut partner_links: Vec<_> = self
                    .graph
                    .connections()
                    .filter(|c| c.to.node == fid && c.to.socket == "partners")
                    .map(|c| (c.order, c.id, c.from.node))
                    .collect();
                partner_links.sort_by_key(|l| l.0);
                let wanted: Vec<(nodez::ConnectionId, String)> = partner_links
                    .into_iter()
                    .filter_map(|(_, cid, n)| self.xref_of.get(&n).map(|x| (cid, x.clone())))
                    .collect();
                let current: Vec<String> = d.husband(&fx).into_iter().chain(d.wife(&fx)).filter(|x| shown.contains(x)).collect();
                for x in &current {
                    if !wanted.iter().any(|(_, w)| w == x) {
                        d.detach(&fx, x);
                    }
                }
                for (cid, x) in &wanted {
                    if !current.contains(x)
                        && let Err(e) = d.add_partner(&fx, x) {
                            problems.push(e);
                            bad_links.push(*cid);
                        }
                }
                // A Couple node wired to partners who already have a family
                // stands for that family rather than a duplicate.
                let fx = if direct && d.children(&fx).is_empty() {
                    let (h, w) = (d.husband(&fx), d.wife(&fx));
                    let other = d
                        .spouse_families(h.as_deref().or(w.as_deref()).unwrap_or(""))
                        .into_iter()
                        .find(|f| *f != fx && d.husband(f) == h && d.wife(f) == w && h.is_some() && w.is_some());
                    match other {
                        Some(o) => {
                            d.delete_family(&fx);
                            self.pinned.remove(&fx);
                            self.pinned.insert(o.clone());
                            self.node_of.remove(&fx);
                            self.map(fid, &o);
                            o
                        }
                        None => fx,
                    }
                } else {
                    fx
                };
                if direct {
                    let married = model::normalize_date(&self.param(fid, "married"));
                    let (date, place) = d.family_event(&fx, "MARR").unwrap_or_default();
                    if model::normalize_date(&model::pretty_date(&date)) != married {
                        d.set_family_event(&fx, "MARR", &married, &place);
                    }
                    continue;
                }

                let wanted_kids: Vec<String> = self
                    .graph
                    .connections()
                    .filter(|c| c.from.node == fid && c.from.socket == "children")
                    .filter_map(|c| self.xref_of.get(&c.to.node).cloned())
                    .collect();
                let current_kids: Vec<String> = d.children(&fx).into_iter().filter(|x| shown.contains(x)).collect();
                for k in &current_kids {
                    // A child wired to another family only lost *this* link if
                    // the graph shows this family as their first.
                    let first = d.parent_families(k).into_iter().find(|f| shown.contains(f));
                    if !wanted_kids.contains(k) && first.as_deref() == Some(fx.as_str()) {
                        d.detach(&fx, k);
                    }
                }
                for k in &wanted_kids {
                    if !current_kids.contains(k) {
                        d.add_child_to(&fx, k);
                    }
                }

                let married = model::normalize_date(&self.param(fid, "married"));
                let (date, place) = d.family_event(&fx, "MARR").unwrap_or_default();
                if model::normalize_date(&model::pretty_date(&date)) != married {
                    d.set_family_event(&fx, "MARR", &married, &place);
                }
            }

            let person_nodes: Vec<(NodeId, String)> = self
                .xref_of
                .iter()
                .filter(|(id, _)| self.graph.node(**id).is_some_and(|n| n.template == person_t))
                .map(|(id, x)| (*id, x.clone()))
                .collect();
            if direct {
                // Each child's Father and Mother wires decide their parents.
                let wired = |node: NodeId, socket: &str| {
                    self.graph
                        .connections()
                        .find(|c| c.to.node == node && c.to.socket == socket)
                        .and_then(|c| self.xref_of.get(&c.from.node).cloned())
                };
                for (id, x) in &person_nodes {
                    let fam = d.parent_families(x).into_iter().next();
                    let (cur_f, cur_m) = match &fam {
                        Some(f) => (d.husband(f), d.wife(f)),
                        None => (None, None),
                    };
                    // A parent outside the drawn neighbourhood isn't wired but stays.
                    let keep = |p: Option<String>| p.filter(|p| !shown.contains(p));
                    let father = wired(*id, "father").or_else(|| keep(cur_f.clone()));
                    let mother = wired(*id, "mother").or_else(|| keep(cur_m.clone()));
                    if (father.clone(), mother.clone()) != (cur_f, cur_m) {
                        d.set_parents(x, father.as_deref(), mother.as_deref());
                    }
                }
            }
            for (id, x) in person_nodes {
                let form = self.form_from_node(d, id, Some(&x));
                if form != d.form_for(&x) {
                    d.apply_form(&x, &form);
                }
            }
        });
        for cid in bad_links {
            self.graph.disconnect(cid);
        }

        // A deletion can take an emptied family with it; if the mapping went
        // stale, rebuild, otherwise just refresh titles. In Direct mode a
        // rewired parent can also make a Couple node appear or vanish.
        let rewired = direct && structural;
        let stale = rewired || self.xref_of.values().any(|x| doc.record(x).is_none());
        if stale {
            self.synced_rev = None;
        } else {
            let ids: Vec<(NodeId, String)> = self.xref_of.iter().map(|(i, x)| (*i, x.clone())).collect();
            for (id, x) in ids {
                if doc.person(&x).is_some() {
                    let title = doc.person(&x).map(|p| p.display.clone()).unwrap_or_default();
                    if let Some(n) = self.graph.node_mut(id) {
                        n.title = title;
                    }
                } else {
                    let title = family_title(doc, &x);
                    if let Some(n) = self.graph.node_mut(id) {
                        n.title = title;
                    }
                }
            }
            self.synced_rev = Some(doc.revision);
        }
        problems
    }

    // ---- the view ------------------------------------------------------------------------

    pub fn show(&mut self, ui: &mut Ui, doc: &mut Document, selected: Option<&str>, actions: &mut Vec<Action>) {
        self.apply_theme(ui);
        let p = Theme::current(ui.ctx()).palette;

        let out_of_scope = self.limited && selected.is_some_and(|s| !self.node_of.contains_key(s));
        if self.synced_rev != Some(doc.revision) || out_of_scope {
            self.rebuild(doc, selected);
        }

        let mut edited = false;
        let k = self.mode.index();
        ui.horizontal(|ui| {
            let mut mode = k;
            ui.add(
                SegmentedControl::from_segments(
                    &mut mode,
                    [
                        Segment::text(LinkMode::Families.label()).hover_text("Parents wire into a Family node, which wires to each child"),
                        Segment::text(LinkMode::Direct.label()).hover_text("Parents wire straight into each child's Father and Mother"),
                    ],
                )
                .size(SegmentedSize::Small)
                .id_salt("graph_link_mode"),
            );
            if mode != k {
                self.set_mode(if mode == 0 { LinkMode::Families } else { LinkMode::Direct });
                ui.ctx().request_repaint();
                return;
            }
            ui.add_space(8.0);
            if ui.add(Button::new(format!("{}  Person", glyphs::PLUS)).size(ButtonSize::Small)).on_hover_text("Add a person node (or Shift+A on the canvas)").clicked() {
                let pos = self.editor.view_center() + vec2(-100.0, -40.0);
                let id = self.graph.add_node(&self.kits[k].library, self.kits[k].person, pos);
                if let Some(n) = self.graph.node_mut(id) {
                    n.title = "New person".into();
                }
                self.editor.state.select_only(id);
                edited = true;
            }
            let (label, tip) = match self.mode {
                LinkMode::Families => ("Family", "Add a family node"),
                LinkMode::Direct => ("Couple", "Add a couple: partners without children, or to record a marriage"),
            };
            if ui.add(Button::new(format!("{}  {label}", glyphs::PLUS)).accent(elegance::Accent::Amber).size(ButtonSize::Small)).on_hover_text(tip).clicked() {
                let pos = self.editor.view_center() + vec2(-90.0, -30.0);
                let id = self.graph.add_node(&self.kits[k].library, self.kits[k].family, pos);
                self.editor.state.select_only(id);
                edited = true;
            }
            if ui.add(Button::new("Auto layout").outline().size(ButtonSize::Small)).on_hover_text("Arrange generations into columns").clicked() {
                self.auto_layout();
                self.remember_positions();
                self.needs_fit = true;
            }
            if ui.add(Button::new(format!("{}  Fit", glyphs::ZOOM_OUT)).outline().size(ButtonSize::Small)).clicked() {
                self.needs_fit = true;
            }
            if self.limited {
                let who = self.scope_root.as_deref().and_then(|x| doc.person(x)).map(|p| p.display.clone()).unwrap_or_default();
                let shown = self.node_of.keys().filter(|x| doc.person(x).is_some()).count();
                let text = format!("{shown} of {} people · within {} of {who}{}", doc.people().len(), self.reach, if self.capped { " (nearest)" } else { "" });
                let badge = ui.add(Badge::new(text, BadgeTone::Info).preserve_case());
                if self.capped {
                    badge.on_hover_text(format!("The tree is big here: only the nearest {NEIGHBOURHOOD_MAX} people and families are drawn."));
                }
                ui.label(RichText::new("Reach").size(12.0).color(p.text_muted));
                let mut i = REACH_CHOICES.iter().position(|r| *r == self.reach).unwrap_or(1);
                ui.add(SegmentedControl::new(&mut i, REACH_CHOICES.map(|r| r.to_string())).size(SegmentedSize::Small).id_salt("graph_reach"))
                    .on_hover_text("How many people away from the selected person to show. Parents, children and partners are 1 away; 2 adds grandparents, grandchildren, siblings and in-laws; and so on.");
                if REACH_CHOICES[i] != self.reach {
                    self.set_reach(REACH_CHOICES[i]);
                    ui.ctx().request_repaint();
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let hint = match self.mode {
                    LinkMode::Families => "Person → Partners makes a couple · Children → Parents adds a child · Del removes",
                    LinkMode::Direct => "Person → a child's Father or Mother · Person → Couple for partners · Del removes",
                };
                let hint = RichText::new(hint)
                    .size(12.0)
                    .color(p.text_faint);
                if ui.available_width() > 560.0 {
                    ui.label(hint);
                } else {
                    ui.label(RichText::new(glyphs::INFO.to_string()).color(p.text_faint)).on_hover_text(hint.text());
                }
            });
        });
        ui.add_space(8.0);

        // A mode switch in the toolbar clears the canvas; rebuild it now.
        if self.synced_rev != Some(doc.revision) {
            self.rebuild(doc, selected);
        }
        let k = self.mode.index();
        let rect = ui.available_rect_before_wrap();
        if self.needs_fit && self.graph.node_count() > 0 {
            self.editor.fit_to_graph(rect, &self.graph, &self.kits[k].library);
            self.editor.state.zoom = self.editor.state.zoom.min(1.0);
            self.needs_fit = false;
        }

        // Follow the app's selection.
        if selected != self.last_selected.as_deref() {
            if let Some(&id) = selected.and_then(|s| self.node_of.get(s))
                && !self.editor.state.is_selected(id) {
                    self.editor.state.select_only(id);
                }
            self.last_selected = selected.map(str::to_string);
        }

        let response = self.editor.show_in(ui, rect, &self.kits[k].library, &mut self.graph);
        if self.graph.node_count() == 0 {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "Empty canvas — add a Person, or press Shift+A",
                egui::FontId::proportional(16.0),
                p.text_faint,
            );
        }

        let mut param_nodes = HashSet::new();
        let mut structural = edited;
        for a in &response.actions {
            match a {
                EditorAction::ParamChanged { node, .. } | EditorAction::InputChanged { node, .. } => {
                    param_nodes.insert(*node);
                }
                EditorAction::ConnectionRejected(e) => {
                    let msg = match e {
                        nodez::ConnectError::WouldCycle => "That would make someone their own ancestor.".to_string(),
                        nodez::ConnectError::TypeMismatch { .. } => match self.mode {
                            LinkMode::Families => "Wire people into a family's Partners, and a family's Children into a person's Parents.".into(),
                            LinkMode::Direct => "Wire a person into a child's Father or Mother, or into a Couple's Partners.".into(),
                        },
                        other => other.to_string(),
                    };
                    Toast::new("Can't connect those").tone(BadgeTone::Warning).description(msg).show(ui.ctx());
                }
                EditorAction::SelectionChanged => {
                    let sel: Vec<NodeId> = self.editor.state.selection().collect();
                    if let [one] = sel.as_slice()
                        && let Some(x) = self.xref_of.get(one)
                            && doc.person(x).is_some() && selected != Some(x.as_str()) {
                                self.last_selected = Some(x.clone());
                                actions.push(Action::Select(x.clone()));
                            }
                }
                EditorAction::NodesMoved(_) => {}
                a if a.is_edit() => structural = true,
                _ => {}
            }
        }
        if structural || !param_nodes.is_empty() {
            let merge = (!structural && param_nodes.len() == 1).then(|| format!("graph-param-{:?}", param_nodes.iter().next()));
            let removed_people = self.xref_of.iter().filter(|(id, x)| !self.graph.contains_node(**id) && doc.person(x).is_some()).count();
            let problems = self.sync_to_doc(doc, merge);
            for msg in problems {
                Toast::new("Can't add that partner").tone(BadgeTone::Warning).description(msg).show(ui.ctx());
            }
            if removed_people > 0 {
                Toast::new(format!("Deleted {removed_people} {}", if removed_people == 1 { "person" } else { "people" }))
                    .description("Undo with Ctrl+Z.")
                    .show(ui.ctx());
            }
        }
        if response.any(|a| matches!(a, EditorAction::NodesMoved(_))) {
            self.remember_positions();
        }
    }
}

fn kit(mode: LinkMode) -> Kit {
    let mut types = TypeRegistry::new();
    let person_ty = types.register(
        DataTypeBuilder::new("Person", Color32::from_rgb(0x60, 0xA5, 0xFA)).description("A person"),
    );
    let family_ty = types.register(
        DataTypeBuilder::new("Family", Color32::from_rgb(0xF5, 0xB0, 0x4A))
            .shape(SocketShape::Diamond)
            .description("A family, whose children list it as their parents"),
    );
    let mut library = NodeLibrary::with_types(types);
    let text = |hint: &str| Widget::Text { multiline: false, hint: hint.into() };
    let mut person = NodeTemplate::new("person", "Person")
        .category("People")
        .width(210.0)
        .description("Someone in the tree")
        .keywords(["individual", "indi"]);
    person = match mode {
        LinkMode::Families => person
            .input(SocketSpec::new("parents", family_ty).label("Parents").optional().description("The family this person was born into"))
            .output(SocketSpec::new("person", person_ty).label("Person").description("Wire into a family's Partners")),
        LinkMode::Direct => person
            .input(SocketSpec::new("father", person_ty).label("Father").optional().description("Wire the father's Person output here"))
            .input(SocketSpec::new("mother", person_ty).label("Mother").optional().description("Wire the mother's Person output here"))
            .output(SocketSpec::new("person", person_ty).label("Person").description("Wire into a child's Father or Mother, or a Couple's Partners")),
    };
    let person = library.register(
        person
            .param(ParamSpec::new("given", text("Given names")).label("Given"))
            .param(ParamSpec::new("surname", text("Surname")).label("Surname"))
            .param(ParamSpec::new("sex", Widget::combo(SEXES)).label("Sex").default_value(Value::Choice("Unknown".into())))
            .param(ParamSpec::new("born", text("e.g. 1850")).label("Born"))
            .param(ParamSpec::new("died", text("")).label("Died")),
    );
    let partners = SocketSpec::new("partners", person_ty).label("Partners").multi().description("Up to two partners");
    let family = library.register(match mode {
        LinkMode::Families => NodeTemplate::new("family", "Family")
            .category("Families")
            .width(180.0)
            .description("A couple and their children")
            .keywords(["marriage", "couple", "fam"])
            .input(partners)
            .output(SocketSpec::new("children", family_ty).label("Children").description("Wire into each child's Parents"))
            .param(ParamSpec::new("married", text("date")).label("Married")),
        LinkMode::Direct => NodeTemplate::new("couple", "Couple")
            .category("Families")
            .width(180.0)
            .description("Two partners: for couples without children, or to record a marriage")
            .keywords(["marriage", "family", "partners"])
            .input(partners)
            .param(ParamSpec::new("married", text("date")).label("Married")),
    });
    Kit { library, person, family }
}

fn family_title(doc: &Document, fam: &str) -> String {
    let surname = |x: Option<String>| x.and_then(|x| doc.person(&x).map(|p| if p.surname.is_empty() { p.given.clone() } else { p.surname.clone() }));
    match (surname(doc.husband(fam)), surname(doc.wife(fam))) {
        (Some(a), Some(b)) if a == b => format!("{a} family"),
        (Some(a), Some(b)) => format!("{a} & {b}"),
        (Some(a), None) | (None, Some(a)) => format!("{a} family"),
        (None, None) => "Family".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nodez::SocketRef;

    fn person(g: &mut FamilyGraph, given: &str, sex: &str) -> NodeId {
        let id = g.graph.add_node(&g.kits[0].library, g.kits[0].person, Pos2::ZERO);
        let n = g.graph.node_mut(id).unwrap();
        n.set_param("given", given);
        n.set_param("surname", "Test");
        n.set_param("sex", Value::Choice(sex.into()));
        id
    }

    #[test]
    fn canvas_edits_reach_the_document() {
        let mut doc = Document::new_empty();
        let mut g = FamilyGraph::new();
        g.rebuild(&doc, None);

        let dad = person(&mut g, "Tom", "Male");
        let mom = person(&mut g, "Ann", "Female");
        let kid = person(&mut g, "Kit", "Unknown");
        let fam = g.graph.add_node(&g.kits[0].library, g.kits[0].family, Pos2::ZERO);
        g.graph.connect(&g.kits[0].library, (dad, "person"), (fam, "partners")).unwrap();
        g.graph.connect(&g.kits[0].library, (mom, "person"), (fam, "partners")).unwrap();
        g.graph.connect(&g.kits[0].library, (fam, "children"), (kid, "parents")).unwrap();
        assert!(g.sync_to_doc(&mut doc, None).is_empty());

        let x = |g: &FamilyGraph, id| g.xref_of[&id].clone();
        assert_eq!(doc.people().len(), 3);
        assert_eq!(doc.father(&x(&g, kid)), Some(x(&g, dad)));
        assert_eq!(doc.mother(&x(&g, kid)), Some(x(&g, mom)));
        assert_eq!(doc.person(&x(&g, mom)).unwrap().display, "Ann Test");

        // A third partner is refused and the wire taken back out.
        let extra = person(&mut g, "Eve", "Female");
        g.graph.connect(&g.kits[0].library, (extra, "person"), (fam, "partners")).unwrap();
        assert_eq!(g.sync_to_doc(&mut doc, None).len(), 1);
        assert_eq!(g.graph.connections().filter(|c| c.to.node == fam && c.to.socket == "partners").count(), 2);

        // A family can't be wired into one of its own partners (a cycle).
        assert!(g.graph.connect(&g.kits[0].library, (fam, "children"), (dad, "parents")).is_err());

        // Editing a parameter renames the person, merging keystrokes into one undo step.
        g.graph.node_mut(kid).unwrap().set_param("given", "Ki");
        g.sync_to_doc(&mut doc, Some("k".into()));
        g.graph.node_mut(kid).unwrap().set_param("given", "Kim");
        g.sync_to_doc(&mut doc, Some("k".into()));
        assert_eq!(doc.person(&x(&g, kid)).unwrap().given, "Kim");
        doc.undo();
        assert_eq!(doc.person(&x(&g, kid)).unwrap().given, "Kit");
        doc.redo();

        // Unwiring a child detaches them.
        g.graph.disconnect_socket(&SocketRef::new(kid, "parents"));
        g.sync_to_doc(&mut doc, None);
        assert_eq!(doc.father(&x(&g, kid)), None);

        // Deleting a node deletes the person.
        let mom_x = x(&g, mom);
        g.graph.remove_node(mom);
        g.sync_to_doc(&mut doc, None);
        assert!(doc.person(&mom_x).is_none());
        assert_eq!(doc.people().len(), 3);

        // A rebuild from the document reproduces the same wiring.
        g.rebuild(&doc, None);
        assert_eq!(g.graph.node_count(), 4);
        assert_eq!(g.graph.connection_count(), 1);
    }

    fn direct_person(g: &mut FamilyGraph, given: &str, sex: &str) -> NodeId {
        let id = g.graph.add_node(&g.kits[1].library, g.kits[1].person, Pos2::ZERO);
        let n = g.graph.node_mut(id).unwrap();
        n.set_param("given", given);
        n.set_param("surname", "Test");
        n.set_param("sex", Value::Choice(sex.into()));
        id
    }

    #[test]
    fn direct_links_find_or_create_families() {
        let mut doc = Document::new_empty();
        let mut g = FamilyGraph::new();
        g.set_mode(LinkMode::Direct);
        g.rebuild(&doc, None);
        fn wire(g: &mut FamilyGraph, from: NodeId, to: NodeId, socket: &str) {
            g.graph.connect(&g.kits[1].library, (from, "person"), (to, socket)).unwrap();
        }

        let dad = direct_person(&mut g, "Tom", "Male");
        let mom = direct_person(&mut g, "Ann", "Female");
        let kid = direct_person(&mut g, "Kit", "Unknown");
        let kid2 = direct_person(&mut g, "Kim", "Female");
        wire(&mut g, dad, kid, "father");
        wire(&mut g, mom, kid, "mother");
        wire(&mut g, dad, kid2, "father");
        wire(&mut g, mom, kid2, "mother");
        assert!(g.sync_to_doc(&mut doc, None).is_empty());
        let x = |g: &FamilyGraph, id| g.xref_of[&id].clone();
        assert_eq!(doc.family_count(), 1, "both children share one family");
        assert_eq!(doc.father(&x(&g, kid2)), Some(x(&g, dad)));
        assert_eq!(doc.siblings(&x(&g, kid)), vec![x(&g, kid2)]);

        // A new mother for the second child moves only that child.
        let mom2 = direct_person(&mut g, "Eve", "Female");
        g.graph.disconnect_socket(&SocketRef::new(kid2, "mother"));
        wire(&mut g, mom2, kid2, "mother");
        g.sync_to_doc(&mut doc, None);
        assert_eq!(doc.family_count(), 2);
        assert_eq!(doc.mother(&x(&g, kid2)), Some(x(&g, mom2)));
        assert_eq!(doc.mother(&x(&g, kid)), Some(x(&g, mom)));

        // Unwiring both parents leaves Tom & Eve as a couple without children.
        g.graph.disconnect_socket(&SocketRef::new(kid2, "father"));
        g.graph.disconnect_socket(&SocketRef::new(kid2, "mother"));
        g.sync_to_doc(&mut doc, None);
        assert_eq!(doc.father(&x(&g, kid2)), None);
        assert_eq!(doc.family_count(), 2, "Tom & Eve stay a couple");
        let (dad_x, mom_x, mom2_x, kid_x) = (x(&g, dad), x(&g, mom), x(&g, mom2), x(&g, kid));

        // A rebuild shows that childless couple as a Couple node, not Tom & Ann.
        g.rebuild(&doc, None);
        let couples: Vec<String> = g.graph.nodes().filter(|n| n.template == g.kits[1].family).map(|n| g.xref_of[&n.id].clone()).collect();
        assert_eq!(couples.len(), 1);
        assert_eq!(doc.wife(&couples[0]), Some(mom2_x.clone()));

        // A Couple node for Tom & Ann reuses their family rather than adding one.
        let couple = g.graph.add_node(&g.kits[1].library, g.kits[1].family, Pos2::ZERO);
        g.graph.node_mut(couple).unwrap().set_param("married", "1901");
        let (d, m) = (g.node_of[&dad_x], g.node_of[&mom_x]);
        wire(&mut g, d, couple, "partners");
        wire(&mut g, m, couple, "partners");
        g.sync_to_doc(&mut doc, None);
        assert_eq!(doc.family_count(), 2);
        let fam = doc.parent_families(&kid_x)[0].clone();
        assert_eq!(doc.family_event(&fam, "MARR").map(|m| m.0), Some("1901".into()));

        // Deleting that Couple node keeps the family: it still has a child.
        g.graph.remove_node(couple);
        g.sync_to_doc(&mut doc, None);
        assert_eq!(doc.father(&kid_x), Some(dad_x));
        assert_eq!(doc.family_count(), 2);
    }
}
