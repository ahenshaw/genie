//! The Reports section: printed-style genealogy reports around the
//! selected person.
//!
//! - **Descendancy**: an indented outline of descendants and their partners.
//! - **Register**: descendants generation by generation in narrative form,
//!   numbered in the style of the NEHGS Register.
//! - **Ahnentafel**: ancestors numbered so a father is 2n and a mother 2n + 1.
//! - **Family group sheet**: one couple, their details and their children.
//! - **Fan chart**: ancestors in rings around the person.
//!
//! The text reports are built as [`Block`]s (no UI), then drawn with names
//! as links, or copied as plain text.

use std::collections::{HashMap, HashSet};
use std::f32::consts::PI;

use egui::{Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use elegance::{Button, ButtonSize, Card, Segment, SegmentedControl, SegmentedSize, Select, Theme, Toast, glyphs};

use crate::app::Action;
use crate::model::{Document, Sex, pretty_date};
use crate::relation::person_picker;
use crate::views::person_context_menu;
use crate::widgets::{fit_text, mix, muted, sex_color};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Descendancy,
    Register,
    Ahnentafel,
    FamilyGroup,
    FanChart,
}

const KINDS: [Kind; 5] = [Kind::Descendancy, Kind::Register, Kind::Ahnentafel, Kind::FamilyGroup, Kind::FanChart];

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Descendancy => "Descendancy",
            Kind::Register => "Register",
            Kind::Ahnentafel => "Ahnentafel",
            Kind::FamilyGroup => "Family group sheet",
            Kind::FanChart => "Fan chart",
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Kind::Descendancy => "Descendants and their partners as an indented outline",
            Kind::Register => "Descendants generation by generation, as numbered narrative",
            Kind::Ahnentafel => "Ancestors numbered: father 2n, mother 2n + 1",
            Kind::FamilyGroup => "A couple, their details and their children",
            Kind::FanChart => "Ancestors in rings around the person",
        }
    }

    /// Generations offered, and the default.
    fn generations(self) -> (std::ops::RangeInclusive<usize>, usize) {
        match self {
            Kind::Descendancy | Kind::Register => (2..=10, 4),
            Kind::Ahnentafel => (2..=12, 5),
            Kind::FanChart => (3..=7, 5),
            Kind::FamilyGroup => (1..=1, 1),
        }
    }
}

// ---- report content --------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Span {
    Text(String),
    /// A person's name, linked to them.
    Person(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Heading(String),
    /// A line of text, indented `indent` steps, after a right-aligned
    /// `marker` ("1.", "+ 2  iii.").
    Line { indent: u8, marker: String, spans: Vec<Span> },
    /// A labelled row on a group sheet.
    Field { indent: u8, label: String, spans: Vec<Span> },
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub title: String,
    pub subtitle: String,
    pub blocks: Vec<Block>,
}

fn text(s: impl Into<String>) -> Span {
    Span::Text(s.into())
}

fn person(x: &str) -> Span {
    Span::Person(x.to_string())
}

fn name(doc: &Document, x: &str) -> String {
    doc.person(x).map(|p| p.display.clone()).unwrap_or_else(|| x.to_string())
}

/// The first of `tags` recorded for a person or family, as (tag, date, place).
fn event(doc: &Document, xref: &str, tags: &[&str]) -> Option<(String, String, String)> {
    let r = doc.record(xref)?;
    tags.iter().find_map(|t| {
        let e = r.child(t)?;
        let (date, place) = (pretty_date(e.child_value("DATE")), e.child_value("PLAC").trim().to_string());
        // "1 DEAT Y" says they died, with nothing more known.
        Some((e.tag.clone(), date, place))
    })
}

/// "14 Mar 1842, Bristol" / "1842" / "Bristol".
fn when_where(date: &str, place: &str) -> String {
    match (date.is_empty(), place.is_empty()) {
        (false, false) => format!("{date}, {place}"),
        (false, true) => date.to_string(),
        (true, false) => place.to_string(),
        (true, true) => String::new(),
    }
}

/// "b. 14 Mar 1842, Bristol; d. 2 Nov 1911, Gloucester".
fn vitals(doc: &Document, x: &str) -> String {
    let mut parts = Vec::new();
    if let Some((tag, d, p)) = event(doc, x, &["BIRT", "CHR", "BAPM"]) {
        let s = when_where(&d, &p);
        if !s.is_empty() {
            parts.push(format!("{} {s}", if tag == "BIRT" { "b." } else { "bap." }));
        }
    }
    if let Some((tag, d, p)) = event(doc, x, &["DEAT", "BURI", "CREM"]) {
        let s = when_where(&d, &p);
        if !s.is_empty() {
            parts.push(format!("{} {s}", if tag == "DEAT" { "d." } else { "bur." }));
        }
    }
    parts.join("; ")
}

/// "m. 1869, Boston" for a family.
fn marriage(doc: &Document, fam: &str) -> String {
    let s = event(doc, fam, &["MARR"]).map(|(_, d, p)| when_where(&d, &p)).unwrap_or_default();
    if s.is_empty() { String::new() } else { format!("m. {s}") }
}

/// Name spans followed by ", details" when there are any.
fn with_details(x: &str, details: &[String]) -> Vec<Span> {
    let details: Vec<&str> = details.iter().map(String::as_str).filter(|d| !d.is_empty()).collect();
    let mut spans = vec![person(x)];
    if !details.is_empty() {
        spans.push(text(format!(", {}", details.join("; "))));
    }
    spans
}

fn has_children(doc: &Document, x: &str) -> bool {
    doc.spouse_families(x).iter().any(|f| !doc.children(f).is_empty())
}

pub fn roman(mut n: usize) -> String {
    const TABLE: [(usize, &str); 13] =
        [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut out = String::new();
    for (v, s) in TABLE {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

fn pronoun(doc: &Document, x: &str) -> &'static str {
    match doc.person(x).map(|p| p.sex) {
        Some(Sex::Male) => "He",
        Some(Sex::Female) => "She",
        _ => "They",
    }
}

fn child_word(doc: &Document, x: &str) -> &'static str {
    match doc.person(x).map(|p| p.sex) {
        Some(Sex::Male) => "son",
        Some(Sex::Female) => "daughter",
        _ => "child",
    }
}

// ---- descendancy -------------------------------------------------------------------------------

pub fn descendancy(doc: &Document, root: &str, gens: usize) -> Report {
    fn walk(doc: &Document, x: &str, generation: usize, gens: usize, seen: &mut HashSet<String>, out: &mut Vec<Block>) {
        let indent = (2 * (generation - 1)) as u8;
        let mut spans = with_details(x, &[vitals(doc, x)]);
        if !seen.insert(x.to_string()) {
            spans.push(text(" (shown above)"));
            out.push(Block::Line { indent, marker: generation.to_string(), spans });
            return;
        }
        out.push(Block::Line { indent, marker: generation.to_string(), spans });
        for fam in doc.spouse_families(x) {
            if let Some(sp) = doc.spouse_in(&fam, x) {
                out.push(Block::Line { indent: indent + 1, marker: "+".into(), spans: with_details(&sp, &[vitals(doc, &sp), marriage(doc, &fam)]) });
            }
            if generation < gens {
                for kid in doc.children(&fam) {
                    walk(doc, &kid, generation + 1, gens, seen, out);
                }
            }
        }
    }
    let mut blocks = Vec::new();
    walk(doc, root, 1, gens, &mut HashSet::new(), &mut blocks);
    Report { title: format!("Descendants of {}", name(doc, root)), subtitle: format!("{gens} generations"), blocks }
}

// ---- register ------------------------------------------------------------------------------------

/// "was born 14 Mar 1842 in Bristol"-style clause, or nothing.
fn clause(verb: &str, date: &str, place: &str) -> Option<String> {
    let date = if date.is_empty() {
        String::new()
    } else if date.chars().next().is_some_and(|c| c.is_ascii_digit()) && date.len() > 4 {
        format!(" on {date}")
    } else if date.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!(" in {date}")
    } else {
        format!(" {}", date.to_lowercase())
    };
    let place = if place.is_empty() { String::new() } else { format!(" in {place}") };
    (!date.is_empty() || !place.is_empty()).then(|| format!("{verb}{date}{place}"))
}

/// "was born … and died …." for a person, or nothing known.
fn life_sentence(doc: &Document, x: &str) -> String {
    let mut parts = Vec::new();
    if let Some((tag, d, p)) = event(doc, x, &["BIRT", "CHR", "BAPM"]) {
        parts.extend(clause(if tag == "BIRT" { "was born" } else { "was baptised" }, &d, &p));
    }
    if let Some((tag, d, p)) = event(doc, x, &["DEAT", "BURI", "CREM"]) {
        let verb = match tag.as_str() {
            "DEAT" => "died",
            "BURI" => "was buried",
            _ => "was cremated",
        };
        parts.extend(clause(verb, &d, &p));
    }
    parts.join(" and ")
}

fn parents_phrase(doc: &Document, x: &str) -> Vec<Span> {
    let (f, m) = (doc.father(x), doc.mother(x));
    let mut spans = Vec::new();
    if f.is_none() && m.is_none() {
        return spans;
    }
    spans.push(text(format!(", {} of ", child_word(doc, x))));
    if let Some(f) = &f {
        spans.push(person(f));
    }
    if f.is_some() && m.is_some() {
        spans.push(text(" and "));
    }
    if let Some(m) = &m {
        spans.push(person(m));
    }
    spans
}

pub fn register(doc: &Document, root: &str, gens: usize) -> Report {
    let mut blocks = Vec::new();
    let mut numbers: HashMap<String, usize> = HashMap::from([(root.to_string(), 1)]);
    let mut next = 2;
    let mut current = vec![root.to_string()];
    for generation in 1..=gens {
        if current.is_empty() {
            break;
        }
        blocks.push(Block::Heading(format!("Generation {generation}")));
        let mut following = Vec::new();
        for x in &current {
            // The person's own paragraph.
            let mut spans = vec![person(x)];
            if generation == 1 {
                spans.extend(parents_phrase(doc, x));
            }
            let life = life_sentence(doc, x);
            spans.push(text(if life.is_empty() { ".".to_string() } else { format!(" {life}.") }));
            blocks.push(Block::Line { indent: 0, marker: format!("{}.", numbers[x]), spans });

            for fam in doc.spouse_families(x) {
                let spouse = doc.spouse_in(&fam, x);
                if let Some(sp) = &spouse {
                    let mut spans = vec![text(format!("{} married ", pronoun(doc, x))), person(sp)];
                    spans.extend(parents_phrase(doc, sp));
                    let (d, p) = event(doc, &fam, &["MARR"]).map(|(_, d, p)| (d, p)).unwrap_or_default();
                    spans.push(text(clause("", &d, &p).map(|c| format!("{c}.")).unwrap_or_else(|| ".".into())));
                    let life = life_sentence(doc, sp);
                    if !life.is_empty() {
                        spans.push(text(format!(" {} {life}.", pronoun(doc, sp))));
                    }
                    blocks.push(Block::Line { indent: 0, marker: String::new(), spans });
                }
                let kids = doc.children(&fam);
                if kids.is_empty() {
                    continue;
                }
                let mut spans = vec![text("Children of "), person(x)];
                if let Some(sp) = &spouse {
                    spans.extend([text(" and "), person(sp)]);
                }
                spans.push(text(":"));
                blocks.push(Block::Line { indent: 0, marker: String::new(), spans });
                for (i, kid) in kids.iter().enumerate() {
                    let numeral = format!("{}.", roman(i + 1));
                    // A child whose own family continues in a later generation gets a number.
                    let marker = match numbers.get(kid) {
                        Some(n) => format!("{n}  {numeral}"),
                        None if generation < gens && has_children(doc, kid) => {
                            numbers.insert(kid.clone(), next);
                            following.push(kid.clone());
                            next += 1;
                            format!("+ {}  {numeral}", next - 1)
                        }
                        None => numeral,
                    };
                    blocks.push(Block::Line { indent: 1, marker, spans: with_details(kid, &[vitals(doc, kid)]) });
                }
            }
        }
        current = following;
    }
    Report { title: format!("Descendants of {}", name(doc, root)), subtitle: "Register report".into(), blocks }
}

// ---- ahnentafel ---------------------------------------------------------------------------------

/// Ancestors by Ahnentafel number (1 = the person), up to `gens`
/// generations. Someone reached twice (cousins who married) is listed
/// once, at their lowest number.
pub fn ahnentafel_numbers(doc: &Document, root: &str, gens: usize) -> Vec<(u64, String)> {
    let mut out = vec![(1u64, root.to_string())];
    let mut seen = HashSet::from([root.to_string()]);
    let mut i = 0;
    while i < out.len() {
        let (n, x) = out[i].clone();
        i += 1;
        if generation_of(n) >= gens {
            continue;
        }
        for (k, parent) in [(2 * n, doc.father(&x)), (2 * n + 1, doc.mother(&x))] {
            if let Some(p) = parent
                && seen.insert(p.clone())
            {
                out.push((k, p));
            }
        }
    }
    out.sort_by_key(|(n, _)| *n);
    out
}

/// 1 → 1, 2–3 → 2, 4–7 → 3, …
fn generation_of(n: u64) -> usize {
    (64 - n.leading_zeros()) as usize
}

fn generation_name(generation: usize) -> String {
    match generation {
        1 => "Generation 1".into(),
        2 => "Generation 2 · Parents".into(),
        3 => "Generation 3 · Grandparents".into(),
        4 => "Generation 4 · Great-grandparents".into(),
        g => {
            let n = g - 3;
            let suffix = match (n % 10, n % 100) {
                (1, x) if x != 11 => "st",
                (2, x) if x != 12 => "nd",
                (3, x) if x != 13 => "rd",
                _ => "th",
            };
            format!("Generation {g} · {n}{suffix} great-grandparents")
        }
    }
}

pub fn ahnentafel(doc: &Document, root: &str, gens: usize) -> Report {
    let list = ahnentafel_numbers(doc, root, gens);
    let by_number: HashMap<u64, &str> = list.iter().map(|(n, x)| (*n, x.as_str())).collect();
    let mut blocks = Vec::new();
    let mut generation = 0;
    for (n, x) in &list {
        if generation_of(*n) != generation {
            generation = generation_of(*n);
            blocks.push(Block::Heading(generation_name(generation)));
        }
        let mut details = vec![vitals(doc, x)];
        // The couple's marriage goes with the husband.
        if n % 2 == 0
            && let Some(wife) = by_number.get(&(n + 1))
            && let Some(fam) = doc.find_family(Some(x), Some(wife))
        {
            details.push(marriage(doc, &fam));
        }
        blocks.push(Block::Line { indent: 0, marker: format!("{n}."), spans: with_details(x, &details) });
    }
    Report { title: format!("Ancestors of {}", name(doc, root)), subtitle: format!("Ahnentafel · {gens} generations"), blocks }
}

// ---- family group sheet -----------------------------------------------------------------------------

/// Families to offer for a group sheet: the person's own, then the one they were born into.
pub fn group_families(doc: &Document, x: &str) -> Vec<String> {
    let mut fams = doc.spouse_families(x);
    fams.extend(doc.parent_families(x));
    fams
}

fn family_label(doc: &Document, fam: &str) -> String {
    let names: Vec<String> = [doc.husband(fam), doc.wife(fam)].into_iter().flatten().map(|x| name(doc, &x)).collect();
    if names.is_empty() { format!("Family {fam}") } else { names.join(" & ") }
}

fn event_fields(doc: &Document, x: &str, indent: u8, out: &mut Vec<Block>) {
    for (label, tags) in [("Born", &["BIRT"][..]), ("Baptised", &["CHR", "BAPM"]), ("Died", &["DEAT"]), ("Buried", &["BURI", "CREM"])] {
        if let Some((_, d, p)) = event(doc, x, tags) {
            let s = when_where(&d, &p);
            if !s.is_empty() {
                out.push(Block::Field { indent, label: label.into(), spans: vec![text(s)] });
            }
        }
    }
    if let Some(occ) = doc.record(x).map(|r| r.child_value("OCCU").trim().to_string()).filter(|o| !o.is_empty()) {
        out.push(Block::Field { indent, label: "Occupation".into(), spans: vec![text(occ)] });
    }
}

pub fn family_group(doc: &Document, fam: &str) -> Report {
    let mut blocks = Vec::new();
    for (role, who) in [("Husband", doc.husband(fam)), ("Wife", doc.wife(fam))] {
        let Some(x) = who else { continue };
        blocks.push(Block::Heading(role.into()));
        blocks.push(Block::Field { indent: 0, label: "Name".into(), spans: vec![person(&x)] });
        event_fields(doc, &x, 0, &mut blocks);
        for (label, parent) in [("Father", doc.father(&x)), ("Mother", doc.mother(&x))] {
            if let Some(p) = parent {
                blocks.push(Block::Field { indent: 0, label: label.into(), spans: vec![person(&p)] });
            }
        }
        let others: Vec<String> = doc.spouse_families(&x).into_iter().filter(|f| f != fam).filter_map(|f| doc.spouse_in(&f, &x)).collect();
        if !others.is_empty() {
            let mut spans = Vec::new();
            for (i, o) in others.iter().enumerate() {
                if i > 0 {
                    spans.push(text(", "));
                }
                spans.push(person(o));
            }
            blocks.push(Block::Field { indent: 0, label: "Other spouses".into(), spans });
        }
    }
    let marr = event(doc, fam, &["MARR"]).map(|(_, d, p)| when_where(&d, &p)).unwrap_or_default();
    let div = event(doc, fam, &["DIV"]).map(|(_, d, p)| when_where(&d, &p));
    if !marr.is_empty() || div.is_some() {
        blocks.push(Block::Heading("Marriage".into()));
        if !marr.is_empty() {
            blocks.push(Block::Field { indent: 0, label: "Married".into(), spans: vec![text(marr)] });
        }
        if let Some(d) = div {
            blocks.push(Block::Field { indent: 0, label: "Divorced".into(), spans: vec![text(if d.is_empty() { "Yes".to_string() } else { d })] });
        }
    }
    let kids = doc.children(fam);
    if !kids.is_empty() {
        blocks.push(Block::Heading(format!("Children ({})", kids.len())));
        for (i, kid) in kids.iter().enumerate() {
            let sex = match doc.person(kid).map(|p| p.sex) {
                Some(Sex::Male) => "M",
                Some(Sex::Female) => "F",
                _ => "",
            };
            blocks.push(Block::Line { indent: 0, marker: format!("{}.  {sex}", i + 1), spans: vec![person(kid)] });
            event_fields(doc, kid, 1, &mut blocks);
            for f in doc.spouse_families(kid) {
                if let Some(sp) = doc.spouse_in(&f, kid) {
                    blocks.push(Block::Field { indent: 1, label: "Spouse".into(), spans: with_details(&sp, &[marriage(doc, &f)]) });
                }
            }
        }
    }
    Report { title: "Family group sheet".into(), subtitle: family_label(doc, fam), blocks }
}

// ---- plain text -------------------------------------------------------------------------------------

pub fn to_text(doc: &Document, r: &Report) -> String {
    let spans = |s: &[Span]| s.iter().map(|s| match s { Span::Text(t) => t.clone(), Span::Person(x) => name(doc, x) }).collect::<String>();
    let mut out = format!("{}\n{}\n", r.title, r.subtitle);
    for b in &r.blocks {
        match b {
            Block::Heading(h) => out.push_str(&format!("\n{h}\n{}\n", "-".repeat(h.chars().count()))),
            Block::Line { indent, marker, spans: s } => {
                let pad = "    ".repeat(*indent as usize);
                let marker = if marker.is_empty() { String::new() } else { format!("{marker} ") };
                out.push_str(&format!("{pad}{marker}{}\n", spans(s)));
            }
            Block::Field { indent, label, spans: s } => {
                out.push_str(&format!("{}{label}: {}\n", "    ".repeat(*indent as usize), spans(s)));
            }
        }
    }
    out
}

// ---- the section --------------------------------------------------------------------------------------

pub struct ReportsView {
    kind: Kind,
    gens: HashMap<Kind, usize>,
    /// The family a group sheet is for, when the person has several.
    family: Option<String>,
    query: String,
    cached: Option<(CacheKey, Report)>,
    /// Who the fan chart's right-click menu is for.
    menu_target: Option<String>,
}

type CacheKey = (u64, Kind, String, usize, Option<String>);

impl Default for ReportsView {
    fn default() -> Self {
        Self { kind: Kind::Descendancy, gens: HashMap::new(), family: None, query: String::new(), cached: None, menu_target: None }
    }
}

impl ReportsView {
    fn generations(&self) -> usize {
        let (range, default) = self.kind.generations();
        self.gens.get(&self.kind).copied().filter(|g| range.contains(g)).unwrap_or(default)
    }

    pub fn show(&mut self, ui: &mut Ui, doc: &Document, selected: Option<&str>, actions: &mut Vec<Action>) {
        let p = Theme::current(ui.ctx()).palette;
        let subject = selected.filter(|x| doc.person(x).is_some());

        let mut kind_idx = KINDS.iter().position(|k| *k == self.kind).unwrap_or(0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                SegmentedControl::from_segments(&mut kind_idx, KINDS.map(|k| Segment::text(k.label()).hover_text(k.hint())))
                    .size(SegmentedSize::Small)
                    .id_salt("report_kind"),
            );
        });
        self.kind = KINDS[kind_idx];
        ui.add_space(6.0);

        let Some(subject) = subject else {
            muted(ui, "Select someone to see reports about them.");
            return;
        };

        let families = group_families(doc, subject);
        if self.family.as_ref().is_none_or(|f| !families.contains(f)) {
            self.family = families.first().cloned();
        }
        let report_text = self.current(doc, subject).map(|r| to_text(doc, &r));
        ui.horizontal(|ui| {
            ui.label(RichText::new("For").color(p.text_muted));
            if let Some(x) = person_picker(ui, doc, "report_subject", Some(subject), &mut self.query) {
                actions.push(Action::Select(x));
            }
            if self.kind == Kind::FamilyGroup {
                if families.len() > 1 {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Family").color(p.text_muted));
                    let options: Vec<(Option<String>, String)> = families.iter().map(|f| (Some(f.clone()), family_label(doc, f))).collect();
                    ui.add(Select::new("report_family", &mut self.family).options(options).width(260.0));
                }
            } else {
                let (range, _) = self.kind.generations();
                let mut gens = self.generations();
                ui.add_space(8.0);
                ui.label(RichText::new("Generations").color(p.text_muted));
                ui.add(Select::new(("report_gens", self.kind), &mut gens).options(range.map(|g| (g, g.to_string()))).width(64.0));
                self.gens.insert(self.kind, gens);
            }
            if let Some(t) = report_text {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let copy = ui.add(Button::new(format!("{}  Copy as text", glyphs::COPY)).outline().size(ButtonSize::Small));
                    if copy.clicked() {
                        ui.ctx().copy_text(t);
                        Toast::new("Report copied").description("Paste it into a document or email.").show(ui.ctx());
                    }
                });
            }
        });
        ui.add_space(10.0);

        if self.kind == Kind::FanChart {
            self.fan_chart(ui, doc, subject, actions);
            return;
        }
        let Some(report) = self.current(doc, subject) else {
            Card::new().show(ui, |ui| {
                ui.set_width(ui.available_width());
                muted(ui, format!("{} isn't in a family yet, so there's no group sheet to show.", name(doc, subject)));
            });
            return;
        };
        egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("report_scroll").show(ui, |ui| {
            Card::new().padding(24.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                draw_report(ui, doc, &report, actions);
            });
        });
    }

    /// The text report for the current settings, built once per change.
    fn current(&mut self, doc: &Document, subject: &str) -> Option<Report> {
        if self.kind == Kind::FanChart {
            return None;
        }
        let gens = self.generations();
        let family = if self.kind == Kind::FamilyGroup { Some(self.family.clone()?) } else { None };
        let key: CacheKey = (doc.revision, self.kind, subject.to_string(), gens, family.clone());
        if let Some((k, r)) = &self.cached
            && *k == key
        {
            return Some(r.clone());
        }
        let r = match self.kind {
            Kind::Descendancy => descendancy(doc, subject, gens),
            Kind::Register => register(doc, subject, gens),
            Kind::Ahnentafel => ahnentafel(doc, subject, gens),
            Kind::FamilyGroup => family_group(doc, family.as_deref()?),
            Kind::FanChart => return None,
        };
        self.cached = Some((key, r.clone()));
        Some(r)
    }

    fn fan_chart(&mut self, ui: &mut Ui, doc: &Document, root: &str, actions: &mut Vec<Action>) {
        let p = Theme::current(ui.ctx()).palette;
        let gens = self.generations();
        let slots: HashMap<u64, String> = ahnentafel_slots(doc, root, gens);

        let avail = ui.available_size();
        let (rect, resp) = ui.allocate_exact_size(avail, Sense::click());
        // A half circle standing on the bottom edge, as large as fits.
        let radius = (rect.width() / 2.0 - 16.0).min(rect.height() - 40.0).max(120.0);
        let top = rect.top() + ((rect.height() - radius) / 2.0 - 12.0).max(16.0);
        let center = pos2(rect.center().x, top + radius);
        let r0 = radius * 0.17;
        let ring = (radius - r0) / (gens - 1) as f32;
        let geom = Fan { center, r0, ring, gens };

        let hovered = resp.hover_pos().and_then(|pos| geom.slot_at(pos));
        let painter = ui.painter_at(rect);
        for (n, x) in std::iter::once((1u64, Some(root.to_string()))).chain((2..(1u64 << gens)).map(|n| (n, slots.get(&n).cloned()))) {
            let (a0, a1, rin, rout) = geom.sector(n);
            let base = x.as_deref().and_then(|x| doc.person(x)).map(|pp| sex_color(&p, pp.sex));
            let fill = match base {
                Some(b) => mix(p.card, b, if hovered == Some(n) { 0.5 } else if p.is_dark { 0.3 } else { 0.2 }),
                None => mix(p.bg, p.card, 0.5),
            };
            let stroke = Stroke::new(1.0, mix(p.card, p.border, 0.9));
            paint_sector(&painter, center, a0, a1, rin, rout, fill, stroke);
            let Some(x) = x else { continue };
            let Some(person) = doc.person(&x) else { continue };
            let text_color = match base {
                Some(b) if p.is_dark => mix(b, Color32::WHITE, 0.75),
                Some(b) => mix(b, Color32::BLACK, 0.45),
                None => p.text,
            };
            fan_label(ui, &painter, &geom, n, &person.display, &person.lifespan(), text_color);
        }

        if let Some(n) = hovered {
            let x = if n == 1 { Some(root.to_string()) } else { slots.get(&n).cloned() };
            if let Some(pp) = x.as_deref().and_then(|x| doc.person(x)) {
                let text = format!("{}\n{}\nAhnentafel #{n}", pp.display, pp.lifespan());
                resp.clone().on_hover_text_at_pointer(text);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked()
                && let Some(x) = x
            {
                actions.push(Action::Select(x));
            }
            if resp.secondary_clicked() {
                self.menu_target = if n == 1 { Some(root.to_string()) } else { slots.get(&n).cloned() };
            }
        }
        if let Some(t) = self.menu_target.clone()
            && doc.person(&t).is_some()
        {
            person_context_menu(&resp, doc, &t, None, actions);
        }
        painter.text(pos2(rect.left() + 4.0, rect.bottom() - 4.0), Align2::LEFT_BOTTOM, "Click someone to centre the chart on them", FontId::proportional(11.5), p.text_faint);
    }
}

/// Ancestors by Ahnentafel number, repeating people who appear twice so
/// the fan has no holes.
fn ahnentafel_slots(doc: &Document, root: &str, gens: usize) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    let mut stack = vec![(1u64, root.to_string())];
    while let Some((n, x)) = stack.pop() {
        if generation_of(n) >= gens {
            continue;
        }
        if let Some(f) = doc.father(&x) {
            out.insert(2 * n, f.clone());
            stack.push((2 * n, f));
        }
        if let Some(m) = doc.mother(&x) {
            out.insert(2 * n + 1, m.clone());
            stack.push((2 * n + 1, m));
        }
    }
    out
}

/// Fan chart geometry: the person in a half disc at the centre, each
/// generation of ancestors in a ring, fathers' lines on the left.
struct Fan {
    center: Pos2,
    r0: f32,
    ring: f32,
    gens: usize,
}

impl Fan {
    /// Angles (screen, radians: −π left … 0 right) and radii of slot `n`.
    fn sector(&self, n: u64) -> (f32, f32, f32, f32) {
        let g = generation_of(n) - 1; // 0 for the person
        if g == 0 {
            return (-PI, 0.0, 0.0, self.r0);
        }
        let count = 1u64 << g;
        let i = (n - count) as f32;
        let step = PI / count as f32;
        let rin = self.r0 + (g - 1) as f32 * self.ring;
        (-PI + i * step, -PI + (i + 1.0) * step, rin, rin + self.ring)
    }

    fn slot_at(&self, pos: Pos2) -> Option<u64> {
        let d = pos - self.center;
        let r = d.length();
        let a = d.y.atan2(d.x);
        if d.y > 0.0 {
            return None;
        }
        if r <= self.r0 {
            return Some(1);
        }
        let g = ((r - self.r0) / self.ring).floor() as usize + 1;
        if g >= self.gens {
            return None;
        }
        let count = 1u64 << g;
        let i = (((a + PI) / PI) * count as f32).floor().clamp(0.0, (count - 1) as f32) as u64;
        Some(count + i)
    }
}

fn polar(c: Pos2, r: f32, a: f32) -> Pos2 {
    pos2(c.x + r * a.cos(), c.y + r * a.sin())
}

fn paint_sector(painter: &egui::Painter, c: Pos2, a0: f32, a1: f32, rin: f32, rout: f32, fill: Color32, stroke: Stroke) {
    let steps = (((a1 - a0) * rout / 6.0).ceil() as usize).clamp(2, 96);
    let mut mesh = egui::Mesh::default();
    for k in 0..=steps {
        let a = a0 + (a1 - a0) * k as f32 / steps as f32;
        mesh.colored_vertex(polar(c, rin, a), fill);
        mesh.colored_vertex(polar(c, rout, a), fill);
        if k > 0 {
            let v = (2 * k) as u32;
            mesh.add_triangle(v - 2, v - 1, v);
            mesh.add_triangle(v - 1, v + 1, v);
        }
    }
    painter.add(mesh);
    let mut outline: Vec<Pos2> = (0..=steps).map(|k| polar(c, rout, a0 + (a1 - a0) * k as f32 / steps as f32)).collect();
    if rin > 0.0 {
        outline.extend((0..=steps).rev().map(|k| polar(c, rin, a0 + (a1 - a0) * k as f32 / steps as f32)));
    }
    painter.add(egui::Shape::closed_line(if rin > 0.0 { outline } else { [vec![c], outline].concat() }, stroke));
}

/// A person's name and years in their sector: across the ring near the
/// centre, along the radius further out where sectors get thin.
fn fan_label(ui: &Ui, painter: &egui::Painter, geom: &Fan, n: u64, name: &str, years: &str, color: Color32) {
    let (a0, a1, rin, rout) = geom.sector(n);
    let mid_a = (a0 + a1) / 2.0;
    let mid_r = if n == 1 { geom.r0 * 0.45 } else { (rin + rout) / 2.0 };
    let g = generation_of(n) - 1;
    // Room along the text, and across it for lines.
    let (along, across, angle) = if g <= 2 {
        let along = if n == 1 { geom.r0 * 1.7 } else { (a1 - a0) * mid_r * 0.9 };
        (along, rout - rin - 6.0, if n == 1 { 0.0 } else { mid_a + PI / 2.0 })
    } else {
        // Radial; flipped on the left so it never reads upside down.
        let flip = mid_a < -PI / 2.0;
        (rout - rin - 10.0, (a1 - a0) * mid_r, if flip { mid_a + PI } else { mid_a })
    };
    let across = if n == 1 { geom.r0 * 0.8 } else { across };
    let size = (across * 0.36).clamp(8.0, 13.0);
    if across < 9.0 || along < 20.0 {
        return;
    }
    let lines: Vec<(String, f32)> = if across >= size * 2.6 && !years.is_empty() {
        vec![(name.to_string(), size), (years.to_string(), size * 0.85)]
    } else {
        vec![(name.to_string(), size)]
    };
    let total: f32 = lines.iter().map(|(_, s)| s * 1.2).sum();
    let center = polar(geom.center, mid_r, mid_a);
    let (dir, normal) = (vec2(angle.cos(), angle.sin()), vec2(-angle.sin(), angle.cos()));
    let mut offset = -total / 2.0;
    for (text, s) in lines {
        let font = FontId::proportional(s);
        let fitted = fit_text(ui, &text, &font, along);
        let galley = painter.layout_no_wrap(fitted, font, color);
        let sz = galley.size();
        // Centre of this line, then back to its top-left corner along the rotated axes.
        let line_center = center + normal * (offset + s * 0.6);
        let top_left = line_center - dir * (sz.x / 2.0) - normal * (sz.y / 2.0);
        painter.add(egui::epaint::TextShape::new(top_left, galley, color).with_angle(angle));
        offset += s * 1.2;
    }
}

fn draw_report(ui: &mut Ui, doc: &Document, r: &Report, actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    ui.label(RichText::new(&r.title).size(22.0).family(crate::fonts::semibold()).color(p.text));
    ui.label(RichText::new(&r.subtitle).size(13.0).color(p.text_muted));
    ui.add_space(8.0);
    const STEP: f32 = 22.0;
    const MARKER_W: f32 = 64.0;
    const LABEL_W: f32 = 110.0;
    for b in &r.blocks {
        match b {
            Block::Heading(h) => {
                ui.add_space(10.0);
                ui.label(RichText::new(h).size(15.0).family(crate::fonts::semibold()).color(p.focus));
                let (line, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
                ui.painter().hline(line.x_range(), line.center().y, Stroke::new(1.0, p.border));
                ui.add_space(2.0);
            }
            Block::Line { indent, marker, spans } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(*indent as f32 * STEP);
                    if !marker.is_empty() || *indent == 0 {
                        let (rect, _) = ui.allocate_exact_size(vec2(MARKER_W, 18.0), Sense::hover());
                        ui.painter().text(Rect::from_min_size(rect.min, rect.size()).right_center() - vec2(8.0, 0.0), Align2::RIGHT_CENTER, marker, FontId::proportional(13.0), p.text_faint);
                    }
                    spans_ui(ui, doc, spans, actions);
                });
            }
            Block::Field { indent, label, spans } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(*indent as f32 * STEP + if *indent > 0 { MARKER_W } else { 0.0 });
                    let (rect, _) = ui.allocate_exact_size(vec2(LABEL_W, 18.0), Sense::hover());
                    ui.painter().text(rect.left_center(), Align2::LEFT_CENTER, label, FontId::proportional(12.5), p.text_muted);
                    spans_ui(ui, doc, spans, actions);
                });
            }
        }
    }
}

/// Text with names as links: click to select, right-click for more.
fn spans_ui(ui: &mut Ui, doc: &Document, spans: &[Span], actions: &mut Vec<Action>) {
    let p = Theme::current(ui.ctx()).palette;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for s in spans {
            match s {
                Span::Text(t) => {
                    ui.label(RichText::new(t).size(13.5).color(p.text));
                }
                Span::Person(x) => {
                    let label = RichText::new(name(doc, x)).size(13.5).color(p.focus);
                    let r = ui.add(egui::Label::new(label).sense(Sense::click()));
                    if r.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if r.clicked() {
                        actions.push(Action::Select(x.clone()));
                    }
                    person_context_menu(&r, doc, x, None, actions);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Document {
        Document::from_bytes(include_bytes!("sample.ged")).0
    }

    fn find(doc: &Document, given: &str) -> String {
        doc.people().iter().find(|p| p.given.starts_with(given)).map(|p| p.xref.clone()).expect(given)
    }

    #[test]
    fn roman_numerals() {
        assert_eq!([1, 4, 9, 14, 40].map(roman), ["i", "iv", "ix", "xiv", "xl"]);
    }

    #[test]
    fn ahnentafel_numbers_parents_as_2n_and_2n_plus_1() {
        let doc = sample();
        let root = find(&doc, "Arthur");
        let list = ahnentafel_numbers(&doc, &root, 4);
        let at = |n: u64| list.iter().find(|(k, _)| *k == n).map(|(_, x)| x.clone());
        assert_eq!(at(1), Some(root.clone()));
        assert_eq!(at(2), doc.father(&root));
        assert_eq!(at(3), doc.mother(&root));
        if let Some(f) = at(2) {
            assert_eq!(at(4), doc.father(&f));
            assert_eq!(at(5), doc.mother(&f));
        }
        assert!(list.iter().all(|(n, _)| generation_of(*n) <= 4));
    }

    #[test]
    fn register_numbers_continuing_children_in_order() {
        let doc = sample();
        let root = find(&doc, "Thomas");
        let r = register(&doc, &root, 4);
        let numbered: Vec<usize> = r
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Line { indent: 0, marker, .. } => marker.trim_end_matches('.').parse().ok(),
                _ => None,
            })
            .collect();
        assert_eq!(numbered.first(), Some(&1));
        assert!(numbered.windows(2).all(|w| w[1] == w[0] + 1), "{numbered:?}");
        // Every "+ n" given to a child gets its own paragraph.
        let plus = r.blocks.iter().filter(|b| matches!(b, Block::Line { marker, .. } if marker.starts_with('+'))).count();
        assert_eq!(plus + 1, numbered.len());
    }

    #[test]
    fn descendancy_and_group_sheet_mention_the_family() {
        let doc = sample();
        let root = find(&doc, "Thomas");
        let text = to_text(&doc, &descendancy(&doc, &root, 3));
        let fam = group_families(&doc, &root).remove(0);
        for kid in doc.children(&fam) {
            assert!(text.contains(&name(&doc, &kid)));
        }
        let sheet = to_text(&doc, &family_group(&doc, &fam));
        assert!(sheet.contains("Husband") && sheet.contains(&name(&doc, &root)));
        assert!(sheet.contains(&format!("Children ({})", doc.children(&fam).len())));
    }

    #[test]
    fn fan_geometry_round_trips() {
        let fan = Fan { center: pos2(0.0, 0.0), r0: 50.0, ring: 40.0, gens: 5 };
        for n in 1..32u64 {
            let (a0, a1, rin, rout) = fan.sector(n);
            let mid = polar(fan.center, if n == 1 { 20.0 } else { (rin + rout) / 2.0 }, (a0 + a1) / 2.0);
            assert_eq!(fan.slot_at(mid), Some(n), "slot {n}");
        }
    }
}
