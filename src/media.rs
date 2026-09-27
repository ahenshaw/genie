//! Documents and photos: GEDCOM multimedia (`OBJE`) records, where they're
//! linked from, and the files behind them.
//!
//! A document is an `OBJE` record holding a path to a file:
//!
//! ```text
//! 0 @M1@ OBJE
//! 1 FILE Hartwell media/census-1900.jpg
//! 2 FORM jpg
//! 3 TYPE census
//! 2 TITL 1900 census, Gloucester
//! 1 _DATE 1900
//! 1 NOTE …
//! ```
//!
//! It is linked with `OBJE @M1@` from a person, a family, one of their facts,
//! or a source citation. A person's profile photo carries `_PRIM Y` on its
//! link. Paths are kept relative to the `.ged` file, and attached files are
//! copied into a media folder beside it, so the tree and its documents move
//! together.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::gedcom::{Node, pointer_to};
use crate::model::{Document, FactKey, GENERAL_FACT, event_label, is_citable};

/// Kinds of document offered in the editor, as written to `TYPE`.
pub const KINDS: [(&str, &str); 9] = [
    ("photo", "Photo"),
    ("certificate", "Certificate"),
    ("census", "Census"),
    ("document", "Record or document"),
    ("newspaper", "Newspaper"),
    ("tombstone", "Headstone"),
    ("letter", "Letter"),
    ("map", "Map"),
    ("other", "Other"),
];

const IMAGE_FORMS: [&str; 8] = ["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaItem {
    pub xref: String,
    /// The path as written in the file.
    pub file: String,
    pub form: String,
    pub kind: String,
    pub title: String,
    pub date: String,
    pub note: String,
    /// Size in bytes, where the exporting program recorded it (`_SIZE`).
    pub size: Option<u64>,
}

impl MediaItem {
    /// False for records exported without their file (Ancestry does this).
    pub fn has_file(&self) -> bool {
        !self.file.trim().is_empty()
    }

    pub fn is_image(&self) -> bool {
        let ext = self.form.to_lowercase();
        let ext = if ext.is_empty() { extension(&self.file) } else { ext };
        IMAGE_FORMS.contains(&ext.as_str())
    }

    /// A title to show, falling back to the file name.
    pub fn display_title(&self) -> String {
        if !self.title.trim().is_empty() {
            return self.title.trim().to_string();
        }
        file_name(&self.file)
    }

    pub fn kind_label(&self) -> String {
        if let Some((_, l)) = KINDS.iter().find(|(k, _)| *k == self.kind) {
            return l.to_string();
        }
        // A type written by another program ("image"): capitalise it.
        let mut c = self.kind.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => "Document".into(),
        }
    }
}

/// Where a document is linked from.
#[derive(Clone, Debug, PartialEq)]
pub enum MediaPlace {
    /// The person, family or source as a whole.
    Whole,
    /// One of their facts.
    Fact(FactKey),
    /// The `n`-th citation on a fact (the general fact for the whole record).
    Citation(FactKey, usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct MediaUse {
    /// The `INDI`, `FAM` or `SOUR` record the link is in.
    pub owner: String,
    pub place: MediaPlace,
    /// Marked as the owner's profile photo.
    pub primary: bool,
}

fn extension(file: &str) -> String {
    Path::new(&file.replace('\\', "/")).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn file_name(file: &str) -> String {
    let f = file.replace('\\', "/");
    f.rsplit('/').next().unwrap_or(&f).to_string()
}

fn read_item(r: &Node) -> MediaItem {
    let file = r.child("FILE");
    // 5.5.1 nests FORM under FILE and TYPE under FORM; 5.5 had them flat.
    let form_node = file.and_then(|f| f.child("FORM")).or_else(|| r.child("FORM"));
    let title = file.map(|f| f.child_value("TITL")).filter(|t| !t.is_empty()).unwrap_or_else(|| r.child_value("TITL"));
    let kind = form_node
        .map(|f| f.child_value("TYPE").to_string())
        .filter(|t| !t.is_empty())
        .or_else(|| file.map(|f| f.child_value("TYPE").to_string()))
        .unwrap_or_default();
    MediaItem {
        xref: r.xref.clone().unwrap_or_default(),
        file: file.map(|f| f.value.clone()).unwrap_or_default(),
        form: form_node.map(|f| f.value.clone()).unwrap_or_default(),
        kind: kind.to_lowercase(),
        title: title.to_string(),
        date: r.child_value("_DATE").to_string(),
        size: form_node.and_then(|f| f.child_value("_SIZE").trim().parse().ok()),
        note: r.children_with("NOTE").find(|n| n.pointer().is_none()).map(|n| n.value.clone()).unwrap_or_default(),
    }
}

/// Visits every `OBJE` pointer in linkable positions of `r`.
fn each_link<'a>(r: &'a Node, mut f: impl FnMut(&'a Node, MediaPlace)) {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut general_cites = 0;
    for c in &r.children {
        match c.tag.as_str() {
            "OBJE" => f(c, MediaPlace::Whole),
            "SOUR" => {
                for o in c.children_with("OBJE") {
                    f(o, MediaPlace::Citation((GENERAL_FACT.to_string(), 0), general_cites));
                }
                general_cites += 1;
            }
            t if is_citable(t) => {
                let n = seen.entry(c.tag.as_str()).or_default();
                let fact = (c.tag.clone(), *n);
                *n += 1;
                let mut cites = 0;
                for g in &c.children {
                    match g.tag.as_str() {
                        "OBJE" => f(g, MediaPlace::Fact(fact.clone())),
                        "SOUR" => {
                            for o in g.children_with("OBJE") {
                                f(o, MediaPlace::Citation(fact.clone(), cites));
                            }
                            cites += 1;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// The node a link in `place` hangs from, created for a missing fact.
fn place_node<'a>(r: &'a mut Node, place: &MediaPlace, create: bool) -> Option<&'a mut Node> {
    let fact_index = |r: &Node, (tag, nth): &FactKey| r.children.iter().enumerate().filter(|(_, c)| &c.tag == tag).map(|(i, _)| i).nth(*nth);
    match place {
        MediaPlace::Whole => Some(r),
        MediaPlace::Fact(key) => {
            let i = match fact_index(r, key) {
                Some(i) => i,
                None if create && event_label(&key.0).is_some() => {
                    r.children.push(Node::new(key.0.clone(), "Y"));
                    r.children.len() - 1
                }
                None => return None,
            };
            Some(&mut r.children[i])
        }
        MediaPlace::Citation(key, n) => {
            let holder = if key.0.is_empty() {
                r
            } else {
                let i = fact_index(r, key)?;
                &mut r.children[i]
            };
            holder.children.iter_mut().filter(|c| c.tag == "SOUR").nth(*n)
        }
    }
}

impl Document {
    pub fn media_items(&self) -> Vec<MediaItem> {
        let mut v: Vec<MediaItem> = self.records().iter().filter(|r| r.tag == "OBJE" && r.xref.is_some()).map(read_item).collect();
        v.sort_by_key(|m| m.display_title().to_lowercase());
        v
    }

    pub fn media_item(&self, xref: &str) -> Option<MediaItem> {
        self.record(xref).filter(|r| r.tag == "OBJE").map(read_item)
    }

    /// Where each document is linked from, keyed by its xref.
    pub fn media_index(&self) -> HashMap<String, Vec<MediaUse>> {
        let mut out: HashMap<String, Vec<MediaUse>> = HashMap::new();
        for r in self.records() {
            if !matches!(r.tag.as_str(), "INDI" | "FAM" | "SOUR") {
                continue;
            }
            let Some(owner) = r.xref.clone() else { continue };
            each_link(r, |n, place| {
                if let Some(m) = n.pointer() {
                    let primary = n.child_value("_PRIM").eq_ignore_ascii_case("y");
                    out.entry(m.to_string()).or_default().push(MediaUse { owner: owner.clone(), place, primary });
                }
            });
        }
        out
    }

    /// `OBJE` links holding their own file instead of pointing at a record
    /// (an older style). Kept on save but not shown.
    pub fn embedded_media_count(&self) -> usize {
        let mut n = 0;
        for r in self.records().iter().filter(|r| matches!(r.tag.as_str(), "INDI" | "FAM" | "SOUR")) {
            each_link(r, |o, _| {
                if o.pointer().is_none() && o.child("FILE").is_some() {
                    n += 1;
                }
            });
        }
        n
    }

    /// The document marked as the person's profile photo, file or not.
    pub fn primary_media(&self, person: &str) -> Option<String> {
        let r = self.record(person)?;
        let link = r.children_with("OBJE").find(|o| o.child_value("_PRIM").eq_ignore_ascii_case("y"))?;
        link.pointer().map(str::to_string)
    }

    /// Image files attached to the person as a whole, profile photo first.
    pub(crate) fn photo_files(&self, person: &str) -> Vec<String> {
        let Some(r) = self.record(person) else { return Vec::new() };
        let mut links: Vec<&Node> = r.children_with("OBJE").collect();
        links.sort_by_key(|o| !o.child_value("_PRIM").eq_ignore_ascii_case("y"));
        let mut out = Vec::new();
        for link in links {
            let Some(item) = link.pointer().and_then(|m| self.record(m)).map(read_item) else { continue };
            if item.has_file() && item.is_image() && !out.contains(&item.file) {
                out.push(item.file);
            }
        }
        out
    }

    /// A label for what a link points from: "Birth", "Citation: 1900 Census".
    pub fn place_label(&self, owner: &str, place: &MediaPlace) -> String {
        let fact_label = |key: &FactKey| {
            if key.0.is_empty() {
                return String::new();
            }
            self.record(owner)
                .and_then(|r| r.children.iter().filter(|c| c.tag == key.0).nth(key.1))
                .map(|c| {
                    let label = event_label(&c.tag).unwrap_or(if c.tag == "NAME" { "Name" } else { &c.tag }).to_string();
                    let date = crate::model::pretty_date(c.child_value("DATE"));
                    if date.is_empty() { label } else { format!("{label} · {date}") }
                })
                .unwrap_or_else(|| key.0.clone())
        };
        match place {
            MediaPlace::Whole => String::new(),
            MediaPlace::Fact(k) => fact_label(k),
            MediaPlace::Citation(k, n) => {
                let title = self
                    .record(owner)
                    .and_then(|r| {
                        let holder = if k.0.is_empty() { Some(r) } else { r.children.iter().filter(|c| c.tag == k.0).nth(k.1) };
                        holder?.children.iter().filter(|c| c.tag == "SOUR").nth(*n).cloned()
                    })
                    .map(|c| match c.pointer() {
                        Some(s) => self.sources().into_iter().find(|(x, _)| x == s).map(|(_, t)| t).unwrap_or_default(),
                        None => c.value.clone(),
                    })
                    .unwrap_or_default();
                let on = fact_label(k);
                if on.is_empty() { format!("Citation: {title}") } else { format!("{on} · citation: {title}") }
            }
        }
    }

    /// "Thomas Hartwell", "Hartwell & Doyle family" or a source's title.
    pub fn owner_label(&self, owner: &str) -> String {
        if let Some(p) = self.person(owner) {
            return p.display.clone();
        }
        match self.record(owner).map(|r| r.tag.as_str()) {
            Some("FAM") => {
                let names: Vec<String> = self.husband(owner).into_iter().chain(self.wife(owner)).filter_map(|x| self.person(&x).map(|p| p.display.clone())).collect();
                if names.is_empty() { "A family".into() } else { names.join(" & ") }
            }
            Some("SOUR") => self.sources().into_iter().find(|(x, _)| x == owner).map(|(_, t)| t).unwrap_or_default(),
            _ => owner.to_string(),
        }
    }

    // ---- editing (inside `mutate`) --------------------------------------------------

    pub fn create_media(&mut self, file: &str, kind: &str, title: &str) -> String {
        let xref = self.next_xref("M");
        let mut rec = Node::record(&xref, "OBJE");
        let mut f = Node::new("FILE", file);
        let form = extension(file);
        if !form.is_empty() {
            let mut form_node = Node::new("FORM", if form == "jpeg" { "jpg".to_string() } else { form });
            if !kind.is_empty() {
                form_node.children.push(Node::new("TYPE", kind));
            }
            f.children.push(form_node);
        }
        if !title.is_empty() {
            f.children.push(Node::new("TITL", title));
        }
        rec.children.push(f);
        self.push_record(rec)
    }

    /// Writes the editable fields back, leaving anything else in place.
    pub fn update_media(&mut self, item: &MediaItem) {
        let Some(r) = self.record_mut(&item.xref) else { return };
        let file = r.ensure_child("FILE");
        if file.value != item.file {
            file.value = item.file.clone();
        }
        let has_form = file.child("FORM").is_some() || !item.kind.is_empty();
        if has_form {
            let form = file.ensure_child("FORM");
            if form.value.is_empty() {
                form.value = extension(&item.file);
            }
            form.set_child_value("TYPE", &item.kind);
        }
        // Title lives under FILE in 5.5.1; keep one written by an older program where it was.
        if r.child("TITL").is_some() {
            r.set_child_value("TITL", &item.title);
        } else {
            r.ensure_child("FILE").set_child_value("TITL", &item.title);
        }
        r.set_child_value("_DATE", &crate::model::normalize_date(&item.date));
        let note = item.note.trim_end();
        match r.children.iter().position(|n| n.tag == "NOTE" && n.pointer().is_none()) {
            Some(i) if note.is_empty() => {
                r.children.remove(i);
            }
            Some(i) => r.children[i].value = note.to_string(),
            None if !note.is_empty() => r.children.push(Node::new("NOTE", note)),
            None => {}
        }
    }

    pub fn attach_media(&mut self, media: &str, owner: &str, place: &MediaPlace) -> bool {
        let p = pointer_to(media);
        let Some(r) = self.record_mut(owner) else { return false };
        let Some(holder) = place_node(r, place, true) else { return false };
        if holder.children.iter().any(|c| c.tag == "OBJE" && c.value == p) {
            return true;
        }
        let pos = holder.children.iter().rposition(|c| c.tag == "OBJE").map(|i| i + 1).unwrap_or_else(|| {
            // Before family links and change dates, after the facts.
            holder.children.iter().position(|c| matches!(c.tag.as_str(), "FAMC" | "FAMS" | "CHAN")).unwrap_or(holder.children.len())
        });
        holder.children.insert(pos, Node::new("OBJE", p));
        true
    }

    pub fn detach_media(&mut self, media: &str, owner: &str, place: &MediaPlace) {
        let p = pointer_to(media);
        if let Some(holder) = self.record_mut(owner).and_then(|r| place_node(r, place, false)) {
            holder.children.retain(|c| !(c.tag == "OBJE" && c.value == p));
        }
    }

    pub fn delete_media(&mut self, media: &str) {
        self.delete_record(media);
    }

    /// Makes `media` the person's profile photo, attaching it if needed.
    pub fn set_primary_photo(&mut self, person: &str, media: &str) {
        self.attach_media(media, person, &MediaPlace::Whole);
        let p = pointer_to(media);
        if let Some(r) = self.record_mut(person) {
            for o in r.children.iter_mut().filter(|c| c.tag == "OBJE") {
                o.children.retain(|c| c.tag != "_PRIM");
                if o.value == p {
                    o.children.push(Node::new("_PRIM", "Y"));
                }
            }
        }
    }

    pub fn clear_primary_photo(&mut self, person: &str) {
        if let Some(r) = self.record_mut(person) {
            for o in r.children.iter_mut().filter(|c| c.tag == "OBJE") {
                o.children.retain(|c| c.tag != "_PRIM");
            }
        }
    }

    /// Points a document at a different file.
    pub fn relink_media(&mut self, media: &str, file: &str) {
        if let Some(r) = self.record_mut(media) {
            r.ensure_child("FILE").value = file.to_string();
        }
    }

    /// After the tree moves from `old_dir` to `new_dir` (Save as), keeps
    /// relative paths pointing at the same files.
    pub fn rebase_media(&mut self, old_dir: &Path, new_dir: &Path) -> bool {
        if old_dir == new_dir {
            return false;
        }
        let mut changed = false;
        let xrefs: Vec<String> = self.media_items().into_iter().map(|m| m.xref).collect();
        for x in xrefs {
            let Some(item) = self.media_item(&x) else { continue };
            if item.file.is_empty() || is_absolute(&item.file) {
                continue;
            }
            let target = old_dir.join(&item.file);
            let file = pathdiff::diff_paths(&target, new_dir).map(|p| path_string(&p)).unwrap_or_else(|| path_string(&target));
            if file != item.file {
                self.relink_media(&x, &file);
                changed = true;
            }
        }
        if changed {
            self.rebuild();
        }
        changed
    }
}

// ---- files -------------------------------------------------------------------------------

fn is_absolute(file: &str) -> bool {
    let f = file.replace('\\', "/");
    Path::new(&f).is_absolute() || f.chars().nth(1) == Some(':') || f.starts_with("//")
}

/// GEDCOM paths use `/`, whatever the platform.
pub fn path_string(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The file a GEDCOM path refers to, relative to the tree's folder.
pub fn resolve(tree: Option<&Path>, file: &str) -> Option<PathBuf> {
    let f = file.trim().trim_start_matches("file://");
    if f.is_empty() {
        return None;
    }
    if is_absolute(f) {
        return Some(PathBuf::from(f));
    }
    Some(tree?.parent()?.join(f))
}

/// The folder attached files are copied into: `<tree name> media`.
pub fn media_folder(tree: &Path) -> Option<PathBuf> {
    let stem = tree.file_stem()?.to_string_lossy().into_owned();
    Some(tree.parent()?.join(format!("{stem} media")))
}

/// Copies `src` into the tree's media folder (unless it's already there)
/// and returns the path to record, relative to the tree.
pub fn import_file(tree: &Path, src: &Path) -> std::io::Result<String> {
    let base = tree.parent().ok_or_else(|| std::io::Error::other("the tree has no folder"))?;
    let folder = media_folder(tree).ok_or_else(|| std::io::Error::other("no media folder"))?;
    std::fs::create_dir_all(&folder)?;
    let src = src.canonicalize()?;
    if src.starts_with(folder.canonicalize()?) {
        let rel = pathdiff::diff_paths(&src, base.canonicalize()?).unwrap_or(src.clone());
        return Ok(path_string(&rel));
    }
    let name = src.file_name().ok_or_else(|| std::io::Error::other("not a file"))?.to_string_lossy().into_owned();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let mut dest = folder.join(&name);
    let mut n = 2;
    while dest.exists() {
        if std::fs::read(&dest).ok() == std::fs::read(&src).ok() {
            break; // The same file was attached before; reuse it.
        }
        dest = folder.join(format!("{stem} ({n}){ext}"));
        n += 1;
    }
    if !dest.exists() {
        std::fs::copy(&src, &dest)?;
    }
    let rel = pathdiff::diff_paths(&dest, base).unwrap_or(dest);
    Ok(path_string(&rel))
}

/// Copies `files` into the tree's media folder and adds each as a document,
/// attached to `person` when given; a person's first photo becomes their
/// profile photo. One undo step. Returns the new documents and any files
/// that couldn't be copied.
pub fn add_files(doc: &mut Document, tree: &Path, files: &[PathBuf], person: Option<&str>) -> (Vec<String>, Vec<(PathBuf, String)>) {
    let mut imported = Vec::new();
    let mut failed = Vec::new();
    for f in files {
        match import_file(tree, f) {
            Ok(rel) => imported.push(rel),
            Err(e) => failed.push((f.clone(), e.to_string())),
        }
    }
    if imported.is_empty() {
        return (Vec::new(), failed);
    }
    let created = doc.mutate(|d| {
        imported
            .iter()
            .map(|rel| {
                let kind = guess_kind(rel);
                let title = Path::new(rel).file_stem().map(|s| s.to_string_lossy().replace(['_', '-'], " ")).unwrap_or_default();
                let m = d.create_media(rel, kind, title.trim());
                if let Some(p) = person {
                    d.attach_media(&m, p, &MediaPlace::Whole);
                    // Take over as profile photo unless the current one can be shown
                    // (an imported tree may mark one whose file never came with it).
                    let current_shows = d
                        .primary_media(p)
                        .and_then(|cur| d.media_item(&cur))
                        .and_then(|it| resolve(Some(tree), &it.file))
                        .is_some_and(|f| f.is_file());
                    if kind == "photo" && !current_shows {
                        d.set_primary_photo(p, &m);
                    }
                }
                m
            })
            .collect()
    });
    (created, failed)
}

/// A sensible kind for a new file, from its name.
pub fn guess_kind(file: &str) -> &'static str {
    let lower = file.to_lowercase();
    if lower.contains("census") {
        "census"
    } else if ["certificate", "cert", "birth", "death", "marriage"].iter().any(|w| lower.contains(w)) {
        "certificate"
    } else if ["grave", "headstone", "tomb"].iter().any(|w| lower.contains(w)) {
        "tombstone"
    } else if IMAGE_FORMS.contains(&extension(file).as_str()) {
        "photo"
    } else {
        "document"
    }
}

/// A file found for a document.
#[derive(Debug, PartialEq)]
pub struct FoundFile {
    pub media: String,
    pub path: PathBuf,
    /// The record had no file at all (rather than one that moved), so the
    /// found file should be copied into the media folder.
    pub was_empty: bool,
}

/// Documents with nothing to show: a file that isn't where the tree says,
/// or no file at all.
pub fn needs_file(tree: Option<&Path>, m: &MediaItem) -> bool {
    !m.has_file() || resolve(tree, &m.file).is_none_or(|p| !p.exists())
}

/// Looks under `folder` for files for documents that need one: by file
/// name for files that moved, and by original name (the title) for records
/// exported without their file. A recorded size must match, which tells
/// apart the many "Image.jpg"s a download folder tends to hold.
pub fn find_files(tree: Option<&Path>, items: &[MediaItem], folder: &Path) -> Vec<FoundFile> {
    let wanted: Vec<&MediaItem> = items.iter().filter(|m| needs_file(tree, m)).collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    let mut by_name: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut stack = vec![folder.to_path_buf()];
    let mut visited = 0;
    while let Some(dir) = stack.pop() {
        visited += 1;
        if visited > 20_000 {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(n) = p.file_name() {
                by_name.entry(n.to_string_lossy().to_lowercase()).or_default().push(p);
            }
        }
    }
    wanted
        .into_iter()
        .filter_map(|m| {
            let name = if m.has_file() { file_name(&m.file) } else { m.title.trim().to_string() };
            let candidates = by_name.get(&name.to_lowercase())?;
            let found = match m.size {
                Some(size) => candidates.iter().find(|p| std::fs::metadata(p).is_ok_and(|md| md.len() == size))?,
                None if candidates.len() == 1 => &candidates[0],
                None => return None,
            };
            Some(FoundFile { media: m.xref.clone(), path: found.clone(), was_empty: !m.has_file() })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PersonForm;

    #[test]
    fn link_places_primary_and_delete() {
        let (mut doc, _) = Document::from_bytes(include_bytes!("sample.ged"));
        let m = doc.mutate(|d| d.create_media("Hartwell media/thomas.jpg", "photo", "Thomas at the yard"));
        let cert = doc.mutate(|d| d.create_media("Hartwell media/birth.pdf", "certificate", ""));
        doc.mutate(|d| {
            assert!(d.attach_media(&m, "I1", &MediaPlace::Whole));
            assert!(d.attach_media(&cert, "I1", &MediaPlace::Fact(("BIRT".into(), 0))));
            // Thomas's only citation is on the person as a whole.
            assert!(d.attach_media(&cert, "I1", &MediaPlace::Citation((GENERAL_FACT.into(), 0), 0)));
            d.set_primary_photo("I1", &m);
        });
        let idx = doc.media_index();
        assert_eq!(idx[&m].len(), 1);
        assert!(idx[&m][0].primary);
        assert_eq!(idx[&cert].len(), 2);
        assert_eq!(doc.person("I1").unwrap().photos.first().map(String::as_str), Some("Hartwell media/thomas.jpg"));
        assert_eq!(doc.place_label("I1", &MediaPlace::Fact(("BIRT".into(), 0))), "Birth · 14 Mar 1842");
        let text = doc.to_gedcom();
        assert!(text.contains("0 @M1@ OBJE\r\n1 FILE Hartwell media/thomas.jpg\r\n2 FORM jpg\r\n3 TYPE photo\r\n2 TITL Thomas at the yard\r\n"), "{text}");
        assert!(text.contains("1 OBJE @M1@\r\n2 _PRIM Y\r\n"));

        // Editing the person keeps their links.
        let form: PersonForm = doc.form_for("I1");
        doc.mutate(|d| d.apply_form("I1", &form));
        assert_eq!(doc.media_index()[&cert].len(), 2);

        // Round trip, then delete: every pointer goes with the record.
        let (mut doc, _) = Document::from_bytes(doc.to_gedcom().as_bytes());
        assert_eq!(doc.media_items().len(), 2);
        doc.mutate(|d| d.delete_media(&cert));
        assert!(!doc.to_gedcom().contains(&format!("@{cert}@")));
    }

    #[test]
    fn adding_files_attaches_and_picks_a_profile_photo() {
        let dir = std::env::temp_dir().join(format!("genie-add-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).unwrap();
        let tree = dir.join("Hartwell.ged");
        std::fs::write(&tree, "0 HEAD\n0 TRLR\n").unwrap();
        for f in ["eliza_1890.jpg", "second-photo.png", "death certificate.pdf"] {
            std::fs::write(dir.join("in").join(f), f.as_bytes()).unwrap();
        }
        let (mut doc, _) = Document::from_bytes(include_bytes!("sample.ged"));
        let files: Vec<PathBuf> = ["eliza_1890.jpg", "second-photo.png", "death certificate.pdf"].iter().map(|f| dir.join("in").join(f)).collect();
        let (created, failed) = add_files(&mut doc, &tree, &files, Some("I4"));
        assert!(failed.is_empty());
        assert_eq!(created.len(), 3);
        assert!(dir.join("Hartwell media/eliza_1890.jpg").exists());
        let first = doc.media_item(&created[0]).unwrap();
        assert_eq!((first.title.as_str(), first.kind.as_str()), ("eliza 1890", "photo"));
        assert_eq!(doc.media_item(&created[2]).unwrap().kind, "certificate");
        // The first photo is the profile photo; the second doesn't take over.
        assert_eq!(doc.person("I4").unwrap().photos.first().map(String::as_str), Some("Hartwell media/eliza_1890.jpg"));
        assert_eq!(doc.media_index().values().flatten().filter(|u| u.owner == "I4").count(), 3);
        // One undo removes the lot.
        assert!(doc.undo());
        assert!(doc.media_items().is_empty());

        // An imported profile photo without a file (as Ancestry exports them)
        // gives way to a real one.
        let ghost = doc.mutate(|d| {
            let g = d.create_media("", "photo", "Image (8).jpg");
            d.set_primary_photo("I4", &g);
            g
        });
        assert!(doc.person("I4").unwrap().photos.is_empty(), "nothing to show yet");
        let (created, _) = add_files(&mut doc, &tree, &files[..1], Some("I4"));
        assert_eq!(doc.primary_media("I4"), Some(created[0].clone()));
        assert!(!doc.media_item(&ghost).unwrap().has_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn avatars_fall_back_to_a_photo_with_a_file() {
        let (mut doc, _) = Document::from_bytes(include_bytes!("sample.ged"));
        doc.mutate(|d| {
            let ghost = d.create_media("", "photo", "Image (8).jpg");
            let real = d.create_media("media/a.jpg", "photo", "");
            let pdf = d.create_media("media/b.pdf", "document", "");
            d.attach_media(&real, "I1", &MediaPlace::Whole);
            d.attach_media(&pdf, "I1", &MediaPlace::Whole);
            d.set_primary_photo("I1", &ghost);
        });
        assert_eq!(doc.person("I1").unwrap().photos, vec!["media/a.jpg".to_string()]);
    }

    #[test]
    fn import_copies_once_and_rebase_keeps_links() {
        let dir = std::env::temp_dir().join(format!("genie-media-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        let tree = dir.join("Family.ged");
        std::fs::write(&tree, "0 HEAD\n0 TRLR\n").unwrap();
        let src = dir.join("elsewhere/scan.jpg");
        std::fs::write(&src, b"image bytes").unwrap();

        let rel = import_file(&tree, &src).unwrap();
        assert_eq!(rel, "Family media/scan.jpg");
        assert!(dir.join("Family media/scan.jpg").exists());
        assert_eq!(import_file(&tree, &src).unwrap(), rel, "same file isn't copied twice");
        std::fs::write(&src, b"different bytes").unwrap();
        assert_eq!(import_file(&tree, &src).unwrap(), "Family media/scan (2).jpg");
        // A file already in the media folder is linked where it is.
        assert_eq!(import_file(&tree, &dir.join("Family media/scan.jpg")).unwrap(), rel);
        assert_eq!(resolve(Some(&tree), &rel).unwrap(), dir.join("Family media/scan.jpg"));

        let mut doc = Document::new_empty();
        let m = doc.mutate(|d| d.create_media(&rel, "photo", ""));
        std::fs::create_dir_all(dir.join("copy")).unwrap();
        assert!(doc.rebase_media(&dir, &dir.join("copy")));
        assert_eq!(doc.media_item(&m).unwrap().file, "../Family media/scan.jpg");

        // Missing files are found again by name.
        let items = vec![MediaItem { xref: "M9".into(), file: "C:\\Old PC\\scan.jpg".into(), ..Default::default() }];
        let found = find_files(Some(&tree), &items, &dir.join("elsewhere"));
        assert_eq!(found, vec![FoundFile { media: "M9".into(), path: dir.join("elsewhere/scan.jpg"), was_empty: false }]);

        // Records exported without a file are matched by title and size.
        std::fs::create_dir_all(dir.join("download/a")).unwrap();
        std::fs::create_dir_all(dir.join("download/b")).unwrap();
        std::fs::write(dir.join("download/a/Image (8).jpg"), vec![0u8; 10]).unwrap();
        std::fs::write(dir.join("download/b/Image (8).jpg"), vec![0u8; 57]).unwrap();
        let ghost = |x: &str, size| MediaItem { xref: x.into(), title: "Image (8).jpg".into(), size, ..Default::default() };
        let found = find_files(Some(&tree), &[ghost("O1", Some(57)), ghost("O2", Some(99)), ghost("O3", None)], &dir.join("download"));
        assert_eq!(found, vec![FoundFile { media: "O1".into(), path: dir.join("download/b/Image (8).jpg"), was_empty: true }]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
