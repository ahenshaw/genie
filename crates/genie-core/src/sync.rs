//! Record-level changes between two versions of a tree, and a three-way
//! merge, so several people can edit one shared tree.
//!
//! A tree's records are keyed by xref. Two versions are compared record by
//! record: each is added, modified or deleted as a whole. That is fine-grained
//! enough to tell who changed which person, and coarse enough that merging
//! never has to understand a record's inside.
//!
//! Merging applies the changes one side made since the common base onto the
//! other side. Changes to different records combine; the same record changed
//! differently on both sides is a conflict, as is a link to a record the other
//! side deleted. Records added on both sides under the same new xref (both
//! editors took `@I124@`) are not a conflict: ours is renumbered.

use std::collections::{HashMap, HashSet};

use crate::gedcom::{Node, as_pointer, pointer_to};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Add,
    Modify,
    Delete,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::Add => "add",
            Action::Modify => "modify",
            Action::Delete => "delete",
        }
    }
}

/// One record that differs between two versions.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    pub xref: String,
    /// `INDI`, `FAM`, `SOUR`, …
    pub tag: String,
    pub action: Action,
}

fn keyed(records: &[Node]) -> HashMap<&str, &Node> {
    records.iter().filter_map(|r| Some((r.xref.as_deref()?, r))).collect()
}

/// The records that differ from `base` to `new`, in `new`'s order (deletions
/// last). Records without an xref (HEAD, TRLR) aren't compared.
pub fn diff(base: &[Node], new: &[Node]) -> Vec<Change> {
    let before = keyed(base);
    let after = keyed(new);
    let mut out = Vec::new();
    for r in new {
        let Some(x) = r.xref.as_deref() else { continue };
        let action = match before.get(x) {
            None => Action::Add,
            Some(old) if *old != r => Action::Modify,
            Some(_) => continue,
        };
        out.push(Change { xref: x.to_string(), tag: r.tag.clone(), action });
    }
    for r in base {
        if let Some(x) = r.xref.as_deref()
            && !after.contains_key(x)
        {
            out.push(Change { xref: x.to_string(), tag: r.tag.clone(), action: Action::Delete });
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides changed the record, differently.
    BothChanged,
    /// One side changed it, the other deleted it.
    ChangedAndDeleted,
    /// It now links to a record that no longer exists.
    DanglingLink { target: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub xref: String,
    pub tag: String,
    pub kind: ConflictKind,
}

/// Which side wins a conflicting record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

#[derive(Debug)]
pub struct Merged {
    pub records: Vec<Node>,
    /// Our records that were renumbered because the other side had added a
    /// different record under the same xref, as (old, new).
    pub renamed: Vec<(String, String)>,
}

/// Applies the changes `ours` made since `base` onto `theirs`. Conflicts are
/// returned unless `resolve` settles them.
pub fn merge(base: &[Node], ours: &[Node], theirs: &[Node]) -> Result<Merged, Vec<Conflict>> {
    merge_resolving(base, ours, theirs, |_| None)
}

/// [`merge`], with `resolve` choosing a side for records that conflict.
pub fn merge_resolving(base: &[Node], ours: &[Node], theirs: &[Node], resolve: impl Fn(&str) -> Option<Side>) -> Result<Merged, Vec<Conflict>> {
    let theirs_map = keyed(theirs);
    let their_changes: HashMap<String, Action> = diff(base, theirs).into_iter().map(|c| (c.xref, c.action)).collect();

    // Our records, with any xref both sides added renumbered.
    let mut ours: Vec<Node> = ours.to_vec();
    let mut renamed = Vec::new();
    let clashes: Vec<String> = diff(base, &ours)
        .into_iter()
        .filter(|c| c.action == Action::Add)
        .filter(|c| {
            let mine = ours.iter().find(|r| r.xref.as_deref() == Some(c.xref.as_str()));
            theirs_map.get(c.xref.as_str()).is_some_and(|t| Some(*t) != mine)
        })
        .map(|c| c.xref)
        .collect();
    if !clashes.is_empty() {
        let mut taken: HashSet<String> = base.iter().chain(ours.iter()).chain(theirs.iter()).filter_map(|r| r.xref.clone()).collect();
        for old in clashes {
            let new = fresh_xref(&old, &taken);
            taken.insert(new.clone());
            rename_xref(&mut ours, &old, &new);
            renamed.push((old, new));
        }
    }
    let ours_map = keyed(&ours);
    let our_changes = diff(base, &ours);

    let mut conflicts = Vec::new();
    // What to do to `theirs` for each of our changes.
    let mut replace: HashMap<String, Node> = HashMap::new();
    let mut delete: HashSet<String> = HashSet::new();
    let mut add: Vec<Node> = Vec::new();
    for c in &our_changes {
        let x = c.xref.as_str();
        let theirs_did = their_changes.get(x).copied();
        let mine = ours_map.get(x).copied();
        let same_result = match (mine, theirs_map.get(x)) {
            (Some(m), Some(t)) => m == *t,
            (None, None) => true,
            _ => false,
        };
        let conflict = match (c.action, theirs_did) {
            (_, None) => None,
            _ if same_result => None,
            (Action::Delete, Some(Action::Modify)) | (Action::Modify, Some(Action::Delete)) => Some(ConflictKind::ChangedAndDeleted),
            _ => Some(ConflictKind::BothChanged),
        };
        let take_ours = match conflict {
            None => theirs_did.is_none() || !same_result,
            Some(kind) => match resolve(x) {
                Some(Side::Ours) => true,
                Some(Side::Theirs) => false,
                None => {
                    conflicts.push(Conflict { xref: c.xref.clone(), tag: c.tag.clone(), kind });
                    continue;
                }
            },
        };
        if !take_ours {
            continue;
        }
        match mine {
            Some(m) if theirs_map.contains_key(x) => {
                replace.insert(c.xref.clone(), m.clone());
            }
            Some(m) => add.push(m.clone()),
            None => {
                delete.insert(c.xref.clone());
            }
        }
    }

    // Theirs, with our changes applied in place; new records go before TRLR.
    let mut records: Vec<Node> = theirs
        .iter()
        .filter(|r| r.xref.as_deref().is_none_or(|x| !delete.contains(x)))
        .map(|r| r.xref.as_deref().and_then(|x| replace.get(x)).cloned().unwrap_or_else(|| r.clone()))
        .collect();
    let at = records.iter().rposition(|r| r.tag == "TRLR" && r.xref.is_none()).unwrap_or(records.len());
    records.splice(at..at, add);

    // Links to records that are gone, and weren't already broken on either side.
    let broken_before: HashSet<(String, String)> = [&ours[..], theirs].iter().flat_map(|side| dangling(side)).collect();
    for (holder, target) in dangling(&records) {
        if broken_before.contains(&(holder.clone(), target.clone())) {
            continue;
        }
        let tag = records.iter().find(|r| r.xref.as_deref() == Some(holder.as_str())).map(|r| r.tag.clone()).unwrap_or_default();
        if resolve(&holder).is_none() {
            conflicts.push(Conflict { xref: holder, tag, kind: ConflictKind::DanglingLink { target } });
        }
    }
    if conflicts.is_empty() {
        Ok(Merged { records, renamed })
    } else {
        conflicts.dedup();
        Err(conflicts)
    }
}

/// Undoes a saved change: every record `after` changed relative to `before`
/// (or just `only`) is put back into `head` the way `before` had it, which
/// means removing records that change added. Returns the new records and
/// what was put back.
pub fn revert(head: &[Node], before: &[Node], after: &[Node], only: Option<&str>) -> (Vec<Node>, Vec<Change>) {
    let undone: Vec<Change> = diff(before, after).into_iter().filter(|c| only.is_none_or(|x| c.xref == x)).collect();
    let old = keyed(before);
    let mut records = head.to_vec();
    for c in &undone {
        let at = records.iter().position(|r| r.xref.as_deref() == Some(c.xref.as_str()));
        match (old.get(c.xref.as_str()), at) {
            (Some(was), Some(i)) => records[i] = (*was).clone(),
            (Some(was), None) => {
                let end = records.iter().rposition(|r| r.tag == "TRLR" && r.xref.is_none()).unwrap_or(records.len());
                records.insert(end, (*was).clone());
            }
            (None, Some(i)) => {
                records.remove(i);
            }
            (None, None) => {}
        }
    }
    (records, undone)
}

/// (record, target) for every pointer in `records` to an xref that isn't there.
pub fn dangling(records: &[Node]) -> Vec<(String, String)> {
    let existing: HashSet<&str> = records.iter().filter_map(|r| r.xref.as_deref()).collect();
    let mut out = Vec::new();
    for r in records {
        let Some(x) = r.xref.as_deref() else { continue };
        each_pointer(r, &mut |p| {
            if !existing.contains(p) {
                out.push((x.to_string(), p.to_string()));
            }
        });
    }
    out
}

fn each_pointer<'a>(n: &'a Node, f: &mut impl FnMut(&'a str)) {
    for c in &n.children {
        if let Some(p) = as_pointer(&c.value) {
            f(p);
        }
        each_pointer(c, f);
    }
}

/// An xref like `old` (same letters) that isn't in `taken`.
fn fresh_xref(old: &str, taken: &HashSet<String>) -> String {
    let prefix: String = old.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let highest = taken
        .iter()
        .filter_map(|x| x.strip_prefix(prefix.as_str())?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    (highest + 1..).map(|n| format!("{prefix}{n}")).find(|x| !taken.contains(x)).expect("an unused number exists")
}

/// Renames record `old` to `new`, and every pointer to it.
pub fn rename_xref(records: &mut [Node], old: &str, new: &str) {
    fn walk(n: &mut Node, old_ptr: &str, new_ptr: &str) {
        for c in &mut n.children {
            if c.value.trim() == old_ptr {
                c.value = new_ptr.to_string();
            }
            walk(c, old_ptr, new_ptr);
        }
    }
    let (old_ptr, new_ptr) = (pointer_to(old), pointer_to(new));
    for r in records.iter_mut() {
        if r.xref.as_deref() == Some(old) {
            r.xref = Some(new.to_string());
        }
        walk(r, &old_ptr, &new_ptr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gedcom::parse;

    const BASE: &str = "0 HEAD\n0 @I1@ INDI\n1 NAME Ann /Lee/\n1 FAMS @F1@\n0 @I2@ INDI\n1 NAME Bob /Lee/\n1 FAMS @F1@\n0 @F1@ FAM\n1 HUSB @I2@\n1 WIFE @I1@\n0 @I3@ INDI\n1 NAME Cy /Lee/\n0 TRLR\n";

    fn recs(s: &str) -> Vec<Node> {
        parse(s).0
    }

    fn edit(s: &str, from: &str, to: &str) -> Vec<Node> {
        assert!(s.contains(from), "{from}");
        recs(&s.replacen(from, to, 1))
    }

    fn get<'a>(r: &'a [Node], x: &str) -> Option<&'a Node> {
        r.iter().find(|n| n.xref.as_deref() == Some(x))
    }

    #[test]
    fn diff_names_each_record_once() {
        let new = BASE.replace("Ann /Lee/", "Anne /Lee/").replace("0 @I3@ INDI\n1 NAME Cy /Lee/\n", "").replace("0 TRLR", "0 @I4@ INDI\n1 NAME Dee /Lee/\n0 TRLR");
        let d = diff(&recs(BASE), &recs(&new));
        let got: Vec<(&str, Action)> = d.iter().map(|c| (c.xref.as_str(), c.action)).collect();
        assert_eq!(got, [("I1", Action::Modify), ("I4", Action::Add), ("I3", Action::Delete)]);
    }

    #[test]
    fn edits_to_different_people_combine() {
        let ours = edit(BASE, "Ann /Lee/", "Anne /Lee/");
        let theirs = edit(BASE, "Bob /Lee/", "Robert /Lee/");
        let m = merge(&recs(BASE), &ours, &theirs).unwrap();
        assert_eq!(get(&m.records, "I1").unwrap().child_value("NAME"), "Anne /Lee/");
        assert_eq!(get(&m.records, "I2").unwrap().child_value("NAME"), "Robert /Lee/");
        assert!(m.renamed.is_empty());
    }

    #[test]
    fn the_same_change_twice_is_not_a_conflict() {
        let both = edit(BASE, "Ann /Lee/", "Anne /Lee/");
        assert!(merge(&recs(BASE), &both, &both).is_ok());
    }

    #[test]
    fn the_same_person_changed_differently_conflicts_until_resolved() {
        let ours = edit(BASE, "Ann /Lee/", "Anne /Lee/");
        let theirs = edit(BASE, "Ann /Lee/", "Annie /Lee/");
        let err = merge(&recs(BASE), &ours, &theirs).unwrap_err();
        assert_eq!(err, [Conflict { xref: "I1".into(), tag: "INDI".into(), kind: ConflictKind::BothChanged }]);
        let mine = merge_resolving(&recs(BASE), &ours, &theirs, |_| Some(Side::Ours)).unwrap();
        assert_eq!(get(&mine.records, "I1").unwrap().child_value("NAME"), "Anne /Lee/");
        let yours = merge_resolving(&recs(BASE), &ours, &theirs, |_| Some(Side::Theirs)).unwrap();
        assert_eq!(get(&yours.records, "I1").unwrap().child_value("NAME"), "Annie /Lee/");
    }

    #[test]
    fn changed_on_one_side_deleted_on_the_other_conflicts() {
        let ours = edit(BASE, "Cy /Lee/", "Cyrus /Lee/");
        let theirs = edit(BASE, "0 @I3@ INDI\n1 NAME Cy /Lee/\n", "");
        let err = merge(&recs(BASE), &ours, &theirs).unwrap_err();
        assert_eq!(err[0].kind, ConflictKind::ChangedAndDeleted);
    }

    #[test]
    fn both_adding_the_same_new_xref_renumbers_ours() {
        let ours_text = BASE.replace("0 TRLR", "0 @I4@ INDI\n1 NAME Dee /Lee/\n1 FAMC @F1@\n0 TRLR").replace("1 WIFE @I1@", "1 WIFE @I1@\n1 CHIL @I4@");
        let theirs = edit(BASE, "0 TRLR", "0 @I4@ INDI\n1 NAME Eve /Kim/\n0 TRLR");
        let m = merge(&recs(BASE), &recs(&ours_text), &theirs).unwrap();
        assert_eq!(m.renamed, [("I4".to_string(), "I5".to_string())]);
        assert_eq!(get(&m.records, "I4").unwrap().child_value("NAME"), "Eve /Kim/");
        assert_eq!(get(&m.records, "I5").unwrap().child_value("NAME"), "Dee /Lee/");
        // Our family's link followed the renumbering.
        assert_eq!(get(&m.records, "F1").unwrap().child_value("CHIL"), "@I5@");
        assert_eq!(m.records.last().unwrap().tag, "TRLR");
    }

    #[test]
    fn linking_to_someone_the_other_side_deleted_conflicts() {
        let ours = edit(BASE, "1 WIFE @I1@", "1 WIFE @I1@\n1 CHIL @I3@");
        let theirs = edit(BASE, "0 @I3@ INDI\n1 NAME Cy /Lee/\n", "");
        let err = merge(&recs(BASE), &ours, &theirs).unwrap_err();
        assert_eq!(err, [Conflict { xref: "F1".into(), tag: "FAM".into(), kind: ConflictKind::DanglingLink { target: "I3".into() } }]);
    }

    #[test]
    fn reverting_puts_records_back_as_they_were() {
        let base = recs(BASE);
        // One save renames Ann and adds Dee; a later save renames Bob.
        let saved = edit(BASE, "Ann /Lee/", "Anne /Lee/");
        let saved = recs(&crate::gedcom::write(&saved).replace("0 TRLR", "0 @I4@ INDI\r\n1 NAME Dee /Lee/\r\n0 TRLR"));
        let head = recs(&crate::gedcom::write(&saved).replace("Bob /Lee/", "Robert /Lee/"));

        // Just Ann: back to "Ann", Dee and Robert untouched.
        let (r, undone) = revert(&head, &base, &saved, Some("I1"));
        assert_eq!(undone.len(), 1);
        assert_eq!(get(&r, "I1").unwrap().child_value("NAME"), "Ann /Lee/");
        assert!(get(&r, "I4").is_some());
        assert_eq!(get(&r, "I2").unwrap().child_value("NAME"), "Robert /Lee/");

        // The whole save: Dee goes too.
        let (r, undone) = revert(&head, &base, &saved, None);
        assert_eq!(undone.len(), 2);
        assert!(get(&r, "I4").is_none());
        assert_eq!(r.last().unwrap().tag, "TRLR");

        // Undoing a deletion brings the record back.
        let deleted = edit(BASE, "0 @I3@ INDI\n1 NAME Cy /Lee/\n", "");
        let (r, _) = revert(&deleted, &base, &deleted, None);
        assert_eq!(get(&r, "I3").unwrap().child_value("NAME"), "Cy /Lee/");
    }

    #[test]
    fn rename_rewrites_every_pointer() {
        let mut r = recs(BASE);
        rename_xref(&mut r, "I1", "I9");
        let text = crate::gedcom::write(&r);
        assert!(text.contains("@I9@ INDI") && text.contains("WIFE @I9@") && !text.contains("@I1@"));
    }
}
