//! The Relationship section: the simplest lineage between two people.

use std::collections::HashSet;

use egui::{Align, Align2, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2, vec2};
use elegance::{Button, ButtonSize, Card, Popover, PopoverSide, Theme, glyphs};

use crate::app::Action;
use crate::kinship::{self, Kinship, Link};
use crate::model::Document;
use crate::views::person_context_menu;
use crate::widgets::{mix, muted, person_chip};

const CHIP_W: f32 = 250.0;
const CHIP_H: f32 = 48.0;
const LABEL_H: f32 = 20.0;
const GAP_X: f32 = 56.0;
const GAP_Y: f32 = 34.0;
const MARGIN: f32 = 24.0;

#[derive(Default)]
pub struct RelationView {
    /// Whose relative the selected person is being described as.
    pub reference: Option<String>,
    path: usize,
    subject_query: String,
    reference_query: String,
    cached: Option<(u64, String, String, Vec<Kinship>)>,
}

impl RelationView {
    pub fn show(&mut self, ui: &mut Ui, doc: &Document, selected: Option<&str>, home: Option<&str>, actions: &mut Vec<Action>) {
        let p = Theme::current(ui.ctx()).palette;
        if self.reference.as_deref().is_some_and(|r| doc.person(r).is_none()) {
            self.reference = None;
        }
        let subject = selected.filter(|x| doc.person(x).is_some());
        let reference_owned = self.reference.clone().or_else(|| home.map(str::to_string)).filter(|x| doc.person(x).is_some());
        let reference = reference_owned.as_deref();

        ui.horizontal(|ui| {
            ui.label(RichText::new("How is").color(p.text_muted));
            if let Some(x) = person_picker(ui, doc, "rel_subject", subject, &mut self.subject_query) {
                actions.push(Action::Select(x));
            }
            ui.label(RichText::new("related to").color(p.text_muted));
            if let Some(x) = person_picker(ui, doc, "rel_reference", reference, &mut self.reference_query) {
                self.reference = Some(x);
            }
            let swap = ui.add(Button::new(format!("{}{}", glyphs::ARROW_LEFT, glyphs::ARROW_RIGHT)).outline().size(ButtonSize::Small).enabled(subject.is_some() && reference.is_some()));
            if swap.on_hover_text("Swap the two people").clicked()
                && let (Some(s), Some(r)) = (subject, reference)
            {
                self.reference = Some(s.to_string());
                actions.push(Action::Select(r.to_string()));
            }
            if let Some(h) = home
                && reference != Some(h)
            {
                let b = ui.add(Button::new(format!("{}  Home person", glyphs::HOME)).outline().size(ButtonSize::Small));
                if b.on_hover_text("Relate to the home person").clicked() {
                    self.reference = Some(h.to_string());
                }
            }
        });
        ui.add_space(10.0);

        let (Some(subject), Some(reference)) = (subject, reference) else {
            Card::new().show(ui, |ui| {
                ui.set_width(ui.available_width());
                muted(ui, "Choose two people above, or right-click anyone and pick a relationship.");
                if home.is_none() {
                    muted(ui, "Tip: set a home person (right-click someone → Set as home person) and it's filled in for you.");
                }
            });
            return;
        };

        let key = (doc.revision, reference.to_string(), subject.to_string());
        if self.cached.as_ref().is_none_or(|c| (c.0, &c.1, &c.2) != (key.0, &key.1, &key.2)) {
            self.cached = Some((key.0, key.1, key.2, kinship::find(doc, reference, subject)));
            self.path = 0;
        }
        let paths = self.cached.as_ref().map(|c| c.3.clone()).unwrap_or_default();
        let name = |x: &str| doc.person(x).map(|p| p.display.clone()).unwrap_or_default();

        let Some(k) = paths.get(self.path.min(paths.len().saturating_sub(1))).cloned() else {
            Card::new().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(format!("{} and {} aren't connected in this file.", name(subject), name(reference))).size(18.0).family(crate::fonts::semibold()));
                muted(ui, "No chain of parents, children or marriages links them.");
            });
            return;
        };

        let chain = kinship::describe(doc, &k);
        Card::new().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(kinship::sentence(&chain, &name(subject), &name(reference))).size(20.0).family(crate::fonts::semibold()).color(p.text));
            ui.horizontal(|ui| {
                ui.label(RichText::new(summary(doc, &k)).color(p.text_muted));
                if paths.len() > 1 {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(Button::new(glyphs::ARROW_RIGHT.to_string()).outline().size(ButtonSize::Small).enabled(self.path + 1 < paths.len())).clicked() {
                            self.path += 1;
                        }
                        ui.label(RichText::new(format!("Lineage {} of {}", self.path + 1, paths.len())).size(12.5).color(p.text_muted));
                        if ui.add(Button::new(glyphs::ARROW_LEFT.to_string()).outline().size(ButtonSize::Small).enabled(self.path > 0)).clicked() {
                            self.path -= 1;
                        }
                    });
                }
            });
        });
        ui.add_space(10.0);

        egui::Frame::new()
            .fill(mix(p.bg, p.card, 0.5))
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(12)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::both().auto_shrink([false, false]).id_salt("rel_chart").show(ui, |ui| {
                    chart(ui, doc, &k, actions);
                });
            });
    }
}

/// A button showing a person; clicking it opens a searchable list.
pub fn person_picker(ui: &mut Ui, doc: &Document, id: &str, current: Option<&str>, query: &mut String) -> Option<String> {
    let label = current.and_then(|x| doc.person(x)).map(|p| p.display.clone()).unwrap_or_else(|| "Choose…".into());
    let trigger = ui.add(Button::new(format!("{label}  {}", glyphs::CHEVRON_DOWN)).outline().size(ButtonSize::Small));
    let mut picked = None;
    Popover::new(id).side(PopoverSide::Bottom).width(320.0).show(&trigger, |ui| {
        let search = crate::widgets::search_input(ui, query, "Search name, year or ID…", (id, "search"), None);
        if !search.has_focus() && query.is_empty() {
            search.request_focus();
        }
        let needle = query.trim().to_lowercase();
        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
            let mut shown = 0;
            for person in doc.people().iter().filter(|p| p.matches(&needle)) {
                if shown == 40 {
                    muted(ui, "Keep typing to narrow the list…");
                    break;
                }
                shown += 1;
                if person_chip(ui, doc, &person.xref, None).clicked() {
                    picked = Some(person.xref.clone());
                }
            }
            if shown == 0 {
                muted(ui, "No one matches.");
            }
        });
    });
    if picked.is_some() {
        query.clear();
        egui::Popup::close_id(ui.ctx(), Popover::popup_id(id));
    }
    picked
}

fn summary(doc: &Document, k: &Kinship) -> String {
    let ups = k.links.iter().filter(|l| **l == Link::Up).count();
    let downs = k.links.iter().filter(|l| **l == Link::Down).count();
    let marriages = k.links.iter().filter(|l| **l == Link::Spouse).count();
    let plural = |n: usize, one: &str| format!("{n} {one}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if ups > 0 {
        parts.push(format!("{} up", plural(ups, "generation")));
    }
    if downs > 0 {
        parts.push(format!("{} down", plural(downs, "generation")));
    }
    if marriages > 0 {
        parts.push(format!("{} by marriage", plural(marriages, "step")));
    }
    // Name the common ancestors of a purely blood relationship.
    if marriages == 0 && ups > 0 && downs > 0 {
        let apex = ups;
        let top = &k.people[apex];
        let mut names = vec![doc.person(top).map(|p| p.display.clone()).unwrap_or_default()];
        if let Some(f) = kinship::shared_family_at(doc, k, apex)
            && let Some(partner) = doc.spouse_in(&f, top)
        {
            names.push(doc.person(&partner).map(|p| p.display.clone()).unwrap_or_default());
        }
        parts.push(format!("common ancestor{} {}", if names.len() > 1 { "s" } else { "" }, names.join(" & ")));
    }
    parts.join(" · ")
}

struct Placed {
    xref: String,
    level: i32,
    col: i32,
    label: String,
    /// Whether this is a common ancestor's partner (drawn but not on the path).
    partner: bool,
}

fn chart(ui: &mut Ui, doc: &Document, k: &Kinship, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;

    // Levels rise with each step to a parent; columns step right at the
    // top of the lineage and at each marriage.
    let mut placed: Vec<Placed> = Vec::new();
    let (mut level, mut col) = (0, 0);
    let mut apex_partner = None;
    for (i, x) in k.people.iter().enumerate() {
        if i > 0 {
            match k.links[i - 1] {
                Link::Up => level -= 1,
                Link::Down => {
                    level += 1;
                    if i >= 2 && k.links[i - 2] == Link::Up {
                        col += 1;
                    }
                }
                Link::Spouse => col += 1,
            }
        }
        let prefix = Kinship { people: k.people[..=i].to_vec(), links: k.links[..i].to_vec() };
        let label = if i == 0 { "Relative to".to_string() } else { capitalise(&kinship::label(&kinship::describe(doc, &prefix))) };
        placed.push(Placed { xref: x.clone(), level, col, label, partner: false });
        // The common ancestor's partner completes the couple at the top.
        if let Some(f) = kinship::shared_family_at(doc, k, i)
            && let Some(partner) = doc.spouse_in(&f, x)
        {
            apex_partner = Some(placed.len());
            placed.push(Placed { xref: partner, level, col: col + 1, label: "Common ancestor".into(), partner: true });
            let apex = placed.len() - 2;
            placed[apex].label = "Common ancestor".into();
        }
    }
    // Husband on the left of the couple, as elsewhere; both are ancestors.
    if let Some(pi) = apex_partner
        && doc.person(&placed[pi - 1].xref).is_some_and(|p| p.sex == crate::model::Sex::Female)
        && doc.person(&placed[pi].xref).is_some_and(|p| p.sex != crate::model::Sex::Female)
    {
        let (left, right) = (placed[pi - 1].col, placed[pi].col);
        placed[pi - 1].col = right;
        placed[pi].col = left;
    }
    // With a partner at the top, the descending side sits under the partner.
    if let Some(pi) = apex_partner {
        let apex_col = placed[pi - 1].col.min(placed[pi].col);
        for pl in placed.iter_mut().skip(pi + 1) {
            if pl.col > apex_col {
                pl.col = pl.col.max(apex_col + 1);
            }
        }
    }

    let min_level = placed.iter().map(|q| q.level).min().unwrap_or(0);
    let max_level = placed.iter().map(|q| q.level).max().unwrap_or(0);
    let max_col = placed.iter().map(|q| q.col).max().unwrap_or(0);
    let row = CHIP_H + LABEL_H + GAP_Y;
    let size = vec2(MARGIN * 2.0 + (max_col + 1) as f32 * (CHIP_W + GAP_X) - GAP_X, MARGIN * 2.0 + (max_level - min_level + 1) as f32 * row - GAP_Y);
    let avail = ui.available_size();
    let (area, _) = ui.allocate_exact_size(size.max(avail), Sense::hover());
    // Centre the chart when it's smaller than the view.
    let offset = vec2(((avail.x - size.x) / 2.0).max(0.0), ((avail.y - size.y) / 2.0).max(0.0));
    let chip_rect = |q: &Placed| {
        let x = area.left() + offset.x + MARGIN + q.col as f32 * (CHIP_W + GAP_X);
        let y = area.top() + offset.y + MARGIN + (q.level - min_level) as f32 * row + LABEL_H;
        Rect::from_min_size(pos2(x, y), vec2(CHIP_W, CHIP_H))
    };
    let line = Stroke::new(1.5, mix(p.border, p.text_faint, 0.5));
    let marriage = Stroke::new(2.0, mix(p.purple, p.border, 0.3));

    // Links along the lineage.
    let on_path: Vec<usize> = (0..placed.len()).filter(|&i| !placed[i].partner).collect();
    for (w, link) in on_path.windows(2).zip(&k.links) {
        let (a, b) = (chip_rect(&placed[w[0]]), chip_rect(&placed[w[1]]));
        match link {
            Link::Spouse => {
                let (l, r) = if a.center().x < b.center().x { (a, b) } else { (b, a) };
                ui.painter().line_segment([l.right_center(), r.left_center()], marriage);
            }
            Link::Up | Link::Down => {
                let (child, parent_idx) = if *link == Link::Up { (a, w[1]) } else { (b, w[0]) };
                let parent = chip_rect(&placed[parent_idx]);
                // From the couple link when the parent is half of the apex couple.
                let from = match apex_partner {
                    Some(pi) if parent_idx + 1 == pi => {
                        let partner = chip_rect(&placed[pi]);
                        pos2((parent.center().x + partner.center().x) / 2.0, parent.center().y)
                    }
                    _ => parent.center_bottom(),
                };
                elbow(ui, from, child.center_top() - vec2(0.0, LABEL_H), line);
            }
        }
    }
    if let Some(pi) = apex_partner {
        let (a, b) = (chip_rect(&placed[pi - 1]), chip_rect(&placed[pi]));
        let (l, r) = if a.center().x < b.center().x { (a, b) } else { (b, a) };
        ui.painter().line_segment([l.right_center(), r.left_center()], marriage);
    }

    // Cards, with their role above each.
    let ends: HashSet<usize> = [0, on_path.last().copied().unwrap_or(0)].into_iter().collect();
    for (i, q) in placed.iter().enumerate() {
        let r = chip_rect(q);
        let label_color = if ends.contains(&i) { p.focus } else { p.text_faint };
        ui.painter().text(r.left_top() - vec2(0.0, 6.0), Align2::LEFT_BOTTOM, q.label.to_uppercase(), FontId::proportional(11.0), label_color);
        let resp = ui.scope_builder(UiBuilder::new().max_rect(r), |ui| person_chip(ui, doc, &q.xref, None)).inner;
        if ends.contains(&i) {
            ui.painter().rect_stroke(r, 8, Stroke::new(2.0, p.focus), egui::StrokeKind::Inside);
        }
        if resp.clicked() {
            actions.push(Action::Select(q.xref.clone()));
        }
        person_context_menu(&resp, doc, &q.xref, None, actions);
    }
}

fn elbow(ui: &Ui, from: Pos2, to: Pos2, stroke: Stroke) {
    let mid = (from.y + to.y) / 2.0;
    let painter = ui.painter();
    painter.line_segment([from, pos2(from.x, mid)], stroke);
    painter.line_segment([pos2(from.x, mid), pos2(to.x, mid)], stroke);
    painter.line_segment([pos2(to.x, mid), to], stroke);
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}
