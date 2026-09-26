//! Bundled Inter typeface (SIL Open Font License, see assets/fonts).
//!
//! Inter replaces egui's default proportional face, which is a light weight
//! that looks thin and uneven at small sizes. A SemiBold cut is registered as
//! its own family for names and headings.
//!
//! The bundled files are subset to drop Inter's Private Use Area glyphs,
//! which would otherwise shadow elegance's icon font (a PUA fallback):
//! `pyftsubset Inter-X.ttf --unicodes="U+0000-DFFF,U+F900-10FFFF" --layout-features='*'`

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::{FontData, FontFamily};

const REGULAR: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");
const SEMIBOLD: &[u8] = include_bytes!("../assets/fonts/Inter-SemiBold.ttf");
const SEMIBOLD_FAMILY: &str = "Inter SemiBold";

/// Cheap after the first call: egui skips fonts it already has.
pub fn install(ctx: &egui::Context) {
    ctx.add_font(FontInsert::new(
        "Inter",
        FontData::from_static(REGULAR),
        vec![InsertFontFamily { family: FontFamily::Proportional, priority: FontPriority::Highest }],
    ));
    ctx.add_font(FontInsert::new(
        SEMIBOLD_FAMILY,
        FontData::from_static(SEMIBOLD),
        vec![InsertFontFamily { family: semibold(), priority: FontPriority::Highest }],
    ));
}

pub fn semibold() -> FontFamily {
    FontFamily::Name(SEMIBOLD_FAMILY.into())
}
