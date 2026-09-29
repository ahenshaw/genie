//! Documents and photos in the UI: the Media section, the profile card, the
//! viewer and the editor.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Align, Align2, FontId, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, Vec2, vec2};
use elegance::{
    Accent, Badge, BadgeTone, Button, ButtonSize, Callout, CalloutTone, Card, ContextMenu, Drawer, DrawerSide, MenuItem,
    Modal, Segment, SegmentedControl, SegmentedSize, Select, TextArea, TextInput, Theme, glyphs,
};

use crate::app::Action;
use crate::media::{self, KINDS, MediaItem, MediaPlace, MediaUse};
use crate::model::{Document, GENERAL_FACT};
use crate::widgets::{media_uri, mix, muted, paint_cover, section_label, tree_path};

const CARD_W: f32 = 180.0;
const CARD_THUMB_H: f32 = 130.0;
const CARD_H: f32 = CARD_THUMB_H + 58.0;
const CARD_GAP: f32 = 14.0;

/// Where every document is linked from, recomputed when the tree changes.
pub fn index(ctx: &egui::Context, doc: &Document) -> Arc<HashMap<String, Vec<MediaUse>>> {
    let key = egui::Id::new("media_index");
    if let Some((rev, idx)) = ctx.data(|d| d.get_temp::<(u64, Arc<HashMap<String, Vec<MediaUse>>>)>(key))
        && rev == doc.revision
    {
        return idx;
    }
    let idx = Arc::new(doc.media_index());
    ctx.data_mut(|d| d.insert_temp(key, (doc.revision, idx.clone())));
    idx
}

/// A file is recorded but isn't where the tree says.
pub fn is_missing(ctx: &egui::Context, item: &MediaItem) -> bool {
    item.has_file() && media::resolve(tree_path(ctx).as_deref(), &item.file).is_none_or(|p| !crate::platform::exists(&p))
}

/// A thumbnail: the image itself, or the file type for other files.
pub fn thumb(ui: &mut Ui, item: &MediaItem, size: Vec2) -> egui::Response {
    let p = Theme::current(ui.ctx()).palette;
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let hovered = resp.hovered();
    ui.painter().rect_filled(rect, 8, mix(p.input_bg, p.card, 0.3));
    let missing = is_missing(ui.ctx(), item);
    let small = size.y < 60.0;
    if !item.has_file() {
        ui.painter().text(rect.center() - vec2(0.0, if small { 0.0 } else { 8.0 }), Align2::CENTER_CENTER, glyphs::INFO.to_string(), FontId::proportional(if small { 13.0 } else { 20.0 }), p.text_faint);
        if !small {
            ui.painter().text(rect.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, "No file", FontId::proportional(11.5), p.text_faint);
        }
    } else if missing {
        ui.painter().text(rect.center() - vec2(0.0, if small { 0.0 } else { 8.0 }), Align2::CENTER_CENTER, glyphs::CIRCLE_ALERT.to_string(), FontId::proportional(if small { 14.0 } else { 22.0 }), p.amber);
        if !small {
            ui.painter().text(rect.center() + vec2(0.0, 14.0), Align2::CENTER_CENTER, "Missing file", FontId::proportional(11.5), p.amber);
        }
    } else if item.is_image() && media_uri(ui.ctx(), &item.file).is_some_and(|uri| paint_cover(ui, &uri, rect, 8)) {
        // Drawn.
    } else if item.is_image() {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "…", FontId::proportional(16.0), p.text_faint);
    } else {
        let ext = media::file_name(&item.file).rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_else(|| "FILE".into());
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, ext, FontId::proportional(if small { 10.0 } else { 20.0 }), p.text_muted);
    }
    let stroke = if hovered { Stroke::new(1.5, p.focus) } else { Stroke::new(1.0, p.border) };
    ui.painter().rect_stroke(rect, 8, stroke, StrokeKind::Inside);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn uses_label(doc: &Document, uses: &[MediaUse]) -> String {
    let mut owners: Vec<String> = Vec::new();
    for u in uses {
        let l = doc.owner_label(&u.owner);
        if !owners.contains(&l) {
            owners.push(l);
        }
    }
    match owners.len() {
        0 => "Not attached".into(),
        1 => owners[0].clone(),
        n => format!("{} +{}", owners[0], n - 1),
    }
}

/// The right-click menu shared by every thumbnail.
fn thumb_menu(resp: &egui::Response, doc: &Document, item: &MediaItem, person: Option<&str>, actions: &mut Vec<Action>) {
    ContextMenu::new(("media_ctx", resp.id)).show(resp, |ui| {
        if ui.add(MenuItem::new("View")).clicked() {
            actions.push(Action::ViewMedia { media: item.xref.clone(), person: person.map(str::to_string) });
        }
        if ui.add(MenuItem::new("Edit…").icon(glyphs::PENCIL.to_string())).clicked() {
            actions.push(Action::EditMedia(item.xref.clone()));
        }
        if let Some(x) = person
            && item.is_image()
        {
            if doc.primary_media(x).as_deref() == Some(item.xref.as_str()) {
                if ui.add(MenuItem::new("Remove as profile photo")).clicked() {
                    actions.push(Action::SetPrimaryPhoto { person: x.to_string(), media: None });
                }
            } else if ui.add(MenuItem::new("Use as profile photo")).clicked() {
                actions.push(Action::SetPrimaryPhoto { person: x.to_string(), media: Some(item.xref.clone()) });
            }
        }
        ui.separator();
        if ui.add(MenuItem::new("Open file").icon(glyphs::EXTERNAL_LINK.to_string())).clicked() {
            open_file(ui.ctx(), item, false);
        }
        if ui.add(MenuItem::new("Show in folder").icon(glyphs::FOLDER_OPEN.to_string())).clicked() {
            open_file(ui.ctx(), item, true);
        }
    });
}

/// Opens the file (or its folder) with the system's own application.
pub fn open_file(ctx: &egui::Context, item: &MediaItem, folder: bool) {
    let Some(path) = media::resolve(tree_path(ctx).as_deref(), &item.file).filter(|p| crate::platform::exists(p)) else {
        elegance::Toast::new("File not found").tone(BadgeTone::Warning).description(item.file.clone()).show(ctx);
        return;
    };
    let target = if folder { path.parent().map(PathBuf::from).unwrap_or(path) } else { path };
    if let Err(e) = crate::platform::open_path(ctx, &target) {
        elegance::Toast::new("Couldn't open it").tone(BadgeTone::Danger).description(e).show(ctx);
    }
}

// ---- the profile card ----------------------------------------------------------------------

/// Documents attached to a person, or to the families they're a partner in.
pub fn person_media(ctx: &egui::Context, doc: &Document, xref: &str) -> Vec<(MediaItem, Vec<MediaUse>)> {
    let idx = index(ctx, doc);
    let fams = doc.spouse_families(xref);
    let mut out: Vec<(MediaItem, Vec<MediaUse>)> = Vec::new();
    for item in doc.media_items() {
        let uses: Vec<MediaUse> = idx
            .get(&item.xref)
            .into_iter()
            .flatten()
            .filter(|u| u.owner == xref || fams.contains(&u.owner))
            .cloned()
            .collect();
        if !uses.is_empty() {
            out.push((item, uses));
        }
    }
    // The profile photo first, then photos, then everything else.
    out.sort_by_key(|(m, uses)| (!uses.iter().any(|u| u.primary && u.owner == xref), !m.is_image()));
    out
}

pub fn profile_card(ui: &mut Ui, doc: &Document, xref: &str, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    let items = person_media(ui.ctx(), doc, xref);
    Card::new().show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Documents & photos").size(16.0).color(p.text));
            if !items.is_empty() {
                ui.add(Badge::new(items.len().to_string(), BadgeTone::Neutral));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new(format!("{}  Add", glyphs::PLUS)).outline().size(ButtonSize::Small)).on_hover_text("Attach files to this person").clicked() {
                    actions.push(Action::PickMedia(Some(xref.to_string())));
                }
            });
        });
        if items.is_empty() {
            muted(ui, "Drop photos or scans onto the window to attach them, or click Add.");
            return;
        }
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(10.0, 10.0);
            for (item, uses) in &items {
                ui.vertical(|ui| {
                    ui.set_width(104.0);
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let r = thumb(ui, item, vec2(104.0, 78.0));
                    if uses.iter().any(|u| u.primary && u.owner == xref) {
                        ui.painter().text(r.rect.right_top() + vec2(-6.0, 6.0), Align2::RIGHT_TOP, "★", FontId::proportional(14.0), p.amber);
                    }
                    let r = r.on_hover_text(item.display_title());
                    if r.clicked() {
                        actions.push(Action::ViewMedia { media: item.xref.clone(), person: Some(xref.to_string()) });
                    }
                    thumb_menu(&r, doc, item, Some(xref), actions);
                    let title = crate::widgets::fit_text(ui, &item.display_title(), &FontId::proportional(12.0), 104.0);
                    ui.label(RichText::new(title).size(12.0).color(p.text));
                    let place = uses.iter().map(|u| doc.place_label(&u.owner, &u.place)).find(|l| !l.is_empty()).unwrap_or_else(|| item.kind_label().to_string());
                    let place = crate::widgets::fit_text(ui, &place, &FontId::proportional(11.0), 104.0);
                    ui.label(RichText::new(place).size(11.0).color(p.text_faint));
                });
            }
        });
    });
}

// ---- the Media section, viewer and editor ------------------------------------------------------

#[derive(Default)]
pub struct MediaUi {
    query: String,
    filter: usize,
    viewer: Option<Viewer>,
    editor: Option<Editor>,
}

struct Viewer {
    open: bool,
    media: String,
    /// The person it was opened from, for "Use as profile photo".
    person: Option<String>,
    scene: Rect,
    confirm_delete: bool,
}

struct Editor {
    open: bool,
    item: MediaItem,
    person_query: String,
    attach_person: Option<String>,
    attach_place: Option<(String, MediaPlace)>,
}

const FILTERS: [&str; 6] = ["All", "Photos", "Documents", "Unattached", "Missing", "No file"];

impl MediaUi {
    pub fn view(&mut self, media: String, person: Option<String>) {
        self.viewer = Some(Viewer { open: true, media, person, scene: Rect::ZERO, confirm_delete: false });
    }

    pub fn edit(&mut self, doc: &Document, media: &str) {
        if let Some(item) = doc.media_item(media) {
            self.editor = Some(Editor { open: true, item, person_query: String::new(), attach_person: None, attach_place: None });
        }
    }

    pub fn section(&mut self, ui: &mut Ui, doc: &Document, actions: &mut Vec<Action>) {
        let p = Theme::current(ui.ctx()).palette;
        let items = doc.media_items();
        let idx = index(ui.ctx(), doc);
        let missing: Vec<bool> = items.iter().map(|m| is_missing(ui.ctx(), m)).collect();
        let counts = [
            items.len(),
            items.iter().filter(|m| m.is_image()).count(),
            items.iter().filter(|m| !m.is_image()).count(),
            items.iter().filter(|m| idx.get(&m.xref).is_none_or(Vec::is_empty)).count(),
            missing.iter().filter(|m| **m).count(),
            items.iter().filter(|m| !m.has_file()).count(),
        ];

        ui.horizontal(|ui| {
            ui.label(RichText::new("Media").size(18.0).family(crate::fonts::semibold()).color(p.text));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add(Button::new(format!("{}  Add files", glyphs::PLUS)).size(ButtonSize::Small)).on_hover_text("Add documents to the tree; attach them to people later").clicked() {
                    actions.push(Action::PickMedia(None));
                }
                if counts[4] + counts[5] > 0
                    && ui
                        .add(Button::new(format!("{}  Find files…", glyphs::SEARCH)).accent(Accent::Amber).size(ButtonSize::Small))
                        .on_hover_text("Search a folder for missing files, and for files belonging to records that have none")
                        .clicked()
                {
                    actions.push(Action::FindMissingMedia);
                }
            });
        });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            // "No file" only appears when there are some.
            let shown_filters = if counts[5] > 0 { FILTERS.len() } else { FILTERS.len() - 1 };
            self.filter = self.filter.min(shown_filters - 1);
            let segments: Vec<Segment> = FILTERS
                .iter()
                .zip(counts)
                .take(shown_filters)
                .enumerate()
                .map(|(i, (f, n))| {
                    let s = Segment::text(*f).count(n.to_string());
                    if i == 4 && n > 0 { s.dot(elegance::SegmentDot::Amber) } else { s }
                })
                .collect();
            ui.add(SegmentedControl::from_segments(&mut self.filter, segments).size(SegmentedSize::Small).id_salt("media_filter"));
            crate::widgets::search_input(ui, &mut self.query, "Search titles, files, people…", "media_search", Some(220.0));
        });
        ui.add_space(8.0);

        if doc.path.is_none() {
            Callout::new(CalloutTone::Info).title("Save the tree to add documents.").body("Files are copied into a media folder next to the tree file.").show(ui, |_| {});
            ui.add_space(8.0);
        }
        if counts[5] > 0 {
            Callout::new(CalloutTone::Info)
                .title(format!("{} document{} came without {}", counts[5], if counts[5] == 1 { "" } else { "s" }, if counts[5] == 1 { "its file" } else { "their files" }))
                .body("Programs like Ancestry export the record of a photo but not the image. Download the images (for example with Family Tree Maker or RootsMagic), then use Find files… and point it at the folder; Genie matches them by name and size and copies them in. Or open one and choose Add file….")
                .multiline()
                .show(ui, |_| {});
            ui.add_space(8.0);
        }
        let embedded = doc.embedded_media_count();
        if embedded > 0 {
            Callout::new(CalloutTone::Neutral)
                .title(format!("{embedded} embedded media link{} not shown", if embedded == 1 { "" } else { "s" }))
                .body("They're written in an older style that Genie keeps but doesn't display.")
                .show(ui, |_| {});
            ui.add_space(8.0);
        }

        let needle = self.query.trim().to_lowercase();
        let shown: Vec<usize> = (0..items.len())
            .filter(|&i| {
                let m = &items[i];
                match self.filter {
                    1 => m.is_image(),
                    2 => !m.is_image(),
                    3 => idx.get(&m.xref).is_none_or(Vec::is_empty),
                    4 => missing[i],
                    5 => !m.has_file(),
                    _ => true,
                }
            })
            .filter(|&i| {
                if needle.is_empty() {
                    return true;
                }
                let m = &items[i];
                let people = idx.get(&m.xref).map(|u| u.iter().map(|u| doc.owner_label(&u.owner)).collect::<Vec<_>>().join(" ")).unwrap_or_default();
                format!("{} {} {} {} {people}", m.title, m.file, m.kind, m.date).to_lowercase().contains(&needle)
            })
            .collect();

        if shown.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                if items.is_empty() {
                    ui.label(RichText::new(glyphs::FOLDER.to_string()).size(40.0).color(p.focus));
                    ui.label(RichText::new("No documents yet").size(20.0).family(crate::fonts::semibold()));
                    muted(ui, "Add photos, certificates and scans here, or drop them on someone's profile.");
                } else {
                    muted(ui, "Nothing matches.");
                }
            });
            return;
        }

        let cols = ((ui.available_width() + CARD_GAP) / (CARD_W + CARD_GAP)).floor().max(1.0) as usize;
        let rows = shown.len().div_ceil(cols);
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("media_grid").show_rows(ui, CARD_H, rows, |ui, range| {
            ui.spacing_mut().item_spacing.y = CARD_GAP;
            for row in range {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = CARD_GAP;
                    for &i in shown.iter().skip(row * cols).take(cols) {
                        let m = &items[i];
                        let uses = idx.get(&m.xref).cloned().unwrap_or_default();
                        ui.vertical(|ui| {
                            ui.set_width(CARD_W);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let r = thumb(ui, m, vec2(CARD_W, CARD_THUMB_H)).on_hover_text(m.display_title());
                            if r.clicked() {
                                actions.push(Action::ViewMedia { media: m.xref.clone(), person: None });
                            }
                            thumb_menu(&r, doc, m, None, actions);
                            let font = FontId::new(13.0, crate::fonts::semibold());
                            ui.label(RichText::new(crate::widgets::fit_text(ui, &m.display_title(), &font, CARD_W)).font(font).color(p.text));
                            let meta = [m.kind_label().to_string(), crate::model::pretty_date(&m.date)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                            ui.label(RichText::new(meta).size(11.5).color(p.text_muted));
                            let who = uses_label(doc, &uses);
                            let color = if uses.is_empty() { p.amber } else { p.text_faint };
                            ui.label(RichText::new(crate::widgets::fit_text(ui, &who, &FontId::proportional(11.5), CARD_W)).size(11.5).color(color));
                        });
                    }
                });
            }
        });
    }

    /// The viewer and editor, drawn over everything. They edit the tree
    /// directly (each change one undo step).
    pub fn overlays(&mut self, ctx: &egui::Context, doc: &mut Document, actions: &mut Vec<Action>) {
        self.viewer_modal(ctx, doc, actions);
        self.editor_drawer(ctx, doc);
    }

    fn viewer_modal(&mut self, ctx: &egui::Context, doc: &mut Document, actions: &mut Vec<Action>) {
        let Some(v) = self.viewer.as_mut() else { return };
        let Some(item) = doc.media_item(&v.media) else {
            self.viewer = None;
            return;
        };
        let p = Theme::current(ctx).palette;
        let uses = index(ctx, doc).get(&item.xref).cloned().unwrap_or_default();
        let missing = is_missing(ctx, &item);
        let subtitle = [item.kind_label().to_string(), crate::model::pretty_date(&item.date)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        let screen = ctx.content_rect();
        let mut close = false;
        let mut delete = false;
        let mut detach: Option<MediaUse> = None;
        let mut edit = false;
        Modal::new("media_viewer", &mut v.open)
            .heading(item.display_title())
            .subtitle(subtitle)
            .max_width(crate::widgets::fit_width(ctx, (screen.width() - 80.0).clamp(600.0, 1400.0)))
            .show(ctx, |ui| {
                let height = (screen.height() - 220.0).clamp(300.0, 900.0);
                ui.horizontal_top(|ui| {
                    let side = 300.0;
                    let view_w = (ui.available_width() - side - 16.0).max(300.0);
                    egui::Frame::new().fill(mix(p.bg, p.card, 0.4)).stroke(Stroke::new(1.0, p.border)).corner_radius(10).show(ui, |ui| {
                        ui.set_min_size(vec2(view_w, height));
                        ui.set_max_size(vec2(view_w, height));
                        let uri = media_uri(ui.ctx(), &item.file);
                        match uri {
                            Some(uri) if item.is_image() && !missing => {
                                egui::Scene::new().zoom_range(0.02..=8.0).max_inner_size(vec2(20_000.0, 20_000.0)).show(ui, &mut v.scene, |ui| {
                                    ui.add(egui::Image::new(uri).fit_to_original_size(1.0));
                                });
                            }
                            _ => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(height / 3.0);
                                    if !item.has_file() {
                                        ui.label(RichText::new(glyphs::INFO.to_string()).size(40.0).color(p.text_faint));
                                        ui.label(RichText::new("This record came without its file").size(16.0));
                                        ui.label(RichText::new("The program it was exported from kept the image back.").size(12.5).color(p.text_muted));
                                        ui.add_space(8.0);
                                        if ui.add(Button::new(format!("{}  Add file…", glyphs::PLUS))).clicked() {
                                            actions.push(Action::ReplaceMediaFile(item.xref.clone()));
                                        }
                                    } else if missing {
                                        ui.label(RichText::new(glyphs::CIRCLE_ALERT.to_string()).size(40.0).color(p.amber));
                                        ui.label(RichText::new("The file isn't where the tree says it is").size(16.0));
                                        ui.label(RichText::new(&item.file).size(12.0).color(p.text_faint));
                                        ui.add_space(8.0);
                                        if ui.add(Button::new(format!("{}  Locate…", glyphs::SEARCH)).accent(Accent::Amber)).clicked() {
                                            actions.push(Action::LocateMedia(item.xref.clone()));
                                        }
                                    } else {
                                        let ext = media::file_name(&item.file).rsplit_once('.').map(|(_, e)| e.to_uppercase()).unwrap_or_else(|| "FILE".into());
                                        ui.label(RichText::new(ext).size(40.0).color(p.text_muted));
                                        ui.label(RichText::new(media::file_name(&item.file)).size(14.0));
                                        ui.add_space(8.0);
                                        if ui.add(Button::new(format!("{}  Open", glyphs::EXTERNAL_LINK))).clicked() {
                                            open_file(ui.ctx(), &item, false);
                                        }
                                    }
                                });
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        ui.set_width(side);
                        ui.spacing_mut().item_spacing.y = 6.0;
                        if !item.note.is_empty() {
                            ui.label(RichText::new(&item.note).size(13.5).color(p.text));
                        }
                        ui.label(RichText::new(&item.file).size(11.5).color(p.text_faint));
                        ui.horizontal_wrapped(|ui| {
                            if ui.add(Button::new(format!("{}  Edit", glyphs::PENCIL)).outline().size(ButtonSize::Small)).clicked() {
                                edit = true;
                            }
                            if ui.add(Button::new(format!("{}  Open", glyphs::EXTERNAL_LINK)).outline().size(ButtonSize::Small).enabled(item.has_file() && !missing)).clicked() {
                                open_file(ui.ctx(), &item, false);
                            }
                            if ui.add(Button::new(format!("{}  Folder", glyphs::FOLDER_OPEN)).outline().size(ButtonSize::Small).enabled(item.has_file() && !missing)).on_hover_text("Show in folder").clicked() {
                                open_file(ui.ctx(), &item, true);
                            }
                            if missing && ui.add(Button::new(format!("{}  Locate…", glyphs::SEARCH)).accent(Accent::Amber).size(ButtonSize::Small)).clicked() {
                                actions.push(Action::LocateMedia(item.xref.clone()));
                            }
                        });
                        if let Some(person) = &v.person
                            && item.is_image()
                        {
                            let is_primary = uses.iter().any(|u| u.primary && &u.owner == person);
                            let label = if is_primary { "Profile photo ✓ — remove" } else { "Use as profile photo" };
                            if ui.add(Button::new(label).outline().size(ButtonSize::Small)).clicked() {
                                actions.push(Action::SetPrimaryPhoto { person: person.clone(), media: (!is_primary).then(|| item.xref.clone()) });
                            }
                        }
                        ui.add_space(6.0);
                        section_label(ui, "Attached to");
                        if uses.is_empty() {
                            muted(ui, "Nothing yet. Edit to attach it to someone.");
                        }
                        for u in &uses {
                            ui.horizontal(|ui| {
                                let who = doc.owner_label(&u.owner);
                                let r = ui.add(egui::Label::new(RichText::new(&who).size(13.5).color(p.focus)).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
                                if r.clicked() && doc.person(&u.owner).is_some() {
                                    actions.push(Action::Select(u.owner.clone()));
                                    actions.push(Action::SetTab(crate::app::TAB_PROFILE));
                                    close = true;
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(egui::Button::new(RichText::new(glyphs::X.to_string()).color(p.text_faint)).frame(false)).on_hover_text("Detach").clicked() {
                                        detach = Some(u.clone());
                                    }
                                });
                            });
                            let place = doc.place_label(&u.owner, &u.place);
                            if !place.is_empty() {
                                ui.label(RichText::new(place).size(12.0).color(p.text_muted));
                            }
                        }
                        ui.add_space(10.0);
                        if v.confirm_delete {
                            muted(ui, "Removes it from the tree and everyone it's attached to. The file stays in the media folder.");
                            ui.horizontal(|ui| {
                                if ui.add(Button::new("Delete").accent(Accent::Red).size(ButtonSize::Small)).clicked() {
                                    delete = true;
                                }
                                if ui.add(Button::new("Cancel").outline().size(ButtonSize::Small)).clicked() {
                                    v.confirm_delete = false;
                                }
                            });
                        } else if ui.add(Button::new(format!("{}  Delete from tree…", glyphs::TRASH)).outline().size(ButtonSize::Small)).clicked() {
                            v.confirm_delete = true;
                        }
                    });
                });
            });
        if let Some(u) = detach {
            doc.mutate(|d| d.detach_media(&item.xref, &u.owner, &u.place));
        }
        if delete {
            doc.mutate(|d| d.delete_media(&item.xref));
            elegance::Toast::new("Document removed from the tree").description("Undo with Ctrl+Z.").show(ctx);
            close = true;
        }
        if edit {
            self.edit(doc, &item.xref);
            close = true;
        }
        if close || self.viewer.as_ref().is_some_and(|v| !v.open) {
            self.viewer = None;
        }
    }

    fn editor_drawer(&mut self, ctx: &egui::Context, doc: &mut Document) {
        let Some(ed) = self.editor.as_mut() else { return };
        let p = Theme::current(ctx).palette;
        let uses = index(ctx, doc).get(&ed.item.xref).cloned().unwrap_or_default();
        let mut save = false;
        let mut close = false;
        let mut detach: Option<MediaUse> = None;
        let mut attach: Option<(String, MediaPlace)> = None;
        let mut replace = false;
        Drawer::new("media_editor", &mut ed.open)
            .side(DrawerSide::Right)
            .width(crate::widgets::fit_width(ctx, 460.0))
            .title("Edit document")
            .subtitle(media::file_name(&ed.item.file))
            .show(ctx, |ui| {
                let footer_h = 60.0;
                let body_h = (ui.available_height() - footer_h).max(0.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), body_h), Layout::top_down(Align::Min), |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        ui.horizontal(|ui| {
                            thumb(ui, &ed.item, vec2(96.0, 72.0));
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&ed.item.file).size(12.0).color(p.text_faint));
                                if ui.add(Button::new("Replace file…").outline().size(ButtonSize::Small)).clicked() {
                                    replace = true;
                                }
                            });
                        });
                        ui.add(TextInput::new(&mut ed.item.title).label("Title").hint("e.g. 1900 census, Gloucester").id_salt("media_title"));
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new("Type").size(12.0).color(p.text_muted));
                                let mut options: Vec<(String, String)> = KINDS.iter().map(|(k, l)| (k.to_string(), l.to_string())).collect();
                                if !ed.item.kind.is_empty() && !KINDS.iter().any(|(k, _)| *k == ed.item.kind) {
                                    options.push((ed.item.kind.clone(), ed.item.kind.clone()));
                                }
                                if ed.item.kind.is_empty() {
                                    options.insert(0, (String::new(), "Not set".into()));
                                }
                                ui.add(Select::new("media_kind", &mut ed.item.kind).options(options).width(190.0));
                            });
                            ui.vertical(|ui| {
                                ui.add(TextInput::new(&mut ed.item.date).label("Date").hint("e.g. 1900").desired_width(170.0).id_salt("media_date"));
                            });
                        });
                        ui.add(TextArea::new(&mut ed.item.note).label("Notes").rows(3).id_salt("media_note"));

                        ui.add_space(6.0);
                        section_label(ui, "Attached to");
                        if uses.is_empty() {
                            muted(ui, "Not attached to anyone yet.");
                        }
                        for u in &uses {
                            ui.horizontal(|ui| {
                                let place = doc.place_label(&u.owner, &u.place);
                                let text = if place.is_empty() { doc.owner_label(&u.owner) } else { format!("{} · {place}", doc.owner_label(&u.owner)) };
                                ui.label(RichText::new(text).size(13.0));
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if ui.add(egui::Button::new(RichText::new(glyphs::X.to_string()).color(p.text_faint)).frame(false)).on_hover_text("Detach").clicked() {
                                        detach = Some(u.clone());
                                    }
                                });
                            });
                        }
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Attach to").size(12.0).color(p.text_muted));
                            if let Some(x) = crate::relation::person_picker(ui, doc, "media_attach_person", ed.attach_person.as_deref(), &mut ed.person_query) {
                                ed.attach_place = Some((x.clone(), MediaPlace::Whole));
                                ed.attach_person = Some(x);
                            }
                        });
                        if let Some(x) = ed.attach_person.clone() {
                            let options = attach_options(doc, &x);
                            if ed.attach_place.as_ref().is_none_or(|pl| !options.iter().any(|(o, _)| o == pl)) {
                                ed.attach_place = options.first().map(|(o, _)| o.clone());
                            }
                            if let Some(place) = ed.attach_place.as_mut() {
                                ui.add(Select::new("media_attach_place", place).options(options).width(ui.available_width()));
                            }
                            if ui.add(Button::new(format!("{}  Attach", glyphs::PLUS)).size(ButtonSize::Small)).clicked() {
                                attach = ed.attach_place.clone();
                            }
                        }
                    });
                });
                ui.separator();
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("Save").accent(Accent::Green)).clicked() {
                        save = true;
                    }
                    if ui.add(Button::new("Close").outline()).clicked() {
                        close = true;
                    }
                });
            });
        if let Some(u) = detach {
            let m = ed.item.xref.clone();
            doc.mutate(|d| d.detach_media(&m, &u.owner, &u.place));
        }
        if let Some((owner, place)) = attach {
            let m = ed.item.xref.clone();
            doc.mutate(|d| d.attach_media(&m, &owner, &place));
            ed.attach_person = None;
        }
        if replace
            && let Some(tree) = doc.path.clone()
            && let Some(src) = crate::platform::pick_file(None)
        {
            match media::import_file(&tree, &src) {
                Ok(rel) => ed.item.file = rel,
                Err(e) => elegance::Toast::new("Couldn't copy the file").tone(BadgeTone::Danger).description(e.to_string()).show(ctx),
            }
        }
        if save {
            let item = ed.item.clone();
            if doc.media_item(&item.xref).as_ref() != Some(&item) {
                doc.mutate(|d| d.update_media(&item));
            }
            elegance::Toast::new("Document saved").tone(BadgeTone::Ok).show(ctx);
            ed.open = false;
        }
        if close || !ed.open {
            self.editor = None;
        }
    }
}

/// What a document can be attached to for one person.
fn attach_options(doc: &Document, x: &str) -> Vec<((String, MediaPlace), String)> {
    let mut out = vec![((x.to_string(), MediaPlace::Whole), "The person".to_string())];
    for (key, label) in doc.facts(x) {
        if key.0 == GENERAL_FACT {
            continue;
        }
        out.push(((x.to_string(), MediaPlace::Fact(key)), label));
    }
    let mut per_fact: HashMap<String, usize> = HashMap::new();
    for c in doc.form_for(x).citations {
        let slot = per_fact.entry(format!("{}#{}", c.fact.0, c.fact.1)).or_default();
        let place = MediaPlace::Citation(c.fact.clone(), *slot);
        *slot += 1;
        let label = doc.place_label(x, &place);
        out.push(((x.to_string(), place), label));
    }
    for f in doc.spouse_families(x) {
        let with = doc.spouse_in(&f, x).and_then(|s| doc.person(&s).map(|p| p.display.clone())).unwrap_or_else(|| "unknown partner".into());
        out.push(((f.clone(), MediaPlace::Whole), format!("Family with {with}")));
        if doc.family_event(&f, "MARR").is_some() {
            out.push(((f.clone(), MediaPlace::Fact(("MARR".into(), 0))), format!("Marriage to {with}")));
        }
    }
    out
}

/// Draws a small row of citation thumbnails; returns the one clicked.
pub fn mini_thumbs(ui: &mut Ui, doc: &Document, media: &[String]) -> Option<String> {
    let mut clicked = None;
    for m in media {
        if let Some(item) = doc.media_item(m) {
            let r = thumb(ui, &item, vec2(34.0, 26.0)).on_hover_text(item.display_title());
            if r.clicked() {
                clicked = Some(m.clone());
            }
        }
    }
    clicked
}
