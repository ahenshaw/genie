//! How two people are related: the simplest lineage between them, and its
//! name in English.
//!
//! The search walks parent, child and spouse links. A parent or child step
//! costs 1 and a marriage step costs [`MARRIAGE`], so a blood connection is
//! always preferred and marriages are only crossed when nothing else joins
//! the two people. Within a stretch of blood links the path may only climb
//! and then descend (never go down to a child and back up to its other
//! parent), which is what makes a path a line of descent rather than a
//! walk through a family.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::model::{Document, Sex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Link {
    /// To a parent.
    Up,
    /// To a child.
    Down,
    /// To a spouse or partner.
    Spouse,
}

/// One lineage from a reference person to a subject.
#[derive(Clone, Debug, PartialEq)]
pub struct Kinship {
    /// `people[0]` is the reference person; the last is the subject.
    pub people: Vec<String>,
    /// `links[i]` leads from `people[i]` to `people[i + 1]`.
    pub links: Vec<Link>,
}

const BLOOD: u32 = 1;
const MARRIAGE: u32 = 100;
/// Distinct equally-short lineages offered at most.
pub const MAX_PATHS: usize = 10;
const MAX_RAW_PATHS: usize = 400;

/// Search state: a person and whether the current blood stretch has
/// started descending (after which it can't climb again).
type State = (String, bool);

fn steps(doc: &Document, (x, descending): &State) -> Vec<(State, Link, u32)> {
    let mut out = Vec::new();
    if !descending {
        for f in doc.parent_families(x) {
            for p in doc.husband(&f).into_iter().chain(doc.wife(&f)) {
                out.push(((p, false), Link::Up, BLOOD));
            }
        }
    }
    for f in doc.spouse_families(x) {
        for c in doc.children(&f) {
            out.push(((c, true), Link::Down, BLOOD));
        }
        if let Some(s) = doc.spouse_in(&f, x) {
            out.push(((s, false), Link::Spouse, MARRIAGE));
        }
    }
    out
}

/// The shortest lineages from `reference` to `subject`, most useful first.
/// Empty when they aren't connected at all.
pub fn find(doc: &Document, reference: &str, subject: &str) -> Vec<Kinship> {
    if reference == subject {
        return vec![Kinship { people: vec![reference.to_string()], links: Vec::new() }];
    }
    let start: State = (reference.to_string(), false);
    let mut dist: HashMap<State, u32> = HashMap::from([(start.clone(), 0)]);
    let mut preds: HashMap<State, Vec<(State, Link)>> = HashMap::new();
    let mut heap = BinaryHeap::from([Reverse((0u32, start.0.clone(), false))]);
    let mut best: Option<u32> = None;
    while let Some(Reverse((d, x, desc))) = heap.pop() {
        if best.is_some_and(|b| d > b) {
            break;
        }
        let state = (x, desc);
        if dist.get(&state).is_some_and(|&known| d > known) {
            continue;
        }
        if state.0 == subject {
            best = Some(d);
            continue;
        }
        for (next, link, cost) in steps(doc, &state) {
            let nd = d + cost;
            match dist.get(&next) {
                Some(&known) if nd > known => {}
                Some(&known) if nd == known => preds.entry(next).or_default().push((state.clone(), link)),
                _ => {
                    dist.insert(next.clone(), nd);
                    preds.insert(next.clone(), vec![(state.clone(), link)]);
                    heap.push(Reverse((nd, next.0, next.1)));
                }
            }
        }
    }
    let Some(best) = best else { return Vec::new() };

    // Every equally short lineage, walked back from the subject.
    let mut raw = Vec::new();
    for end in [(subject.to_string(), false), (subject.to_string(), true)] {
        if dist.get(&end) == Some(&best) {
            let mut people = vec![end.0.clone()];
            let mut links = Vec::new();
            collect(&preds, &start, &end, &mut people, &mut links, &mut raw);
        }
    }

    // Lineages through either parent of the same couple are one lineage.
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for k in raw {
        if seen.insert(identity(doc, &k)) {
            out.push(k);
            if out.len() >= MAX_PATHS {
                break;
            }
        }
    }
    out
}

fn collect(
    preds: &HashMap<State, Vec<(State, Link)>>,
    start: &State,
    at: &State,
    people: &mut Vec<String>,
    links: &mut Vec<Link>,
    out: &mut Vec<Kinship>,
) {
    if out.len() >= MAX_RAW_PATHS {
        return;
    }
    if at == start {
        out.push(Kinship {
            people: people.iter().rev().cloned().collect(),
            links: links.iter().rev().copied().collect(),
        });
        return;
    }
    for (prev, link) in preds.get(at).into_iter().flatten() {
        if people.contains(&prev.0) && prev != start {
            continue;
        }
        people.push(prev.0.clone());
        links.push(*link);
        collect(preds, start, prev, people, links, out);
        people.pop();
        links.pop();
    }
}

/// The lineage with each common-ancestor couple standing in for whichever
/// of the two the path happened to go through.
fn identity(doc: &Document, k: &Kinship) -> Vec<String> {
    (0..k.people.len())
        .map(|i| match shared_family_at(doc, k, i) {
            Some(f) => format!("family {f}"),
            None => k.people[i].clone(),
        })
        .collect()
}

/// Where the lineage turns from climbing to descending at `people[i]`,
/// the birth family both neighbours share (a full, not half, relation).
pub fn shared_family_at(doc: &Document, k: &Kinship, i: usize) -> Option<String> {
    if i == 0 || i + 1 >= k.people.len() || k.links[i - 1] != Link::Up || k.links[i] != Link::Down {
        return None;
    }
    let a: HashSet<String> = doc.parent_families(&k.people[i - 1]).into_iter().collect();
    doc.parent_families(&k.people[i + 1]).into_iter().find(|f| a.contains(f))
}

// ---- naming ------------------------------------------------------------------------

fn sexed(sex: Sex, male: &str, female: &str, neutral: &str) -> String {
    match sex {
        Sex::Male => male,
        Sex::Female => female,
        Sex::Unknown => neutral,
    }
    .to_string()
}

fn ordinal(n: usize) -> String {
    let words = ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];
    words.get(n.wrapping_sub(1)).map(|w| w.to_string()).unwrap_or_else(|| format!("{n}th"))
}

fn times(n: usize) -> String {
    match n {
        1 => "once".into(),
        2 => "twice".into(),
        _ => format!("{n} times"),
    }
}

fn greats(n: usize) -> String {
    match n {
        0 => String::new(),
        1 => "great-".into(),
        2 => "great-great-".into(),
        _ => format!("{n}× great-"),
    }
}

/// The name for a blood relative `up` generations above the common
/// ancestor on the reference side and `down` below it on the subject side.
pub fn blood_term(up: usize, down: usize, sex: Sex, half: bool) -> String {
    let half = if half { "half-" } else { "" };
    match (up, down) {
        (0, 0) => "self".into(),
        (0, 1) => sexed(sex, "son", "daughter", "child"),
        (0, d) => greats(d - 2) + &sexed(sex, "grandson", "granddaughter", "grandchild"),
        (1, 0) => sexed(sex, "father", "mother", "parent"),
        (u, 0) => greats(u - 2) + &sexed(sex, "grandfather", "grandmother", "grandparent"),
        (1, 1) => format!("{half}{}", sexed(sex, "brother", "sister", "sibling")),
        (1, 2) => format!("{half}{}", sexed(sex, "nephew", "niece", "nephew or niece")),
        (1, d) => format!("{half}{}grand{}", greats(d - 3), sexed(sex, "nephew", "niece", "nephew or niece")),
        (u, 1) => format!("{half}{}{}", greats(u - 2), sexed(sex, "uncle", "aunt", "uncle or aunt")),
        (u, d) => {
            let removed = u.abs_diff(d);
            let base = format!("{half}{} cousin", ordinal(u.min(d) - 1));
            if removed == 0 { base } else { format!("{base} {} removed", times(removed)) }
        }
    }
}

fn sex_of(doc: &Document, x: &str) -> Sex {
    doc.person(x).map(|p| p.sex).unwrap_or_default()
}

/// How the subject relates to the reference person, as a chain of terms
/// read outward from the reference: `["first cousin", "wife"]` means "the
/// wife of the reference's first cousin". Most relationships are one term.
pub fn describe(doc: &Document, k: &Kinship) -> Vec<String> {
    let end = k.people.last().map(String::as_str).unwrap_or("");
    let sex = sex_of(doc, end);
    use Link::*;
    // Common relationships by marriage have names of their own.
    let named = match k.links.as_slice() {
        [Spouse] => Some(sexed(sex, "husband", "wife", "spouse")),
        [Spouse, Up] => Some(sexed(sex, "father-in-law", "mother-in-law", "parent-in-law")),
        [Spouse, Up, Up] => Some(sexed(sex, "grandfather-in-law", "grandmother-in-law", "grandparent-in-law")),
        [Down, Spouse] => Some(sexed(sex, "son-in-law", "daughter-in-law", "child-in-law")),
        [Down, Down, Spouse] => Some(sexed(sex, "grandson-in-law", "granddaughter-in-law", "grandchild-in-law")),
        [Up, Down, Spouse] | [Spouse, Up, Down] => Some(sexed(sex, "brother-in-law", "sister-in-law", "sibling-in-law")),
        [Up, Spouse] => Some(sexed(sex, "stepfather", "stepmother", "step-parent")),
        [Spouse, Down] => Some(sexed(sex, "stepson", "stepdaughter", "stepchild")),
        [Up, Spouse, Down] => Some(sexed(sex, "stepbrother", "stepsister", "step-sibling")),
        _ => None,
    };
    if let Some(n) = named {
        return vec![n];
    }

    // Otherwise name each blood stretch and each marriage in turn.
    let mut chain = Vec::new();
    let mut i = 0;
    while i < k.links.len() {
        if k.links[i] == Spouse {
            chain.push(sexed(sex_of(doc, &k.people[i + 1]), "husband", "wife", "spouse"));
            i += 1;
            continue;
        }
        let start = i;
        let ups = k.links[i..].iter().take_while(|l| **l == Up).count();
        let downs = k.links[i + ups..].iter().take_while(|l| **l == Down).count();
        i += ups + downs;
        let half = ups > 0 && downs > 0 && shared_family_at(doc, k, start + ups).is_none();
        chain.push(blood_term(ups, downs, sex_of(doc, &k.people[i]), half));
    }
    if chain.is_empty() {
        chain.push("self".into());
    }
    chain
}

/// "Linda Ferris is Arthur Hartwell's daughter-in-law."
pub fn sentence(chain: &[String], subject: &str, reference: &str) -> String {
    match chain {
        [only] if only == "self" => format!("{subject} is the same person."),
        [only] => format!("{subject} is {reference}'s {only}."),
        [first, rest @ ..] => {
            let outer: Vec<String> = rest.iter().rev().map(|t| format!("the {t} of")).collect();
            format!("{subject} is {} {reference}'s {first}.", outer.join(" "))
        }
        [] => String::new(),
    }
}

/// A short label: "first cousin", or "wife of first cousin".
pub fn label(chain: &[String]) -> String {
    chain.iter().rev().cloned().collect::<Vec<_>>().join(" of ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Document {
        Document::from_bytes(include_bytes!("sample.ged")).0
    }

    fn rel(doc: &Document, reference: &str, subject: &str) -> String {
        let k = find(doc, reference, subject);
        assert!(!k.is_empty(), "{reference} → {subject} not connected");
        label(&describe(doc, &k[0]))
    }

    #[test]
    fn terms() {
        assert_eq!(blood_term(2, 2, Sex::Male, false), "first cousin");
        assert_eq!(blood_term(3, 2, Sex::Female, false), "first cousin once removed");
        assert_eq!(blood_term(4, 3, Sex::Unknown, false), "second cousin once removed");
        assert_eq!(blood_term(5, 0, Sex::Male, false), "3× great-grandfather");
        assert_eq!(blood_term(1, 1, Sex::Female, true), "half-sister");
        assert_eq!(blood_term(3, 1, Sex::Male, false), "great-uncle");
    }

    #[test]
    fn sample_family() {
        let doc = sample();
        // I1 Thomas; I3 William (son); I9 Arthur (grandson); I12 Robert;
        // I15 Michael; I10 Dorothy; I13 Joan; I4 Eliza; I14 Linda (Robert's wife);
        // I6 Clara (William's wife); I7 Samuel (Clara's father); I2 Margaret.
        assert_eq!(rel(&doc, "I1", "I15"), "great-great-grandson");
        assert_eq!(rel(&doc, "I15", "I1"), "great-great-grandfather");
        assert_eq!(rel(&doc, "I9", "I10"), "sister");
        assert_eq!(rel(&doc, "I10", "I12"), "nephew");
        assert_eq!(rel(&doc, "I12", "I4"), "great-aunt");
        assert_eq!(rel(&doc, "I9", "I14"), "daughter-in-law");
        assert_eq!(rel(&doc, "I3", "I7"), "father-in-law");
        assert_eq!(rel(&doc, "I13", "I14"), "sister-in-law");
        assert_eq!(rel(&doc, "I1", "I2"), "wife", "spouses, not the parents of a shared child");
        // Blood always wins over marriage when both connect.
        let k = &find(&doc, "I12", "I10")[0];
        assert!(!k.links.contains(&Link::Spouse));
        // Through either grandparent it's one lineage.
        assert_eq!(find(&doc, "I9", "I10").len(), 1);
    }

    #[test]
    fn sentences() {
        assert_eq!(sentence(&["first cousin".into()], "Ann", "Bob"), "Ann is Bob's first cousin.");
        assert_eq!(
            sentence(&["first cousin".into(), "wife".into()], "Ann", "Bob"),
            "Ann is the wife of Bob's first cousin."
        );
    }
}
