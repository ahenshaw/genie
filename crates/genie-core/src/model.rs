//! Genealogy model layered over the raw GEDCOM record tree.
//!
//! [`Document`] owns the records and offers typed read accessors plus the
//! editing operations the UI needs. Every edit goes through
//! [`Document::mutate`], which snapshots for undo and refreshes the caches.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::gedcom::{self, Node, pointer_to};

const UNDO_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Sex {
    Male,
    Female,
    #[default]
    Unknown,
}

impl Sex {
    pub fn from_gedcom(v: &str) -> Self {
        match v.trim().chars().next().map(|c| c.to_ascii_uppercase()) {
            Some('M') => Sex::Male,
            Some('F') => Sex::Female,
            _ => Sex::Unknown,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Sex::Male => "M",
            Sex::Female => "F",
            Sex::Unknown => "U",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Sex::Male => "Male",
            Sex::Female => "Female",
            Sex::Unknown => "Unknown",
        }
    }
}

/// Cheap per-person data used by lists, search, and the tree canvas.
#[derive(Clone, Debug)]
pub struct PersonSummary {
    pub xref: String,
    /// Image files attached to the person (as written in the GEDCOM),
    /// their profile photo first. Avatars use the first that exists.
    pub photos: Vec<String>,
    pub given: String,
    pub surname: String,
    pub display: String,
    pub sex: Sex,
    pub birth_year: Option<i32>,
    pub death_year: Option<i32>,
    pub is_dead: bool,
    search: String,
}

impl PersonSummary {
    pub fn initials(&self) -> String {
        let g = self.given.chars().find(|c| c.is_alphanumeric());
        let s = self.surname.chars().find(|c| c.is_alphanumeric());
        match (g, s) {
            (Some(g), Some(s)) => format!("{}{}", g.to_uppercase(), s.to_uppercase()),
            (Some(c), None) | (None, Some(c)) => c.to_uppercase().to_string(),
            _ => "?".into(),
        }
    }

    pub fn lifespan(&self) -> String {
        match (self.birth_year, self.death_year) {
            (Some(b), Some(d)) => format!("{b} – {d}"),
            (Some(b), None) if self.is_dead => format!("{b} – ?"),
            (Some(b), None) => format!("b. {b}"),
            (None, Some(d)) => format!("d. {d}"),
            (None, None) if self.is_dead => "deceased".into(),
            (None, None) => String::new(),
        }
    }

    pub fn matches(&self, needle_lower: &str) -> bool {
        needle_lower.split_whitespace().all(|w| self.search.contains(w))
    }
}

/// A single life event as shown on a person's timeline.
#[derive(Clone, Debug)]
pub struct EventView {
    pub tag: String,
    pub label: String,
    pub date: String,
    pub place: String,
    pub detail: String,
    pub sort: Option<(i32, u8, u8)>,
    /// Events that belong to relatives (a child's birth, a spouse's death).
    pub related: Option<String>,
}

/// Editable fields for a person. Loaded from and applied to an `INDI`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersonForm {
    pub given: String,
    pub surname: String,
    /// Name suffix such as `Jr` or `III`.
    pub suffix: String,
    pub sex: Sex,
    pub birth_date: String,
    pub birth_place: String,
    pub death_date: String,
    pub death_place: String,
    pub deceased: bool,
    pub occupation: String,
    pub note: String,
    pub citations: Vec<CitationForm>,
    /// Events besides birth, death and occupation (which have fields of
    /// their own): residences, census, emigration, burial, ….
    pub events: Vec<EventForm>,
    /// The person's marriages, one per family they're a partner in.
    pub marriages: Vec<MarriageForm>,
}

/// One of a person's events, as the editor shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EventForm {
    /// Which event of the record this is, as (tag, n) when the form was
    /// made; `None` for one being added.
    pub origin: Option<FactKey>,
    pub tag: String,
    pub date: String,
    pub place: String,
    /// What it was: the event's own text (a school, a regiment), or for a
    /// generic `EVEN` its type ("Military service").
    pub detail: String,
}

/// A marriage: the family's `MARR` date and place.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarriageForm {
    pub family: String,
    /// Who to, for the editor's label.
    pub spouse: String,
    pub date: String,
    pub place: String,
}

/// Events a person can be given in the editor, in the order its menu offers them.
pub const PERSON_EVENTS: [&str; 26] = [
    "RESI", "CENS", "EMIG", "IMMI", "NATU", "EDUC", "GRAD", "RETI", "CHR", "BAPM", "CHRA", "CONF", "FCOM", "BARM", "BASM", "BLES",
    "ORDN", "ADOP", "BURI", "CREM", "PROB", "WILL", "RELI", "TITL", "PROP", "EVEN",
];

/// Events that have their own fields in the editor, not the events list.
fn has_own_field(tag: &str) -> bool {
    matches!(tag, "BIRT" | "DEAT" | "OCCU")
}

fn event_detail(n: &Node) -> String {
    if n.tag == "EVEN" {
        n.child_value("TYPE").to_string()
    } else if n.value == "Y" || n.pointer().is_some() {
        String::new()
    } else {
        n.value.clone()
    }
}

/// Which fact a citation supports: a tag (`""` for the person as a whole)
/// and which occurrence of that tag (for repeated events like `RESI`).
pub type FactKey = (String, usize);

pub const GENERAL_FACT: &str = "";

/// Reliability as GEDCOM's `QUAY` defines it.
pub const QUALITY_LABELS: [&str; 4] = ["Unreliable", "Questionable", "Secondary", "Primary"];

#[derive(Clone, Debug, PartialEq)]
pub enum CiteSource {
    /// A pointer to a `SOUR` record.
    Record(String),
    /// A source record to create when the form is saved.
    New { title: String, author: String, publication: String },
    /// An old-style citation holding its own text instead of a pointer.
    Text(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct CitationForm {
    pub fact: FactKey,
    pub source: CiteSource,
    pub page: String,
    /// `QUAY` 0–3, or unrated.
    pub quality: Option<u8>,
    /// The citation as loaded, so subordinate lines Genie doesn't edit
    /// (`DATA`, `TEXT`, `NOTE`, `OBJE`, …) survive an edit.
    original: Option<Node>,
}

impl CitationForm {
    /// Documents attached to this citation.
    pub fn media(&self) -> Vec<String> {
        self.original
            .iter()
            .flat_map(|n| n.children_with("OBJE"))
            .filter_map(|o| o.pointer().map(str::to_string))
            .collect()
    }

    pub fn new(fact: FactKey, source: CiteSource) -> Self {
        Self { fact, source, page: String::new(), quality: None, original: None }
    }
}

/// A citation as shown on the profile.
#[derive(Clone, Debug)]
pub struct CitationView {
    /// Documents attached to the citation (scans of the record).
    pub media: Vec<String>,
    pub fact: String,
    pub title: String,
    pub author: String,
    pub page: String,
    pub quality: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Relation {
    Father,
    Mother,
    Spouse,
    /// Child within a specific spouse family, or a new/first family.
    Child(Option<String>),
    Sibling,
}

impl Relation {
    pub fn label(&self) -> &'static str {
        match self {
            Relation::Father => "Father",
            Relation::Mother => "Mother",
            Relation::Spouse => "Spouse",
            Relation::Child(_) => "Child",
            Relation::Sibling => "Sibling",
        }
    }
    pub fn implied_sex(&self) -> Option<Sex> {
        match self {
            Relation::Father => Some(Sex::Male),
            Relation::Mother => Some(Sex::Female),
            _ => None,
        }
    }
}

#[derive(Default)]
pub struct Document {
    records: Vec<Node>,
    index: HashMap<String, usize>,
    people: Vec<PersonSummary>,
    person_pos: HashMap<String, usize>,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    /// Bumped on every change so views can cache derived data.
    pub revision: u64,
    undo: Vec<Vec<Node>>,
    redo: Vec<Vec<Node>>,
    /// Consecutive edits sharing a key collapse into one undo step.
    merge_key: Option<String>,
}

impl Document {
    pub fn new_empty() -> Self {
        let head = Node::new("HEAD", "")
            .with_child(Node::new("SOUR", "GENIE").with_child(Node::new("NAME", "Genie")))
            .with_child(
                Node::new("GEDC", "")
                    .with_child(Node::new("VERS", "5.5.1"))
                    .with_child(Node::new("FORM", "LINEAGE-LINKED")),
            )
            .with_child(Node::new("CHAR", "UTF-8"));
        let mut d = Self { records: vec![head], ..Default::default() };
        d.rebuild();
        d
    }

    pub fn from_bytes(bytes: &[u8]) -> (Self, Vec<String>) {
        let decoded = gedcom::decode(bytes);
        let (mut records, warnings) = gedcom::parse(&decoded.text);
        records.retain(|r| r.tag != "TRLR");
        let mut notes: Vec<String> = warnings.iter().map(|w| format!("Line {}: {}", w.line, w.message)).collect();
        if let Some(enc) = decoded.lossy_encoding {
            notes.insert(0, format!("File is {enc}-encoded; it will be saved as UTF-8."));
        }
        if !records.iter().any(|r| r.tag == "HEAD") {
            records.insert(0, Self::new_empty().records.remove(0));
        }
        let mut d = Self { records, ..Default::default() };
        d.rebuild();
        (d, notes)
    }

    /// A document from already-parsed records, as [`Document::from_bytes`]
    /// would make it: without TRLR, and with a HEAD.
    pub fn from_records(mut records: Vec<Node>) -> Self {
        records.retain(|r| r.tag != "TRLR");
        if !records.iter().any(|r| r.tag == "HEAD") {
            records.insert(0, Self::new_empty().records.remove(0));
        }
        let mut d = Self { records, ..Default::default() };
        d.rebuild();
        d
    }

    pub fn to_gedcom(&self) -> String {
        let mut recs = self.records.clone();
        if let Some(head) = recs.iter_mut().find(|r| r.tag == "HEAD") {
            head.set_child_value("CHAR", "UTF-8");
        }
        recs.push(Node::new("TRLR", ""));
        gedcom::write(&recs)
    }

    pub fn file_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled.ged".into())
    }

    // ---- caches -------------------------------------------------------------

    pub(crate) fn rebuild(&mut self) {
        self.index.clear();
        for (i, r) in self.records.iter().enumerate() {
            if let Some(x) = &r.xref {
                self.index.insert(x.clone(), i);
            }
        }
        let mut people: Vec<PersonSummary> = self
            .records
            .iter()
            .filter(|r| r.tag == "INDI" && r.xref.is_some())
            .map(summarize)
            .collect();
        people.sort_by(|a, b| {
            (a.surname.to_lowercase(), a.given.to_lowercase(), a.birth_year)
                .cmp(&(b.surname.to_lowercase(), b.given.to_lowercase(), b.birth_year))
        });
        for person in &mut people {
            person.photos = self.photo_files(&person.xref);
        }
        self.person_pos = people.iter().enumerate().map(|(i, p)| (p.xref.clone(), i)).collect();
        self.people = people;
        self.revision += 1;
    }

    /// Runs an edit as one undoable step.
    pub fn mutate<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.mutate_merge(None, f)
    }

    /// Like [`Self::mutate`], but folds into the previous step when both
    /// carry the same `key` (e.g. successive keystrokes in one field).
    pub fn mutate_merge<R>(&mut self, key: Option<String>, f: impl FnOnce(&mut Self) -> R) -> R {
        let merge = key.is_some() && key == self.merge_key && !self.undo.is_empty();
        let snapshot = (!merge).then(|| self.records.clone());
        let r = f(self);
        if let Some(snapshot) = snapshot {
            self.undo.push(snapshot);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.merge_key = key;
        self.redo.clear();
        self.dirty = true;
        self.rebuild();
        r
    }

    /// Discards the most recent step entirely (used when an edit fails).
    pub fn rollback(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.records = prev;
            self.merge_key = None;
            self.rebuild();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
    pub fn undo(&mut self) -> bool {
        self.merge_key = None;
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.records, prev));
        self.dirty = true;
        self.rebuild();
        true
    }
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.records, next));
        self.dirty = true;
        self.rebuild();
        true
    }

    // ---- reading ------------------------------------------------------------

    pub fn people(&self) -> &[PersonSummary] {
        &self.people
    }

    pub fn person(&self, xref: &str) -> Option<&PersonSummary> {
        self.person_pos.get(xref).map(|&i| &self.people[i])
    }

    /// Whether `xref` is an individual. Reads the records rather than the
    /// summary cache, so it also holds mid-edit for people just created.
    fn is_person(&self, xref: &str) -> bool {
        self.record(xref).is_some_and(|r| r.tag == "INDI")
    }

    pub fn record(&self, xref: &str) -> Option<&Node> {
        self.index.get(xref).map(|&i| &self.records[i])
    }

    /// A record to edit in place. Use it inside [`Document::mutate`], which
    /// refreshes the caches afterwards.
    pub fn record_mut(&mut self, xref: &str) -> Option<&mut Node> {
        let i = *self.index.get(xref)?;
        Some(&mut self.records[i])
    }

    pub fn records(&self) -> &[Node] {
        &self.records
    }

    /// Direct access for bulk edits (merges); call inside `mutate`, which
    /// rebuilds the indexes afterwards.
    pub(crate) fn records_mut(&mut self) -> &mut Vec<Node> {
        &mut self.records
    }

    pub fn family_count(&self) -> usize {
        self.records.iter().filter(|r| r.tag == "FAM").count()
    }

    pub fn source_count(&self) -> usize {
        self.records.iter().filter(|r| r.tag == "SOUR").count()
    }

    fn pointers<'a>(&'a self, xref: &str, tag: &'a str) -> Vec<String> {
        self.record(xref)
            .map(|r| r.children_with(tag).filter_map(|c| c.pointer()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn parent_families(&self, xref: &str) -> Vec<String> {
        self.pointers(xref, "FAMC")
            .into_iter()
            .filter(|f| self.record(f).is_some())
            .collect()
    }

    pub fn spouse_families(&self, xref: &str) -> Vec<String> {
        self.pointers(xref, "FAMS")
            .into_iter()
            .filter(|f| self.record(f).is_some())
            .collect()
    }

    pub fn husband(&self, fam: &str) -> Option<String> {
        self.pointers(fam, "HUSB").into_iter().find(|x| self.is_person(x))
    }

    pub fn wife(&self, fam: &str) -> Option<String> {
        self.pointers(fam, "WIFE").into_iter().find(|x| self.is_person(x))
    }

    pub fn children(&self, fam: &str) -> Vec<String> {
        let mut kids: Vec<String> = self
            .pointers(fam, "CHIL")
            .into_iter()
            .filter(|x| self.is_person(x))
            .collect();
        // Stable sort by birth year; unknown years keep file order at the end.
        kids.sort_by_key(|k| self.person(k).and_then(|p| p.birth_year).unwrap_or(i32::MAX));
        kids
    }

    pub fn spouse_in(&self, fam: &str, xref: &str) -> Option<String> {
        let h = self.husband(fam);
        let w = self.wife(fam);
        if h.as_deref() == Some(xref) { w } else { h }
    }

    pub fn father(&self, xref: &str) -> Option<String> {
        self.parent_families(xref).iter().find_map(|f| self.husband(f))
    }

    pub fn mother(&self, xref: &str) -> Option<String> {
        self.parent_families(xref).iter().find_map(|f| self.wife(f))
    }

    pub fn siblings(&self, xref: &str) -> Vec<String> {
        let mut out = Vec::new();
        for f in self.parent_families(xref) {
            for c in self.children(&f) {
                if c != xref && !out.contains(&c) {
                    out.push(c);
                }
            }
        }
        out
    }

    pub fn family_event(&self, fam: &str, tag: &str) -> Option<(String, String)> {
        let ev = self.record(fam)?.child(tag)?;
        Some((ev.child_value("DATE").to_string(), ev.child_value("PLAC").to_string()))
    }

    /// Notes attached to a record: inline text plus resolved `NOTE` records.
    pub fn notes(&self, xref: &str) -> Vec<String> {
        let Some(r) = self.record(xref) else { return Vec::new() };
        r.children_with("NOTE")
            .filter_map(|n| match n.pointer() {
                Some(p) => self.record(p).map(|nr| nr.value.clone()),
                None => Some(n.value.clone()),
            })
            .filter(|s| !s.trim().is_empty())
            .collect()
    }

    /// Titles of the sources cited anywhere in a record.
    /// All source records, as (xref, title), sorted by title.
    pub fn sources(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = self
            .records
            .iter()
            .filter(|r| r.tag == "SOUR")
            .filter_map(|r| r.xref.clone().map(|x| (x.clone(), source_title(r, &x))))
            .collect();
        v.sort_by_key(|a| a.1.to_lowercase());
        v
    }

    /// The facts on a person that a citation can support, labelled for
    /// display. Birth and death are always offered.
    pub fn facts(&self, xref: &str) -> Vec<(FactKey, String)> {
        let mut out = vec![((GENERAL_FACT.to_string(), 0), "Person (general)".to_string()), (("NAME".to_string(), 0), "Name".to_string())];
        let mut seen: HashMap<&str, usize> = HashMap::new();
        if let Some(r) = self.record(xref) {
            for c in &r.children {
                let Some(label) = event_label(&c.tag) else { continue };
                let n = seen.entry(c.tag.as_str()).or_default();
                let date = pretty_date(c.child_value("DATE"));
                let label = if date.is_empty() { label.to_string() } else { format!("{label} · {date}") };
                out.push(((c.tag.clone(), *n), label));
                *n += 1;
            }
        }
        for (tag, label) in [("BIRT", "Birth"), ("DEAT", "Death")] {
            if !out.iter().any(|((t, _), _)| t == tag) {
                out.push(((tag.to_string(), 0), label.to_string()));
            }
        }
        out
    }

    /// Citations on a person (and the events of their marriages), labelled
    /// with the fact each supports.
    pub fn citation_views(&self, xref: &str) -> Vec<CitationView> {
        let facts = self.facts(xref);
        let mut out: Vec<CitationView> = self
            .form_for(xref)
            .citations
            .into_iter()
            .map(|c| {
                let fact = facts.iter().find(|(k, _)| *k == c.fact).map(|(_, l)| l.clone()).unwrap_or_else(|| c.fact.0.clone());
                self.citation_view(fact, &c)
            })
            .collect();
        for fam in self.spouse_families(xref) {
            let Some(f) = self.record(&fam) else { continue };
            let with = self.spouse_in(&fam, xref).and_then(|s| self.person(&s).map(|p| p.display.clone()));
            for ev in &f.children {
                let Some(label) = event_label(&ev.tag) else { continue };
                let label = match &with {
                    Some(w) => format!("{label} · {w}"),
                    None => label.to_string(),
                };
                for c in ev.children_with("SOUR") {
                    out.push(self.citation_view(label.clone(), &citation_from_node((ev.tag.clone(), 0), c)));
                }
            }
        }
        out
    }

    fn citation_view(&self, fact: String, c: &CitationForm) -> CitationView {
        let (title, author) = match &c.source {
            CiteSource::Record(x) => match self.record(x) {
                Some(r) => (source_title(r, x), r.child_value("AUTH").to_string()),
                None => (format!("Missing source {x}"), String::new()),
            },
            CiteSource::New { title, author, .. } => (title.clone(), author.clone()),
            CiteSource::Text(t) => (t.clone(), String::new()),
        };
        CitationView { media: c.media(), fact, title, author, page: c.page.clone(), quality: c.quality }
    }

    pub fn timeline(&self, xref: &str) -> Vec<EventView> {
        let Some(rec) = self.record(xref) else { return Vec::new() };
        let mut out = Vec::new();
        for c in &rec.children {
            if let Some(label) = event_label(&c.tag) {
                out.push(event_view(c, label, None));
            }
        }
        for fam in self.spouse_families(xref) {
            let spouse = self.spouse_in(&fam, xref);
            let spouse_name = spouse.as_deref().and_then(|s| self.person(s)).map(|p| p.display.clone());
            if let Some(frec) = self.record(&fam) {
                for c in &frec.children {
                    if let Some(label) = event_label(&c.tag) {
                        let mut ev = event_view(c, label, spouse.clone());
                        ev.related = None;
                        if let Some(n) = &spouse_name {
                            ev.detail = if ev.detail.is_empty() { format!("with {n}") } else { format!("{} · with {n}", ev.detail) };
                        }
                        out.push(ev);
                    }
                }
                // A partnership recorded without its marriage still belongs
                // on the timeline, undated, so it isn't missing and the gap
                // is plain to see.
                if let Some(n) = &spouse_name
                    && frec.child("MARR").is_none()
                {
                    out.push(EventView {
                        tag: "MARR".into(),
                        label: "Marriage".into(),
                        date: String::new(),
                        place: String::new(),
                        detail: format!("with {n} · date not recorded"),
                        sort: None,
                        related: None,
                    });
                }
            }
            for kid in self.children(&fam) {
                let Some(p) = self.person(&kid) else { continue };
                if let Some(birth) = self.record(&kid).and_then(|r| r.child("BIRT").or_else(|| r.child("CHR"))) {
                    let rel = match p.sex {
                        Sex::Male => "Birth of son",
                        Sex::Female => "Birth of daughter",
                        Sex::Unknown => "Birth of child",
                    };
                    let mut ev = event_view(birth, rel, Some(kid.clone()));
                    ev.detail = p.display.clone();
                    if ev.sort.is_some() {
                        out.push(ev);
                    }
                }
            }
            if let Some(sp) = &spouse
                && let (Some(p), Some(d)) = (self.person(sp), self.record(sp).and_then(|r| r.child("DEAT"))) {
                    let mut ev = event_view(d, "Death of spouse", Some(sp.clone()));
                    ev.detail = p.display.clone();
                    if ev.sort.is_some() {
                        out.push(ev);
                    }
                }
        }
        // Own birth first, own death/burial last, everything else by date.
        let rank = |e: &EventView| match (e.tag.as_str(), &e.related) {
            ("BIRT", None) => 0,
            ("CHR" | "BAPM", None) => 1,
            ("DEAT", None) => 3,
            ("BURI" | "CREM", None) => 4,
            _ => 2,
        };
        out.sort_by(|a, b| {
            let (ra, rb) = (rank(a), rank(b));
            if ((ra == 2) != (rb == 2) || (ra != 2 && rb != 2))
                && ra != rb {
                    return ra.cmp(&rb);
                }
            match (a.sort, b.sort) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
        // Drop relatives' events that fall after the person's own death.
        let own_end = out
            .iter()
            .filter(|e| e.related.is_none() && matches!(e.tag.as_str(), "DEAT" | "BURI" | "CREM"))
            .filter_map(|e| e.sort)
            .min();
        if let Some(end) = own_end {
            out.retain(|e| e.related.is_none() || e.sort.is_none_or(|s| s <= end));
        }
        out
    }

    pub fn form_for(&self, xref: &str) -> PersonForm {
        let Some(r) = self.record(xref) else { return PersonForm::default() };
        let (given, surname) = r.child("NAME").map(split_name).unwrap_or_default();
        let ev = |tag: &str, sub: &str| r.child(tag).map(|e| e.child_value(sub).to_string()).unwrap_or_default();
        PersonForm {
            given,
            surname,
            suffix: r.child("NAME").map(name_suffix).unwrap_or_default(),
            sex: Sex::from_gedcom(r.child_value("SEX")),
            birth_date: ev("BIRT", "DATE"),
            birth_place: ev("BIRT", "PLAC"),
            death_date: ev("DEAT", "DATE"),
            death_place: ev("DEAT", "PLAC"),
            deceased: r.child("DEAT").is_some(),
            occupation: r.child_value("OCCU").to_string(),
            note: r
                .children_with("NOTE")
                .find(|n| n.pointer().is_none())
                .map(|n| n.value.clone())
                .unwrap_or_default(),
            citations: read_citations(r),
            events: {
                let mut seen: HashMap<&str, usize> = HashMap::new();
                r.children
                    .iter()
                    .filter_map(|c| {
                        let n = seen.entry(c.tag.as_str()).or_default();
                        let key = (c.tag.clone(), *n);
                        *n += 1;
                        (event_label(&c.tag).is_some() && !has_own_field(&c.tag)).then(|| EventForm {
                            origin: Some(key),
                            tag: c.tag.clone(),
                            date: c.child_value("DATE").to_string(),
                            place: c.child_value("PLAC").to_string(),
                            detail: event_detail(c),
                        })
                    })
                    .collect()
            },
            marriages: self
                .spouse_families(xref)
                .into_iter()
                .filter_map(|f| {
                    let fam = self.record(&f)?;
                    let spouse = self.spouse_in(&f, xref).and_then(|s| self.person(&s).map(|p| p.display.clone())).unwrap_or_else(|| "unknown partner".into());
                    let marr = fam.child("MARR");
                    Some(MarriageForm {
                        family: f,
                        spouse,
                        date: marr.map(|m| m.child_value("DATE").to_string()).unwrap_or_default(),
                        place: marr.map(|m| m.child_value("PLAC").to_string()).unwrap_or_default(),
                    })
                })
                .collect(),
        }
    }

    /// Distinct place names, most-used first, for autocomplete.
    pub fn places(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        fn walk<'a>(n: &'a Node, counts: &mut HashMap<&'a str, usize>) {
            for c in &n.children {
                if c.tag == "PLAC" && !c.value.trim().is_empty() {
                    *counts.entry(c.value.trim()).or_default() += 1;
                }
                walk(c, counts);
            }
        }
        for r in &self.records {
            walk(r, &mut counts);
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    }

    // ---- editing (call inside `mutate`) --------------------------------------

    pub(crate) fn next_xref(&self, prefix: &str) -> String {
        let max = self
            .index
            .keys()
            .filter_map(|k| k.strip_prefix(prefix).and_then(|n| n.parse::<u64>().ok()))
            .max()
            .unwrap_or(0);
        format!("{prefix}{}", max + 1)
    }

    pub(crate) fn push_record(&mut self, node: Node) -> String {
        let xref = node.xref.clone().expect("record needs xref");
        self.index.insert(xref.clone(), self.records.len());
        self.records.push(node);
        xref
    }

    pub fn create_person(&mut self, form: &PersonForm) -> String {
        let xref = self.next_xref("I");
        self.push_record(Node::record(&xref, "INDI"));
        self.apply_form(&xref, form);
        xref
    }

    pub fn apply_form(&mut self, xref: &str, form: &PersonForm) {
        // New sources first: they need records of their own.
        let pointers: Vec<Option<String>> = form
            .citations
            .iter()
            .map(|c| match &c.source {
                CiteSource::Record(x) => Some(x.clone()),
                CiteSource::New { title, author, publication } => Some(self.create_source(title, author, publication)),
                CiteSource::Text(_) => None,
            })
            .collect();
        // Citations are only rewritten when they've changed, so an unchanged
        // save leaves them exactly where they were.
        let citations_changed = self.record(xref).is_none_or(|r| read_citations(r) != form.citations);
        let Some(r) = self.record_mut(xref) else { return };
        let original_order = fact_keys(r);
        if citations_changed {
            strip_citations(r);
        }
        let given = form.given.trim();
        let surname = form.surname.trim();
        let suffix = form.suffix.trim();
        let name_unchanged = r.child("NAME").is_some_and(|n| {
            let (g, s) = split_name(n);
            g == given && s == surname && name_suffix(n) == suffix
        });
        if !name_unchanged {
            let name = r.ensure_child("NAME");
            name.value = match (given.is_empty(), surname.is_empty()) {
                (_, false) => format!("{given} /{surname}/ {suffix}").trim().to_string(),
                (false, true) => format!("{given} {suffix}").trim().to_string(),
                (true, true) => suffix.to_string(),
            };
            if name.child("GIVN").is_some() || !given.is_empty() && name.children.iter().any(|c| c.tag == "SURN") {
                name.set_child_value("GIVN", given);
            }
            if name.child("SURN").is_some() {
                name.set_child_value("SURN", surname);
            }
            if name.child("NSFX").is_some() || !suffix.is_empty() && name.children.iter().any(|c| c.tag == "SURN" || c.tag == "GIVN") {
                name.set_child_value("NSFX", suffix);
            }
        }
        if form.sex == Sex::Unknown && r.child("SEX").is_none() {
            // leave absent
        } else {
            r.ensure_child("SEX").value = form.sex.code().into();
        }
        set_event(r, "BIRT", &form.birth_date, &form.birth_place, false);
        set_event(r, "DEAT", &form.death_date, &form.death_place, form.deceased);
        r.set_child_value("OCCU", &form.occupation);
        let note = form.note.trim_end();
        match r.children.iter().position(|n| n.tag == "NOTE" && n.pointer().is_none()) {
            Some(i) if note.is_empty() => {
                r.children.remove(i);
            }
            Some(i) => r.children[i].value = note.to_string(),
            None if !note.is_empty() => r.children.push(Node::new("NOTE", note)),
            None => {}
        }
        if citations_changed {
            for (c, pointer) in form.citations.iter().zip(pointers) {
                write_citation(r, c, pointer);
            }
        }
        // Events after citations, which were written against the events as
        // they were. Only what changed is touched, so an edit elsewhere
        // doesn't rewrite every date.
        apply_events(r, &form.events);
        // Lines that were already there keep their place; new ones go where
        // GEDCOM convention puts them (names first, family links last).
        let family_links = original_order.iter().position(|(t, _)| t == "FAMC" || t == "FAMS").unwrap_or(original_order.len());
        let keys = fact_keys(r);
        let mut order: Vec<(f32, Node)> = keys
            .iter()
            .zip(std::mem::take(&mut r.children))
            .map(|(k, node)| {
                let rank = match original_order.iter().position(|o| o == k) {
                    Some(i) => i as f32,
                    None => match k.0.as_str() {
                        "NAME" => -2.0,
                        "SEX" => -1.0,
                        "FAMC" | "FAMS" => f32::MAX,
                        _ => family_links as f32 - 0.5,
                    },
                };
                (rank, node)
            })
            .collect();
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        r.children = order.into_iter().map(|(_, n)| n).collect();

        // Marriages live on the families.
        for m in &form.marriages {
            let Some(fam) = self.record_mut(&m.family) else { continue };
            let marr = fam.child("MARR");
            let (date, place) = (marr.map(|n| n.child_value("DATE")).unwrap_or(""), marr.map(|n| n.child_value("PLAC")).unwrap_or(""));
            if normalize_date(date) != normalize_date(&m.date) || place.trim() != m.place.trim() {
                set_event(fam, "MARR", &normalize_date(&m.date), &m.place, false);
            }
        }
    }

    pub fn create_source(&mut self, title: &str, author: &str, publication: &str) -> String {
        let xref = self.next_xref("S");
        let mut rec = Node::record(&xref, "SOUR");
        rec.set_child_value("TITL", title);
        rec.set_child_value("AUTH", author);
        rec.set_child_value("PUBL", publication);
        self.push_record(rec)
    }

    fn create_family(&mut self) -> String {
        let xref = self.next_xref("F");
        self.push_record(Node::record(&xref, "FAM"))
    }

    fn add_pointer(&mut self, on: &str, tag: &str, target: &str) {
        let p = pointer_to(target);
        if let Some(r) = self.record_mut(on)
            && !r.children.iter().any(|c| c.tag == tag && c.value == p) {
                // Keep pointers grouped after existing ones of the same kind.
                let pos = r
                    .children
                    .iter()
                    .rposition(|c| c.tag == tag)
                    .map(|i| i + 1)
                    .unwrap_or(r.children.len());
                r.children.insert(pos, Node::new(tag, p));
            }
    }

    fn set_spouse_role(&mut self, fam: &str, person: &str) -> Result<(), String> {
        let sex = self.person(person).map(|p| p.sex).unwrap_or_else(|| {
            Sex::from_gedcom(self.record(person).map(|r| r.child_value("SEX")).unwrap_or(""))
        });
        let (h, w) = (self.husband(fam), self.wife(fam));
        let tag = match sex {
            Sex::Male if h.is_none() => "HUSB",
            Sex::Female if w.is_none() => "WIFE",
            Sex::Unknown if h.is_none() => "HUSB",
            Sex::Unknown if w.is_none() => "WIFE",
            _ => return Err("That family already has both partners.".into()),
        };
        self.add_pointer(fam, tag, person);
        self.add_pointer(person, "FAMS", fam);
        Ok(())
    }

    /// Links `other` to `anchor` as the given relation.
    pub fn link(&mut self, anchor: &str, rel: &Relation, other: &str) -> Result<(), String> {
        if anchor == other {
            return Err("A person can't be their own relative.".into());
        }
        match rel {
            Relation::Father | Relation::Mother => {
                let fam = self.parent_families(anchor).into_iter().next();
                let fam = match fam {
                    Some(f) => {
                        let taken = if *rel == Relation::Father { self.husband(&f) } else { self.wife(&f) };
                        if taken.is_some() {
                            return Err(format!("This person already has a {}.", rel.label().to_lowercase()));
                        }
                        f
                    }
                    None => {
                        let f = self.create_family();
                        self.add_pointer(&f, "CHIL", anchor);
                        self.add_pointer(anchor, "FAMC", &f);
                        f
                    }
                };
                let tag = if *rel == Relation::Father { "HUSB" } else { "WIFE" };
                self.add_pointer(&fam, tag, other);
                self.add_pointer(other, "FAMS", &fam);
            }
            Relation::Spouse => {
                // Reuse a family where the anchor has no partner yet.
                let open = self.spouse_families(anchor).into_iter().find(|f| self.spouse_in(f, anchor).is_none());
                let fam = match open {
                    Some(f) => f,
                    None => {
                        let f = self.create_family();
                        self.set_spouse_role(&f, anchor)?;
                        f
                    }
                };
                self.set_spouse_role(&fam, other)?;
            }
            Relation::Child(fam) => {
                let fam = match fam {
                    Some(f) => f.clone(),
                    None => match self.spouse_families(anchor).into_iter().next() {
                        Some(f) => f,
                        None => {
                            let f = self.create_family();
                            self.set_spouse_role(&f, anchor)?;
                            f
                        }
                    },
                };
                self.add_pointer(&fam, "CHIL", other);
                self.add_pointer(other, "FAMC", &fam);
            }
            Relation::Sibling => {
                let fam = match self.parent_families(anchor).into_iter().next() {
                    Some(f) => f,
                    None => {
                        let f = self.create_family();
                        self.add_pointer(&f, "CHIL", anchor);
                        self.add_pointer(anchor, "FAMC", &f);
                        f
                    }
                };
                self.add_pointer(&fam, "CHIL", other);
                self.add_pointer(other, "FAMC", &fam);
            }
        }
        Ok(())
    }

    /// Removes `person` from `fam` (as partner or child) and tidies up.
    pub fn unlink(&mut self, fam: &str, person: &str) {
        self.detach(fam, person);
        self.drop_family_if_empty(fam);
    }

    /// Removes `person` from `fam` without deleting a family left empty.
    pub fn detach(&mut self, fam: &str, person: &str) {
        let p = pointer_to(person);
        let f = pointer_to(fam);
        if let Some(r) = self.record_mut(fam) {
            r.children.retain(|c| !(matches!(c.tag.as_str(), "HUSB" | "WIFE" | "CHIL") && c.value == p));
        }
        if let Some(r) = self.record_mut(person) {
            r.children.retain(|c| !(matches!(c.tag.as_str(), "FAMS" | "FAMC") && c.value == f));
        }
    }

    pub fn families(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|r| r.tag == "FAM")
            .filter_map(|r| r.xref.clone())
            .collect()
    }

    pub fn new_family(&mut self) -> String {
        self.create_family()
    }

    pub fn add_partner(&mut self, fam: &str, person: &str) -> Result<(), String> {
        self.set_spouse_role(fam, person)
    }

    /// The family whose partners are exactly `husband` and `wife`.
    pub fn find_family(&self, husband: Option<&str>, wife: Option<&str>) -> Option<String> {
        let anchor = husband.or(wife)?;
        self.spouse_families(anchor)
            .into_iter()
            .find(|f| self.husband(f).as_deref() == husband && self.wife(f).as_deref() == wife)
    }

    /// Makes `father` and `mother` the parents of `child` (their first birth
    /// family): reuses a family with exactly those partners, else creates
    /// one, and tidies away a family the child leaves empty. Other birth
    /// families (adoptive, say) are left alone.
    pub fn set_parents(&mut self, child: &str, father: Option<&str>, mother: Option<&str>) {
        let current = self.parent_families(child).into_iter().next();
        if let Some(f) = &current
            && self.husband(f).as_deref() == father
            && self.wife(f).as_deref() == mother
        {
            return;
        }
        // Where the old birth family sat, so the new one takes its place.
        let slot = self.record(child).and_then(|r| r.children.iter().position(|c| c.tag == "FAMC"));
        if let Some(f) = &current {
            self.detach(f, child);
            self.drop_family_if_empty(f);
        }
        if father.is_none() && mother.is_none() {
            return;
        }
        let fam = match self.find_family(father, mother) {
            Some(f) => f,
            None => {
                let f = self.create_family();
                for (tag, who) in [("HUSB", father), ("WIFE", mother)] {
                    if let Some(p) = who {
                        self.add_pointer(&f, tag, p);
                        self.add_pointer(p, "FAMS", &f);
                    }
                }
                f
            }
        };
        self.add_child_to(&fam, child);
        let p = pointer_to(&fam);
        if let (Some(slot), Some(r)) = (slot, self.record_mut(child))
            && let Some(i) = r.children.iter().position(|c| c.tag == "FAMC" && c.value == p)
        {
            let node = r.children.remove(i);
            r.children.insert(slot.min(r.children.len()), node);
        }
    }

    pub fn add_child_to(&mut self, fam: &str, person: &str) {
        self.add_pointer(fam, "CHIL", person);
        self.add_pointer(person, "FAMC", fam);
    }

    pub fn delete_family(&mut self, fam: &str) {
        self.remove_record(fam);
    }

    /// Removes a record of any kind and every pointer to it.
    pub(crate) fn delete_record(&mut self, xref: &str) {
        self.remove_record(xref);
    }

    pub fn set_family_event(&mut self, fam: &str, tag: &str, date: &str, place: &str) {
        if let Some(r) = self.record_mut(fam) {
            set_event(r, tag, date, place, false);
        }
    }

    fn drop_family_if_empty(&mut self, fam: &str) {
        let Some(r) = self.record(fam) else { return };
        let members: Vec<String> = r
            .children
            .iter()
            .filter(|c| matches!(c.tag.as_str(), "HUSB" | "WIFE" | "CHIL"))
            .filter_map(|c| c.pointer().map(str::to_string))
            .collect();
        let has_data = r
            .children
            .iter()
            .any(|c| !matches!(c.tag.as_str(), "HUSB" | "WIFE" | "CHIL" | "CHAN" | "_UID" | "RIN"));
        if members.is_empty() || (members.len() == 1 && !has_data) {
            for m in &members {
                let fp = pointer_to(fam);
                if let Some(mr) = self.record_mut(m) {
                    mr.children.retain(|c| !(matches!(c.tag.as_str(), "FAMS" | "FAMC") && c.value == fp));
                }
            }
            self.remove_record(fam);
        }
    }

    fn remove_record(&mut self, xref: &str) {
        self.records.retain(|r| r.xref.as_deref() != Some(xref));
        let p = pointer_to(xref);
        for r in &mut self.records {
            strip_pointers(r, &p);
        }
        self.index = self
            .records
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.xref.clone().map(|x| (x, i)))
            .collect();
    }

    /// Deletes a person without tidying away families they leave empty.
    pub fn delete_person_only(&mut self, xref: &str) {
        self.remove_record(xref);
    }

    pub fn delete_person(&mut self, xref: &str) {
        let fams: Vec<String> = self
            .parent_families(xref)
            .into_iter()
            .chain(self.spouse_families(xref))
            .collect();
        self.remove_record(xref);
        for f in fams {
            self.drop_family_if_empty(&f);
        }
    }
}

fn strip_pointers(n: &mut Node, p: &str) {
    n.children.retain(|c| {
        !(c.value == p && matches!(c.tag.as_str(), "HUSB" | "WIFE" | "CHIL" | "FAMS" | "FAMC" | "ASSO" | "ALIA" | "OBJE"))
    });
    for c in &mut n.children {
        strip_pointers(c, p);
    }
}

/// Brings the record's editable events in line with `events`: changed ones
/// updated in place (keeping their sources and anything else under them),
/// removed ones dropped, new ones added.
fn apply_events(r: &mut Node, events: &[EventForm]) {
    // Where each existing event is, by its (tag, n).
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut at: HashMap<FactKey, usize> = HashMap::new();
    for (i, c) in r.children.iter().enumerate() {
        let n = seen.entry(c.tag.clone()).or_default();
        if event_label(&c.tag).is_some() && !has_own_field(&c.tag) {
            at.insert((c.tag.clone(), *n), i);
        }
        *n += 1;
    }
    let kept: std::collections::HashSet<&FactKey> = events.iter().filter_map(|e| e.origin.as_ref()).collect();
    let mut remove: Vec<usize> = at.iter().filter(|(k, _)| !kept.contains(k)).map(|(_, &i)| i).collect();
    for e in events {
        let Some(i) = e.origin.as_ref().and_then(|k| at.get(k)).copied() else { continue };
        let node = &mut r.children[i];
        let unchanged = node.tag == e.tag
            && normalize_date(node.child_value("DATE")) == normalize_date(&e.date)
            && node.child_value("PLAC").trim() == e.place.trim()
            && event_detail(node).trim() == e.detail.trim();
        if unchanged {
            continue;
        }
        node.tag = e.tag.clone();
        write_event(node, e);
    }
    remove.sort_unstable();
    for i in remove.into_iter().rev() {
        r.children.remove(i);
    }
    for e in events.iter().filter(|e| e.origin.is_none()) {
        if e.tag.is_empty() || e.date.trim().is_empty() && e.place.trim().is_empty() && e.detail.trim().is_empty() {
            continue;
        }
        let mut node = Node::new(e.tag.clone(), "");
        write_event(&mut node, e);
        r.children.push(node);
    }
}

fn write_event(node: &mut Node, e: &EventForm) {
    node.set_child_value("DATE", &normalize_date(&e.date));
    node.set_child_value("PLAC", &e.place);
    if node.tag == "EVEN" {
        node.set_child_value("TYPE", &e.detail);
        if node.value == "Y" {
            node.value.clear();
        }
    } else {
        node.value = e.detail.trim().to_string();
    }
    // With nothing else to say, "Y" records that it happened.
    if node.value.is_empty() && node.children.is_empty() {
        node.value = "Y".into();
    }
    node.children.sort_by_key(|c| match c.tag.as_str() {
        "TYPE" => 0,
        "DATE" => 1,
        "PLAC" => 2,
        _ => 3,
    });
}

fn set_event(r: &mut Node, tag: &str, date: &str, place: &str, keep_empty: bool) {
    let date = date.trim();
    let place = place.trim();
    if date.is_empty() && place.is_empty() {
        if let Some(i) = r.children.iter().position(|c| c.tag == tag) {
            let ev = &mut r.children[i];
            ev.children.retain(|c| c.tag != "DATE" && c.tag != "PLAC");
            if keep_empty {
                if ev.children.is_empty() {
                    ev.value = "Y".into();
                }
            } else if ev.children.is_empty() {
                r.children.remove(i);
            }
        } else if keep_empty {
            r.children.push(Node::new(tag, "Y"));
        }
        return;
    }
    let ev = r.ensure_child(tag);
    if ev.value == "Y" {
        ev.value.clear();
    }
    ev.set_child_value("DATE", date);
    ev.set_child_value("PLAC", place);
    ev.children.sort_by_key(|c| match c.tag.as_str() {
        "TYPE" => 0,
        "DATE" => 1,
        "PLAC" => 2,
        _ => 3,
    });
}

fn source_title(r: &Node, xref: &str) -> String {
    [r.child_value("TITL"), r.child_value("ABBR")]
        .into_iter()
        .map(|t| t.lines().next().unwrap_or("").trim())
        .find(|t| !t.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Untitled source ({xref})"))
}

/// Tags whose `SOUR` children the editor manages: the person as a whole,
/// their names, and their events.
pub(crate) fn is_citable(tag: &str) -> bool {
    tag == "NAME" || event_label(tag).is_some()
}

fn citation_from_node(fact: FactKey, n: &Node) -> CitationForm {
    CitationForm {
        fact,
        source: match n.pointer() {
            Some(x) => CiteSource::Record(x.to_string()),
            None => CiteSource::Text(n.value.clone()),
        },
        page: n.child_value("PAGE").to_string(),
        quality: n.child_value("QUAY").trim().parse().ok().filter(|q| *q <= 3),
        original: Some(n.clone()),
    }
}

fn read_citations(r: &Node) -> Vec<CitationForm> {
    let mut out = Vec::new();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for c in &r.children {
        if c.tag == "SOUR" {
            out.push(citation_from_node((GENERAL_FACT.to_string(), 0), c));
        } else if is_citable(&c.tag) {
            let n = seen.entry(c.tag.as_str()).or_default();
            for s in c.children_with("SOUR") {
                out.push(citation_from_node((c.tag.clone(), *n), s));
            }
            *n += 1;
        }
    }
    out
}

/// (tag, occurrence) for each child, identifying lines across an edit.
fn fact_keys(r: &Node) -> Vec<FactKey> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    r.children
        .iter()
        .map(|c| {
            let n = seen.entry(c.tag.as_str()).or_default();
            *n += 1;
            (c.tag.clone(), *n - 1)
        })
        .collect()
}

fn strip_citations(r: &mut Node) {
    r.children.retain(|c| c.tag != "SOUR");
    for c in r.children.iter_mut().filter(|c| is_citable(&c.tag)) {
        c.children.retain(|s| s.tag != "SOUR");
    }
}

fn write_citation(r: &mut Node, c: &CitationForm, pointer: Option<String>) {
    let mut node = c.original.clone().unwrap_or_else(|| Node::new("SOUR", ""));
    node.value = match (pointer, &c.source) {
        (Some(x), _) => pointer_to(&x),
        (None, CiteSource::Text(t)) => t.clone(),
        (None, _) => String::new(),
    };
    node.set_child_value("PAGE", &c.page);
    match c.quality {
        Some(q) => node.ensure_child("QUAY").value = q.to_string(),
        None => node.children.retain(|n| n.tag != "QUAY"),
    }
    // Standard order inside a citation: PAGE first, QUAY after the rest.
    node.children.sort_by_key(|n| match n.tag.as_str() {
        "PAGE" => 0,
        "QUAY" => 2,
        _ => 1,
    });
    let (tag, nth) = &c.fact;
    if tag.is_empty() {
        r.children.push(node);
        return;
    }
    let target = match r.children.iter().enumerate().filter(|(_, n)| &n.tag == tag).map(|(i, _)| i).nth(*nth) {
        Some(i) => i,
        None => {
            r.children.push(Node::new(tag.clone(), if tag == "DEAT" || tag == "BIRT" { "Y" } else { "" }));
            r.children.len() - 1
        }
    };
    r.children[target].children.push(node);
}

fn summarize(r: &Node) -> PersonSummary {
    let (given, surname) = r.child("NAME").map(split_name).unwrap_or_default();
    let suffix = r.child("NAME").map(name_suffix).unwrap_or_default();
    let mut display = [given.as_str(), surname.as_str(), suffix.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if display.is_empty() {
        display = "Unnamed person".into();
    }
    let year = |tags: &[&str]| {
        tags.iter()
            .find_map(|t| r.child(t).and_then(|e| year_of(e.child_value("DATE"))))
    };
    let birth_year = year(&["BIRT", "CHR", "BAPM"]);
    let death_year = year(&["DEAT", "BURI", "CREM"]);
    let is_dead = death_year.is_some() || r.child("DEAT").is_some() || r.child("BURI").is_some();
    let xref = r.xref.clone().unwrap_or_default();
    let search = format!("{display} {xref} {}", birth_year.map(|y| y.to_string()).unwrap_or_default()).to_lowercase();
    PersonSummary {
        xref,
        photos: Vec::new(),
        given,
        surname,
        display,
        sex: Sex::from_gedcom(r.child_value("SEX")),
        birth_year,
        death_year,
        is_dead,
        search,
    }
}

/// The suffix after `/Surname/`, or the `NSFX` part when the name has none.
pub fn name_suffix(name: &Node) -> String {
    let suffix = name.value.splitn(3, '/').nth(2).unwrap_or("").trim();
    if suffix.is_empty() { name.child_value("NSFX").trim().to_string() } else { suffix.to_string() }
}

/// Splits `Given /Surname/ Suffix` into (given, surname).
pub fn split_name(name: &Node) -> (String, String) {
    let v = &name.value;
    let mut parts = v.splitn(3, '/');
    let given = parts.next().unwrap_or("").trim().to_string();
    let surname = parts.next().unwrap_or("").trim().to_string();
    let given = if given.is_empty() { name.child_value("GIVN").to_string() } else { given };
    let surname = if surname.is_empty() { name.child_value("SURN").to_string() } else { surname };
    (given, surname)
}

fn event_view(n: &Node, label: &str, related: Option<String>) -> EventView {
    let date = n.child_value("DATE").to_string();
    let typ = n.child_value("TYPE");
    let label = if n.tag == "EVEN" && !typ.is_empty() { typ.to_string() } else { label.to_string() };
    let mut detail = String::new();
    if !n.value.is_empty() && n.value != "Y" && n.pointer().is_none() {
        detail = n.value.clone();
    }
    if n.tag != "EVEN" && !typ.is_empty() {
        detail = if detail.is_empty() { typ.to_string() } else { format!("{detail} · {typ}") };
    }
    EventView {
        tag: n.tag.clone(),
        label,
        sort: sort_key(&date),
        date: pretty_date(&date),
        place: n.child_value("PLAC").to_string(),
        detail,
        related,
    }
}

pub fn event_label(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "BIRT" => "Birth",
        "CHR" => "Christening",
        "BAPM" => "Baptism",
        "DEAT" => "Death",
        "BURI" => "Burial",
        "CREM" => "Cremation",
        "ADOP" => "Adoption",
        "BARM" => "Bar mitzvah",
        "BASM" => "Bat mitzvah",
        "BLES" => "Blessing",
        "CHRA" => "Adult christening",
        "CONF" => "Confirmation",
        "FCOM" => "First communion",
        "ORDN" => "Ordination",
        "NATU" => "Naturalization",
        "EMIG" => "Emigration",
        "IMMI" => "Immigration",
        "CENS" => "Census",
        "PROB" => "Probate",
        "WILL" => "Will",
        "GRAD" => "Graduation",
        "RETI" => "Retirement",
        "EVEN" => "Event",
        "RESI" => "Residence",
        "OCCU" => "Occupation",
        "EDUC" => "Education",
        "RELI" => "Religion",
        "TITL" => "Title",
        "NATI" => "Nationality",
        "PROP" => "Property",
        "DSCR" => "Description",
        "MARR" => "Marriage",
        "DIV" => "Divorce",
        "DIVF" => "Divorce filed",
        "ENGA" => "Engagement",
        "MARB" => "Marriage banns",
        "MARC" => "Marriage contract",
        "MARL" => "Marriage license",
        "MARS" => "Marriage settlement",
        "ANUL" => "Annulment",
        _ => return None,
    })
}

// ---- dates ------------------------------------------------------------------

const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];

pub fn year_of(date: &str) -> Option<i32> {
    sort_key(date).map(|k| k.0)
}

/// (year, month, day) of the first date in a GEDCOM date phrase.
pub fn sort_key(date: &str) -> Option<(i32, u8, u8)> {
    let tokens: Vec<String> = date
        .split(|c: char| c.is_whitespace() || c == ',' || c == '-')
        .filter(|t| !t.is_empty())
        .map(|t| t.to_uppercase())
        .collect();
    let mut day = 0u8;
    let mut month = 0u8;
    for (i, t) in tokens.iter().enumerate() {
        if let Some(m) = MONTHS.iter().position(|m| t.starts_with(m)) {
            month = m as u8 + 1;
            continue;
        }
        let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() {
            continue;
        }
        let n: i32 = digits.parse().ok()?;
        let next_is_month = tokens.get(i + 1).is_some_and(|n| MONTHS.iter().any(|m| n.starts_with(m)));
        if digits.len() <= 2 && next_is_month && (1..=31).contains(&n) {
            day = n as u8;
        } else if digits.len() >= 3 {
            let year = if t.ends_with("BC") || tokens.get(i + 1).is_some_and(|n| n == "B.C.") { -n } else { n };
            return Some((year, month, day));
        }
    }
    None
}

/// Human-friendly rendering of a GEDCOM date phrase.
pub fn pretty_date(date: &str) -> String {
    let d = date.trim();
    if d.is_empty() {
        return String::new();
    }
    if d.starts_with('(') {
        return d.trim_matches(|c| c == '(' || c == ')').to_string();
    }
    d.split_whitespace()
        .map(|t| {
            let u = t.to_uppercase();
            match u.as_str() {
                "ABT" | "ABT." => "about".to_string(),
                "EST" | "EST." => "estimated".to_string(),
                "CAL" | "CAL." => "calculated".to_string(),
                "BEF" | "BEF." => "before".to_string(),
                "AFT" | "AFT." => "after".to_string(),
                "BET" => "between".to_string(),
                "AND" => "and".to_string(),
                "FROM" => "from".to_string(),
                "TO" => "to".to_string(),
                "INT" => "interpreted".to_string(),
                _ => match MONTHS.iter().position(|m| *m == u) {
                    Some(i) => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][i].to_string(),
                    None => t.to_string(),
                },
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Accepts friendly input ("3 march 1850", "c. 1850", "1850-03-03") and
/// returns canonical GEDCOM, or the trimmed input unchanged if unsure.
pub fn normalize_date(input: &str) -> String {
    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }
    // ISO yyyy-mm-dd
    let iso: Vec<&str> = s.split('-').collect();
    if iso.len() == 3 && iso[0].len() == 4 && iso.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty()) {
        let m: usize = iso[1].parse().unwrap_or(0);
        let d: u32 = iso[2].parse().unwrap_or(0);
        if (1..=12).contains(&m) && (1..=31).contains(&d) {
            return format!("{d} {} {}", MONTHS[m - 1], iso[0]);
        }
    }
    s.split_whitespace()
        .map(|t| {
            let u = t.trim_end_matches(',').to_uppercase();
            match u.as_str() {
                "ABOUT" | "C." | "C" | "CIRCA" | "CA." | "CA" | "~" => "ABT".to_string(),
                "BEFORE" => "BEF".to_string(),
                "AFTER" => "AFT".to_string(),
                "BETWEEN" => "BET".to_string(),
                "ESTIMATED" => "EST".to_string(),
                "CALCULATED" => "CAL".to_string(),
                _ => match MONTHS.iter().find(|m| u.len() >= 3 && u.starts_with(*m)) {
                    Some(m) => m.to_string(),
                    None => u,
                },
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_and_marriages_are_edited_in_place() {
        let text = "0 HEAD\n0 @I1@ INDI\n1 NAME Eugene /Henshaw/\n1 BIRT\n2 DATE 2 JUN 1866\n\
1 RESI\n2 DATE 1900\n2 PLAC Bermuda, Chesterfield, Virginia\n2 SOUR @S1@\n3 PAGE 1900 census\n\
1 CENS\n2 DATE 1910\n1 RESI\n2 DATE 26 February 1920\n2 PLAC Midlothian\n1 FAMS @F1@\n1 FAMS @F2@\n\
0 @I3@ INDI\n1 NAME Laura /Bohannon/\n1 FAMS @F2@\n\
0 @F1@ FAM\n1 HUSB @I1@\n1 MARR\n2 DATE 26 February 1889\n0 @F2@ FAM\n1 HUSB @I1@\n1 WIFE @I3@\n0 @S1@ SOUR\n1 TITL Census\n0 TRLR\n";
        let (mut doc, _) = Document::from_bytes(text.as_bytes());
        let mut form = doc.form_for("I1");
        let tags: Vec<&str> = form.events.iter().map(|e| e.tag.as_str()).collect();
        assert_eq!(tags, ["RESI", "CENS", "RESI"]);
        assert_eq!(form.marriages.iter().map(|m| m.spouse.as_str()).collect::<Vec<_>>(), ["unknown partner", "Laura Bohannon"]);

        // Move the first residence, drop the census, add an emigration,
        // and date the second marriage; leave the 1920 residence alone.
        form.events[0].place = "Richmond, Virginia".into();
        form.events.remove(1);
        form.events.push(EventForm { origin: None, tag: "EMIG".into(), date: "1885".into(), place: "Liverpool".into(), detail: String::new() });
        form.marriages[1].date = "about 1925".into();
        doc.mutate(|d| d.apply_form("I1", &form));

        let r = doc.record("I1").unwrap();
        let resi: Vec<&Node> = r.children_with("RESI").collect();
        assert_eq!(resi[0].child_value("PLAC"), "Richmond, Virginia");
        // Its source came along.
        assert_eq!(resi[0].child("SOUR").unwrap().child_value("PAGE"), "1900 census");
        // Untouched: not even its date rewritten.
        assert_eq!(resi[1].child_value("DATE"), "26 February 1920");
        assert!(r.child("CENS").is_none());
        assert_eq!(r.child("EMIG").unwrap().child_value("PLAC"), "Liverpool");
        assert_eq!(doc.family_event("F2", "MARR").unwrap().0, "ABT 1925");
        // The first marriage wasn't touched.
        assert_eq!(doc.family_event("F1", "MARR").unwrap().0, "26 February 1889");
    }

    #[test]
    fn a_marriage_without_details_still_shows_on_the_timeline() {
        let text = "0 HEAD\n0 @I1@ INDI\n1 NAME Eugene /Henshaw/\n1 FAMS @F1@\n1 FAMS @F2@\n\
0 @I2@ INDI\n1 NAME Ann /First/\n1 FAMS @F1@\n0 @I3@ INDI\n1 NAME Laura /Second/\n1 FAMS @F2@\n\
0 @F1@ FAM\n1 HUSB @I1@\n1 WIFE @I2@\n1 MARR\n2 DATE 26 FEB 1889\n\
0 @F2@ FAM\n1 HUSB @I1@\n1 WIFE @I3@\n0 TRLR\n";
        let (doc, _) = Document::from_bytes(text.as_bytes());
        let marriages: Vec<(String, String)> = doc.timeline("I1").into_iter().filter(|e| e.tag == "MARR").map(|e| (e.date, e.detail)).collect();
        assert_eq!(
            marriages,
            [("26 Feb 1889".to_string(), "with Ann First".to_string()), (String::new(), "with Laura Second · date not recorded".to_string())]
        );
    }

    #[test]
    fn dates() {
        assert_eq!(sort_key("12 MAR 1850"), Some((1850, 3, 12)));
        assert_eq!(sort_key("ABT 1850"), Some((1850, 0, 0)));
        assert_eq!(sort_key("BET 1850 AND 1860"), Some((1850, 0, 0)));
        assert_eq!(pretty_date("ABT 12 MAR 1850"), "about 12 Mar 1850");
        assert_eq!(normalize_date("c. 3 march 1850"), "ABT 3 MAR 1850");
        assert_eq!(normalize_date("1850-03-03"), "3 MAR 1850");
    }

    #[test]
    fn build_from_scratch() {
        let mut d = Document::new_empty();
        let me = d.mutate(|d| {
            d.create_person(&PersonForm { given: "Ada".into(), surname: "Lane".into(), sex: Sex::Female, ..Default::default() })
        });
        let dad = d.mutate(|d| {
            let x = d.create_person(&PersonForm { given: "Tom".into(), surname: "Lane".into(), ..Default::default() });
            d.link(&me, &Relation::Father, &x).unwrap();
            x
        });
        let mom = d.mutate(|d| {
            let x = d.create_person(&PersonForm { given: "Mae".into(), sex: Sex::Female, ..Default::default() });
            d.link(&me, &Relation::Mother, &x).unwrap();
            x
        });
        let sib = d.mutate(|d| {
            let x = d.create_person(&PersonForm { given: "Bo".into(), surname: "Lane".into(), sex: Sex::Male, ..Default::default() });
            d.link(&me, &Relation::Sibling, &x).unwrap();
            x
        });
        assert_eq!(d.father(&me), Some(dad.clone()));
        assert_eq!(d.mother(&me), Some(mom.clone()));
        assert_eq!(d.siblings(&me), vec![sib.clone()]);
        assert_eq!(d.father(&sib), Some(dad.clone()));
        assert_eq!(d.family_count(), 1);

        // Round-trips through GEDCOM text.
        let (d2, warnings) = Document::from_bytes(d.to_gedcom().as_bytes());
        assert!(warnings.is_empty());
        assert_eq!(d2.people().len(), 4);
        assert_eq!(d2.mother(&sib), Some(mom.clone()));

        d.mutate(|d| d.delete_person(&dad));
        assert_eq!(d.father(&me), None);
        assert_eq!(d.mother(&me), Some(mom));
        assert!(d.undo());
        assert_eq!(d.father(&me), Some(dad));
    }

    #[test]
    fn citations_round_trip_and_preserve_unknown_lines() {
        let src = "0 HEAD\r\n0 @I1@ INDI\r\n1 NAME Ann /Lee/\r\n1 BIRT\r\n2 DATE 1850\r\n2 SOUR @S1@\r\n3 PAGE p. 4\r\n3 DATA\r\n4 TEXT Ann, dau. of John\r\n3 QUAY 3\r\n1 RESI\r\n2 DATE 1860\r\n1 RESI\r\n2 DATE 1870\r\n2 SOUR Family bible\r\n0 @S1@ SOUR\r\n1 TITL Parish register\r\n0 TRLR\r\n";
        let (mut doc, _) = Document::from_bytes(src.as_bytes());
        let form = doc.form_for("I1");
        assert_eq!(form.citations.len(), 2);
        assert_eq!(form.citations[0].fact, ("BIRT".to_string(), 0));
        assert_eq!(form.citations[0].source, CiteSource::Record("S1".into()));
        assert_eq!(form.citations[0].page, "p. 4");
        assert_eq!(form.citations[0].quality, Some(3));
        assert_eq!(form.citations[1].fact, ("RESI".to_string(), 1), "second residence");
        assert_eq!(form.citations[1].source, CiteSource::Text("Family bible".into()));

        // Saving an unchanged form changes nothing.
        let before = doc.to_gedcom();
        doc.mutate(|d| d.apply_form("I1", &form));
        assert_eq!(doc.to_gedcom(), before);

        // Edit: re-rate the first, drop the bible, cite the name with a new source.
        let mut edited = form.clone();
        edited.citations[0].quality = Some(2);
        edited.citations.remove(1);
        let mut c = CitationForm::new(("NAME".into(), 0), CiteSource::New { title: "1880 Census".into(), author: "US Census Bureau".into(), publication: String::new() });
        c.page = "ED 12, sheet 3".into();
        edited.citations.push(c);
        doc.mutate(|d| d.apply_form("I1", &edited));
        let text = doc.to_gedcom();
        assert!(text.contains("2 SOUR @S1@\r\n3 PAGE p. 4\r\n3 DATA\r\n4 TEXT Ann, dau. of John\r\n3 QUAY 2\r\n"), "{text}");
        assert!(!text.contains("Family bible"));
        assert!(text.contains("1 NAME Ann /Lee/\r\n2 SOUR @S2@\r\n3 PAGE ED 12, sheet 3\r\n"), "{text}");
        assert!(text.contains("0 @S2@ SOUR\r\n1 TITL 1880 Census\r\n1 AUTH US Census Bureau\r\n"), "{text}");
        assert_eq!(doc.sources().len(), 2);

        let views = doc.citation_views("I1");
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].fact, "Name");
        assert_eq!(views[1].fact, "Birth · 1850");
        assert_eq!(views[1].title, "Parish register");
    }

    #[test]
    fn name_suffix_is_editable() {
        let src = "0 HEAD\r\n0 @I1@ INDI\r\n1 NAME Eugene /Henshaw/ Jr\r\n2 GIVN Eugene\r\n2 SURN Henshaw\r\n2 NSFX Jr\r\n0 @I2@ INDI\r\n1 NAME Ann /Lee/\r\n2 GIVN Ann\r\n2 SURN Lee\r\n0 TRLR\r\n";
        let (mut doc, _) = Document::from_bytes(src.as_bytes());
        let form = doc.form_for("I1");
        assert_eq!((form.given.as_str(), form.suffix.as_str()), ("Eugene", "Jr"));
        assert_eq!(doc.person("I1").unwrap().display, "Eugene Henshaw Jr");

        let before = doc.to_gedcom();
        doc.mutate(|d| d.apply_form("I1", &form));
        assert_eq!(doc.to_gedcom(), before, "unchanged save is lossless");

        let mut sr = form.clone();
        sr.suffix = "Sr".into();
        doc.mutate(|d| d.apply_form("I1", &sr));
        assert!(doc.to_gedcom().contains("1 NAME Eugene /Henshaw/ Sr\r\n2 GIVN Eugene\r\n2 SURN Henshaw\r\n2 NSFX Sr\r\n"));

        let mut none = sr.clone();
        none.suffix.clear();
        doc.mutate(|d| d.apply_form("I1", &none));
        let text = doc.to_gedcom();
        assert!(text.contains("1 NAME Eugene /Henshaw/\r\n2 GIVN Eugene\r\n2 SURN Henshaw\r\n0 @I2@"), "{text}");
        assert_eq!(doc.person("I1").unwrap().display, "Eugene Henshaw");

        let mut ann = doc.form_for("I2");
        ann.suffix = "III".into();
        doc.mutate(|d| d.apply_form("I2", &ann));
        assert!(doc.to_gedcom().contains("1 NAME Ann /Lee/ III\r\n2 GIVN Ann\r\n2 SURN Lee\r\n2 NSFX III\r\n"));
    }

    /// `GENIE_GED=path/to/file.ged cargo test -- --ignored real_file`
    #[test]
    #[ignore]
    fn real_file_round_trip() {
        let path = std::env::var("GENIE_GED").expect("set GENIE_GED");
        let bytes = std::fs::read(&path).unwrap();
        let t = std::time::Instant::now();
        let (doc, notes) = Document::from_bytes(&bytes);
        println!("loaded {} people, {} families in {:?}; {} notes", doc.people().len(), doc.family_count(), t.elapsed(), notes.len());
        for n in notes.iter().take(5) {
            println!("  {n}");
        }
        let once = doc.to_gedcom();
        let (again, _) = Document::from_bytes(once.as_bytes());
        assert_eq!(again.people().len(), doc.people().len());
        // Only HEAD may differ (CHAR becomes UTF-8).
        assert!(again.records()[1..] == doc.records()[1..], "records survive a round trip");
        assert_eq!(again.to_gedcom(), once, "saving is idempotent");
        let t = std::time::Instant::now();
        for p in doc.people().iter().take(200) {
            let _ = doc.timeline(&p.xref);
        }
        println!("200 timelines in {:?}", t.elapsed());
        let t = std::time::Instant::now();
        let people = doc.people();
        let mut connected = 0;
        for p in people.iter().step_by((people.len() / 50).max(1)).take(50) {
            if !crate::kinship::find(&doc, &people[0].xref, &p.xref).is_empty() {
                connected += 1;
            }
        }
        println!("50 relationship searches in {:?} ({connected} connected)", t.elapsed());
        let t = std::time::Instant::now();
        let dupes = crate::dedup::find_duplicate_people(&doc, &Default::default());
        let likely = dupes.iter().filter(|c| c.likely()).count();
        println!("{} possible duplicate people ({likely} likely) in {:?}", dupes.len(), t.elapsed());
        for c in dupes.iter().take(8) {
            let n = |x: &str| doc.person(x).map(|p| format!("{} {}", p.display, p.lifespan())).unwrap_or_default();
            println!("  {:>3}  {}  <>  {}   [{}]", c.score, n(&c.a), n(&c.b), c.reasons.join(", "));
        }
        let groups = crate::dedup::find_duplicate_sources(&doc);
        println!("{} groups of duplicate sources ({} extra copies)", groups.len(), groups.iter().map(|g| g.len() - 1).sum::<usize>());

        // Merge every duplicate source and the likely people; nothing may be
        // left pointing at a record that no longer exists.
        let mut merged = Document::from_bytes(once.as_bytes()).0;
        merged.mutate(|d| {
            for g in &groups {
                for other in &g[1..] {
                    d.merge_records(&g[0], other);
                }
            }
        });
        let mut n = 0;
        for c in dupes.iter().filter(|c| c.likely()) {
            if merged.person(&c.a).is_some() && merged.person(&c.b).is_some() {
                merged.mutate(|d| d.merge_people(&c.a, &c.b, Default::default()));
                n += 1;
            }
        }
        let text = merged.to_gedcom();
        let (reloaded, _) = Document::from_bytes(text.as_bytes());
        let mut dangling = Vec::new();
        fn walk(doc: &Document, n: &Node, out: &mut Vec<String>) {
            for c in &n.children {
                if let Some(x) = c.pointer()
                    && doc.record(x).is_none()
                {
                    out.push(format!("{} {}", c.tag, c.value));
                }
                walk(doc, c, out);
            }
        }
        for r in reloaded.records() {
            walk(&reloaded, r, &mut dangling);
        }
        println!("merged {n} likely pairs and {} source groups: {} people left, {} dangling references", groups.len(), reloaded.people().len(), dangling.len());
        assert!(dangling.is_empty(), "dangling after merges: {:?}", &dangling[..dangling.len().min(10)]);
        // Re-applying every unchanged form must not alter the file.
        let mut edited = Document::from_bytes(once.as_bytes()).0;
        let xrefs: Vec<String> = edited.people().iter().take(500).map(|p| p.xref.clone()).collect();
        for x in &xrefs {
            let form = edited.form_for(x);
            edited.mutate(|d| d.apply_form(x, &form));
        }
        let (a, b) = (edited.to_gedcom(), once.clone());
        if a != b {
            let line = a.lines().zip(b.lines()).position(|(x, y)| x != y).unwrap_or(0);
            panic!("unchanged forms altered the file near line {line}: {:?} vs {:?}", a.lines().nth(line), b.lines().nth(line));
        }
    }
}
