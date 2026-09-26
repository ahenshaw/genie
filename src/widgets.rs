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

/// A filled circle with initials, cheaper than `Avatar` for long lists.
pub fn paint_avatar(ui: &Ui, center: egui::Pos2, radius: f32, person: &PersonSummary) {
    let p = Theme::current(ui.ctx()).palette;
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

/// A clickable card showing one person. Returns the click response.
pub fn person_chip(ui: &mut Ui, doc: &Document, xref: &str, caption: Option<&str>) -> Response {
    let Some(person) = doc.person(xref) else {
        return ui.label("(missing person)");
    };
    let theme = Theme::current(ui.ctx());
    let p = &theme.palette;
    let width = ui.available_width().clamp(160.0, 360.0);
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
