//! Finding and merging duplicates: people entered twice, and sources that
//! appear more than once (common in trees imported from online services).

use std::collections::{HashMap, HashSet};

use crate::gedcom::{Node, pointer_to};
use crate::model::{Document, Sex, event_label};

/// A pair of people who may be the same person.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub a: String,
    pub b: String,
    /// 0–100; higher is more likely.
    pub score: u32,
    /// What the score is based on, for display.
    pub reasons: Vec<String>,
}

impl Candidate {
    pub fn likely(&self) -> bool {
        self.score >= LIKELY
    }
}

const SHOW: u32 = 55;
const LIKELY: u32 = 80;

/// American soundex: "Henshaw" and "Hinshaw" both give H520.
pub fn soundex(s: &str) -> String {
    let letters: Vec<char> = s.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_uppercase()).collect();
    let Some(&first) = letters.first() else { return String::new() };
    let code = |c: char| match c {
        'B' | 'F' | 'P' | 'V' => '1',
        'C' | 'G' | 'J' | 'K' | 'Q' | 'S' | 'X' | 'Z' => '2',
        'D' | 'T' => '3',
        'L' => '4',
        'M' | 'N' => '5',
        'R' => '6',
        _ => '0',
    };
    let mut out = String::from(first);
    let mut last = code(first);
    for &c in &letters[1..] {
        let d = code(c);
        if d != '0' && d != last {
            out.push(d);
            if out.len() == 4 {
                break;
            }
        }
        // H and W don't separate letters with the same code; vowels do.
        if c != 'H' && c != 'W' {
            last = d;
        }
    }
    while out.len() < 4 {
        out.push('0');
    }
    out
}

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// How alike two given names are, allowing initials ("J." for "John")
/// and a missing middle name.
fn given_similarity(a: &str, b: &str) -> f64 {
    let (wa, wb) = (words(a), words(b));
    let (Some(fa), Some(fb)) = (wa.first(), wb.first()) else { return 0.0 };
    let whole = strsim::jaro_winkler(&wa.join(" "), &wb.join(" "));
    let first = if fa == fb {
        1.0
    } else if (fa.len() == 1 && fb.starts_with(fa.as_str())) || (fb.len() == 1 && fa.starts_with(fb.as_str())) {
        0.9
    } else {
        strsim::jaro_winkler(fa, fb)
    };
    whole.max(first * 0.97)
}

struct Features {
    xref: String,
    given: String,
    sex: Sex,
    birth: Option<i32>,
    death: Option<i32>,
    birth_place: String,
    parents: Vec<String>,
    spouses: Vec<String>,
    kin: HashSet<String>,
}

fn first_place(doc: &Document, x: &str, tag: &str) -> String {
    let place = doc.record(x).and_then(|r| r.child(tag)).map(|e| e.child_value("PLAC").to_string()).unwrap_or_default();
    place.split(',').next().unwrap_or("").trim().to_lowercase()
}

fn features(doc: &Document, x: &str) -> Features {
    let p = doc.person(x).expect("a person");
    let parents: Vec<String> = doc.father(x).into_iter().chain(doc.mother(x)).collect();
    let spouses: Vec<String> = doc.spouse_families(x).iter().filter_map(|f| doc.spouse_in(f, x)).collect();
    let children: Vec<String> = doc.spouse_families(x).iter().flat_map(|f| doc.children(f)).collect();
    let kin = parents.iter().chain(&spouses).chain(&children).cloned().collect();
    Features {
        xref: x.to_string(),
        given: p.given.clone(),
        sex: p.sex,
        birth: p.birth_year,
        death: p.death_year,
        birth_place: first_place(doc, x, "BIRT"),
        parents,
        spouses,
        kin,
    }
}

fn score(doc: &Document, a: &Features, b: &Features) -> Option<Candidate> {
    if a.kin.contains(&b.xref) || b.kin.contains(&a.xref) {
        return None; // Parent, child or spouse of each other.
    }
    if a.sex != Sex::Unknown && b.sex != Sex::Unknown && a.sex != b.sex {
        return None;
    }
    let mut score: i32 = 0;
    let mut reasons = Vec::new();

    let g = given_similarity(&a.given, &b.given);
    if g < 0.82 {
        return None;
    }
    if g > 0.97 {
        score += 40;
        reasons.push("same given name".to_string());
    } else {
        score += 28;
        reasons.push("similar given name".to_string());
    }

    for (label, x, y, full, near) in [("born", a.birth, b.birth, 25, 14), ("died", a.death, b.death, 15, 8)] {
        if let (Some(x), Some(y)) = (x, y) { match x.abs_diff(y) {
            0 => {
                score += full;
                reasons.push(format!("both {label} {x}"));
            }
            1..=2 => {
                score += near;
                reasons.push(format!("{label} within {} years", x.abs_diff(y)));
            }
            3..=5 => {}
            _ => return None,
        } }
    }
    if !a.birth_place.is_empty() && a.birth_place == b.birth_place {
        score += 10;
        reasons.push(format!("both born in {}", title_case(&a.birth_place)));
    }
    let shared_parent = a.parents.iter().any(|p| b.parents.contains(p));
    if shared_parent {
        score += 25;
        reasons.push("same parent".into());
    }
    let shared_spouse = a.spouses.iter().any(|s| b.spouses.contains(s));
    if shared_spouse {
        score += 30;
        reasons.push("same spouse".into());
    } else if spouse_names_match(doc, a, b) {
        score += 15;
        reasons.push("spouses with the same name".into());
    }
    // With nothing but a name in common, it's too weak to show.
    if a.birth.is_none() && b.birth.is_none() && !shared_parent && !shared_spouse {
        score -= 10;
    }
    let score = score.clamp(0, 100) as u32;
    if score < SHOW {
        return None;
    }
    // Offer to keep the record more of the tree hangs on (its known
    // ancestors and descendants), then the one with more recorded about it.
    let weight = |x: &str| (connections(doc, x), doc.record(x).map(count_lines).unwrap_or(0));
    let (keep, other) = if weight(&b.xref) > weight(&a.xref) { (&b.xref, &a.xref) } else { (&a.xref, &b.xref) };
    Some(Candidate { a: keep.clone(), b: other.clone(), score, reasons })
}

/// How many people are linked to `x` by descent: known ancestors plus
/// known descendants.
fn connections(doc: &Document, x: &str) -> usize {
    const CAP: usize = 50_000;
    let mut seen = HashSet::from([x.to_string()]);
    for up in [true, false] {
        let mut level = vec![x.to_string()];
        while !level.is_empty() && seen.len() < CAP {
            let next: Vec<String> = level
                .iter()
                .flat_map(|p| {
                    if up {
                        doc.father(p).into_iter().chain(doc.mother(p)).collect::<Vec<_>>()
                    } else {
                        doc.spouse_families(p).iter().flat_map(|f| doc.children(f)).collect()
                    }
                })
                .filter(|p| seen.insert(p.clone()))
                .collect();
            level = next;
        }
    }
    seen.len() - 1
}

fn count_lines(n: &Node) -> usize {
    1 + n.children.iter().map(count_lines).sum::<usize>()
}

fn spouse_names_match(doc: &Document, a: &Features, b: &Features) -> bool {
    let names = |f: &Features| -> Vec<String> { f.spouses.iter().filter_map(|s| doc.person(s).map(|p| p.display.to_lowercase())).collect() };
    let (na, nb) = (names(a), names(b));
    na.iter().any(|x| nb.iter().any(|y| strsim::jaro_winkler(x, y) > 0.93))
}

fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Pairs of people who may be duplicates, most likely first. `dismissed`
/// holds pairs the user has said are different people ("a|b", sorted).
pub fn find_duplicate_people(doc: &Document, dismissed: &HashSet<String>) -> Vec<Candidate> {
    // Compare only people whose surnames sound alike.
    let mut blocks: HashMap<String, Vec<Features>> = HashMap::new();
    for p in doc.people() {
        if p.given.trim().is_empty() {
            continue;
        }
        let key = soundex(&p.surname);
        blocks.entry(key).or_default().push(features(doc, &p.xref));
    }
    let mut out = Vec::new();
    for group in blocks.values() {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                if dismissed.contains(&pair_key(&a.xref, &b.xref)) {
                    continue;
                }
                if let Some(c) = score(doc, a, b) {
                    out.push(c);
                }
            }
        }
    }
    out.sort_by(|x, y| y.score.cmp(&x.score).then_with(|| x.a.cmp(&y.a)));
    out
}

pub fn pair_key(a: &str, b: &str) -> String {
    if a < b { format!("{a}|{b}") } else { format!("{b}|{a}") }
}

/// Groups of sources that look like the same source (same title and
/// author), each with the xref to keep first.
pub fn find_duplicate_sources(doc: &Document) -> Vec<Vec<String>> {
    let mut groups: HashMap<(String, String), Vec<String>> = HashMap::new();
    for r in doc.records().iter().filter(|r| r.tag == "SOUR") {
        let Some(x) = &r.xref else { continue };
        let title = words(r.child_value("TITL")).join(" ");
        if title.is_empty() {
            continue;
        }
        let author = words(r.child_value("AUTH")).join(" ");
        groups.entry((title, author)).or_default().push(x.clone());
    }
    let mut out: Vec<Vec<String>> = groups.into_values().filter(|g| g.len() > 1).collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

// ---- merging --------------------------------------------------------------------

/// Which record's value wins for a field when merging two people.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Side {
    #[default]
    Keep,
    Other,
}

/// Choices for the fields shown side by side; everything else is combined.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MergeChoices {
    pub name: Side,
    pub sex: Side,
    pub birth: Side,
    pub death: Side,
    pub occupation: Side,
}

/// Tags that link records together rather than describe the person.
const LINKS: [&str; 2] = ["FAMC", "FAMS"];
/// Bookkeeping that belongs to one record and shouldn't be copied over.
const BOOKKEEPING: [&str; 4] = ["CHAN", "RIN", "_UID", "_UPD"];

/// The same fact recorded the same way (same value, date and place).
fn same_fact(a: &Node, b: &Node) -> bool {
    a.tag == b.tag && a.value.trim() == b.value.trim() && a.child_value("DATE") == b.child_value("DATE") && a.child_value("PLAC") == b.child_value("PLAC")
}

/// Adds `other`'s citations, notes, documents and details to `into`,
/// skipping ones it already has.
fn absorb(into: &mut Node, other: &Node) {
    for c in &other.children {
        if !into.children.iter().any(|x| x == c) && !(c.tag != "SOUR" && c.tag != "NOTE" && c.tag != "OBJE" && into.child(&c.tag).is_some()) {
            into.children.push(c.clone());
        }
    }
}

/// Merges `other`'s children into `base`: the same fact recorded twice is
/// combined; anything else is kept alongside.
fn combine(base: &mut Vec<Node>, other: &[Node]) {
    for c in other {
        if BOOKKEEPING.contains(&c.tag.as_str()) || LINKS.contains(&c.tag.as_str()) && base.iter().any(|x| x == c) {
            continue;
        }
        if let Some(existing) = base.iter_mut().find(|x| same_fact(x, c) && (event_label(&x.tag).is_some() || x.tag == "NAME")) {
            absorb(existing, c);
        } else if !base.iter().any(|x| x == c) {
            base.push(c.clone());
        }
    }
}

fn repoint(n: &mut Node, from: &str, to: &str) {
    if n.value == from {
        n.value = to.to_string();
    }
    for c in &mut n.children {
        repoint(c, from, to);
    }
}

/// Drops repeated link lines (a family listing the same child twice).
fn dedupe_links(n: &mut Node) {
    let mut seen = HashSet::new();
    n.children.retain(|c| {
        if matches!(c.tag.as_str(), "HUSB" | "WIFE" | "CHIL" | "FAMC" | "FAMS") && c.pointer().is_some() {
            seen.insert((c.tag.clone(), c.value.clone()))
        } else {
            true
        }
    });
}

impl Document {
    /// Points every reference to `drop` at `keep`, and removes `drop`.
    fn replace_record(&mut self, keep: &str, drop: &str) {
        let (from, to) = (pointer_to(drop), pointer_to(keep));
        for r in self.records_mut() {
            if r.xref.as_deref() != Some(drop) {
                repoint(r, &from, &to);
                dedupe_links(r);
            }
        }
        self.delete_record(drop);
    }

    /// Merges person `drop` into `keep` (call inside `mutate`). Chosen
    /// values come first; the others are kept as alternatives.
    pub fn merge_people(&mut self, keep: &str, drop: &str, choices: MergeChoices) {
        let (Some(a), Some(b)) = (self.record(keep).cloned(), self.record(drop).cloned()) else { return };
        let mut children = a.children.clone();

        // The chosen version of each compared field goes first; the other
        // stays as an alternative unless it says the same thing.
        for (tag, side) in [("NAME", choices.name), ("BIRT", choices.birth), ("DEAT", choices.death), ("OCCU", choices.occupation)] {
            let theirs = b.child(tag).cloned();
            if side == Side::Other
                && let Some(t) = theirs
            {
                match children.iter().position(|c| c.tag == tag) {
                    Some(i) => {
                        let mut t = t;
                        if same_fact(&children[i], &t) {
                            absorb(&mut t, &children[i]);
                            children[i] = t;
                        } else {
                            children.insert(i, t);
                        }
                    }
                    None => children.push(t),
                }
            }
        }
        if choices.sex == Side::Other
            && let Some(s) = b.child("SEX").cloned()
        {
            children.retain(|c| c.tag != "SEX");
            let pos = children.iter().position(|c| c.tag == "NAME").map(|i| i + 1).unwrap_or(0);
            children.insert(pos, s);
        }
        let rest: Vec<Node> = b.children.iter().filter(|c| c.tag != "SEX").cloned().collect();
        combine(&mut children, &rest);
        // A second name that's just the same name isn't worth keeping.
        let mut names_seen = HashSet::new();
        children.retain(|c| c.tag != "NAME" || names_seen.insert(c.value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()));
        // Keep the usual order: names and sex first, family links last.
        children.sort_by_key(|c| match c.tag.as_str() {
            "NAME" => 0,
            "SEX" => 1,
            "FAMC" | "FAMS" => 3,
            _ => 2,
        });
        if let Some(r) = self.record_mut(keep) {
            r.children = children;
        }
        self.replace_record(keep, drop);

        // The same couple may now be recorded twice; combine those families.
        loop {
            let fams = self.spouse_families(keep);
            let mut pair = None;
            'find: for (i, f) in fams.iter().enumerate() {
                for g in &fams[i + 1..] {
                    let (h, w) = (self.husband(f), self.wife(f));
                    if h.is_some() && w.is_some() && h == self.husband(g) && w == self.wife(g) {
                        pair = Some((f.clone(), g.clone()));
                        break 'find;
                    }
                }
            }
            let Some((f, g)) = pair else { break };
            self.merge_records(&f, &g);
        }
    }

    /// Merges any two records of the same kind (families, sources): the
    /// second's details are combined into the first, then every reference
    /// is repointed. Call inside `mutate`.
    pub fn merge_records(&mut self, keep: &str, drop: &str) {
        let (Some(mut a), Some(b)) = (self.record(keep).cloned(), self.record(drop).cloned()) else { return };
        combine(&mut a.children, &b.children);
        dedupe_links(&mut a);
        if let Some(r) = self.record_mut(keep) {
            *r = a;
        }
        self.replace_record(keep, drop);
    }
}

/// Years for a side-by-side summary.
pub fn fact_summary(doc: &Document, x: &str, tag: &str) -> String {
    let Some(e) = doc.record(x).and_then(|r| r.child(tag)) else { return String::new() };
    let date = crate::model::pretty_date(e.child_value("DATE"));
    let place = e.child_value("PLAC");
    let value = if e.value == "Y" { "" } else { e.value.as_str() };
    [value, &date, place].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soundex_codes() {
        assert_eq!(soundex("Henshaw"), "H520");
        assert_eq!(soundex("Hinshaw"), "H520");
        assert_eq!(soundex("Robert"), "R163");
        assert_eq!(soundex("Rupert"), "R163");
        assert_eq!(soundex("Ashcraft"), "A261");
        assert_eq!(soundex("Tymczak"), "T522");
    }

    const TREE: &str = "0 HEAD
0 @I1@ INDI
1 NAME Thomas /Hartwell/
1 SEX M
1 BIRT
2 DATE 14 MAR 1842
2 PLAC Bristol, England
1 FAMS @F1@
1 FAMC @F9@
0 @I2@ INDI
1 NAME Margaret /Doyle/
1 SEX F
1 FAMS @F1@
1 FAMS @F2@
0 @I3@ INDI
1 NAME Tho. /Hartwell/
1 SEX M
1 BIRT
2 DATE ABT 1842
2 PLAC Bristol, Gloucestershire, England
2 SOUR @S2@
1 OCCU Shipwright
1 NOTE Built schooners.
1 FAMS @F2@
0 @I4@ INDI
1 NAME William /Hartwell/
1 SEX M
1 FAMC @F1@
0 @I5@ INDI
1 NAME Eliza /Hartwell/
1 SEX F
1 FAMC @F2@
0 @I6@ INDI
1 NAME Thomas /Hartwell/
1 SEX M
1 BIRT
2 DATE 1901
0 @F1@ FAM
1 HUSB @I1@
1 WIFE @I2@
1 CHIL @I4@
0 @F2@ FAM
1 HUSB @I3@
1 WIFE @I2@
1 MARR
2 DATE 1869
1 CHIL @I5@
0 @F9@ FAM
1 CHIL @I1@
0 @S1@ SOUR
1 TITL Parish register
0 @S2@ SOUR
1 TITL Parish  Register
1 NOTE copy
0 @S3@ SOUR
1 TITL Census
0 TRLR
";

    #[test]
    fn finds_the_likely_pair_and_not_the_namesake() {
        let (doc, _) = Document::from_bytes(TREE.as_bytes());
        let found = find_duplicate_people(&doc, &HashSet::new());
        assert_eq!(found.len(), 1, "{found:?}");
        let c = &found[0];
        assert_eq!(pair_key(&c.a, &c.b), "I1|I3");
        // Each has one child, so the one with more recorded is offered to keep.
        assert_eq!(c.a, "I3");
        assert!(c.likely(), "{c:?}");
        assert!(c.reasons.iter().any(|r| r == "same spouse"));
        // Dismissed pairs stay hidden.
        let hidden = HashSet::from([pair_key("I3", "I1")]);
        assert!(find_duplicate_people(&doc, &hidden).is_empty());
    }

    #[test]
    fn the_better_connected_record_is_offered_to_keep() {
        // I1 has more written about him; I2 is the one with a child.
        let tree = "0 HEAD
0 @I1@ INDI
1 NAME John /Smith/
1 SEX M
1 BIRT
2 DATE 1800
2 PLAC Leeds
1 OCCU Weaver
1 NOTE A long note about John.
1 RESI
2 DATE 1830
0 @I2@ INDI
1 NAME John /Smith/
1 SEX M
1 BIRT
2 DATE 1800
1 FAMS @F1@
0 @I3@ INDI
1 NAME Mary /Smith/
1 FAMC @F1@
0 @F1@ FAM
1 HUSB @I2@
1 CHIL @I3@
0 TRLR
";
        let (doc, _) = Document::from_bytes(tree.as_bytes());
        let found = find_duplicate_people(&doc, &HashSet::new());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].a, "I2");
    }

    #[test]
    fn merging_people_combines_everything_and_their_families() {
        let (mut doc, _) = Document::from_bytes(TREE.as_bytes());
        doc.mutate(|d| d.merge_people("I1", "I3", MergeChoices::default()));
        assert!(doc.person("I3").is_none());
        let r = doc.record("I1").unwrap();
        // The kept name first, the variant kept as an alternative.
        let names: Vec<&str> = r.children_with("NAME").map(|n| n.value.as_str()).collect();
        assert_eq!(names, ["Thomas /Hartwell/", "Tho. /Hartwell/"]);
        // Both births kept (they differ), the chosen one first.
        let births: Vec<&str> = r.children_with("BIRT").map(|b| b.child_value("DATE")).collect();
        assert_eq!(births, ["14 MAR 1842", "ABT 1842"]);
        assert_eq!(r.child_value("OCCU"), "Shipwright");
        assert_eq!(r.child_value("NOTE"), "Built schooners.");
        // The two Thomas & Margaret families became one, with both children.
        assert_eq!(doc.spouse_families("I1").len(), 1);
        let fam = &doc.spouse_families("I1")[0];
        let mut kids = doc.children(fam);
        kids.sort();
        assert_eq!(kids, ["I4", "I5"]);
        assert_eq!(doc.family_event(fam, "MARR").map(|m| m.0), Some("1869".into()));
        assert_eq!(doc.father("I5"), Some("I1".into()));
        assert_eq!(doc.spouse_families("I2").len(), 1, "Margaret isn't left in a phantom family");
        assert!(!doc.to_gedcom().contains("@I3@"));
        // One step to undo.
        assert!(doc.undo());
        assert!(doc.person("I3").is_some());
    }

    #[test]
    fn choosing_the_other_side_puts_its_values_first() {
        let (mut doc, _) = Document::from_bytes(TREE.as_bytes());
        let choices = MergeChoices { birth: Side::Other, ..Default::default() };
        doc.mutate(|d| d.merge_people("I1", "I3", choices));
        let r = doc.record("I1").unwrap();
        let births: Vec<&str> = r.children_with("BIRT").map(|b| b.child_value("DATE")).collect();
        assert_eq!(births, ["ABT 1842", "14 MAR 1842"]);
    }

    #[test]
    fn duplicate_sources_group_and_merge() {
        let (mut doc, _) = Document::from_bytes(TREE.as_bytes());
        let groups = find_duplicate_sources(&doc);
        assert_eq!(groups.len(), 1);
        let mut g = groups[0].clone();
        g.sort();
        assert_eq!(g, ["S1", "S2"]);
        doc.mutate(|d| d.merge_records("S1", "S2"));
        assert!(doc.record("S2").is_none());
        assert_eq!(doc.record("S1").unwrap().child_value("NOTE"), "copy");
        // I3's birth citation now points at S1.
        assert!(doc.to_gedcom().contains("2 SOUR @S1@"));
    }
}
