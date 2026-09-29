//! The Tools menu's dialogs: finding and merging duplicate people and
//! duplicate sources.

use std::collections::{HashMap, HashSet};

use egui::{Align, Layout, RichText, Sense, Stroke, Ui, vec2};
use elegance::{Accent, Badge, BadgeTone, Button, ButtonSize, Modal, Theme, Toast, glyphs};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use crate::app::Action;
use crate::wikitree::{self, Progress, Report};
use crate::dedup::{self, Candidate, MergeChoices, Side};
use crate::model::Document;
use crate::widgets::{mix, muted, paint_avatar, section_label};

/// Height of each record's ancestry map in the comparison.
const MAP_H: f32 = 190.0;

#[derive(Default)]
pub struct ToolsUi {
    people: Option<PeopleDupes>,
    sources: Option<SourceDupes>,
    wikitree: Option<WikiTreeDialog>,
}

struct WikiTreeDialog {
    open: bool,
    /// WikiTree IDs to start from, comma-separated.
    start: String,
    suggestions: Vec<(String, String)>,
    stage: WtStage,
}

enum WtStage {
    Setup,
    Running { rx: Receiver<Progress>, cancel: Arc<AtomicBool>, status: String, done: usize, total: usize },
    Review { report: Report, checked: Vec<bool> },
    Failed(String),
}

struct PeopleDupes {
    open: bool,
    list: Vec<Candidate>,
    selected: usize,
    /// Keep `b` instead of `a` for the selected pair.
    swapped: bool,
    choices: MergeChoices,
}

struct SourceDupes {
    open: bool,
    groups: Vec<Vec<String>>,
}

impl ToolsUi {
    pub fn open_people(&mut self, doc: &Document, dismissed: &HashSet<String>) {
        self.people = Some(PeopleDupes {
            open: true,
            list: dedup::find_duplicate_people(doc, dismissed),
            selected: 0,
            swapped: false,
            choices: MergeChoices::default(),
        });
    }

    pub fn open_sources(&mut self, doc: &Document) {
        self.sources = Some(SourceDupes { open: true, groups: dedup::find_duplicate_sources(doc) });
    }

    pub fn open_wikitree(&mut self, doc: &Document) {
        let suggestions = wikitree::suggested_starts(doc);
        let start = suggestions.iter().take(5).map(|(id, _)| id.clone()).collect::<Vec<_>>().join(", ");
        self.wikitree = Some(WikiTreeDialog { open: true, start, suggestions, stage: WtStage::Setup });
    }

    pub fn overlays(&mut self, ctx: &egui::Context, doc: &mut Document, dismissed: &mut HashSet<String>, actions: &mut Vec<Action>) {
        self.people_dialog(ctx, doc, dismissed, actions);
        self.sources_dialog(ctx, doc);
        self.wikitree_dialog(ctx, doc, actions);
    }

    fn wikitree_dialog(&mut self, ctx: &egui::Context, doc: &mut Document, actions: &mut Vec<Action>) {
        let Some(st) = self.wikitree.as_mut() else { return };
        let p = Theme::current(ctx).palette;

        // Collect progress from the search thread.
        let mut finished = None;
        if let WtStage::Running { rx, status, done, total, .. } = &mut st.stage {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Progress::Status(s) => *status = s,
                    Progress::Count { done: d, total: t } => (*done, *total) = (d, t),
                    Progress::Done(r) => finished = Some(r),
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
        match finished {
            Some(Ok(report)) => {
                let checked = report.photos.iter().map(|ph| !ph.looks_like_badge()).collect();
                st.stage = WtStage::Review { report, checked };
            }
            Some(Err(e)) if e == "Cancelled." => st.stage = WtStage::Setup,
            Some(Err(e)) => st.stage = WtStage::Failed(e),
            None => {}
        }

        let mut search = false;
        let mut cancel_search = false;
        let mut add = false;
        let mut close = false;
        let tree = doc.path.clone();
        Modal::new("wikitree_photos", &mut st.open)
            .heading("Get photos from WikiTree")
            .subtitle("Photos from wikitree.com for people in your tree")
            .max_width(crate::widgets::fit_width(ctx, 760.0))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                match &mut st.stage {
                    WtStage::Setup => {
                        muted(ui, "Start from the WikiTree profile of someone in your tree. Genie fetches their relatives from WikiTree (up to 12 generations back and 5 down), matches them to your tree by name and dates, and shows the photos it finds for you to choose from. Nothing changes until you add them.");
                        if tree.is_none() {
                            elegance::Callout::new(elegance::CalloutTone::Warning).title("Save the tree first: photos are copied into a media folder next to it.").show(ui, |_| {});
                        }
                        ui.add(elegance::TextInput::new(&mut st.start).label("WikiTree IDs to start from").hint("e.g. Henshaw-1012 — separate several with commas").id_salt("wt_start"));
                        if !st.suggestions.is_empty() {
                            ui.label(RichText::new("Found in your tree:").size(12.0).color(p.text_muted));
                            ui.horizontal_wrapped(|ui| {
                                for (id, who) in &st.suggestions {
                                    let r = ui.add(egui::Button::new(RichText::new(format!("{id} · {who}")).size(12.5)).corner_radius(12));
                                    if r.clicked() && !st.start.split(',').any(|s| s.trim() == id) {
                                        if !st.start.trim().is_empty() {
                                            st.start.push_str(", ");
                                        }
                                        st.start.push_str(id);
                                    }
                                }
                            });
                        }
                        ui.horizontal(|ui| {
                            let ok = tree.is_some() && !st.start.trim().is_empty();
                            if ui.add(Button::new(format!("{}  Search WikiTree", glyphs::SEARCH)).accent(Accent::Green).enabled(ok)).clicked() {
                                search = true;
                            }
                            ui.label(RichText::new("Requests are spaced out, as WikiTree asks; a large family takes a minute or two.").size(12.0).color(p.text_faint));
                        });
                    }
                    WtStage::Running { status, done, total, .. } => {
                        ui.horizontal(|ui| {
                            ui.add(elegance::Spinner::new().size(18.0));
                            ui.label(RichText::new(status.as_str()).size(14.0));
                        });
                        if *total > 0 {
                            ui.add(elegance::ProgressBar::new(*done as f32 / *total as f32).text(format!("{done} of {total}")));
                        }
                        if ui.add(Button::new("Cancel").outline()).clicked() {
                            cancel_search = true;
                        }
                    }
                    WtStage::Failed(e) => {
                        elegance::Callout::new(elegance::CalloutTone::Danger).title(e.clone()).show(ui, |_| {});
                        if ui.add(Button::new("Back").outline()).clicked() {
                            st.stage = WtStage::Setup;
                        }
                    }
                    WtStage::Review { report, checked } => {
                        let people: HashSet<&str> = report.photos.iter().map(|ph| ph.xref.as_str()).collect();
                        let mut line = format!(
                            "{} of {} WikiTree profiles matched people in your tree · {} new photo{} for {} {}",
                            report.matched.len(),
                            report.profiles,
                            report.photos.len(),
                            if report.photos.len() == 1 { "" } else { "s" },
                            people.len(),
                            if people.len() == 1 { "person" } else { "people" }
                        );
                        if report.already > 0 {
                            line.push_str(&format!(" · {} already imported", report.already));
                        }
                        muted(ui, line);
                        if report.photos.is_empty() {
                            muted(ui, "No new photos. Try starting from someone in another branch of the family.");
                        }
                        egui::ScrollArea::vertical().max_height(460.0).show(ui, |ui| {
                            for (i, ph) in report.photos.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.checkbox(&mut checked[i], "");
                                    let (r, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                                    let uri = crate::platform::image_uri(&ph.file);
                                    if !crate::widgets::paint_cover(ui, &uri, r, 6) {
                                        ui.painter().rect_filled(r, 6, p.input_bg);
                                    }
                                    ui.vertical(|ui| {
                                        ui.spacing_mut().item_spacing.y = 1.0;
                                        ui.label(RichText::new(&ph.title).size(14.0).color(p.text));
                                        let who = doc.person(&ph.xref).map(|q| format!("{}  {}", q.display, q.lifespan())).unwrap_or_default();
                                        let theirs = report.matched.get(&ph.xref).map(|m| {
                                            let years = match (m.birth, m.death) {
                                                (Some(b), Some(d)) => format!("{b} – {d}"),
                                                (Some(b), None) => format!("b. {b}"),
                                                _ => String::new(),
                                            };
                                            format!("{} {years}", m.display)
                                        });
                                        ui.label(RichText::new(format!("For {who}  ↔  WikiTree: {}", theirs.unwrap_or_default())).size(12.5).color(p.text_muted));
                                        ui.horizontal(|ui| {
                                            let kind = if ph.kind == "photo" { "Photo".to_string() } else { format!("{} (added as a document)", ph.kind) };
                                            ui.label(RichText::new(format!("{kind} · {}×{}", ph.width, ph.height)).size(12.0).color(p.text_faint));
                                            let link = ui.add(egui::Label::new(RichText::new(format!("WikiTree {}", ph.profile)).size(12.0).color(p.focus).underline()).sense(Sense::click()));
                                            if link.on_hover_text(&ph.page_url).clicked() {
                                                crate::platform::open_url(ui.ctx(), &ph.page_url);
                                            }
                                            if ph.looks_like_badge() {
                                                ui.add(Badge::new("Small — may be a badge", BadgeTone::Warning).preserve_case());
                                            }
                                        });
                                    });
                                });
                                ui.add_space(4.0);
                            }
                        });
                        let n = checked.iter().filter(|c| **c).count();
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add(Button::new(format!("Add {n} photo{}", if n == 1 { "" } else { "s" })).accent(Accent::Green).enabled(n > 0)).clicked() {
                                add = true;
                            }
                            if ui.add(Button::new("Close").outline()).clicked() {
                                close = true;
                            }
                            ui.label(RichText::new("WikiTree photos are shared by its members and may be copyrighted.").size(11.5).color(p.text_faint));
                        });
                    }
                }
            });

        if search {
            let starts: Vec<String> = st.start.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let cancel = Arc::new(AtomicBool::new(false));
            let rx = wikitree::start(starts, wikitree::tree_people(doc), wikitree::imported_pages(doc), std::env::temp_dir().join("genie-wikitree"), cancel.clone());
            st.stage = WtStage::Running { rx, cancel, status: "Contacting WikiTree…".into(), done: 0, total: 0 };
        }
        if cancel_search && let WtStage::Running { cancel, status, .. } = &mut st.stage {
            cancel.store(true, Ordering::Relaxed);
            *status = "Stopping…".into();
        }
        if add
            && let (WtStage::Review { report, checked }, Some(tree)) = (&st.stage, &tree)
        {
            let chosen: Vec<&wikitree::FoundPhoto> = report.photos.iter().zip(checked).filter(|(_, c)| **c).map(|(ph, _)| ph).collect();
            let (n, failed) = wikitree::import(doc, tree, &chosen);
            for f in failed {
                Toast::new("Couldn't add a photo").tone(BadgeTone::Danger).description(f).show(ctx);
            }
            if n > 0 {
                Toast::new(format!("Added {n} photo{} from WikiTree", if n == 1 { "" } else { "s" })).tone(BadgeTone::Ok).description("Undo with Ctrl+Z.").show(ctx);
                if let Some(first) = chosen.first() {
                    actions.push(Action::Select(first.xref.clone()));
                }
            }
            close = true;
        }
        // Closing mid-search stops it.
        if (close || !st.open)
            && let WtStage::Running { cancel, .. } = &st.stage
        {
            cancel.store(true, Ordering::Relaxed);
        }
        if close || !st.open {
            self.wikitree = None;
        }
    }

    fn people_dialog(&mut self, ctx: &egui::Context, doc: &mut Document, dismissed: &mut HashSet<String>, actions: &mut Vec<Action>) {
        let Some(st) = self.people.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        let screen = ctx.content_rect();
        let mut merge = false;
        let mut not_dupe = false;
        let mut skip = false;
        let likely = st.list.iter().filter(|c| c.likely()).count();
        let subtitle = match st.list.len() {
            0 => "No likely duplicates found".to_string(),
            n => format!("{n} pair{} to review · {likely} likely", if n == 1 { "" } else { "s" }),
        };
        Modal::new("dupe_people", &mut st.open)
            .heading("Find duplicate people")
            .subtitle(subtitle)
            .max_width(crate::widgets::fit_width(ctx, (screen.width() - 80.0).clamp(700.0, 1200.0)))
            .show(ctx, |ui| {
                let height = (screen.height() - 240.0).clamp(300.0, 760.0);
                if st.list.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(glyphs::CIRCLE_CHECK.to_string()).size(36.0).color(p.green));
                        ui.label(RichText::new("Nothing to merge").size(18.0));
                        muted(ui, "No one in this tree looks like they were entered twice.");
                    });
                    ui.add_space(20.0);
                    return;
                }
                st.selected = st.selected.min(st.list.len() - 1);
                ui.horizontal_top(|ui| {
                    // The list of pairs.
                    ui.vertical(|ui| {
                        ui.set_width(330.0);
                        egui::ScrollArea::vertical().id_salt("dupe_list").max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                            for (i, c) in st.list.iter().enumerate() {
                                if pair_row(ui, doc, c, i == st.selected).clicked() && i != st.selected {
                                    st.selected = i;
                                    st.swapped = false;
                                    st.choices = MergeChoices::default();
                                }
                            }
                        });
                    });
                    ui.add_space(12.0);
                    // The selected pair side by side.
                    ui.vertical(|ui| {
                        ui.set_min_height(height);
                        egui::ScrollArea::vertical().id_salt("dupe_compare").max_height(height).auto_shrink([false, true]).show(ui, |ui| {
                        let c = st.list[st.selected].clone();
                        let (keep, other) = if st.swapped { (c.b.clone(), c.a.clone()) } else { (c.a.clone(), c.b.clone()) };
                        compare(ui, doc, &keep, &other, &c, &mut st.choices);
                        ui.add_space(10.0);
                        ui.horizontal_wrapped(|ui| {
                            let keep_name = doc.person(&keep).map(|p| p.display.clone()).unwrap_or_default();
                            if ui.add(Button::new(format!("Merge into {keep_name}")).accent(Accent::Green)).clicked() {
                                merge = true;
                            }
                            if ui.add(Button::new(format!("{}{}  Swap sides", glyphs::ARROW_LEFT, glyphs::ARROW_RIGHT)).outline()).on_hover_text("Keep the other record instead").clicked() {
                                st.swapped = !st.swapped;
                                st.choices = MergeChoices::default();
                            }
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.add(Button::new("Skip").outline()).on_hover_text("Decide later").clicked() {
                                    skip = true;
                                }
                                if ui.add(Button::new("Not duplicates").outline()).on_hover_text("They're different people; don't suggest them again").clicked() {
                                    not_dupe = true;
                                }
                            });
                        });
                        ui.label(RichText::new("Merging keeps everything: the other record's names, events, sources and documents are added, and differing values are kept as alternatives. Undo with Ctrl+Z.").size(12.0).color(p.text_faint));
                        });
                    });
                });
            });

        if st.list.is_empty() {
            if !st.open {
                self.people = None;
            }
            return;
        }
        let c = st.list[st.selected].clone();
        let (keep, other) = if st.swapped { (c.b.clone(), c.a.clone()) } else { (c.a.clone(), c.b.clone()) };
        if merge {
            let name = doc.person(&keep).map(|p| p.display.clone()).unwrap_or_default();
            let choices = st.choices;
            doc.mutate(|d| d.merge_people(&keep, &other, choices));
            Toast::new(format!("Merged into {name}")).tone(BadgeTone::Ok).description("Undo with Ctrl+Z.").show(ctx);
            actions.push(Action::MergedPerson { kept: keep.clone(), removed: other.clone() });
            // Pairs with the removed record are gone; scores may have changed.
            st.list = dedup::find_duplicate_people(doc, dismissed);
        } else if not_dupe {
            dismissed.insert(dedup::pair_key(&c.a, &c.b));
            st.list.remove(st.selected);
        } else if skip {
            st.selected = (st.selected + 1) % st.list.len().max(1);
        }
        if merge || not_dupe || skip {
            st.swapped = false;
            st.choices = MergeChoices::default();
        }
        if !st.open {
            self.people = None;
        }
    }

    fn sources_dialog(&mut self, ctx: &egui::Context, doc: &mut Document) {
        let Some(st) = self.sources.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        // How often each source is cited, to keep the most-used copy.
        let mut cited: HashMap<String, usize> = HashMap::new();
        fn count(n: &crate::gedcom::Node, cited: &mut HashMap<String, usize>) {
            for c in &n.children {
                if c.tag == "SOUR"
                    && let Some(x) = c.pointer()
                {
                    *cited.entry(x.to_string()).or_default() += 1;
                }
                count(c, cited);
            }
        }
        for r in doc.records() {
            count(r, &mut cited);
        }
        let mut merge: Vec<Vec<String>> = Vec::new();
        let extra: usize = st.groups.iter().map(|g| g.len() - 1).sum();
        Modal::new("dupe_sources", &mut st.open)
            .heading("Find duplicate sources")
            .subtitle(if st.groups.is_empty() { "No duplicate sources".to_string() } else { format!("{} sources recorded more than once · {extra} extra copies", st.groups.len()) })
            .max_width(crate::widgets::fit_width(ctx, 760.0))
            .show(ctx, |ui| {
                if st.groups.is_empty() {
                    ui.add_space(12.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(glyphs::CIRCLE_CHECK.to_string()).size(36.0).color(p.green));
                        muted(ui, "Every source in this tree has its own title.");
                    });
                    ui.add_space(12.0);
                    return;
                }
                muted(ui, "Sources with the same title and author. Merging keeps one copy, adds the others' notes and details to it, and points every citation at it.");
                ui.add_space(6.0);
                egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                    for g in &st.groups {
                        let r = doc.record(&g[0]);
                        let title = r.map(|r| r.child_value("TITL").to_string()).unwrap_or_default();
                        let author = r.map(|r| r.child_value("AUTH").to_string()).unwrap_or_default();
                        let uses: usize = g.iter().map(|x| cited.get(x).copied().unwrap_or(0)).sum();
                        egui::Frame::new().fill(p.input_bg).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(10).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(&title).size(14.0).color(p.text));
                                    let meta = [author.clone(), format!("{} copies · cited {uses} time{}", g.len(), if uses == 1 { "" } else { "s" })]
                                        .into_iter()
                                        .filter(|s| !s.is_empty())
                                        .collect::<Vec<_>>()
                                        .join(" · ");
                                    ui.label(RichText::new(meta).size(12.0).color(p.text_muted));
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(Button::new("Merge").size(ButtonSize::Small)).clicked() {
                                        merge.push(g.clone());
                                    }
                                });
                            });
                        });
                        ui.add_space(6.0);
                    }
                });
                ui.add_space(6.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new(format!("Merge all {}", st.groups.len())).accent(Accent::Green)).clicked() {
                        merge = st.groups.clone();
                    }
                });
            });
        if !merge.is_empty() {
            let n = merge.len();
            doc.mutate(|d| {
                for g in &merge {
                    // Keep the most-cited copy.
                    let mut g = g.clone();
                    g.sort_by_key(|x| std::cmp::Reverse(cited.get(x).copied().unwrap_or(0)));
                    for other in &g[1..] {
                        d.merge_records(&g[0], other);
                    }
                }
            });
            Toast::new(format!("Merged {n} source{}", if n == 1 { "" } else { "s" })).tone(BadgeTone::Ok).description("Undo with Ctrl+Z.").show(ctx);
            st.groups = dedup::find_duplicate_sources(doc);
        }
        if !st.open {
            self.sources = None;
        }
    }
}

fn pair_row(ui: &mut Ui, doc: &Document, c: &Candidate, selected: bool) -> egui::Response {
    let p = Theme::current(ui.ctx()).palette;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
    let fill = if selected {
        mix(p.card, p.focus, 0.14)
    } else if resp.hovered() {
        mix(p.card, p.focus, 0.05)
    } else {
        p.card
    };
    ui.painter().rect_filled(rect, 8, fill);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(10.0, 6.0))));
    // Selectable labels would take the click meant for the row.
    child.style_mut().interaction.selectable_labels = false;
    child.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            for x in [&c.a, &c.b] {
                if let Some(person) = doc.person(x) {
                    ui.label(RichText::new(format!("{}  {}", person.display, person.lifespan())).size(13.0).color(p.text));
                }
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (label, tone) = if c.likely() { ("Likely", BadgeTone::Ok) } else { ("Possible", BadgeTone::Warning) };
            ui.add(Badge::new(label, tone));
        });
    });
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The two records side by side, with a choice for each compared field.
fn compare(ui: &mut Ui, doc: &Document, keep: &str, other: &str, c: &Candidate, choices: &mut MergeChoices) {
    let p = Theme::current(ui.ctx()).palette;
    // Label column (130) plus the gaps between cells (3 × 8).
    let col = ((ui.available_width() - 130.0 - 24.0) / 2.0).max(150.0);
    ui.horizontal(|ui| {
        ui.add_space(130.0);
        for (x, label) in [(keep, "Keep"), (other, "Merge in")] {
            ui.vertical(|ui| {
                ui.set_width(col);
                if let Some(person) = doc.person(x) {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                        paint_avatar(ui, r.center(), 17.0, person);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(label.to_uppercase()).size(10.5).color(if label == "Keep" { p.focus } else { p.text_faint }));
                            ui.label(RichText::new(&person.display).size(15.0).family(crate::fonts::semibold()));
                        });
                    });
                }
            });
        }
    });
    // Each record's ancestry, as far back as it goes: the quickest way to
    // see which one carries the lineage.
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        ui.add_space(130.0);
        for x in [keep, other] {
            ui.vertical(|ui| {
                ui.set_width(col);
                ui.label(RichText::new(crate::minimap::summary_line(ui.ctx(), doc, x)).size(11.5).color(p.text_muted));
                egui::Frame::new().fill(mix(p.bg, p.card, 0.5)).stroke(Stroke::new(1.0, p.border)).corner_radius(8).inner_margin(6).show(ui, |ui| {
                    ui.set_width(col - 12.0);
                    let mut none = Vec::new();
                    crate::minimap::draw(ui, doc, x, &mut none, crate::minimap::MapOptions::lineage(col - 12.0, MAP_H));
                });
            });
            ui.add_space(8.0);
        }
    });
    ui.add_space(4.0);
    ui.label(RichText::new(format!("Why: {}", c.reasons.join(" · "))).size(12.0).color(p.text_muted));
    ui.add_space(6.0);

    let name = |x: &str| doc.record(x).and_then(|r| r.child("NAME")).map(|n| n.value.replace('/', "")).unwrap_or_default();
    let sex = |x: &str| doc.person(x).map(|p| p.sex.label().to_string()).unwrap_or_default();
    let rows: [(&str, String, String, &mut Side); 5] = [
        ("Name", name(keep), name(other), &mut choices.name),
        ("Sex", sex(keep), sex(other), &mut choices.sex),
        ("Birth", dedup::fact_summary(doc, keep, "BIRT"), dedup::fact_summary(doc, other, "BIRT"), &mut choices.birth),
        ("Death", dedup::fact_summary(doc, keep, "DEAT"), dedup::fact_summary(doc, other, "DEAT"), &mut choices.death),
        ("Occupation", dedup::fact_summary(doc, keep, "OCCU"), dedup::fact_summary(doc, other, "OCCU"), &mut choices.occupation),
    ];
    for (label, a, b, side) in rows {
        ui.horizontal(|ui| {
            ui.add_sized(vec2(122.0, 30.0), egui::Label::new(RichText::new(label).size(12.5).color(p.text_muted)));
            ui.add_space(8.0);
            let differ = !a.is_empty() && !b.is_empty() && a != b;
            // With only one side filled in, that side is what's kept.
            if !differ && a.is_empty() && !b.is_empty() {
                *side = Side::Other;
            } else if !differ {
                *side = Side::Keep;
            }
            for (value, this) in [(&a, Side::Keep), (&b, Side::Other)] {
                let chosen = *side == this && !value.is_empty();
                let (rect, resp) = ui.allocate_exact_size(vec2(col, 30.0), if differ { Sense::click() } else { Sense::hover() });
                let fill = if chosen && differ { mix(p.card, p.green, 0.14) } else { p.input_bg };
                let stroke = if chosen && differ { Stroke::new(1.5, p.green) } else { Stroke::new(1.0, p.border) };
                ui.painter().rect(rect, 6, fill, stroke, egui::StrokeKind::Inside);
                let text = if value.is_empty() { "—".to_string() } else { value.clone() };
                let font = egui::FontId::proportional(13.0);
                let text = crate::widgets::fit_text(ui, &text, &font, col - 34.0);
                ui.painter().text(rect.left_center() + vec2(10.0, 0.0), egui::Align2::LEFT_CENTER, text, font, if value.is_empty() { p.text_faint } else { p.text });
                if chosen && differ {
                    ui.painter().text(rect.right_center() - vec2(10.0, 0.0), egui::Align2::RIGHT_CENTER, glyphs::CHECK.to_string(), egui::FontId::proportional(14.0), p.green);
                }
                if differ && resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Use this one; the other is kept as an alternative").clicked() {
                    *side = this;
                }
                ui.add_space(8.0);
            }
        });
    }

    ui.add_space(6.0);
    section_label(ui, "Also on each record");
    let facts = |x: &str| -> Vec<String> {
        let parents: Vec<String> = doc.father(x).into_iter().chain(doc.mother(x)).filter_map(|y| doc.person(&y).map(|p| p.display.clone())).collect();
        let spouses: Vec<String> = doc.spouse_families(x).iter().filter_map(|f| doc.spouse_in(f, x)).filter_map(|y| doc.person(&y).map(|p| p.display.clone())).collect();
        let kids: usize = doc.spouse_families(x).iter().map(|f| doc.children(f).len()).sum();
        let events = doc.record(x).map(|r| r.children.iter().filter(|c| crate::model::event_label(&c.tag).is_some()).count()).unwrap_or(0);
        let cites = doc.form_for(x).citations.len();
        vec![
            if parents.is_empty() { "—".into() } else { parents.join(", ") },
            if spouses.is_empty() { "—".into() } else { spouses.join(", ") },
            format!("{kids} children · {events} events · {cites} citations"),
        ]
    };
    let (fa, fb) = (facts(keep), facts(other));
    for (i, label) in ["Parents", "Spouses", "Records"].iter().enumerate() {
        ui.horizontal(|ui| {
            ui.add_sized(vec2(122.0, 22.0), egui::Label::new(RichText::new(*label).size(12.5).color(p.text_muted)));
            ui.add_space(8.0);
            for v in [&fa[i], &fb[i]] {
                let font = egui::FontId::proportional(12.5);
                let t = crate::widgets::fit_text(ui, v, &font, col - 10.0);
                ui.add_sized(vec2(col, 22.0), egui::Label::new(RichText::new(t).font(font).color(p.text)));
                ui.add_space(8.0);
            }
        });
    }
}
