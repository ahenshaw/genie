//! Small painted building blocks shared by the views.

use egui::{Align2, Color32, CornerRadius, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};
use elegance::{AvatarTone, Palette, Theme};

use crate::model::{Document, PersonSummary, Sex};

pub fn sex_color(p: &Palette, sex: Sex) -> Color32 {
    match sex {
        Sex::Male => p.blue,
        Sex::Female => p.purple,
        Sex::Unknown => p.text_faint,
    }
}

pub fn sex_tone(sex: Sex) -> AvatarTone {
    match sex {
        Sex::Male => AvatarTone::Sky,
        Sex::Female => AvatarTone::Purple,
        Sex::Unknown => AvatarTone::Neutral,
    }
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// The open tree's path, published by the app each frame, so media paths
/// (relative to it) can be resolved anywhere.
pub fn tree_path(ctx: &egui::Context) -> Option<std::path::PathBuf> {
    ctx.data(|d| d.get_temp::<Option<std::path::PathBuf>>(egui::Id::new("genie_tree_path"))).flatten()
}

/// An image URI for a GEDCOM media path, if the file exists.
pub fn media_uri(ctx: &egui::Context, file: &str) -> Option<String> {
    let path = crate::media::resolve(tree_path(ctx).as_deref(), file)?;
    crate::platform::exists(&path).then(|| crate::platform::image_uri(&path))
}

/// Paints an image cropped to fill `rect` (like CSS `object-fit: cover`).
/// Returns false while it's still loading or if it can't be read.
pub fn paint_cover(ui: &Ui, uri: &str, rect: Rect, rounding: impl Into<CornerRadius>) -> bool {
    let poll = ui.ctx().try_load_texture(uri, egui::TextureOptions::LINEAR, egui::SizeHint::Scale(1.0.into()));
    let Ok(egui::load::TexturePoll::Ready { texture }) = poll else { return false };
    let (iw, ih) = (texture.size.x.max(1.0), texture.size.y.max(1.0));
    let (rw, rh) = (rect.width().max(1.0), rect.height().max(1.0));
    let scale = (rw / iw).max(rh / ih);
    let (u, v) = ((rw / scale / iw).min(1.0), (rh / scale / ih).min(1.0));
    let uv = Rect::from_center_size(pos2(0.5, 0.5), vec2(u, v));
    egui::Image::from_texture(texture).uv(uv).corner_radius(rounding).paint_at(ui, rect);
    true
}

/// A filled circle with initials (or their photo), cheaper than `Avatar`
/// for long lists.
pub fn paint_avatar(ui: &Ui, center: egui::Pos2, radius: f32, person: &PersonSummary) {
    let p = Theme::current(ui.ctx()).palette;
    if let Some(uri) = person.photos.iter().find_map(|f| media_uri(ui.ctx(), f)) {
        let rect = Rect::from_center_size(center, vec2(radius * 2.0, radius * 2.0));
        if paint_cover(ui, &uri, rect, CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)) {
            ui.painter().circle_stroke(center, radius, Stroke::new(1.0, mix(p.card, sex_color(&p, person.sex), 0.6)));
            return;
        }
    }
    let base = sex_color(&p, person.sex);
    let fill = mix(p.card, base, if p.is_dark { 0.35 } else { 0.22 });
    let painter = ui.painter();
    painter.circle(center, radius, fill, Stroke::new(1.0, mix(p.card, base, 0.6)));
    painter.text(
        center,
        Align2::CENTER_CENTER,
        person.initials(),
        FontId::proportional(radius * 0.8),
        if p.is_dark { mix(base, Color32::WHITE, 0.7) } else { mix(base, Color32::BLACK, 0.35) },
    );
}

/// Truncates `text` with an ellipsis to fit `width` at `font`.
pub fn fit_text(ui: &Ui, text: &str, font: &FontId, width: f32) -> String {
    let measure = |s: &str| ui.fonts_mut(|f| f.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x);
    if measure(text) <= width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let s: String = chars.iter().collect::<String>().trim_end().to_string() + "…";
        if measure(&s) <= width {
            return s;
        }
    }
    "…".into()
}

/// Widest a [`person_chip`] is drawn.
pub const CHIP_MAX_W: f32 = 360.0;

/// A clickable card showing one person. Returns the click response.
pub fn person_chip(ui: &mut Ui, doc: &Document, xref: &str, caption: Option<&str>) -> Response {
    let Some(person) = doc.person(xref) else {
        return ui.label("(missing person)");
    };
    let theme = Theme::current(ui.ctx());
    let p = &theme.palette;
    let width = ui.available_width().clamp(160.0, CHIP_MAX_W);
    let height = 48.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(width, height), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let fill = if hovered { mix(p.card, p.focus, 0.08) } else { p.input_bg };
        let stroke = if hovered { mix(p.border, p.focus, 0.6) } else { p.border };
        ui.painter().rect(rect, CornerRadius::same(8), fill, Stroke::new(1.0, stroke), StrokeKind::Inside);
        paint_avatar(ui, pos2(rect.left() + 24.0, rect.center().y), 15.0, person);
        let text_x = rect.left() + 48.0;
        let text_w = rect.right() - text_x - 10.0;
        let name_font = FontId::proportional(14.0);
        let name = fit_text(ui, &person.display, &name_font, text_w);
        let sub = match (caption, person.lifespan()) {
            (Some(c), l) if !l.is_empty() => format!("{c} · {l}"),
            (Some(c), _) => c.to_string(),
            (None, l) => l,
        };
        let has_sub = !sub.is_empty();
        let name_y = if has_sub { rect.top() + 16.0 } else { rect.center().y };
        ui.painter().text(pos2(text_x, name_y), Align2::LEFT_CENTER, name, name_font, p.text);
        if has_sub {
            let small = FontId::proportional(12.0);
            let sub = fit_text(ui, &sub, &small, text_w);
            ui.painter().text(pos2(text_x, rect.top() + 33.0), Align2::LEFT_CENTER, sub, small, p.text_muted);
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A dashed placeholder inviting the user to add a relative.
pub fn add_chip(ui: &mut Ui, label: &str) -> Response {
    let theme = Theme::current(ui.ctx());
    let p = &theme.palette;
    let width = ui.available_width().clamp(160.0, 360.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 40.0), Sense::click());
    if ui.is_rect_visible(rect) {
        let hovered = resp.hovered();
        let color = if hovered { p.focus } else { p.text_faint };
        if hovered {
            ui.painter().rect_filled(rect, CornerRadius::same(8), mix(p.card, p.focus, 0.06));
        }
        dashed_rect(ui, rect, color);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            format!("{}  {label}", elegance::glyphs::PLUS),
            FontId::proportional(13.0),
            color,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn dashed_rect(ui: &Ui, rect: Rect, color: Color32) {
    let r = rect.shrink(0.5);
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    let shapes = egui::Shape::dashed_line(&pts, Stroke::new(1.0, color), 5.0, 4.0);
    ui.painter().extend(shapes);
}

/// Small uppercase section label.
pub fn section_label(ui: &mut Ui, text: &str) {
    let p = Theme::current(ui.ctx()).palette;
    ui.add_space(4.0);
    ui.label(egui::RichText::new(text.to_uppercase()).size(11.0).color(p.text_faint).strong());
    ui.add_space(2.0);
}

pub fn muted(ui: &mut Ui, text: impl Into<String>) {
    let p = Theme::current(ui.ctx()).palette;
    ui.label(egui::RichText::new(text.into()).color(p.text_muted));
}

/// A joined pair of large back/forward buttons.
pub fn history_buttons(ui: &mut Ui, can_back: bool, can_forward: bool) -> (Response, Response) {
    let p = Theme::current(ui.ctx()).palette;
    let size = vec2(46.0, 36.0);
    let (rect, _) = ui.allocate_exact_size(vec2(size.x * 2.0 + 1.0, size.y), Sense::hover());
    let left = Rect::from_min_size(rect.min, size);
    let right = Rect::from_min_size(pos2(left.right() + 1.0, rect.top()), size);
    let r = 10u8;
    let mut out = Vec::new();
    for (i, (half, enabled, glyph)) in [(left, can_back, elegance::glyphs::ARROW_LEFT), (right, can_forward, elegance::glyphs::ARROW_RIGHT)]
        .into_iter()
        .enumerate()
    {
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let resp = ui.interact(half, ui.id().with(("history_btn", i)), sense);
        let corners = if i == 0 {
            CornerRadius { nw: r, sw: r, ne: 0, se: 0 }
        } else {
            CornerRadius { nw: 0, sw: 0, ne: r, se: r }
        };
        let (fill, fg) = match (enabled, resp.is_pointer_button_down_on(), resp.hovered()) {
            (false, _, _) => (mix(p.bg, p.card, 0.6), p.text_faint),
            (true, true, _) => (mix(p.card, p.focus, 0.45), Color32::WHITE),
            (true, false, true) => (mix(p.card, p.focus, 0.32), if p.is_dark { Color32::WHITE } else { p.text }),
            (true, false, false) => (mix(p.card, p.focus, 0.18), p.focus),
        };
        ui.painter().rect_filled(half, corners, fill);
        ui.painter().text(half.center(), Align2::CENTER_CENTER, glyph.to_string(), FontId::proportional(20.0), fg);
        out.push(if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp });
    }
    let border = if can_back || can_forward { mix(p.border, p.focus, 0.45) } else { p.border };
    ui.painter().rect_stroke(Rect::from_min_max(left.min, right.max), CornerRadius::same(r), Stroke::new(1.0, border), StrokeKind::Inside);
    ui.painter().line_segment([pos2(left.right() + 0.5, rect.top() + 7.0), pos2(left.right() + 0.5, rect.bottom() - 7.0)], Stroke::new(1.0, border));
    let fwd = out.pop().unwrap();
    let back = out.pop().unwrap();
    (back, fwd)
}

/// A search box with a × to clear it (shown when there's text). Esc while
/// typing clears it too.
pub fn search_input(ui: &mut Ui, text: &mut String, hint: &str, id: impl elegance::IdSalt, width: Option<f32>) -> Response {
    let p = Theme::current(ui.ctx()).palette;
    let mut input = elegance::TextInput::new(&mut *text).hint(hint).id_salt(id);
    if let Some(w) = width {
        input = input.desired_width(w);
    }
    let resp = ui.add(input);
    let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if !text.is_empty() && (resp.has_focus() || resp.lost_focus()) && escaped {
        text.clear();
        resp.request_focus();
    }
    if !text.is_empty() {
        let r = Rect::from_center_size(pos2(resp.rect.right() - 15.0, resp.rect.center().y), vec2(22.0, 22.0));
        let x = ui.interact(r, resp.id.with("clear"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        let color = if x.hovered() {
            ui.painter().circle_filled(r.center(), 9.5, mix(p.input_bg, p.border, 0.7));
            p.text
        } else {
            p.text_faint
        };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, elegance::glyphs::X.to_string(), FontId::proportional(13.0), color);
        if x.on_hover_text("Clear").clicked() {
            text.clear();
            resp.request_focus();
        }
    }
    resp
}
