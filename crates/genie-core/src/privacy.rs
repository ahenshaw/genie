//! What a guest may see: the people who have died.
//!
//! Living people stay in the tree, so lines through them still connect, but
//! as "Private" placeholders: sex and family links only, no name, dates,
//! places, notes or documents. A family with a living partner keeps its
//! members and loses its events and notes. Anything then left unreachable (a
//! note, source or document only a living person pointed at) is dropped, as
//! is the submitter's address.
//!
//! Someone counts as deceased when a death, burial or cremation is recorded,
//! or when they were born at least `living_years` ago. Without a birth date of
//! their own, the dates of close relatives decide it, conservatively: a
//! parent was born at least [`PARENT_GAP`] years before their first child, a
//! child at most [`LAST_CHILD`] years after their parents, and partners within
//! [`SPOUSE_GAP`] years of each other. Someone with no dates to go on at all
//! counts as living.

use std::collections::{HashMap, HashSet};

use crate::gedcom::{Node, as_pointer};
use crate::model::year_of;

/// How long ago someone must have been born to be presumed dead.
pub const LIVING_YEARS: i32 = 100;
const PARENT_GAP: i32 = 15;
const LAST_CHILD: i32 = 50;
const SPOUSE_GAP: i32 = 20;

struct Person {
    dead: bool,
    /// The latest year they could have been born, as far as is known.
    born_by: Option<i32>,
    parents: Vec<String>,
    children: Vec<String>,
    partners: Vec<String>,
}

fn event_year(r: &Node, tags: &[&str]) -> Option<i32> {
    tags.iter().filter_map(|t| r.child(t)).find_map(|e| year_of(e.child_value("DATE")))
}

/// The people in `records` who are, or may be, alive in `this_year`.
pub fn living_people(records: &[Node], living_years: i32, this_year: i32) -> HashSet<String> {
    let fams: HashMap<&str, &Node> = records.iter().filter(|r| r.tag == "FAM").filter_map(|r| Some((r.xref.as_deref()?, r))).collect();
    let members = |fam: &str, tags: &[&str]| -> Vec<String> {
        fams.get(fam)
            .map(|f| f.children.iter().filter(|c| tags.contains(&c.tag.as_str())).filter_map(|c| c.pointer().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    let mut people: HashMap<String, Person> = HashMap::new();
    for r in records.iter().filter(|r| r.tag == "INDI") {
        let Some(x) = r.xref.clone() else { continue };
        let links = |tag: &str| r.children_with(tag).filter_map(|c| c.pointer().map(str::to_string)).collect::<Vec<_>>();
        let parents = links("FAMC").iter().flat_map(|f| members(f, &["HUSB", "WIFE"])).collect();
        let (mut children, mut partners) = (Vec::new(), Vec::new());
        for f in links("FAMS") {
            children.extend(members(&f, &["CHIL"]));
            partners.extend(members(&f, &["HUSB", "WIFE"]).into_iter().filter(|p| *p != x));
        }
        let dead = ["DEAT", "BURI", "CREM"].iter().any(|t| r.child(t).is_some());
        let born_by = event_year(r, &["BIRT", "CHR", "BAPM"]);
        people.insert(x, Person { dead, born_by, parents, children, partners });
    }

    // Estimate birth years from relatives, a few generations out.
    for _ in 0..4 {
        let known: HashMap<String, i32> = people.iter().filter_map(|(x, p)| Some((x.clone(), p.born_by?))).collect();
        let mut changed = false;
        for p in people.values_mut().filter(|p| p.born_by.is_none()) {
            let from_children = p.children.iter().filter_map(|c| known.get(c)).min().map(|y| y - PARENT_GAP);
            let from_parents = p.parents.iter().filter_map(|c| known.get(c)).max().map(|y| y + LAST_CHILD);
            let from_partners = p.partners.iter().filter_map(|c| known.get(c)).max().map(|y| y + SPOUSE_GAP);
            // The earliest of these limits is the tightest.
            if let Some(y) = [from_children, from_parents, from_partners].into_iter().flatten().min() {
                p.born_by = Some(y);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let cutoff = this_year - living_years;
    people.into_iter().filter(|(_, p)| !p.dead && p.born_by.is_none_or(|y| y > cutoff)).map(|(x, _)| x).collect()
}

/// The tree as a guest sees it.
pub fn guest_view(records: &[Node], living_years: i32, this_year: i32) -> Vec<Node> {
    let living = living_people(records, living_years, this_year);
    let keep_links = |r: &Node, tags: &[&str]| -> Vec<Node> {
        r.children.iter().filter(|c| tags.contains(&c.tag.as_str())).map(|c| Node::new(c.tag.clone(), c.value.clone())).collect()
    };
    let mut out: Vec<Node> = Vec::with_capacity(records.len());
    for r in records {
        let x = r.xref.as_deref();
        match r.tag.as_str() {
            "INDI" if x.is_some_and(|x| living.contains(x)) => {
                let mut p = Node::record(x.unwrap_or_default(), "INDI");
                p.children.push(Node::new("NAME", "Private"));
                p.children.extend(keep_links(r, &["SEX", "FAMC", "FAMS"]));
                out.push(p);
            }
            "FAM" if r.children.iter().any(|c| matches!(c.tag.as_str(), "HUSB" | "WIFE") && c.pointer().is_some_and(|p| living.contains(p))) => {
                let mut f = Node::record(x.unwrap_or_default(), "FAM");
                f.children = keep_links(r, &["HUSB", "WIFE", "CHIL"]);
                out.push(f);
            }
            "SUBM" | "SUBN" => {}
            "HEAD" => {
                let mut h = r.clone();
                h.children.retain(|c| !matches!(c.tag.as_str(), "SUBM" | "SUBN"));
                out.push(h);
            }
            _ => out.push(r.clone()),
        }
    }

    // Keep other records only while something visible still points at them.
    let by_xref: HashMap<&str, &Node> = out.iter().filter_map(|r| Some((r.xref.as_deref()?, r))).collect();
    let mut reachable: HashSet<String> = HashSet::new();
    let mut stack: Vec<&Node> = out.iter().filter(|r| matches!(r.tag.as_str(), "INDI" | "FAM")).collect();
    while let Some(n) = stack.pop() {
        each_pointer(n, &mut |p| {
            if reachable.insert(p.to_string())
                && let Some(target) = by_xref.get(p)
            {
                stack.push(target);
            }
        });
    }
    out.into_iter()
        .filter(|r| matches!(r.tag.as_str(), "HEAD" | "TRLR" | "INDI" | "FAM") || r.xref.as_deref().is_some_and(|x| reachable.contains(x)))
        .collect()
}

fn each_pointer(n: &Node, f: &mut impl FnMut(&str)) {
    for c in &n.children {
        if let Some(p) = as_pointer(&c.value) {
            f(p);
        }
        each_pointer(c, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gedcom::{parse, write};

    fn get<'a>(r: &'a [Node], x: &str) -> &'a Node {
        r.iter().find(|n| n.xref.as_deref() == Some(x)).expect(x)
    }

    #[test]
    fn nothing_about_the_living_reaches_a_guest() {
        let text = crate::gedcom::decode(include_bytes!("sample.ged")).text;
        let (records, _) = parse(&text);
        let living = living_people(&records, LIVING_YEARS, 2026);
        assert!(!living.is_empty());
        let view = guest_view(&records, LIVING_YEARS, 2026);
        let out = write(&view);
        let dead_text = write(&view.iter().filter(|r| r.tag == "INDI" && !living.contains(r.xref.as_deref().unwrap_or(""))).cloned().collect::<Vec<_>>());
        for x in &living {
            let r = get(&records, x);
            // Their name, and every date and place of theirs no deceased person shares.
            let mut secrets = vec![r.child_value("NAME").to_string()];
            for e in &r.children {
                for tag in ["DATE", "PLAC"] {
                    let v = e.child_value(tag);
                    if !v.is_empty() && !dead_text.contains(v) {
                        secrets.push(v.to_string());
                    }
                }
            }
            for s in secrets.iter().filter(|s| !s.is_empty()) {
                assert!(!out.contains(s.as_str()), "{s:?} of {x} leaked");
            }
            let placeholder = get(&view, x);
            assert_eq!(placeholder.child_value("NAME"), "Private");
            assert!(placeholder.children.iter().all(|c| matches!(c.tag.as_str(), "NAME" | "SEX" | "FAMC" | "FAMS")));
        }
    }

    #[test]
    fn the_dead_are_shown_in_full() {
        let text = crate::gedcom::decode(include_bytes!("sample.ged")).text;
        let (records, _) = parse(&text);
        let view = guest_view(&records, LIVING_YEARS, 2026);
        let living = living_people(&records, LIVING_YEARS, 2026);
        for r in records.iter().filter(|r| r.tag == "INDI" && !living.contains(r.xref.as_deref().unwrap())) {
            assert_eq!(get(&view, r.xref.as_deref().unwrap()), r);
        }
    }

    const FAMILY: &str = "0 HEAD\n1 SUBM @U1@\n0 @U1@ SUBM\n1 NAME Me\n1 ADDR 1 Main St\n\
0 @I1@ INDI\n1 NAME Old /Timer/\n1 FAMS @F1@\n\
0 @I2@ INDI\n1 NAME Kid /Timer/\n1 BIRT\n2 DATE 1850\n1 FAMC @F1@\n\
0 @I3@ INDI\n1 NAME No /Dates/\n1 NOTE @N1@\n1 OBJE @M1@\n\
0 @I4@ INDI\n1 NAME Young /One/\n1 BIRT\n2 DATE 1990\n2 PLAC Somewhere\n1 FAMS @F2@\n\
0 @I5@ INDI\n1 NAME Late /Spouse/\n1 DEAT Y\n1 FAMS @F2@\n\
0 @F1@ FAM\n1 HUSB @I1@\n1 CHIL @I2@\n\
0 @F2@ FAM\n1 HUSB @I5@\n1 WIFE @I4@\n1 MARR\n2 DATE 2015\n\
0 @N1@ NOTE Private thoughts\n0 @M1@ OBJE\n1 FILE media/a.jpg\n0 TRLR\n";

    #[test]
    fn relatives_dates_decide_for_the_undated() {
        let (records, _) = parse(FAMILY);
        let living = living_people(&records, LIVING_YEARS, 2026);
        // Old Timer's child was born in 1850, so he was born by 1835.
        assert!(!living.contains("I1") && !living.contains("I2"));
        // Nothing to go on: presumed living.
        assert!(living.contains("I3"));
        assert!(living.contains("I4") && !living.contains("I5"));
    }

    #[test]
    fn records_only_the_living_reached_are_dropped() {
        let (records, _) = parse(FAMILY);
        let view = guest_view(&records, LIVING_YEARS, 2026);
        let out = write(&view);
        for gone in ["Private thoughts", "media/a.jpg", "Main St", "Somewhere", "2015", "1990", "No /Dates/", "Young /One/"] {
            assert!(!out.contains(gone), "{gone} leaked");
        }
        // The family with a living partner keeps its members, not its events.
        let f2 = get(&view, "F2");
        assert_eq!(f2.children.iter().map(|c| c.tag.as_str()).collect::<Vec<_>>(), ["HUSB", "WIFE"]);
        assert!(view.iter().find(|r| r.tag == "HEAD").unwrap().child("SUBM").is_none());
        assert_eq!(get(&view, "I5").child_value("NAME"), "Late /Spouse/");
    }
}
