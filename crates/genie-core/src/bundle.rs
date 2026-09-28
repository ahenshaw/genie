//! Bundles: a tree and its documents in one file, to move to another
//! computer.
//!
//! The format is GEDZIP (`.gdz`) from the GEDCOM 7 specification: a zip
//! with the tree at the root as `gedcom.ged`, and each document stored at
//! the relative path its `FILE` line gives. Genie writes GEDCOM 5.5.1
//! inside, which Genie reads back but some other programs may not.

use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::media::{self, path_string};
use crate::model::Document;

pub const EXTENSION: &str = "gdz";
const TREE_ENTRY: &str = "gedcom.ged";

/// What an export left out.
#[derive(Debug, Default)]
pub struct ExportSummary {
    pub files: usize,
    /// Documents whose file couldn't be found, by title.
    pub missing: Vec<String>,
}

/// A path as written in `FILE`, if it can go into the bundle unchanged:
/// relative, and staying inside the tree's folder.
pub fn safe_relative(file: &str) -> Option<String> {
    let f = file.trim().trim_start_matches("file://").replace('\\', "/");
    if f.is_empty() || f.starts_with('/') || f.chars().nth(1) == Some(':') {
        return None;
    }
    let ok = Path::new(&f).components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    ok.then(|| f.trim_start_matches("./").to_string())
}

/// `name`, or `stem (2).ext`, … whichever isn't taken yet.
fn unique(name: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() && !s.ends_with('/') => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| !taken(c)).expect("some name is free")
}

/// Writes `doc` and the documents it refers to as a bundle. Files kept
/// outside the tree's folder go into its media folder in the bundle, with
/// the bundled copy of the tree pointing at them there; the tree itself is
/// left as it is.
pub fn write<W: Write + Seek>(doc: &Document, out: W) -> std::io::Result<ExportSummary> {
    let tree = doc.path.as_deref();
    let stem = Path::new(&doc.file_name()).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Tree".into());
    let media_dir = format!("{stem} media");

    // Which file goes where in the bundle, and the FILE lines that change.
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    let mut by_source: HashMap<PathBuf, String> = HashMap::new();
    let mut relinks: Vec<(String, String)> = Vec::new();
    let mut summary = ExportSummary::default();
    for item in doc.media_items().iter().filter(|m| m.has_file()) {
        let Some(src) = media::resolve(tree, &item.file).filter(|p| p.is_file()) else {
            summary.missing.push(item.display_title());
            continue;
        };
        let name = match by_source.get(&src) {
            Some(name) => name.clone(),
            None => {
                let wanted = safe_relative(&item.file).unwrap_or_else(|| format!("{media_dir}/{}", media::file_name(&item.file)));
                let name = unique(&wanted, |c| c == TREE_ENTRY || entries.iter().any(|(n, _)| n == c));
                entries.push((name.clone(), src.clone()));
                by_source.insert(src, name.clone());
                name
            }
        };
        if name != item.file {
            relinks.push((item.xref.clone(), name));
        }
    }
    summary.files = entries.len();

    let mut copy = Document::from_bytes(doc.to_gedcom().as_bytes()).0;
    for (m, file) in &relinks {
        copy.relink_media(m, file);
    }

    let mut zip = ZipWriter::new(out);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    zip.start_file(TREE_ENTRY, deflate)?;
    zip.write_all(copy.to_gedcom().as_bytes())?;
    for (name, src) in &entries {
        // Photos and scans are compressed already.
        let method = if media::guess_kind(name) == "document" && !name.to_lowercase().ends_with(".pdf") { CompressionMethod::Deflated } else { CompressionMethod::Stored };
        let mut options = SimpleFileOptions::default().compression_method(method).large_file(true);
        if let Some(t) = modified(src) {
            options = options.last_modified_time(t);
        }
        zip.start_file(name.as_str(), options)?;
        std::io::copy(&mut std::fs::File::open(src)?, &mut zip)?;
    }
    zip.finish()?;
    Ok(summary)
}

/// When a file was last changed, in UTC, as zip records it.
fn modified(p: &Path) -> Option<zip::DateTime> {
    let t = std::fs::metadata(p).and_then(|m| m.modified()).ok()?;
    let t = time::OffsetDateTime::from(t);
    zip::DateTime::try_from(time::PrimitiveDateTime::new(t.date(), t.time())).ok()
}

/// Writes a bundle to `dest` (through a temporary file, so a failure
/// leaves nothing half written).
pub fn export(doc: &Document, dest: &Path) -> std::io::Result<ExportSummary> {
    let mut tmp = dest.as_os_str().to_owned();
    tmp.push(".partial");
    let result = std::fs::File::create(&tmp).and_then(|f| write(doc, std::io::BufWriter::new(f)));
    match result {
        Ok(s) => std::fs::rename(&tmp, dest).map(|_| s),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Unpacks `bundle` into a new folder inside `parent`, named after the
/// bundle, and returns the path of the tree to open. Entries that would
/// land outside that folder are refused.
pub fn import(bundle: &Path, parent: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(bundle).map_err(|e| e.to_string())?;
    let name = bundle.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Tree".into());
    unpack(file, parent, &name)
}

fn unpack<R: Read + Seek>(reader: R, parent: &Path, name: &str) -> Result<PathBuf, String> {
    let mut zip = ZipArchive::new(reader).map_err(|e| format!("Not a readable bundle: {e}"))?;
    // The tree: gedcom.ged, or failing that the only .ged at the top.
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let top_ged: Vec<&String> = names.iter().filter(|n| !n.contains('/') && n.to_lowercase().ends_with(".ged")).collect();
    let tree_entry = match names.iter().find(|n| *n == TREE_ENTRY) {
        Some(t) => t.clone(),
        None if top_ged.len() == 1 => top_ged[0].clone(),
        None => return Err("The bundle has no gedcom.ged in it.".into()),
    };

    let folder_name = unique(name, |c| parent.join(c).exists());
    let folder = parent.join(&folder_name);
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let tree = folder.join(format!("{folder_name}.ged"));
    let result = (|| -> Result<(), String> {
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
            if entry.is_dir() {
                continue;
            }
            let dest = if entry.name() == tree_entry {
                tree.clone()
            } else {
                let rel = entry.enclosed_name().ok_or_else(|| format!("The bundle has an unsafe path in it: {}", entry.name()))?;
                folder.join(rel)
            };
            if let Some(dir) = dest.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let mut out = std::fs::File::create(&dest).map_err(|e| format!("{}: {e}", path_string(&dest)))?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => Ok(tree),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&folder);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(doc: &Document, xref: &str) -> media::MediaItem {
        doc.media_items().into_iter().find(|m| m.xref.trim_matches('@') == xref).expect(xref)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("genie-bundle-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn round_trip_keeps_every_document() {
        let dir = scratch("round");
        std::fs::create_dir_all(dir.join("home/Family media")).unwrap();
        std::fs::create_dir_all(dir.join("elsewhere")).unwrap();
        std::fs::write(dir.join("home/Family media/scan.jpg"), b"inside").unwrap();
        std::fs::write(dir.join("elsewhere/scan.jpg"), b"outside, same name").unwrap();
        let outside = path_string(&dir.join("elsewhere/scan.jpg"));
        let ged = format!(
            "0 HEAD\n0 @M1@ OBJE\n1 FILE Family media/scan.jpg\n0 @M2@ OBJE\n1 FILE {outside}\n0 @M3@ OBJE\n1 FILE gone.jpg\n2 TITL Lost photo\n0 TRLR\n"
        );
        let tree = dir.join("home/Family.ged");
        std::fs::write(&tree, &ged).unwrap();
        let mut doc = Document::from_bytes(ged.as_bytes()).0;
        doc.path = Some(tree.clone());

        let gdz = dir.join("Family.gdz");
        let summary = export(&doc, &gdz).unwrap();
        assert_eq!(summary.files, 2);
        assert_eq!(summary.missing, ["Lost photo"]);
        // The tree on disk still points outside.
        assert_eq!(item(&doc, "M2").file, outside);

        let opened = import(&gdz, &dir.join("other computer")).unwrap();
        assert_eq!(opened, dir.join("other computer/Family/Family.ged"));
        let mut back = Document::from_bytes(&std::fs::read(&opened).unwrap()).0;
        back.path = Some(opened.clone());
        let read = |m: &str| std::fs::read(media::resolve(back.path.as_deref(), &item(&back, m).file).unwrap()).unwrap();
        assert_eq!(read("M1"), b"inside");
        assert_eq!(read("M2"), b"outside, same name");

        // Importing again doesn't overwrite the first copy.
        assert_eq!(import(&gdz, &dir.join("other computer")).unwrap(), dir.join("other computer/Family (2)/Family (2).ged"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_paths_that_escape_the_folder() {
        let dir = scratch("escape");
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = ZipWriter::new(&mut buf);
        zip.start_file(TREE_ENTRY, SimpleFileOptions::default()).unwrap();
        zip.write_all(b"0 HEAD\n0 TRLR\n").unwrap();
        zip.start_file("../evil.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        buf.set_position(0);
        let err = unpack(buf, &dir, "Bad").unwrap_err();
        assert!(err.contains("unsafe"), "{err}");
        assert!(!dir.join("evil.txt").exists() && !dir.join("Bad").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn only_plain_relative_paths_are_kept() {
        assert_eq!(safe_relative("Family media/a.jpg").as_deref(), Some("Family media/a.jpg"));
        assert_eq!(safe_relative("media\\a.jpg").as_deref(), Some("media/a.jpg"));
        for bad in ["/home/a.jpg", "C:\\a.jpg", "../a.jpg", "a/../../b.jpg", ""] {
            assert_eq!(safe_relative(bad), None, "{bad}");
        }
    }
}
