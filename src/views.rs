//! The people list and the Profile, Overview and GEDCOM sections.

use std::collections::HashMap;
use std::sync::Arc;

use egui::{Align, Align2, Color32, CornerRadius, FontId, Layout, RichText, Sense, Stroke, Ui, pos2, vec2};
use elegance::{
    Accent, Avatar, AvatarSize, Badge, BadgeTone, Button, ButtonSize, Card, ContextMenu, Menu, MenuItem,
    ProgressBar, SegmentedControl, SegmentedSize, StatCard, TextInput, Theme, glyphs,
};

use crate::app::{Action, TAB_GRAPH, TAB_TREE};
use crate::gedcom;
use crate::model::{Document, Relation, Sex};
use crate::widgets::{add_chip, fit_text, mix, muted, paint_avatar, person_chip, section_label, sex_tone};

const ROW_H: f32 = 50.0;

pub fn current_year() -> i32 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    1970 + (secs / 31_556_952) as i32
}

// ---- people list ------------------------------------------------------------------

pub fn people_panel(
    ui: &mut Ui,
    doc: &Document,
    search: &mut String,
    filter: &mut usize,
    focus_search: &mut bool,
    selected: Option<&str>,
    actions: &mut Vec<Action>,
) {
    let p = Theme::current(ui.ctx()).palette;
    let people = doc.people();
    ui.horizontal(|ui| {
        ui.label(RichText::new("People").size(16.0).strong().color(p.text));
        ui.add(Badge::new(people.len().to_string(), BadgeTone::Neutral));
    });
    ui.add_space(6.0);
    let resp = ui.add(
        TextInput::new(search)
            .hint("Search name, year or ID…")
            .id_salt("people_search")
            .desired_width(ui.available_width()),
    );
    if *focus_search {
        resp.request_focus();
        *focus_search = false;
    }
    ui.add_space(4.0);
    let (men, women) = people.iter().fold((0, 0), |(m, w), p| match p.sex {
        Sex::Male => (m + 1, w),
        Sex::Female => (m, w + 1),
        Sex::Unknown => (m, w),
    });
    ui.add(
        SegmentedControl::from_segments(
            filter,
            [
                elegance::Segment::text("All"),
                elegance::Segment::text("Men").count(men.to_string()),
                elegance::Segment::text("Women").count(women.to_string()),
            ],
        )
        .size(SegmentedSize::Small)
        .fill()
        .id_salt("sex_filter"),
    );
    ui.add_space(6.0);

    let needle = search.trim().to_lowercase();
    let visible: Vec<usize> = people
        .iter()
        .enumerate()
        .filter(|(_, person)| match *filter {
            1 => person.sex == Sex::Male,
            2 => person.sex == Sex::Female,
            _ => true,
        })
        .filter(|(_, person)| needle.is_empty() || person.matches(&needle))
        .map(|(i, _)| i)
        .collect();

    // Keyboard navigation from the search box.
    let sel_pos = selected.and_then(|s| visible.iter().position(|&i| people[i].xref == s));
    let mut nav_to = None;
    if resp.has_focus() && !visible.is_empty() {
        let (down, up, enter) = ui.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp), i.key_pressed(egui::Key::Enter)));
        if down {
            nav_to = Some(sel_pos.map(|i| (i + 1).min(visible.len() - 1)).unwrap_or(0));
        } else if up {
            nav_to = Some(sel_pos.map(|i| i.saturating_sub(1)).unwrap_or(0));
        } else if enter && sel_pos.is_none() {
            nav_to = Some(0);
        }
    }
    if let Some(i) = nav_to {
        actions.push(Action::Select(people[visible[i]].xref.clone()));
    }

    if visible.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            if people.is_empty() {
                muted(ui, "No one here yet.");
                ui.add_space(8.0);
                if ui.add(Button::new(format!("{}  Add first person", glyphs::PLUS)).accent(Accent::Green)).clicked() {
                    actions.push(Action::NewPerson);
                }
            } else {
                muted(ui, "No matches.");
            }
        });
        return;
    }

    // Reveal the selection when it changes from outside the list.
    let last_id = egui::Id::new("people_last_sel");
    let last: Option<String> = ui.data(|d| d.get_temp(last_id));
    let target = nav_to.or_else(|| (last.as_deref() != selected).then_some(sel_pos).flatten());
    ui.data_mut(|d| d.insert_temp(last_id, selected.map(str::to_string)));

    let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("people_scroll");
    if let Some(i) = target {
        let view_h = ui.available_height();
        let offset: f32 = ui.data(|d| d.get_temp(egui::Id::new("people_offset"))).unwrap_or(0.0);
        let y = i as f32 * (ROW_H + ui.spacing().item_spacing.y);
        if y < offset || y + ROW_H > offset + view_h {
            area = area.vertical_scroll_offset((y - view_h / 2.0 + ROW_H / 2.0).max(0.0));
        }
    }
    let out = area.show_rows(ui, ROW_H, visible.len(), |ui, range| {
        for i in range {
            let person = &people[visible[i]];
            let is_sel = selected == Some(person.xref.as_str());
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
            let painter = ui.painter();
            if is_sel {
                painter.rect_filled(rect, CornerRadius::same(8), mix(p.bg, p.focus, 0.16));
                painter.rect_filled(
                    egui::Rect::from_min_size(rect.min + vec2(0.0, 12.0), vec2(3.0, ROW_H - 24.0)),
                    CornerRadius::same(2),
                    p.focus,
                );
            } else if resp.hovered() {
                painter.rect_filled(rect, CornerRadius::same(8), mix(p.bg, p.card, 0.8));
            }
            paint_avatar(ui, pos2(rect.left() + 26.0, rect.center().y), 16.0, person);
            let x = rect.left() + 52.0;
            let w = rect.right() - x - 8.0;
            let name_font = FontId::proportional(14.0);
            let name = fit_text(ui, &person.display, &name_font, w);
            let life = person.lifespan();
            let name_y = if life.is_empty() { rect.center().y } else { rect.top() + 17.0 };
            let painter = ui.painter();
            painter.text(pos2(x, name_y), Align2::LEFT_CENTER, name, name_font, if is_sel { p.text } else { mix(p.text, p.text_muted, 0.15) });
            if !life.is_empty() {
                painter.text(pos2(x, rect.top() + 34.0), Align2::LEFT_CENTER, life, FontId::proportional(12.0), p.text_faint);
            }
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                ui.data_mut(|d| d.insert_temp(last_id, Some(person.xref.clone())));
                actions.push(Action::Select(person.xref.clone()));
            }
            if resp.double_clicked() {
                actions.push(Action::Edit(person.xref.clone()));
            }
            person_context_menu(&resp, doc, &person.xref, None, actions);
        }
    });
    ui.data_mut(|d| d.insert_temp(egui::Id::new("people_offset"), out.state.offset.y));
}

/// Right-click menu shared by every place a person appears.
pub fn person_context_menu(resp: &egui::Response, doc: &Document, xref: &str, fam: Option<&str>, actions: &mut Vec<Action>) {
    ContextMenu::new(format!("ctx_{xref}_{}", fam.unwrap_or(""))).show(resp, |ui| {
        if ui.add(MenuItem::new("Open profile")).clicked() {
            actions.push(Action::Select(xref.to_string()));
            actions.push(Action::SetTab(0));
        }
        if ui.add(MenuItem::new("Show in tree")).clicked() {
            actions.push(Action::Select(xref.to_string()));
            actions.push(Action::SetTab(TAB_TREE));
        }
        if ui.add(MenuItem::new("Edit…").icon(glyphs::PENCIL.to_string())).clicked() {
            actions.push(Action::Edit(xref.to_string()));
        }
        elegance::SubMenuItem::new("Add relative").icon(glyphs::PLUS.to_string()).show(ui, |ui| {
            add_relative_items(ui, doc, xref, actions);
        });
        ui.separator();
        if let Some(f) = fam
            && ui.add(MenuItem::new("Remove from this family")).clicked() {
                actions.push(Action::Unlink { fam: f.to_string(), person: xref.to_string() });
            }
        if ui.add(MenuItem::new("Delete person…").icon(glyphs::TRASH.to_string()).danger()).clicked() {
            actions.push(Action::Delete(xref.to_string()));
        }
    });
}

fn add_relative_items(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let has_father = doc.father(xref).is_some();
    let has_mother = doc.mother(xref).is_some();
    if ui.add(MenuItem::new("Father").enabled(!has_father)).clicked() {
        actions.push(Action::AddRelative(xref.to_string(), Relation::Father));
    }
    if ui.add(MenuItem::new("Mother").enabled(!has_mother)).clicked() {
        actions.push(Action::AddRelative(xref.to_string(), Relation::Mother));
    }
    if ui.add(MenuItem::new("Partner")).clicked() {
        actions.push(Action::AddRelative(xref.to_string(), Relation::Spouse));
    }
    let fams = doc.spouse_families(xref);
    if fams.len() <= 1 {
        if ui.add(MenuItem::new("Child")).clicked() {
            actions.push(Action::AddRelative(xref.to_string(), Relation::Child(fams.first().cloned())));
        }
    } else {
        for f in fams {
            let with = doc
                .spouse_in(&f, xref)
                .and_then(|s| doc.person(&s).map(|p| p.display.clone()))
                .unwrap_or_else(|| "unknown partner".into());
            if ui.add(MenuItem::new(format!("Child with {with}"))).clicked() {
                actions.push(Action::AddRelative(xref.to_string(), Relation::Child(Some(f.clone()))));
            }
        }
    }
    if ui.add(MenuItem::new("Sibling")).clicked() {
        actions.push(Action::AddRelative(xref.to_string(), Relation::Sibling));
    }
}

// ---- empty state ----------------------------------------------------------------------

pub fn empty_tree(ui: &mut Ui, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    ui.vertical_centered(|ui| {
        ui.add_space((ui.available_height() * 0.2).max(20.0));
        ui.label(RichText::new(glyphs::NETWORK.to_string()).size(56.0).color(p.focus));
        ui.add_space(8.0);
        ui.label(RichText::new("Start your family tree").size(26.0).family(crate::fonts::semibold()).color(p.text));
        ui.add_space(4.0);
        ui.label(
            RichText::new("Begin with yourself or the person you know most about,\nthen add their parents, partners and children.")
                .size(15.0)
                .color(p.text_muted),
        );
        ui.add_space(18.0);
        if ui.add(Button::new(format!("{}  Add the first person", glyphs::PLUS)).accent(Accent::Green).size(ButtonSize::Large)).clicked() {
            actions.push(Action::NewPerson);
        }
        ui.add_space(10.0);
        if ui.add(Button::new("Or build it visually in the Graph").outline()).clicked() {
            actions.push(Action::SetTab(TAB_GRAPH));
        }
    });
}

// ---- profile -----------------------------------------------------------------------------

pub fn profile(ui: &mut Ui, doc: &Document, selected: Option<&str>, actions: &mut Vec<Action>) {
    let Some(xref) = selected else {
        muted(ui, "Select someone from the list.");
        return;
    };
    let Some(person) = doc.person(xref) else { return };
    let theme = Theme::current(ui.ctx());
    let p = theme.palette;

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt(("profile", xref)).show(ui, |ui| {
        ui.set_max_width(ui.available_width() - 12.0);
        ui.spacing_mut().item_spacing.y = 12.0;
        // Hero.
        Card::new().padding(20.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Avatar::new(person.initials()).size(AvatarSize::XLarge).tone(sex_tone(person.sex)).surface(p.card));
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new(&person.display).size(26.0).family(crate::fonts::semibold()).color(p.text));
                    let mut line = person.lifespan();
                    let now = current_year();
                    match (person.birth_year, person.death_year) {
                        (Some(b), Some(d)) if d >= b => line = format!("{line} · aged about {}", d - b),
                        (Some(b), None) if !person.is_dead && now - b < 110 => line = format!("{line} · about {} today", now - b),
                        _ => {}
                    }
                    if !line.is_empty() {
                        ui.label(RichText::new(line).size(14.0).color(p.text_muted));
                    }
                    ui.horizontal(|ui| {
                        let tone = match person.sex {
                            Sex::Male => BadgeTone::Info,
                            Sex::Female => BadgeTone::Info,
                            Sex::Unknown => BadgeTone::Neutral,
                        };
                        ui.add(Badge::new(person.sex.label(), tone));
                        let living = !person.is_dead && person.birth_year.is_none_or(|b| current_year() - b < 110);
                        if person.is_dead {
                            ui.add(Badge::new("Deceased", BadgeTone::Neutral));
                        } else if living {
                            ui.add(Badge::new("Living", BadgeTone::Ok));
                        }
                        ui.add(Badge::new(&person.xref, BadgeTone::Neutral).preserve_case());
                        let occ = doc.record(xref).map(|r| r.child_value("OCCU")).unwrap_or("");
                        if !occ.is_empty() {
                            ui.add(Badge::new(occ, BadgeTone::Neutral).preserve_case());
                        }
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    let more = ui.add(Button::new("⋯").outline().size(ButtonSize::Small));
                    Menu::new("profile_more").show_below(&more, |ui| {
                        if ui.add(MenuItem::new("Show in tree")).clicked() {
                            actions.push(Action::SetTab(TAB_TREE));
                        }
                        if ui.add(MenuItem::new("Show in graph")).clicked() {
                            actions.push(Action::SetTab(TAB_GRAPH));
                        }
                        ui.separator();
                        if ui.add(MenuItem::new("Delete person…").icon(glyphs::TRASH.to_string()).danger()).clicked() {
                            actions.push(Action::Delete(xref.to_string()));
                        }
                    });
                    let add = ui.add(Button::new(format!("{}  Add relative", glyphs::PLUS)).size(ButtonSize::Small));
                    Menu::new("profile_add").show_below(&add, |ui| add_relative_items(ui, doc, xref, actions));
                    if ui.add(Button::new(format!("{}  Edit", glyphs::PENCIL)).outline().size(ButtonSize::Small)).clicked() {
                        actions.push(Action::Edit(xref.to_string()));
                    }
                });
            });
        });

        let wide = ui.available_width() > 860.0;
        let left = |ui: &mut Ui, actions: &mut Vec<Action>| {
            timeline_card(ui, doc, xref, actions);
            let notes = doc.notes(xref);
            if !notes.is_empty() {
                Card::new().heading("Notes").show(ui, |ui| {
                    for n in notes {
                        ui.label(RichText::new(n).size(14.0).color(p.text));
                        ui.add_space(4.0);
                    }
                });
            }
            sources_card(ui, doc, xref, actions);
        };
        if wide {
            ui.columns(2, |cols| {
                left(&mut cols[0], actions);
                crate::minimap::family_map(&mut cols[1], doc, xref, actions);
                family_card(&mut cols[1], doc, xref, actions);
            });
        } else {
            crate::minimap::family_map(ui, doc, xref, actions);
            left(ui, actions);
            family_card(ui, doc, xref, actions);
        }
    });
}

fn quality_badge(q: u8) -> Badge {
    let tone = match q {
        3 => BadgeTone::Ok,
        2 => BadgeTone::Info,
        1 => BadgeTone::Warning,
        _ => BadgeTone::Danger,
    };
    Badge::new(crate::model::QUALITY_LABELS[q as usize], tone)
}

fn sources_card(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    let cites = doc.citation_views(xref);
    Card::new().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Sources").size(16.0).color(p.text));
            if !cites.is_empty() {
                ui.add(Badge::new(cites.len().to_string(), BadgeTone::Neutral));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let label = if cites.is_empty() { "Add a source" } else { "Add" };
                if ui.add(Button::new(format!("{}  {label}", glyphs::PLUS)).outline().size(ButtonSize::Small)).clicked() {
                    actions.push(Action::AddCitation(xref.to_string()));
                }
            });
        });
        if cites.is_empty() {
            muted(ui, "Nothing here is cited yet. Record where each fact came from so it can be checked later.");
            return;
        }
        ui.add_space(4.0);
        // Group by the fact supported, in the order the facts first appear.
        let mut order: Vec<&str> = Vec::new();
        for c in &cites {
            if !order.contains(&c.fact.as_str()) {
                order.push(&c.fact);
            }
        }
        for fact in order {
            ui.add_space(4.0);
            section_label(ui, fact);
            for c in cites.iter().filter(|c| c.fact == fact) {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(glyphs::FOLDER.to_string()).size(13.0).color(p.text_faint));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&c.title).size(13.5).color(p.text));
                            if let Some(q) = c.quality {
                                ui.add(quality_badge(q));
                            }
                        });
                        let detail: Vec<&str> = [c.page.as_str(), c.author.as_str()].into_iter().filter(|s| !s.is_empty()).collect();
                        if !detail.is_empty() {
                            ui.label(RichText::new(detail.join(" · ")).size(12.0).color(p.text_muted));
                        }
                    });
                });
            }
        }
    });
}

fn timeline_card(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    Card::new().heading("Life events").show(ui, |ui| {
        let events = doc.timeline(xref);
        if events.is_empty() {
            muted(ui, "No events recorded yet.");
            ui.add_space(6.0);
            if ui.add(Button::new(format!("{}  Add birth details", glyphs::PLUS)).outline().size(ButtonSize::Small)).clicked() {
                actions.push(Action::Edit(xref.to_string()));
            }
            return;
        }
        let n = events.len();
        for (i, ev) in events.iter().enumerate() {
            let related = ev.related.is_some();
            let date_w = 118.0;
            let row_top = ui.cursor().top();
            let resp = ui
                .horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let (date_rect, _) = ui.allocate_exact_size(vec2(date_w, 20.0), Sense::hover());
                    let date = if ev.date.is_empty() { "—".to_string() } else { ev.date.clone() };
                    let font = FontId::proportional(12.5);
                    let date = fit_text(ui, &date, &font, date_w - 10.0);
                    ui.painter().text(date_rect.left_center(), Align2::LEFT_CENTER, date, font, p.text_muted);
                    ui.add_space(26.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        let title_color = if related { p.text_muted } else { p.text };
                        ui.label(RichText::new(&ev.label).size(14.0).strong().color(title_color));
                        if !ev.detail.is_empty() {
                            ui.label(RichText::new(&ev.detail).size(13.0).color(p.text_muted));
                        }
                        if !ev.place.is_empty() {
                            ui.label(RichText::new(format!("{}  {}", glyphs::PIN, ev.place)).size(12.5).color(p.text_faint));
                        }
                    });
                })
                .response;
            // Rail and dot.
            let x = resp.rect.left() + date_w + 12.0;
            let dot_y = row_top + 10.0;
            let color = if related { p.text_faint } else { event_color(&p, &ev.tag) };
            if i + 1 < n {
                ui.painter().line_segment([pos2(x, dot_y + 6.0), pos2(x, resp.rect.bottom() + 14.0)], Stroke::new(1.5, p.border));
            }
            if related {
                ui.painter().circle(pos2(x, dot_y), 4.0, p.card, Stroke::new(1.5, color));
            } else {
                ui.painter().circle(pos2(x, dot_y), 5.0, color, Stroke::new(2.0, mix(p.card, color, 0.3)));
            }
            if let Some(other) = &ev.related {
                let r = ui.interact(resp.rect, ui.id().with(("ev", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
                if r.clicked() {
                    actions.push(Action::Select(other.clone()));
                }
            }
            ui.add_space(4.0);
        }
    });
}

fn event_color(p: &elegance::Palette, tag: &str) -> Color32 {
    match tag {
        "BIRT" | "CHR" | "BAPM" => p.green,
        "DEAT" | "BURI" | "CREM" => p.text_muted,
        "MARR" | "ENGA" => p.purple,
        "DIV" | "ANUL" => p.red,
        "EMIG" | "IMMI" | "NATU" => p.amber,
        _ => p.focus,
    }
}

fn family_card(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    Card::new().heading("Family").show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        section_label(ui, "Parents");
        let pfams = doc.parent_families(xref);
        let father = doc.father(xref);
        let mother = doc.mother(xref);
        let fam0 = pfams.first().map(String::as_str);
        match &father {
            Some(f) => {
                let r = person_chip(ui, doc, f, Some("Father"));
                chip_actions(&r, doc, f, fam0, actions);
            }
            None => {
                if add_chip(ui, "Add father").clicked() {
                    actions.push(Action::AddRelative(xref.to_string(), Relation::Father));
                }
            }
        }
        match &mother {
            Some(m) => {
                let r = person_chip(ui, doc, m, Some("Mother"));
                chip_actions(&r, doc, m, fam0, actions);
            }
            None => {
                if add_chip(ui, "Add mother").clicked() {
                    actions.push(Action::AddRelative(xref.to_string(), Relation::Mother));
                }
            }
        }

        let sibs = doc.siblings(xref);
        section_label(ui, &format!("Siblings{}", if sibs.is_empty() { String::new() } else { format!(" · {}", sibs.len()) }));
        for s in &sibs {
            let fam = pfams.iter().find(|f| doc.children(f).contains(s)).map(String::as_str);
            let r = person_chip(ui, doc, s, None);
            chip_actions(&r, doc, s, fam, actions);
        }
        if add_chip(ui, "Add sibling").clicked() {
            actions.push(Action::AddRelative(xref.to_string(), Relation::Sibling));
        }

        let fams = doc.spouse_families(xref);
        for f in &fams {
            ui.add_space(6.0);
            let marr = doc.family_event(f, "MARR");
            let mut label = "Partner".to_string();
            if let Some((date, place)) = &marr {
                let d = crate::model::pretty_date(date);
                label = match (d.is_empty(), place.is_empty()) {
                    (false, _) => format!("Married {d}"),
                    (true, false) => "Married".into(),
                    _ => "Married".into(),
                };
            }
            section_label(ui, &label);
            match doc.spouse_in(f, xref) {
                Some(s) => {
                    let r = person_chip(ui, doc, &s, None);
                    chip_actions(&r, doc, &s, Some(f), actions);
                }
                None => {
                    if add_chip(ui, "Add partner").clicked() {
                        actions.push(Action::AddRelative(xref.to_string(), Relation::Spouse));
                    }
                }
            }
            let kids = doc.children(f);
            if !kids.is_empty() {
                ui.label(RichText::new(format!("Children · {}", kids.len())).size(12.0).color(p.text_faint));
            }
            ui.indent(("kids", f), |ui| {
                for k in &kids {
                    let r = person_chip(ui, doc, k, None);
                    chip_actions(&r, doc, k, Some(f), actions);
                }
                if add_chip(ui, "Add child").clicked() {
                    actions.push(Action::AddRelative(xref.to_string(), Relation::Child(Some(f.clone()))));
                }
            });
        }
        if fams.is_empty() {
            section_label(ui, "Partners & children");
            if add_chip(ui, "Add partner").clicked() {
                actions.push(Action::AddRelative(xref.to_string(), Relation::Spouse));
            }
            if add_chip(ui, "Add child").clicked() {
                actions.push(Action::AddRelative(xref.to_string(), Relation::Child(None)));
            }
        } else if ui.add(Button::new(format!("{}  Another partner", glyphs::PLUS)).outline().size(ButtonSize::Small)).clicked() {
            actions.push(Action::AddRelative(xref.to_string(), Relation::Spouse));
        }
    });
}

fn chip_actions(r: &egui::Response, doc: &Document, xref: &str, fam: Option<&str>, actions: &mut Vec<Action>) {
    if r.clicked() {
        actions.push(Action::Select(xref.to_string()));
    }
    person_context_menu(r, doc, xref, fam, actions);
}

// ---- overview ------------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    people: usize,
    families: usize,
    sources: usize,
    men: usize,
    women: usize,
    surnames: Vec<(String, usize)>,
    places: Vec<(String, usize)>,
    earliest: Option<i32>,
    latest: Option<i32>,
    avg_life: Option<f32>,
    decades: Vec<f32>,
    decade_start: i32,
    no_birth: Vec<String>,
    isolated: Vec<String>,
}

fn compute_stats(doc: &Document) -> Stats {
    let mut s = Stats {
        people: doc.people().len(),
        families: doc.family_count(),
        sources: doc.source_count(),
        ..Default::default()
    };
    let mut surnames: HashMap<&str, usize> = HashMap::new();
    let mut lives = Vec::new();
    let mut births = Vec::new();
    for person in doc.people() {
        match person.sex {
            Sex::Male => s.men += 1,
            Sex::Female => s.women += 1,
            _ => {}
        }
        if !person.surname.is_empty() {
            *surnames.entry(person.surname.as_str()).or_default() += 1;
        }
        if let Some(b) = person.birth_year {
            births.push(b);
            if let Some(d) = person.death_year
                && d >= b && d - b < 120 {
                    lives.push((d - b) as f32);
                }
        } else {
            s.no_birth.push(person.xref.clone());
        }
        if doc.parent_families(&person.xref).is_empty() && doc.spouse_families(&person.xref).is_empty() {
            s.isolated.push(person.xref.clone());
        }
    }
    s.earliest = births.iter().min().copied();
    s.latest = births.iter().max().copied();
    if !lives.is_empty() {
        s.avg_life = Some(lives.iter().sum::<f32>() / lives.len() as f32);
    }
    if let (Some(a), Some(b)) = (s.earliest, s.latest) {
        let start = a.div_euclid(10) * 10;
        let n = ((b.div_euclid(10) * 10 - start) / 10 + 1).clamp(1, 200) as usize;
        let mut d = vec![0.0f32; n];
        for y in births {
            let i = ((y.div_euclid(10) * 10 - start) / 10) as usize;
            if i < n {
                d[i] += 1.0;
            }
        }
        s.decades = d;
        s.decade_start = start;
    }
    let mut sn: Vec<(String, usize)> = surnames.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    sn.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    s.surnames = sn;
    s.places = doc.places();
    s
}

pub fn overview(ui: &mut Ui, doc: &Document, notes: &[String], actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    let key = egui::Id::new("stats_cache");
    let cached: Option<(u64, Arc<Stats>)> = ui.data(|d| d.get_temp(key));
    let stats = match cached {
        Some((rev, s)) if rev == doc.revision => s,
        _ => {
            let s = Arc::new(compute_stats(doc));
            ui.data_mut(|d| d.insert_temp(key, (doc.revision, s.clone())));
            s
        }
    };
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("overview").show(ui, |ui| {
        ui.set_max_width(ui.available_width() - 12.0);
        ui.spacing_mut().item_spacing.y = 12.0;
        ui.columns(4, |c| {
            c[0].add(
                StatCard::new("People")
                    .accent(Accent::Blue)
                    .value(stats.people.to_string())
                    .trend(format!("{} men · {} women", stats.men, stats.women))
                    .width(c[0].available_width()),
            );
            c[1].add(
                StatCard::new("Families")
                    .accent(Accent::Purple)
                    .value(stats.families.to_string())
                    .trend(format!("{} surnames", stats.surnames.len()))
                    .width(c[1].available_width()),
            );
            let span = match (stats.earliest, stats.latest) {
                (Some(a), Some(b)) => format!("{} years", b - a),
                _ => "—".into(),
            };
            c[2].add(
                StatCard::new("Births span")
                    .accent(Accent::Green)
                    .value(span)
                    .trend(match (stats.earliest, stats.latest) {
                        (Some(a), Some(b)) => format!("{a} to {b}, by decade"),
                        _ => "no birth dates yet".into(),
                    })
                    .sparkline(&stats.decades)
                    .width(c[2].available_width()),
            );
            c[3].add(
                StatCard::new("Average lifespan")
                    .accent(Accent::Amber)
                    .value(stats.avg_life.map(|a| format!("{a:.0}")).unwrap_or_else(|| "—".into()))
                    .unit(if stats.avg_life.is_some() { "yrs" } else { "" })
                    .trend(match stats.sources {
                        1 => "1 source".to_string(),
                        n => format!("{n} sources"),
                    })
                    .width(c[3].available_width()),
            );
        });

        if !notes.is_empty() {
            elegance::Callout::new(elegance::CalloutTone::Warning)
                .title(format!("{} note{} from import", notes.len(), if notes.len() == 1 { "" } else { "s" }))
                .body(notes.iter().take(8).cloned().collect::<Vec<_>>().join("\n"))
                .multiline()
                .show(ui, |_| {});
        }

        ui.columns(2, |c| {
            Card::new().heading("Top surnames").show(&mut c[0], |ui| {
                bar_list(ui, &stats.surnames, 10, Accent::Blue, &p);
            });
            Card::new().heading("Top places").show(&mut c[1], |ui| {
                bar_list(ui, &stats.places, 10, Accent::Green, &p);
            });
        });

        Card::new().heading("Needs attention").show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if stats.no_birth.is_empty() && stats.isolated.is_empty() {
                ui.label(RichText::new(format!("{}  Everyone has a birth date and a family link.", glyphs::CIRCLE_CHECK)).color(p.green));
            }
            for (title, list) in [("No birth or baptism date", &stats.no_birth), ("Not linked to any family", &stats.isolated)] {
                if list.is_empty() {
                    continue;
                }
                elegance::CollapsingSection::new(title, format!("{title} · {}", list.len())).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        for x in list.iter().take(200) {
                            if let Some(person) = doc.person(x) {
                                let r = ui.add(egui::Button::new(RichText::new(&person.display).size(13.0)).corner_radius(12));
                                if r.clicked() {
                                    actions.push(Action::Select(x.clone()));
                                    actions.push(Action::SetTab(0));
                                }
                            }
                        }
                    });
                });
            }
        });
    });
}

fn bar_list(ui: &mut Ui, items: &[(String, usize)], n: usize, accent: Accent, p: &elegance::Palette) {
    if items.is_empty() {
        muted(ui, "Nothing recorded yet.");
        return;
    }
    let max = items.first().map(|x| x.1).unwrap_or(1).max(1) as f32;
    for (name, count) in items.iter().take(n) {
        ui.horizontal(|ui| {
            let w = ui.available_width();
            let font = FontId::proportional(13.0);
            let label = fit_text(ui, name, &font, w * 0.45);
            let (r, _) = ui.allocate_exact_size(vec2(w * 0.45, 18.0), Sense::hover());
            ui.painter().text(r.left_center(), Align2::LEFT_CENTER, label, font, p.text);
            ui.add_sized(vec2(w * 0.5, 14.0), ProgressBar::new(*count as f32 / max).accent(accent).text(count.to_string()));
        });
    }
}

// ---- raw GEDCOM ------------------------------------------------------------------------------

pub fn source(ui: &mut Ui, doc: &Document, selected: Option<&str>, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    let Some(xref) = selected else { return };
    let mut records = vec![xref.to_string()];
    records.extend(doc.parent_families(xref));
    records.extend(doc.spouse_families(xref));
    let text: String = records
        .iter()
        .filter_map(|x| doc.record(x))
        .map(|r| gedcom::write(std::slice::from_ref(r)))
        .collect();

    ui.horizontal(|ui| {
        ui.label(RichText::new("GEDCOM records").size(16.0).strong());
        ui.label(RichText::new("as they will be saved · the person and their families").color(p.text_faint));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.add(Button::new(format!("{}  Copy", glyphs::COPY)).outline().size(ButtonSize::Small)).clicked() {
                ui.ctx().copy_text(text.replace("\r\n", "\n"));
                elegance::Toast::new("Copied to clipboard").show(ui.ctx());
            }
        });
    });
    ui.add_space(8.0);
    Card::new().padding(14.0).show(ui, |ui| {
        egui::ScrollArea::both().auto_shrink([false, false]).id_salt("gedcom_src").show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 1.0);
            let mono = FontId::monospace(13.0);
            for line in text.lines() {
                let (level, rest) = line.split_once(' ').unwrap_or((line, ""));
                let (xref_part, rest) = if rest.starts_with('@') {
                    let (a, b) = rest.split_once(' ').unwrap_or((rest, ""));
                    (Some(a), b)
                } else {
                    (None, rest)
                };
                let (tag, value) = rest.split_once(' ').unwrap_or((rest, ""));
                let depth: usize = level.parse().unwrap_or(0);
                ui.horizontal(|ui| {
                    let mut job = egui::text::LayoutJob::default();
                    let fmt = |c: Color32| egui::TextFormat { font_id: mono.clone(), color: c, ..Default::default() };
                    job.append(&"  ".repeat(depth), 0.0, fmt(p.text));
                    job.append(level, 0.0, fmt(p.text_faint));
                    if let Some(x) = xref_part {
                        job.append(&format!(" {x}"), 0.0, fmt(p.amber));
                    }
                    let tag_color = if depth == 0 { p.purple } else { p.focus };
                    job.append(&format!(" {tag}"), 0.0, fmt(tag_color));
                    ui.label(job);
                    if value.is_empty() {
                        return;
                    }
                    ui.label(egui::RichText::new(" ").font(mono.clone()));
                    match gedcom::as_pointer(value) {
                        Some(target) if doc.person(target).is_some() => {
                            let r = ui
                                .add(egui::Label::new(RichText::new(value).font(mono.clone()).color(p.amber).underline()).sense(Sense::click()))
                                .on_hover_text(doc.person(target).map(|x| x.display.clone()).unwrap_or_default())
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            if r.clicked() {
                                actions.push(Action::Select(target.to_string()));
                            }
                        }
                        Some(_) => {
                            ui.label(RichText::new(value).font(mono.clone()).color(p.amber));
                        }
                        None => {
                            ui.label(RichText::new(value).font(mono.clone()).color(p.text));
                        }
                    }
                });
            }
        });
    });
}
