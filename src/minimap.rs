//! The profile's "Family map": a compact hourglass of the person's
//! ancestors (above) and descendants (below), drawn as avatar dots.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, pos2, vec2};
use elegance::{Button, ButtonSize, Card, Theme, glyphs};

use crate::app::Action;
use crate::homepath::{self, HomePath};
use crate::model::{Document, PersonSummary, Relation};
use crate::tidy::{self, TidyNode};
use crate::widgets::{mix, paint_avatar, sex_color};

/// Most ancestor generations drawn (parents = 1).
const MAX_UP: usize = 3;
const MAX_CHILDREN: usize = 10;
const MAX_GRANDCHILDREN_EACH: usize = 6;
const ROW_H: f32 = 50.0;
const MAX_WIDTH: f32 = 480.0;
/// Walks over huge files stop counting here.
const COUNT_CAP: usize = 20_000;

#[derive(Clone, Copy, Default)]
struct Counts {
    ancestors: usize,
    generations_up: usize,
    descendants: usize,
    generations_down: usize,
}

fn counts(doc: &Document, xref: &str) -> Counts {
    let mut c = Counts::default();
    let mut seen = HashSet::from([xref.to_string()]);
    let mut level = vec![xref.to_string()];
    while !level.is_empty() && seen.len() < COUNT_CAP {
        let next: Vec<String> = level
            .iter()
            .flat_map(|x| doc.father(x).into_iter().chain(doc.mother(x)))
            .filter(|x| seen.insert(x.clone()))
            .collect();
        if !next.is_empty() {
            c.generations_up += 1;
            c.ancestors += next.len();
        }
        level = next;
    }
    let mut level = vec![xref.to_string()];
    while !level.is_empty() && seen.len() < COUNT_CAP {
        let next: Vec<String> = level
            .iter()
            .flat_map(|x| doc.spouse_families(x).into_iter().flat_map(|f| doc.children(&f)))
            .filter(|x| seen.insert(x.clone()))
            .collect();
        if !next.is_empty() {
            c.generations_down += 1;
            c.descendants += next.len();
        }
        level = next;
    }
    c
}

fn summary(c: Counts) -> String {
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if c.generations_up > 0 {
        parts.push(format!("{} back", plural(c.generations_up, "generation", "generations")));
        parts.push(plural(c.ancestors, "ancestor", "ancestors"));
    } else {
        parts.push("no ancestors yet".into());
    }
    if c.descendants > 0 {
        parts.push(plural(c.descendants, "descendant", "descendants"));
    } else {
        parts.push("no descendants yet".into());
    }
    parts.join(" · ")
}

pub fn family_map(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    let key = egui::Id::new(("family_map_counts", xref));
    let cached: Option<(u64, Arc<Counts>)> = ui.data(|d| d.get_temp(key));
    let c = match cached {
        Some((rev, c)) if rev == doc.revision => *c,
        _ => {
            let c = counts(doc, xref);
            ui.data_mut(|d| d.insert_temp(key, (doc.revision, Arc::new(c))));
            c
        }
    };

    Card::new().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Family map").size(16.0).color(p.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let open = ui.add(Button::new(format!("{}  Open tree", glyphs::NETWORK)).outline().size(ButtonSize::Small));
                if open.on_hover_text("Ancestors and descendants together").clicked() {
                    actions.push(Action::OpenTreeBoth);
                }
            });
        });
        ui.label(egui::RichText::new(summary(c)).size(12.5).color(p.text_muted));
        ui.add_space(6.0);
        draw(ui, doc, xref, actions, MapOptions::profile());
    });
}

/// What to draw and how much room to take.
#[derive(Clone, Copy, Debug)]
pub struct MapOptions {
    /// Ancestor generations to show at most.
    pub max_up: usize,
    /// Dashed "add a parent" circles where a parent is unknown.
    pub empty_slots: bool,
    pub descendants: bool,
    /// Clicking a circle selects that person.
    pub interactive: bool,
    pub max_width: f32,
    /// Shrink to fit this height too (deep pedigrees).
    pub max_height: Option<f32>,
}

impl MapOptions {
    /// The profile's Family map.
    pub fn profile() -> Self {
        Self { max_up: MAX_UP, empty_slots: true, descendants: true, interactive: true, max_width: MAX_WIDTH, max_height: None }
    }

    /// Every known generation of ancestors, shrunk to fit a box.
    pub fn lineage(width: f32, height: f32) -> Self {
        Self { max_up: usize::MAX, empty_slots: false, descendants: false, interactive: false, max_width: width, max_height: Some(height) }
    }
}

/// Dots drawn at most, however deep the pedigree (royal lines collapse
/// onto the same ancestors many times over).
const MAX_DOTS: usize = 1500;

/// "19 generations back · 249 ancestors · 4 descendants", cached per tree revision.
pub fn summary_line(ctx: &egui::Context, doc: &Document, xref: &str) -> String {
    let key = egui::Id::new(("family_map_counts", xref));
    let cached: Option<(u64, Arc<Counts>)> = ctx.data(|d| d.get_temp(key));
    let c = match cached {
        Some((rev, c)) if rev == doc.revision => *c,
        _ => {
            let c = counts(doc, xref);
            ctx.data_mut(|d| d.insert_temp(key, (doc.revision, Arc::new(c))));
            c
        }
    };
    summary(c)
}

struct Map<'a> {
    doc: &'a Document,
    actions: &'a mut Vec<Action>,
    interactive: bool,
    home: Option<&'a str>,
}

impl Map<'_> {
    fn node(&mut self, ui: &mut Ui, center: Pos2, radius: f32, xref: &str, is_root: bool) {
        let p = Theme::current(ui.ctx()).palette;
        let Some(person) = self.doc.person(xref) else { return };
        let hit = Rect::from_center_size(center, vec2(radius * 2.0 + 4.0, radius * 2.0 + 4.0));
        let resp = ui.interact(hit, ui.id().with(("map", xref, center.x as i32, center.y as i32)), Sense::click());
        if is_root {
            // Marked by a glow and ring, not by size, so the tree keeps its shape.
            ui.painter().circle_filled(center, radius + 5.0, p.focus.gamma_multiply(0.18));
            ui.painter().circle_stroke(center, radius + 2.5, Stroke::new(1.75, p.focus));
        } else if resp.hovered() {
            ui.painter().circle_stroke(center, radius + 3.0, Stroke::new(1.5, mix(p.border, p.focus, 0.7)));
        } else if self.home == Some(xref) {
            ui.painter().circle_stroke(center, radius + 2.5, Stroke::new(1.75, homepath::color(&p)));
        }
        if radius >= 10.0 {
            paint_avatar(ui, center, radius, person);
        } else {
            let base = sex_color(&p, person.sex);
            ui.painter().circle(center, radius, mix(p.card, base, if p.is_dark { 0.45 } else { 0.32 }), Stroke::new(1.0, mix(p.card, base, 0.65)));
        }
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_ui(|ui| hover_card(ui, person));
        if resp.clicked() && !is_root && self.interactive {
            self.actions.push(Action::Select(xref.to_string()));
        }
    }

    fn empty_slot(&mut self, ui: &mut Ui, center: Pos2, radius: f32, child: &str, rel: Relation) {
        let p = Theme::current(ui.ctx()).palette;
        let hit = Rect::from_center_size(center, vec2(radius * 2.0 + 4.0, radius * 2.0 + 4.0));
        let resp = ui.interact(hit, ui.id().with(("map_add", child, rel.label())), Sense::click());
        let color = if resp.hovered() { p.focus } else { mix(p.border, p.text_faint, 0.4) };
        let pts: Vec<Pos2> = (0..=24)
            .map(|i| {
                let a = i as f32 / 24.0 * std::f32::consts::TAU;
                center + vec2(a.cos(), a.sin()) * radius
            })
            .collect();
        ui.painter().extend(egui::Shape::dashed_line(&pts, Stroke::new(1.0, color), 3.0, 3.0));
        if radius >= 9.0 {
            ui.painter().text(center, Align2::CENTER_CENTER, glyphs::PLUS.to_string(), FontId::proportional(radius * 0.8), color);
        }
        let who = self.doc.person(child).map(|p| p.display.as_str()).unwrap_or("");
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(format!("Add {} of {who}", rel.label().to_lowercase()));
        if resp.clicked() {
            self.actions.push(Action::AddRelative(child.to_string(), rel));
        }
    }
}

fn hover_card(ui: &mut Ui, person: &PersonSummary) {
    let p = Theme::current(ui.ctx()).palette;
    ui.label(egui::RichText::new(&person.display).size(14.0).family(crate::fonts::semibold()).color(p.text));
    let life = person.lifespan();
    if !life.is_empty() {
        ui.label(egui::RichText::new(life).size(12.0).color(p.text_muted));
    }
}

/// Right-angled connector, as in the Tree view: a trunk from `from` to a
/// horizontal bus halfway to the branches, then a drop to each branch.
/// `path` picks out the branch on the way home, drawn over the rest.
fn elbow(ui: &Ui, from: Pos2, branches: &[(Pos2, Color32)], trunk: Color32, path: Option<(usize, Color32)>) {
    let Some(first) = branches.first() else { return };
    let painter = ui.painter();
    let snap = |p: Pos2| pos2(painter.round_to_pixel_center(p.x), painter.round_to_pixel_center(p.y));
    let from = snap(from);
    let bus_y = painter.round_to_pixel_center((from.y + first.0.y) / 2.0);
    let stroke = |c| Stroke::new(1.0, c);
    painter.line_segment([from, pos2(from.x, bus_y)], stroke(trunk));
    let lo = branches.iter().map(|b| b.0.x).fold(from.x, f32::min);
    let hi = branches.iter().map(|b| b.0.x).fold(from.x, f32::max);
    painter.line_segment([snap(pos2(lo, bus_y)), snap(pos2(hi, bus_y))], stroke(trunk));
    for (to, color) in branches {
        let to = snap(*to);
        painter.line_segment([pos2(to.x, bus_y), to], stroke(*color));
    }
    if let Some((i, color)) = path
        && let Some((to, _)) = branches.get(i)
    {
        let to = snap(*to);
        let hl = Stroke::new(2.0, color);
        painter.line_segment([from, pos2(from.x, bus_y)], hl);
        painter.line_segment([pos2(from.x, bus_y), pos2(to.x, bus_y)], hl);
        painter.line_segment([pos2(to.x, bus_y), to], hl);
    }
}

/// Up to `max` of `all`, always including `keep` (the way home) when it's
/// there. Returns those shown and how many were left out.
fn pick(all: &[String], max: usize, keep: Option<&String>) -> (Vec<String>, usize) {
    let mut shown: Vec<String> = all.iter().take(max).cloned().collect();
    if let Some(k) = keep
        && all.contains(k)
        && !shown.contains(k)
    {
        shown.pop();
        // Keep birth order: put it where it falls among those shown.
        let at = all.iter().position(|x| x == k).unwrap_or(0).min(shown.len());
        shown.insert(at, k.clone());
    }
    let hidden = all.len() - shown.len();
    (shown, hidden)
}

/// A circle in the map's layout.
enum Dot {
    Person(String),
    /// A missing parent, offered as a place to add one.
    Empty { child: String, rel: Relation },
    /// "+N" for relatives not drawn.
    More(usize),
    /// The point children hang from (the couple link).
    Hub,
}

#[derive(Default)]
struct DotTree {
    nodes: Vec<TidyNode>,
    dots: Vec<Dot>,
    generation: Vec<usize>,
}

impl DotTree {
    fn push(&mut self, dot: Dot, size: f32, generation: usize) -> usize {
        self.nodes.push(TidyNode::new(size));
        self.dots.push(dot);
        self.generation.push(generation);
        self.nodes.len() - 1
    }
}

fn ancestor_radius(g: usize) -> f32 {
    match g {
        0 => 12.0,
        1 => 11.0,
        2 => 9.0,
        _ => 7.0,
    }
}

/// How many generations of known ancestors `x` has. Memoised: a pedigree
/// that collapses onto the same ancestors is visited once per person, not
/// once per path.
fn generations_up(doc: &Document, x: &str, memo: &mut HashMap<String, usize>, active: &mut HashSet<String>) -> usize {
    if let Some(&d) = memo.get(x) {
        return d;
    }
    if !active.insert(x.to_string()) {
        return 0; // Someone recorded as their own ancestor.
    }
    let d = [doc.father(x), doc.mother(x)]
        .into_iter()
        .flatten()
        .map(|p| 1 + generations_up(doc, &p, memo, active))
        .max()
        .unwrap_or(0);
    active.remove(x);
    memo.insert(x.to_string(), d);
    d
}

#[allow(clippy::too_many_arguments)]
fn push_ancestors(t: &mut DotTree, doc: &Document, x: &str, i: usize, g: usize, up: usize, empty_slots: bool, path: &mut Vec<String>) {
    if g >= up || path.iter().any(|p| p == x) || t.nodes.len() >= MAX_DOTS {
        return;
    }
    path.push(x.to_string());
    for (parent, rel) in [(doc.father(x), Relation::Father), (doc.mother(x), Relation::Mother)] {
        let size = ancestor_radius(g + 1) * 2.0;
        let c = match parent {
            Some(p) if path.contains(&p) => continue,
            Some(p) => {
                let c = t.push(Dot::Person(p.clone()), size, g + 1);
                push_ancestors(t, doc, &p, c, g + 1, up, empty_slots, path);
                c
            }
            None if empty_slots => t.push(Dot::Empty { child: x.to_string(), rel }, size, g + 1),
            None => continue,
        };
        t.nodes[i].children.push(c);
    }
    path.pop();
}

const DOT_GAP: f32 = 8.0;
const CHILD_R: f32 = 10.0;
const GRANDCHILD_R: f32 = 6.0;
const MORE_W: f32 = 26.0;
const SPOUSE_GAP: f32 = 34.0;
const SPOUSE_R: f32 = 11.0;

pub fn draw(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>, opts: MapOptions) {
    let p = Theme::current(ui.ctx()).palette;
    let line = mix(p.border, p.text_faint, 0.35);
    let faint = mix(line, p.card, 0.5);
    let home_color = homepath::color(&p);
    let path: Option<std::sync::Arc<HomePath>> = if opts.interactive { homepath::find(ui.ctx(), doc, xref) } else { None };
    let path = path.as_deref();
    let joins = |a: &str, b: &str| path.is_some_and(|h| h.joins(a, b));
    let down = path.map(|h| h.down_line()).unwrap_or(&[]);

    // Ancestors, laid out compactly around the root at x = 0: every known
    // generation, plus a row of "add" slots where those are wanted.
    let known = generations_up(doc, xref, &mut HashMap::new(), &mut HashSet::new());
    let up = if opts.empty_slots { known.min(opts.max_up.saturating_sub(1)) + 1 } else { known.min(opts.max_up) };
    let mut anc = DotTree::default();
    let root = anc.push(Dot::Person(xref.to_string()), ancestor_radius(0) * 2.0, 0);
    push_ancestors(&mut anc, doc, xref, root, 0, up, opts.empty_slots, &mut Vec::new());
    let anc_x = tidy::layout(&anc.nodes, root, DOT_GAP);

    // Partners and descendants. With more than one partner the first sits
    // left of the root and the rest to the right, and each couple's
    // children hang from their own link, on their own side, so it's plain
    // who is whose.
    let fams = doc.spouse_families(xref);
    let sides = fams.len() >= 2;
    let spouse_x = |j: usize| match (sides, j) {
        (true, 0) => -(SPOUSE_GAP + 6.0),
        (true, j) => SPOUSE_GAP * j as f32 + 6.0,
        (false, j) => SPOUSE_GAP * (j + 1) as f32 + 6.0,
    };
    let spouses: Vec<(String, f32)> = fams
        .iter()
        .enumerate()
        .filter_map(|(j, f)| doc.spouse_in(f, xref).map(|s| (s, spouse_x(j))))
        .collect();
    let with_kids = fams.iter().filter(|f| !doc.children(f).is_empty()).count().max(1);
    let cap = (MAX_CHILDREN / with_kids).max(4);
    let mut desc = DotTree::default();
    // Per family: its hub, the nodes it owns, and where its children hang from.
    let mut groups: Vec<(usize, std::ops::Range<usize>, f32)> = Vec::new();
    for (j, f) in fams.iter().enumerate() {
        let kids = if opts.descendants { doc.children(f) } else { Vec::new() };
        if kids.is_empty() {
            continue;
        }
        let start = desc.nodes.len();
        let hub = desc.push(Dot::Hub, 0.0, 0);
        let (shown_kids, more_kids) = pick(&kids, cap, down.first());
        for k in &shown_kids {
            let c = desc.push(Dot::Person(k.clone()), CHILD_R * 2.0, 1);
            desc.nodes[hub].children.push(c);
            let gk: Vec<String> = doc.spouse_families(k).iter().flat_map(|f| doc.children(f)).collect();
            let (shown, more) = pick(&gk, MAX_GRANDCHILDREN_EACH, down.get(1));
            for g in shown {
                let gc = desc.push(Dot::Person(g), GRANDCHILD_R * 2.0, 2);
                desc.nodes[c].children.push(gc);
            }
            if more > 0 {
                let m = desc.push(Dot::More(more), MORE_W, 2);
                desc.nodes[c].children.push(m);
            }
        }
        if more_kids > 0 {
            let m = desc.push(Dot::More(more_kids), MORE_W, 1);
            desc.nodes[hub].children.push(m);
        }
        // Midway along the couple's link; from the root when the other
        // parent is unknown.
        let anchor = if doc.spouse_in(f, xref).is_some() { spouse_x(j) / 2.0 } else { 0.0 };
        groups.push((hub, start..desc.nodes.len(), anchor));
    }
    // Each family centred under its couple, then pushed outward until the
    // left family ends left of the root and the rest start right of it.
    let mut desc_x = vec![0.0; desc.nodes.len()];
    let split = sides && groups.len() >= 2 && groups[0].2 < 0.0;
    let mut right_edge = -DOT_GAP / 2.0;
    for (g, (hub, range, anchor)) in groups.iter().enumerate() {
        let xs = tidy::layout(&desc.nodes, *hub, DOT_GAP);
        let lo_f = range.clone().map(|i| xs[i] - desc.nodes[i].size / 2.0).fold(0.0, f32::min);
        let hi_f = range.clone().map(|i| xs[i] + desc.nodes[i].size / 2.0).fold(0.0, f32::max);
        let offset = match (split, g) {
            (false, _) => *anchor,
            (true, 0) => anchor.min(-DOT_GAP / 2.0 - hi_f),
            (true, _) => anchor.max(right_edge + DOT_GAP - lo_f),
        };
        if split {
            right_edge = if g == 0 { -DOT_GAP / 2.0 } else { offset + hi_f };
        }
        for i in range.clone() {
            desc_x[i] = offset + xs[i];
        }
    }
    let down = desc.generation.iter().copied().max().unwrap_or(0);
    let anchor_of: HashMap<usize, f32> = groups.iter().map(|(hub, _, a)| (*hub, *a)).collect();

    // Horizontal extent of everything, relative to the root.
    let mut lo = -ancestor_radius(0);
    let mut hi = ancestor_radius(0);
    for (_, x) in &spouses {
        lo = lo.min(x - SPOUSE_R);
        hi = hi.max(x + SPOUSE_R);
    }
    for (i, n) in anc.nodes.iter().enumerate() {
        lo = lo.min(anc_x[i] - n.size / 2.0);
        hi = hi.max(anc_x[i] + n.size / 2.0);
    }
    for (i, n) in desc.nodes.iter().enumerate() {
        lo = lo.min(desc_x[i] - n.size / 2.0);
        hi = hi.max(desc_x[i] + n.size / 2.0);
    }

    // Shrink to fit the width (and height, if limited); never enlarge.
    let avail = ui.available_width().min(opts.max_width);
    let rows = (up + 1 + down) as f32;
    let row_h = match opts.max_height {
        Some(h) => (h / rows).min(ROW_H),
        None => ROW_H,
    };
    let scale = (avail / (hi - lo)).min(row_h / ROW_H).min(1.0);
    let height = rows * row_h;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width().min(opts.max_width.max(avail)), height), Sense::hover());
    let x_of = |v: f32| rect.center().x + (v - (lo + hi) / 2.0) * scale;
    let r_of = |r: f32| (r * scale).max(if opts.max_height.is_some() { 2.0 } else { 3.0 });
    let row_y = |row: isize| rect.top() + row_h / 2.0 + (row + up as isize) as f32 * row_h;
    let anc_pos = |i: usize| pos2(x_of(anc_x[i]), row_y(-(anc.generation[i] as isize)));
    let desc_pos = |i: usize| pos2(x_of(desc_x[i]), row_y(desc.generation[i] as isize));
    let root_pos = anc_pos(root);

    let mut map = Map { doc, actions, interactive: opts.interactive, home: path.map(|h| h.home()) };

    // Ancestor links: each person up to a bar joining their two parents.
    for (i, n) in anc.nodes.iter().enumerate() {
        if n.children.is_empty() {
            continue;
        }
        let branches: Vec<(Pos2, Color32)> = n
            .children
            .iter()
            .map(|&c| (anc_pos(c), if matches!(anc.dots[c], Dot::Person(_)) { line } else { faint }))
            .collect();
        let on_path = match &anc.dots[i] {
            Dot::Person(x) => n.children.iter().position(|&c| matches!(&anc.dots[c], Dot::Person(y) if joins(x, y))),
            _ => None,
        };
        elbow(ui, anc_pos(i), &branches, line, on_path.map(|b| (b, home_color)));
    }
    for (i, dot) in anc.dots.iter().enumerate().skip(1) {
        let r = r_of(ancestor_radius(anc.generation[i]));
        match dot {
            Dot::Person(x) => map.node(ui, anc_pos(i), r, x, false),
            Dot::Empty { child, rel } => map.empty_slot(ui, anc_pos(i), r - 1.0, child, rel.clone()),
            _ => {}
        }
    }

    // Descendant links, then dots: children hang from the partner link.
    for (i, n) in desc.nodes.iter().enumerate() {
        let from = match anchor_of.get(&i) {
            Some(a) => pos2(x_of(*a), root_pos.y),
            None => desc_pos(i),
        };
        let people: Vec<usize> = n.children.iter().copied().filter(|&c| matches!(desc.dots[c], Dot::Person(_))).collect();
        let branches: Vec<(Pos2, Color32)> = people.iter().map(|&c| (desc_pos(c), line)).collect();
        let parent = match &desc.dots[i] {
            Dot::Person(x) => x.as_str(),
            _ => xref,
        };
        let on_path = people.iter().position(|&c| matches!(&desc.dots[c], Dot::Person(y) if joins(parent, y)));
        elbow(ui, from, &branches, line, on_path.map(|b| (b, home_color)));
    }
    for (i, dot) in desc.dots.iter().enumerate() {
        match dot {
            Dot::Person(x) => {
                let r = if desc.generation[i] == 1 { CHILD_R } else { GRANDCHILD_R };
                map.node(ui, desc_pos(i), r_of(r), x, false);
            }
            Dot::More(n) => {
                ui.painter().text(desc_pos(i), Align2::CENTER_CENTER, format!("+{n}"), FontId::proportional(10.5), p.text_faint);
            }
            _ => {}
        }
    }

    // The couple last, over the links that leave them.
    let r0 = r_of(ancestor_radius(0));
    for (s, x) in &spouses {
        let c = pos2(x_of(*x), root_pos.y);
        let dir = if *x < 0.0 { -1.0 } else { 1.0 };
        let stroke = if joins(xref, s) { Stroke::new(2.0, home_color) } else { Stroke::new(1.5, mix(p.purple, p.border, 0.3)) };
        ui.painter().line_segment([root_pos + vec2(dir * (r0 + 1.0), 0.0), c - vec2(dir * r_of(SPOUSE_R), 0.0)], stroke);
        map.node(ui, c, r_of(SPOUSE_R), s, false);
    }
    map.node(ui, root_pos, r_of(ancestor_radius(0)), xref, true);
}

#[cfg(test)]
mod tests {
    use super::pick;

    fn names(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("P{i}")).collect()
    }

    #[test]
    fn pick_keeps_the_way_home() {
        let all = names(9);
        assert_eq!(pick(&all, 6, None), (names(6), 3));
        // Already shown: nothing changes.
        assert_eq!(pick(&all, 6, Some(&"P2".to_string())), (names(6), 3));
        // Beyond the cut: it replaces the last shown, in birth order.
        let (shown, hidden) = pick(&all, 6, Some(&"P8".to_string()));
        assert_eq!(shown, ["P0", "P1", "P2", "P3", "P4", "P8"]);
        assert_eq!(hidden, 3);
        // Someone not among them is ignored.
        assert_eq!(pick(&all, 6, Some(&"X".to_string())), (names(6), 3));
    }
}
