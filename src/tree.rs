//! Pan-and-zoom pedigree (ancestors) and descendant charts.
//!
//! Layout happens in "world" coordinates; everything is then painted at its
//! real on-screen size (fonts are sized `size × zoom`) rather than drawn at
//! 1:1 and scaled as a bitmap layer, so text stays crisp at every zoom.

use std::collections::HashSet;

use egui::epaint::CubicBezierShape;
use egui::{
    Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, pos2, vec2,
};
use elegance::{Button, ButtonSize, Segment, SegmentedControl, SegmentedSize, Theme, glyphs};

use crate::app::Action;
use crate::fonts;
use crate::model::{Document, PersonSummary, Relation};
use crate::tidy::{self, TidyNode};
use crate::views::person_context_menu;
use crate::widgets::{dashed_rect, fit_text, mix, paint_avatar, sex_color};

const BOX_W: f32 = 220.0;
const BOX_H: f32 = 64.0;
const ADD_H: f32 = 44.0;
const GAP_X: f32 = 64.0;
const GAP_Y: f32 = 14.0;
const GAP_V: f32 = 72.0;
const COUPLE_GAP: f32 = 24.0;
/// Horizontal gap between neighbouring boxes in the upward pedigree.
const PEDIGREE_GAP: f32 = 16.0;
/// Horizontal gap between neighbouring descendant subtrees.
const DESC_GAP: f32 = 28.0;
/// Children stacked in a column hang this far right of their spine.
const STACK_INDENT: f32 = 28.0;
/// Room right of a stacked box for its "more below" count.
const STACK_TAIL: f32 = 44.0;
/// Stack a family's children once there are at least this many leaves.
const STACK_MIN: usize = 3;
const MODE_ANCESTORS: usize = 0;
const MODE_BOTH: usize = 2;
const GENERATIONS: [&str; 5] = ["3", "4", "5", "6", "7"];
const ZOOM_MIN: f32 = 0.08;
const ZOOM_MAX: f32 = 1.75;
/// Below this zoom only box outlines are drawn; text would be illegible.
const DETAIL_ZOOM: f32 = 0.35;

pub struct TreeView {
    mode: usize,
    gens_idx: usize,
    /// Screen offset of the world origin from the canvas' top-left.
    pan: Vec2,
    zoom: f32,
    fit_pending: bool,
    last_bounds: Option<Rect>,
    last_key: Option<(String, usize, usize)>,
}

impl Default for TreeView {
    fn default() -> Self {
        Self { mode: 0, gens_idx: 1, pan: Vec2::ZERO, zoom: 1.0, fit_pending: true, last_bounds: None, last_key: None }
    }
}

/// Maps world coordinates to the screen while drawing one frame.
struct Canvas<'a> {
    doc: &'a Document,
    root: &'a str,
    actions: &'a mut Vec<Action>,
    origin: Pos2,
    zoom: f32,
    clip: Rect,
}

impl TreeView {
    pub fn show_both(&mut self) {
        self.mode = MODE_BOTH;
    }

    pub fn show(&mut self, ui: &mut Ui, doc: &Document, selected: Option<&str>, actions: &mut Vec<Action>) {
        let p = Theme::current(ui.ctx()).palette;
        let Some(root) = selected.filter(|x| doc.person(x).is_some()) else {
            crate::widgets::muted(ui, "Select someone to see their tree.");
            return;
        };
        let gens = self.gens_idx + 3;

        ui.horizontal(|ui| {
            ui.add(
                SegmentedControl::from_segments(
                    &mut self.mode,
                    [
                        Segment::text("Ancestors").hover_text("Parents, grandparents… fanning out to the right"),
                        Segment::text("Descendants").hover_text("Partners, children, grandchildren… downward"),
                        Segment::text("Both").hover_text("Ancestors above and descendants below the selected person"),
                    ],
                )
                .size(SegmentedSize::Small)
                .id_salt("tree_mode"),
            );
            ui.add_space(12.0);
            ui.label(RichText::new("Generations").size(12.0).color(p.text_muted));
            ui.add(SegmentedControl::new(&mut self.gens_idx, GENERATIONS).size(SegmentedSize::Small).id_salt("tree_gens"));
            if self.mode == MODE_BOTH {
                ui.label(RichText::new("each way").size(12.0).color(p.text_faint));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new(format!("{}  Fit", glyphs::ZOOM_OUT)).outline().size(ButtonSize::Small)).clicked() {
                    self.fit_pending = true;
                }
                if ui.add(Button::new("1:1").outline().size(ButtonSize::Small)).on_hover_text("Actual size").clicked() {
                    self.zoom_to(1.0, self.last_canvas_center(ui));
                }
                ui.label(RichText::new(format!("{:.0}%", self.zoom * 100.0)).size(12.0).color(p.text_muted));
                if ui.available_width() > 420.0 {
                    ui.label(RichText::new("Drag to pan · Ctrl+scroll to zoom · click to focus").size(12.0).color(p.text_faint));
                }
            });
        });
        ui.add_space(8.0);

        let key = (root.to_string(), self.mode, gens);
        if self.last_key.as_ref() != Some(&key) {
            self.fit_pending = true;
            self.last_key = Some(key);
        }

        Frame::new()
            .fill(mix(p.bg, p.card, 0.5))
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(12)
            .show(ui, |ui| {
                let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
                ui.data_mut(|d| d.insert_temp(egui::Id::new("tree_canvas_size"), rect.size()));
                self.handle_input(ui, rect, &resp);

                let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
                child.set_clip_rect(rect.shrink(1.0));
                // Fitting needs the layout's bounds, which drawing produces;
                // on the frame a fit is pending, draw invisibly first.
                let fitting = self.fit_pending;
                if fitting {
                    child.set_invisible();
                }
                let bounds = {
                    let mut canvas = Canvas { doc, root, actions, origin: rect.min + self.pan, zoom: self.zoom, clip: rect };
                    match self.mode {
                        MODE_ANCESTORS => canvas.ancestors(&mut child, gens),
                        MODE_BOTH => canvas.hourglass(&mut child, gens),
                        _ => canvas.descendants(&mut child, gens),
                    }
                };
                self.last_bounds = Some(bounds.expand(40.0));
                if fitting {
                    self.fit(rect, bounds.expand(40.0));
                    self.fit_pending = false;
                    ui.ctx().request_repaint();
                }
            });
    }

    fn last_canvas_center(&self, ui: &Ui) -> Vec2 {
        ui.data(|d| d.get_temp::<Vec2>(egui::Id::new("tree_canvas_size"))).unwrap_or(Vec2::ZERO) / 2.0
    }

    fn handle_input(&mut self, ui: &Ui, rect: Rect, resp: &egui::Response) {
        if resp.dragged() {
            self.pan += resp.drag_delta();
        }
        if !resp.contains_pointer() {
            return;
        }
        let (zoom_delta, scroll, pointer) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta, i.pointer.hover_pos()));
        if zoom_delta != 1.0 {
            let anchor = pointer.map(|p| p - rect.min).unwrap_or(rect.size() / 2.0);
            // egui's per-notch factor is large for a chart; soften it.
            self.zoom_to(self.zoom * zoom_delta.powf(0.6), anchor);
        } else if scroll != Vec2::ZERO {
            self.pan += scroll;
        }
    }

    /// Changes zoom keeping the world point under `anchor` (canvas-relative) fixed.
    fn zoom_to(&mut self, zoom: f32, anchor: Vec2) {
        let zoom = zoom.clamp(ZOOM_MIN, ZOOM_MAX);
        let world = (anchor - self.pan) / self.zoom;
        self.zoom = zoom;
        self.pan = anchor - world * zoom;
    }

    fn fit(&mut self, rect: Rect, bounds: Rect) {
        let zoom = (rect.width() / bounds.width()).min(rect.height() / bounds.height());
        self.zoom = zoom.clamp(ZOOM_MIN, 1.0);
        self.pan = rect.size() / 2.0 - bounds.center().to_vec2() * self.zoom;
    }
}

impl Canvas<'_> {
    fn pt(&self, world: Pos2) -> Pos2 {
        self.origin + world.to_vec2() * self.zoom
    }

    fn rect(&self, world: Rect) -> Rect {
        Rect::from_min_max(self.pt(world.min), self.pt(world.max))
    }

    fn px(&self, v: f32) -> f32 {
        v * self.zoom
    }

    fn font(&self, size: f32) -> FontId {
        FontId::proportional(size * self.zoom)
    }

    fn stroke(&self, width: f32, color: Color32) -> Stroke {
        Stroke::new((width * self.zoom).max(1.0), color)
    }

    fn radius(&self, r: f32) -> CornerRadius {
        CornerRadius::same((r * self.zoom).round().clamp(1.0, 255.0) as u8)
    }

    fn line(&self, ui: &Ui, a: Pos2, b: Pos2, stroke: Stroke) {
        ui.painter().line_segment([self.pt(a), self.pt(b)], stroke);
    }

    fn detailed(&self) -> bool {
        self.zoom >= DETAIL_ZOOM
    }

    fn person_box(&mut self, ui: &mut Ui, world: Rect, xref: &str) {
        let rect = self.rect(world);
        if !rect.intersects(self.clip) {
            return;
        }
        let p = Theme::current(ui.ctx()).palette;
        let Some(person) = self.doc.person(xref) else { return };
        let id = ui.id().with(("box", xref, world.min.x as i32, world.min.y as i32));
        let resp = ui.interact(rect.intersect(self.clip), id, Sense::click());
        let is_root = xref == self.root;
        let hovered = resp.hovered();
        let accent = sex_color(&p, person.sex);
        let painter = ui.painter();
        let shadow = Color32::from_black_alpha(if p.is_dark { 60 } else { 18 });
        painter.rect_filled(rect.translate(vec2(0.0, self.px(3.0))), self.radius(10.0), shadow);
        let fill = if hovered { mix(p.card, p.focus, 0.06) } else { p.card };
        let stroke = if is_root {
            Stroke::new(2.0, p.focus)
        } else if hovered {
            Stroke::new(1.5, mix(p.border, p.focus, 0.6))
        } else {
            Stroke::new(1.0, p.border)
        };
        painter.rect(rect, self.radius(10.0), fill, stroke, StrokeKind::Inside);

        if self.detailed() {
            let stripe = Rect::from_min_size(world.min + vec2(6.0, 12.0), vec2(4.0, world.height() - 24.0));
            painter.rect_filled(self.rect(stripe), self.radius(2.0), accent);
            self.avatar(ui, world.min + vec2(34.0, world.height() / 2.0), 16.0, person);
            let x = world.left() + 60.0;
            let w = self.px(world.right() - x - 10.0);
            let name_font = FontId::new(self.px(14.0), fonts::semibold());
            let name = fit_text(ui, &person.display, &name_font, w);
            let life = person.lifespan();
            let name_y = if life.is_empty() { world.center().y } else { world.top() + 24.0 };
            ui.painter().text(self.pt(pos2(x, name_y)), Align2::LEFT_CENTER, name, name_font, p.text);
            if !life.is_empty() {
                ui.painter().text(self.pt(pos2(x, world.top() + 43.0)), Align2::LEFT_CENTER, life, self.font(12.0), p.text_muted);
            }
        } else {
            painter.rect_filled(rect.shrink(self.px(14.0)), self.radius(4.0), mix(p.card, accent, 0.35));
        }

        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        let resp = if self.detailed() { resp } else { resp.on_hover_text(&person.display) };
        if resp.clicked() {
            self.actions.push(Action::Select(xref.to_string()));
        }
        if resp.double_clicked() {
            self.actions.push(Action::Edit(xref.to_string()));
        }
        person_context_menu(&resp, self.doc, xref, None, self.actions);
    }

    fn avatar(&self, ui: &Ui, center: Pos2, radius: f32, person: &PersonSummary) {
        paint_avatar(ui, self.pt(center), self.px(radius), person);
    }

    fn add_box(&mut self, ui: &mut Ui, world: Rect, label: &str, anchor: &str, rel: Relation) {
        let rect = self.rect(world);
        if !rect.intersects(self.clip) {
            return;
        }
        let p = Theme::current(ui.ctx()).palette;
        let resp = ui.interact(rect.intersect(self.clip), ui.id().with(("add", anchor, label)), Sense::click());
        let color = if resp.hovered() { p.focus } else { p.text_faint };
        if resp.hovered() {
            ui.painter().rect_filled(rect, self.radius(10.0), mix(p.card, p.focus, 0.05));
        }
        dashed_rect(ui, rect, color);
        if self.detailed() {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("{}  {label}", glyphs::PLUS), self.font(13.0), color);
        }
        if resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(label).clicked() {
            self.actions.push(Action::AddRelative(anchor.to_string(), rel));
        }
    }

    /// Builds the pedigree as a tree for [`tidy::layout`]: each person's
    /// children in the layout are their father and mother (or an "add"
    /// slot where one is unknown). `size` gives a slot's breadth.
    fn ancestor_tree(&self, gens: usize, size: impl Fn(&Slot) -> f32) -> AncestorTree {
        // Show every known generation plus one row of "add" slots.
        fn deepest(doc: &Document, x: &str, g: usize, gens: usize, path: &mut Vec<String>) -> usize {
            if g + 1 >= gens || path.iter().any(|p| p == x) {
                return g;
            }
            path.push(x.to_string());
            let d = [doc.father(x), doc.mother(x)]
                .into_iter()
                .flatten()
                .map(|p| deepest(doc, &p, g + 1, gens, path))
                .max()
                .unwrap_or(g);
            path.pop();
            d
        }
        let last_gen = (deepest(self.doc, self.root, 0, gens, &mut Vec::new()) + 1).min(gens - 1);

        let mut t = AncestorTree::default();
        let mut path = Vec::new();
        self.push_ancestor(&mut t, Slot::Person(self.root.to_string()), 0, last_gen, &mut path, &size);
        t
    }

    fn push_ancestor(&self, t: &mut AncestorTree, slot: Slot, g: usize, last_gen: usize, path: &mut Vec<String>, size: &impl Fn(&Slot) -> f32) -> usize {
        let i = t.nodes.len();
        t.nodes.push(TidyNode::new(size(&slot)));
        t.generation.push(g);
        let person = match &slot {
            Slot::Person(x) => Some(x.clone()),
            Slot::Add { .. } => None,
        };
        t.slots.push(slot);
        let Some(x) = person else { return i };
        if g >= last_gen || path.contains(&x) {
            return i;
        }
        path.push(x.clone());
        for (parent, rel) in [(self.doc.father(&x), Relation::Father), (self.doc.mother(&x), Relation::Mother)] {
            let slot = match parent {
                // Someone who is their own ancestor would recurse forever.
                Some(p) if path.contains(&p) => continue,
                Some(p) => Slot::Person(p),
                None => Slot::Add { child: x.clone(), rel },
            };
            let c = self.push_ancestor(t, slot, g + 1, last_gen, path, size);
            t.nodes[i].children.push(c);
        }
        path.pop();
        i
    }

    fn draw_slot(&mut self, ui: &mut Ui, slot: &Slot, center: Pos2) -> Rect {
        match slot {
            Slot::Person(x) => {
                let r = Rect::from_center_size(center, vec2(BOX_W, BOX_H));
                self.person_box(ui, r, x);
                r
            }
            Slot::Add { child, rel } => {
                let r = Rect::from_center_size(center, vec2(BOX_W, ADD_H));
                let label = if *rel == Relation::Father { "Add father" } else { "Add mother" };
                self.add_box(ui, r, label, child, rel.clone());
                r
            }
        }
    }

    fn ancestors(&mut self, ui: &mut Ui, gens: usize) -> Rect {
        let p = Theme::current(ui.ctx()).palette;
        let t = self.ancestor_tree(gens, |s| if matches!(s, Slot::Person(_)) { BOX_H } else { ADD_H });
        let ys = tidy::layout(&t.nodes, 0, GAP_Y);
        let center = |i: usize| pos2(t.generation[i] as f32 * (BOX_W + GAP_X) + BOX_W / 2.0, ys[i]);

        // Connectors first, so boxes paint over them.
        for (i, n) in t.nodes.iter().enumerate() {
            for &c in &n.children {
                let from = center(i) + vec2(BOX_W / 2.0, 0.0);
                let to = center(c) - vec2(BOX_W / 2.0, 0.0);
                let color = if matches!(t.slots[c], Slot::Person(_)) { mix(p.border, p.text_faint, 0.5) } else { p.border };
                let mid = (from.x + to.x) / 2.0;
                let pts = [from, pos2(mid, from.y), pos2(mid, to.y), to].map(|q| self.pt(q));
                ui.painter().add(CubicBezierShape::from_points_stroke(pts, false, Color32::TRANSPARENT, self.stroke(1.5, color)));
            }
        }
        let mut bounds = Rect::NOTHING;
        for (i, slot) in t.slots.iter().enumerate() {
            bounds = bounds.union(self.draw_slot(ui, slot, center(i)));
        }
        bounds
    }

    /// Ancestors fanning upward, descendants downward, meeting at the root.
    fn hourglass(&mut self, ui: &mut Ui, gens: usize) -> Rect {
        let d = self.descendant_tree(gens);
        let root_center = d.person_rect(0).center();
        let up = self.ancestors_up(ui, gens, root_center);
        let down = self.draw_descendants(ui, &d);
        up.union(down)
    }

    /// Draws the pedigree above `root_center`, skipping the root's own box
    /// (it belongs to the descendant half).
    fn ancestors_up(&mut self, ui: &mut Ui, gens: usize, root_center: Pos2) -> Rect {
        let p = Theme::current(ui.ctx()).palette;
        let t = self.ancestor_tree(gens, |_| BOX_W);
        let xs = tidy::layout(&t.nodes, 0, PEDIGREE_GAP);
        let center = |i: usize| pos2(root_center.x + xs[i], root_center.y - t.generation[i] as f32 * (BOX_H + GAP_V));
        let height = |i: usize| if matches!(t.slots[i], Slot::Person(_)) { BOX_H } else { ADD_H };

        for (i, n) in t.nodes.iter().enumerate() {
            if n.children.is_empty() {
                continue;
            }
            let child_top = center(i) - vec2(0.0, BOX_H / 2.0);
            let bus_y = child_top.y - GAP_V / 2.0;
            let line = self.stroke(1.5, mix(p.border, p.text_faint, 0.5));
            self.line(ui, child_top, pos2(child_top.x, bus_y), line);
            for &c in &n.children {
                let parent_bottom = center(c) + vec2(0.0, height(c) / 2.0);
                let color = if matches!(t.slots[c], Slot::Person(_)) { mix(p.border, p.text_faint, 0.5) } else { p.border };
                let stroke = self.stroke(1.5, color);
                self.line(ui, pos2(child_top.x, bus_y), pos2(parent_bottom.x, bus_y), stroke);
                self.line(ui, pos2(parent_bottom.x, bus_y), parent_bottom, stroke);
            }
        }
        let mut bounds = Rect::NOTHING;
        for (i, slot) in t.slots.iter().enumerate().skip(1) {
            bounds = bounds.union(self.draw_slot(ui, slot, center(i)));
        }
        bounds
    }

    fn descendants(&mut self, ui: &mut Ui, gens: usize) -> Rect {
        let d = self.descendant_tree(gens);
        self.draw_descendants(ui, &d)
    }

    fn families_of(&self, x: &str) -> Vec<(String, Option<String>, Vec<String>)> {
        self.doc
            .spouse_families(x)
            .into_iter()
            .map(|f| {
                let sp = self.doc.spouse_in(&f, x);
                let kids = self.doc.children(&f);
                (f, sp, kids)
            })
            .collect()
    }

    fn couple_width(&self, x: &str, depth: usize) -> f32 {
        let fams = self.families_of(x);
        let spouses = fams.iter().filter(|f| f.1.is_some()).count();
        let add_slot = depth == 0 && spouses == 0;
        let n = 1 + spouses + usize::from(add_slot);
        n as f32 * BOX_W + (n - 1) as f32 * COUPLE_GAP
    }

    /// Each node is a "unit": a person and their partners side by side.
    /// Its layout children are all their children, family by family; a
    /// run of childless children is stacked in one column instead.
    fn descendant_tree(&self, gens: usize) -> DescendantTree {
        let mut t = DescendantTree::default();
        let mut seen = HashSet::new();
        self.push_descendant(&mut t, self.root.to_string(), 0, 0, gens, &mut seen);
        t.xs = tidy::layout(&t.nodes, 0, DESC_GAP);
        t
    }

    /// Adds `x`'s subtree; returns (unit index, layout node index).
    fn push_descendant(&self, t: &mut DescendantTree, x: String, depth: usize, family: usize, gens: usize, seen: &mut HashSet<String>) -> (usize, usize) {
        // Someone reached twice (cousins who married) is drawn again, alone.
        let repeat = !seen.insert(x.clone());
        let width = if repeat { BOX_W } else { self.couple_width(&x, depth) };
        let n = t.nodes.len();
        t.nodes.push(TidyNode::new(width));
        let u = t.units.len();
        let fams = self.families_of(&x);
        let hidden = if !repeat && depth + 1 >= gens { fams.iter().map(|f| f.2.len()).sum() } else { 0 };
        t.units.push(Unit { xref: x, depth, family, width, repeat, hidden, kids: Vec::new(), place: Place::Node(n) });
        if repeat || depth + 1 >= gens {
            return (u, n);
        }
        let mut kids = Vec::new();
        for (fi, (_, _, children)) in fams.iter().enumerate() {
            for k in children {
                kids.push(self.push_descendant(t, k.clone(), depth + 1, fi, gens, seen));
            }
        }
        t.units[u].kids = kids.iter().map(|k| k.0).collect();

        let leaves = kids.iter().all(|&(_, kn)| t.nodes[kn].children.is_empty());
        let one_family = kids.iter().all(|&(ku, _)| t.units[ku].family == t.units[kids[0].0].family);
        if kids.len() >= STACK_MIN && leaves && one_family {
            // One tall layout node stands in for the whole column.
            let widest = kids.iter().map(|&(ku, _)| t.units[ku].width).fold(0.0, f32::max);
            let tail = if kids.iter().any(|&(ku, _)| t.units[ku].hidden > 0) { STACK_TAIL } else { 0.0 };
            let height = kids.len() as f32 * (BOX_H + GAP_Y) - GAP_Y;
            let pitch = BOX_H + GAP_V;
            let span = ((height + GAP_V) / pitch).ceil() as usize;
            let g = t.nodes.len();
            t.nodes.push(TidyNode { size: STACK_INDENT + widest + tail, span, children: Vec::new() });
            for (row, &(ku, _)) in kids.iter().enumerate() {
                t.units[ku].place = Place::Stacked { group: g, row };
            }
            t.nodes[n].children = vec![g];
        } else {
            t.nodes[n].children = kids.iter().map(|k| k.1).collect();
        }
        (u, n)
    }

    fn draw_descendants(&mut self, ui: &mut Ui, t: &DescendantTree) -> Rect {
        let p = Theme::current(ui.ctx()).palette;
        let line = self.stroke(1.5, mix(p.border, p.text_faint, 0.5));
        let link = self.stroke(2.0, mix(p.purple, p.border, 0.3));
        let mut bounds = Rect::NOTHING;

        // Connectors: couples, then each family down to its children.
        for (i, u) in t.units.iter().enumerate() {
            if u.repeat {
                continue;
            }
            let me = t.person_rect(i);
            let mut drops = Vec::new();
            let mut slot = 1;
            for (_, sp, _) in self.families_of(&u.xref) {
                if sp.is_some() {
                    let r_left = me.left() + slot as f32 * (BOX_W + COUPLE_GAP);
                    self.line(ui, pos2(r_left - COUPLE_GAP, me.center().y), pos2(r_left, me.center().y), link);
                    drops.push(pos2(r_left - COUPLE_GAP / 2.0, me.center().y));
                    slot += 1;
                } else {
                    drops.push(me.center_bottom());
                }
            }
            let bus_y = me.bottom() + GAP_V / 2.0;
            for (fi, from) in drops.iter().enumerate() {
                let kids: Vec<usize> = u.kids.iter().copied().filter(|&c| t.units[c].family == fi).collect();
                let Some(&first) = kids.first() else { continue };
                self.line(ui, *from, pos2(from.x, bus_y), line);
                if let Place::Stacked { .. } = t.units[first].place {
                    // A spine down the left of the column, a stub to each box.
                    let spine_x = t.person_rect(first).left() - STACK_INDENT / 2.0;
                    let last = t.person_rect(*kids.last().unwrap());
                    self.line(ui, pos2(from.x.min(spine_x), bus_y), pos2(from.x.max(spine_x), bus_y), line);
                    self.line(ui, pos2(spine_x, bus_y), pos2(spine_x, last.center().y), line);
                    for &c in &kids {
                        let r = t.person_rect(c);
                        self.line(ui, pos2(spine_x, r.center().y), r.left_center(), line);
                    }
                } else {
                    let tops: Vec<Pos2> = kids.iter().map(|&c| t.person_rect(c).center_top()).collect();
                    let lo = tops.iter().map(|q| q.x).fold(from.x, f32::min);
                    let hi = tops.iter().map(|q| q.x).fold(from.x, f32::max);
                    self.line(ui, pos2(lo, bus_y), pos2(hi, bus_y), line);
                    for top in tops {
                        self.line(ui, pos2(top.x, bus_y), top, line);
                    }
                }
            }
        }

        // Boxes.
        for (i, u) in t.units.iter().enumerate() {
            let me = t.person_rect(i);
            bounds = bounds.union(me);
            self.person_box(ui, me, &u.xref);
            if u.repeat {
                continue;
            }
            let fams = self.families_of(&u.xref);
            let mut slot = 1;
            for (_, sp, _) in &fams {
                if let Some(s) = sp {
                    let r = me.translate(vec2(slot as f32 * (BOX_W + COUPLE_GAP), 0.0));
                    self.person_box(ui, r, s);
                    bounds = bounds.union(r);
                    slot += 1;
                }
            }
            let has_kids = fams.iter().any(|f| !f.2.is_empty());
            if u.depth == 0 && fams.iter().all(|f| f.1.is_none()) {
                let r = Rect::from_min_size(pos2(me.right() + COUPLE_GAP, me.top() + (BOX_H - ADD_H) / 2.0), vec2(BOX_W, ADD_H));
                self.add_box(ui, r, "Add partner", &u.xref, Relation::Spouse);
                bounds = bounds.union(r);
            }
            if u.hidden > 0 {
                let text = format!("{} {} more", glyphs::CHEVRON_DOWN, u.hidden);
                let (at, align, r) = if let Place::Stacked { .. } = u.place {
                    // Beside the box: below it is the next sibling.
                    let right = me.left() + u.width;
                    let r = Rect::from_min_size(pos2(right + 6.0, me.center().y - 8.0), vec2(STACK_TAIL - 6.0, 16.0));
                    (r.left_center(), Align2::LEFT_CENTER, r)
                } else {
                    let r = Rect::from_min_size(me.left_bottom() + vec2(0.0, 6.0), vec2(BOX_W, 16.0));
                    (r.center(), Align2::CENTER_CENTER, r)
                };
                if self.detailed() {
                    ui.painter().text(self.pt(at), align, text, self.font(11.5), p.text_faint);
                }
                bounds = bounds.union(r);
            } else if u.depth == 0 && !has_kids {
                let r = Rect::from_min_size(pos2(me.left(), me.bottom() + GAP_V), vec2(BOX_W, ADD_H));
                self.line(ui, me.center_bottom(), r.center_top(), self.stroke(1.5, p.border));
                let fam = fams.first().map(|f| f.0.clone());
                self.add_box(ui, r, "Add child", &u.xref, Relation::Child(fam));
                bounds = bounds.union(r);
            }
        }
        bounds
    }
}

/// A place in the pedigree: a known ancestor or an invitation to add one.
enum Slot {
    Person(String),
    Add { child: String, rel: Relation },
}

#[derive(Default)]
struct AncestorTree {
    nodes: Vec<TidyNode>,
    slots: Vec<Slot>,
    generation: Vec<usize>,
}

enum Place {
    /// Positioned by its own layout node.
    Node(usize),
    /// The `row`-th box in a stacked column laid out as node `group`.
    Stacked { group: usize, row: usize },
}

struct Unit {
    xref: String,
    depth: usize,
    /// Which of the parent's families this unit was born into.
    family: usize,
    width: f32,
    repeat: bool,
    /// Children not drawn because of the generation limit.
    hidden: usize,
    /// Units of this person's children, as drawn.
    kids: Vec<usize>,
    place: Place,
}

#[derive(Default)]
struct DescendantTree {
    nodes: Vec<TidyNode>,
    units: Vec<Unit>,
    /// Centre of each layout node, with the root's at 0.
    xs: Vec<f32>,
}

impl DescendantTree {
    /// The unit's own (first) box.
    fn person_rect(&self, u: usize) -> Rect {
        let unit = &self.units[u];
        let row_top = unit.depth as f32 * (BOX_H + GAP_V);
        let (left, top) = match unit.place {
            Place::Node(n) => (self.xs[n] - unit.width / 2.0, row_top),
            Place::Stacked { group, row } => (
                self.xs[group] - self.nodes[group].size / 2.0 + STACK_INDENT,
                row_top + row as f32 * (BOX_H + GAP_Y),
            ),
        };
        Rect::from_min_size(pos2(left, top), vec2(BOX_W, BOX_H))
    }
}
