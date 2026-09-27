//! Getting photos from WikiTree (wikitree.com) for people in the tree.
//!
//! Starting from one or more WikiTree profile IDs, relatives are fetched in
//! bulk, matched to people in the tree by name and dates (only where exactly
//! one person fits), and the photos on matched profiles are downloaded for
//! review. Nothing touches the tree until the user adds what they want.
//!
//! WikiTree asks API users to identify their app (`appId`) and to go easy
//! on the servers, so requests are spaced out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::gedcom::Node;
use crate::media::{self, MediaPlace};
use crate::model::Document;

const API: &str = "https://api.wikitree.com/api.php";
const SITE: &str = "https://www.wikitree.com";
const APP_ID: &str = "Genie";
const USER_AGENT: &str = "Genie genealogy app (https://github.com/ahenshaw/genie)";
/// Minimum gap between requests.
const SPACING: Duration = Duration::from_millis(1200);
const ANCESTORS: &str = "12";
const DESCENDANTS: &str = "5";
/// Images smaller than this each way are badges and icons, not photos.
const BADGE_SIZE: u32 = 200;

/// A person in the tree, as the search needs them.
#[derive(Clone, Debug)]
pub struct TreePerson {
    pub xref: String,
    pub given: String,
    pub surname: String,
    pub birth: Option<i32>,
    pub death: Option<i32>,
    /// A WikiTree ID already recorded for them.
    pub wikitree: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Profile {
    /// "Henshaw-367".
    pub id: String,
    pub first: String,
    pub surnames: Vec<String>,
    pub birth: Option<i32>,
    pub death: Option<i32>,
    pub has_photo: bool,
    pub display: String,
}

#[derive(Clone, Debug)]
pub struct FoundPhoto {
    pub xref: String,
    pub profile: String,
    pub title: String,
    /// WikiTree's photo type: "photo", "source", …
    pub kind: String,
    pub width: u32,
    pub height: u32,
    /// The photo's page on WikiTree, recorded in the note.
    pub page_url: String,
    /// Where it was downloaded to, for review.
    pub file: PathBuf,
}

impl FoundPhoto {
    pub fn looks_like_badge(&self) -> bool {
        self.width < BADGE_SIZE && self.height < BADGE_SIZE
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub profiles: usize,
    /// Tree xref → WikiTree profile.
    pub matched: HashMap<String, Profile>,
    pub photos: Vec<FoundPhoto>,
    /// Photos left out because they were imported before.
    pub already: usize,
}

pub enum Progress {
    Status(String),
    Count { done: usize, total: usize },
    Done(Result<Report, String>),
}

// ---- matching (no network) -------------------------------------------------------------

fn norm(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ").to_lowercase()
}

fn year(v: &Value) -> Option<i32> {
    let s = v.as_str()?;
    let y: i32 = s.get(..4)?.parse().ok()?;
    (y > 0).then_some(y)
}

pub fn profile_from_json(v: &Value) -> Option<Profile> {
    let id = v.get("Name")?.as_str()?.to_string();
    let first = norm(v.get("FirstName").and_then(Value::as_str).unwrap_or(""));
    let mut surnames = Vec::new();
    for k in ["LastNameAtBirth", "LastNameCurrent"] {
        let s = norm(v.get(k).and_then(Value::as_str).unwrap_or(""));
        if !s.is_empty() && !surnames.contains(&s) {
            surnames.push(s);
        }
    }
    let photo = v.get("Photo").and_then(Value::as_str).unwrap_or("");
    let display = [v.get("FirstName"), v.get("MiddleName"), v.get("LastNameAtBirth")]
        .iter()
        .filter_map(|x| x.and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Some(Profile {
        id,
        first,
        surnames,
        birth: v.get("BirthDate").and_then(year),
        death: v.get("DeathDate").and_then(year),
        has_photo: !photo.is_empty(),
        display,
    })
}

/// Pairs tree people with WikiTree profiles: by a recorded WikiTree ID, or
/// by first name, surname and birth year (and death year where both have
/// one) when exactly one person and one profile fit each other.
pub fn match_people(people: &[TreePerson], profiles: &[Profile]) -> HashMap<String, Profile> {
    let mut out: HashMap<String, Profile> = HashMap::new();
    let by_id: HashMap<&str, &Profile> = profiles.iter().map(|p| (p.id.as_str(), p)).collect();
    for t in people {
        if let Some(p) = t.wikitree.as_deref().and_then(|id| by_id.get(id)) {
            out.insert(t.xref.clone(), (*p).clone());
        }
    }
    let taken: HashSet<String> = out.values().map(|p| p.id.clone()).collect();
    let mut proposals: HashMap<String, Vec<&Profile>> = HashMap::new();
    for p in profiles.iter().filter(|p| !taken.contains(&p.id)) {
        let (Some(first), Some(by)) = (p.first.split(' ').next().filter(|f| !f.is_empty()), p.birth) else { continue };
        let fits: Vec<&TreePerson> = people
            .iter()
            .filter(|t| !out.contains_key(&t.xref))
            .filter(|t| t.birth == Some(by))
            .filter(|t| p.surnames.contains(&norm(&t.surname)))
            .filter(|t| norm(&t.given).split(' ').next() == Some(first))
            .filter(|t| !(t.death.is_some() && p.death.is_some() && t.death != p.death))
            .collect();
        if let [one] = fits.as_slice() {
            proposals.entry(one.xref.clone()).or_default().push(p);
        }
    }
    for (xref, ps) in proposals {
        if let [one] = ps.as_slice() {
            out.insert(xref, (*one).clone());
        }
    }
    out
}

/// The full-size image behind a WikiTree thumbnail URL:
/// `/photo.php/thumb/c/c0/X.jpg/300px-X.jpg` → `/photo.php/c/c0/X.jpg`.
pub fn original_url(thumb: &str) -> Option<String> {
    let parts: Vec<&str> = thumb.split('/').collect();
    let i = parts.iter().position(|p| *p == "thumb")?;
    let path = [&parts[..i], &parts[i + 1..parts.len() - 1]].concat().join("/");
    Some(format!("{SITE}{path}"))
}

// ---- reading the tree ------------------------------------------------------------------

fn wikitree_id_in(text: &str) -> Option<String> {
    let i = text.find("wikitree.com/wiki/")? + "wikitree.com/wiki/".len();
    let id: String = text[i..].chars().take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
    id.contains('-').then_some(id)
}

/// WikiTree IDs recorded on a person: `REFN` with `TYPE WikiTree`, or a
/// wikitree.com profile link in a note or web address.
fn recorded_id(r: &Node) -> Option<String> {
    for c in &r.children {
        if c.tag == "REFN" && c.child_value("TYPE").eq_ignore_ascii_case("wikitree") {
            return Some(c.value.trim().to_string());
        }
    }
    r.children.iter().filter(|c| matches!(c.tag.as_str(), "NOTE" | "WWW" | "_LINK" | "_URL")).find_map(|c| wikitree_id_in(&c.value))
}

pub fn tree_people(doc: &Document) -> Vec<TreePerson> {
    doc.people()
        .iter()
        .map(|p| TreePerson {
            xref: p.xref.clone(),
            given: p.given.clone(),
            surname: p.surname.clone(),
            birth: p.birth_year,
            death: p.death_year,
            wikitree: doc.record(&p.xref).and_then(recorded_id),
        })
        .collect()
}

/// WikiTree IDs to offer as starting points: recorded on people, or named
/// in notes of photos imported from WikiTree before. (ID, person's name).
pub fn suggested_starts(doc: &Document) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |id: String, name: String| {
        if !out.iter().any(|(i, _)| *i == id) {
            out.push((id, name));
        }
    };
    for t in tree_people(doc) {
        if let Some(id) = t.wikitree {
            push(id, doc.person(&t.xref).map(|p| p.display.clone()).unwrap_or_default());
        }
    }
    let idx = doc.media_index();
    for item in doc.media_items() {
        let Some(rest) = item.note.strip_prefix("From WikiTree profile ") else { continue };
        let id: String = rest.chars().take_while(|c| *c != ':').collect();
        if let Some(owner) = idx.get(&item.xref).and_then(|u| u.first()) {
            push(id, doc.owner_label(&owner.owner));
        }
    }
    out
}

/// Photo pages already imported, so they aren't offered again.
pub fn imported_pages(doc: &Document) -> HashSet<String> {
    doc.media_items().into_iter().filter_map(|m| m.note.split_once(": ").map(|(_, url)| url.trim().to_string())).filter(|u| u.starts_with(SITE)).collect()
}

// ---- the search, on a background thread -------------------------------------------------------

struct Client {
    agent: ureq::Agent,
    last: Option<Instant>,
}

impl Client {
    fn new() -> Self {
        let agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(60))).build().into();
        Self { agent, last: None }
    }

    fn pace(&mut self) {
        if let Some(t) = self.last {
            let wait = SPACING.saturating_sub(t.elapsed());
            std::thread::sleep(wait);
        }
        self.last = Some(Instant::now());
    }

    fn api(&mut self, params: &[(&str, &str)]) -> Result<Value, String> {
        self.pace();
        let mut req = self.agent.get(API).header("User-Agent", USER_AGENT).query("appId", APP_ID);
        for (k, v) in params {
            req = req.query(*k, *v);
        }
        let text = req.call().map_err(|e| describe(&e))?.body_mut().read_to_string().map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("unexpected reply from WikiTree: {e}"))?;
        let first = v.get(0).cloned().unwrap_or(Value::Null);
        match first.get("status") {
            Some(Value::String(s)) if !s.is_empty() => Err(format!("WikiTree says: {s}")),
            _ => Ok(first),
        }
    }

    fn download(&mut self, url: &str, dest: &Path) -> Result<(), String> {
        self.pace();
        let bytes = self
            .agent
            .get(url)
            .header("User-Agent", USER_AGENT)
            .call()
            .map_err(|e| describe(&e))?
            .body_mut()
            .with_config()
            .limit(60 * 1024 * 1024)
            .read_to_vec()
            .map_err(|e| e.to_string())?;
        std::fs::write(dest, bytes).map_err(|e| e.to_string())
    }
}

fn describe(e: &ureq::Error) -> String {
    match e {
        ureq::Error::StatusCode(429) => "WikiTree is limiting requests right now; try again in a while.".into(),
        ureq::Error::StatusCode(code) => format!("WikiTree replied with HTTP {code}"),
        other => format!("Couldn't reach WikiTree: {other}"),
    }
}

/// Starts a search. Progress arrives on the returned channel, ending with
/// `Progress::Done`. Setting `cancel` stops it between requests.
pub fn start(starts: Vec<String>, people: Vec<TreePerson>, skip_pages: HashSet<String>, download_to: PathBuf, cancel: Arc<AtomicBool>) -> Receiver<Progress> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let result = run(&starts, &people, &skip_pages, &download_to, &cancel, &tx);
        let _ = tx.send(Progress::Done(result));
    });
    rx
}

fn run(starts: &[String], people: &[TreePerson], skip_pages: &HashSet<String>, download_to: &Path, cancel: &AtomicBool, tx: &Sender<Progress>) -> Result<Report, String> {
    let stopped = || if cancel.load(Ordering::Relaxed) { Err("Cancelled.".to_string()) } else { Ok(()) };
    let say = |s: String| {
        let _ = tx.send(Progress::Status(s));
    };
    let mut client = Client::new();

    // 1. Relatives of each starting profile, in bulk.
    let mut profiles: HashMap<String, Profile> = HashMap::new();
    let fields = "Id,Name,FirstName,MiddleName,LastNameAtBirth,LastNameCurrent,BirthDate,DeathDate,Photo,Spouses";
    for (i, id) in starts.iter().enumerate() {
        stopped()?;
        say(format!("Fetching relatives of {id} ({} of {})…", i + 1, starts.len()));
        let v = client.api(&[("action", "getPeople"), ("keys", id), ("ancestors", ANCESTORS), ("descendants", DESCENDANTS), ("nuclear", "1"), ("limit", "1000"), ("fields", fields)])?;
        let Some(map) = v.get("people").and_then(Value::as_object) else { continue };
        for p in map.values() {
            let mut all = vec![p];
            if let Some(sp) = p.get("Spouses").and_then(Value::as_array) {
                all.extend(sp.iter());
            }
            for q in all {
                if let Some(prof) = profile_from_json(q) {
                    profiles.entry(prof.id.clone()).or_insert(prof);
                }
            }
        }
    }
    if profiles.is_empty() {
        return Err("WikiTree returned no profiles for that ID. Check it's a profile ID like Henshaw-1012.".into());
    }

    // 2. Match them to the tree.
    say(format!("Matching {} WikiTree profiles to the tree…", profiles.len()));
    let list: Vec<Profile> = profiles.into_values().collect();
    let matched = match_people(people, &list);

    // 3. Photos on matched profiles that have one.
    let with_photos: Vec<(&String, &Profile)> = matched.iter().filter(|(_, p)| p.has_photo).collect();
    let mut photos = Vec::new();
    let mut already = 0;
    for (n, (xref, prof)) in with_photos.iter().enumerate() {
        stopped()?;
        say(format!("Checking photos on {} matched profiles…", with_photos.len()));
        let _ = tx.send(Progress::Count { done: n, total: with_photos.len() });
        let v = client.api(&[("action", "getPhotos"), ("key", &prof.id), ("limit", "10")])?;
        for ph in v.get("photos").and_then(Value::as_array).into_iter().flatten() {
            let s = |k: &str| ph.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            let page_url = format!("{SITE}{}", s("URL"));
            if skip_pages.contains(&page_url) {
                already += 1;
                continue;
            }
            let Some(url) = original_url(&s("URL_300")) else { continue };
            photos.push((url, FoundPhoto {
                xref: (*xref).clone(),
                profile: prof.id.clone(),
                title: s("Title").split_whitespace().collect::<Vec<_>>().join(" "),
                kind: s("Type").to_lowercase(),
                width: s("Width").parse().unwrap_or(0),
                height: s("Height").parse().unwrap_or(0),
                page_url,
                file: download_to.join(format!("wikitree-{}", s("ImageName"))),
            }));
        }
    }

    // 4. Download them for review.
    std::fs::create_dir_all(download_to).map_err(|e| e.to_string())?;
    let total = photos.len();
    let mut out = Vec::new();
    for (n, (url, photo)) in photos.into_iter().enumerate() {
        stopped()?;
        say(format!("Downloading {total} photos…"));
        let _ = tx.send(Progress::Count { done: n, total });
        if !photo.file.exists() {
            client.download(&url, &photo.file)?;
        }
        out.push(photo);
    }
    Ok(Report { profiles: list.len(), matched, photos: out, already })
}

// ---- adding to the tree --------------------------------------------------------------------

/// Copies the chosen photos into the media folder and attaches them, noting
/// where each came from and recording each person's WikiTree ID. One undo
/// step. Returns how many were added, and any that couldn't be copied.
pub fn import(doc: &mut Document, tree: &Path, photos: &[&FoundPhoto]) -> (usize, Vec<String>) {
    let mut copied = Vec::new();
    let mut failed = Vec::new();
    for p in photos {
        match media::import_file(tree, &p.file) {
            Ok(rel) => copied.push((rel, *p)),
            Err(e) => failed.push(format!("{}: {e}", p.title)),
        }
    }
    let n = copied.len();
    if n == 0 {
        return (0, failed);
    }
    doc.mutate(|d| {
        for (rel, p) in &copied {
            let kind = if p.kind == "photo" { "photo" } else { "document" };
            let m = d.create_media(rel, kind, &p.title);
            if let Some(mut item) = d.media_item(&m) {
                item.note = format!("From WikiTree profile {}: {}", p.profile, p.page_url);
                d.update_media(&item);
            }
            d.attach_media(&m, &p.xref, &MediaPlace::Whole);
            if kind == "photo" && !media::primary_shows(d, &p.xref, tree) {
                d.set_primary_photo(&p.xref, &m);
            }
            record_id(d, &p.xref, &p.profile);
        }
    });
    (n, failed)
}

fn record_id(d: &mut Document, xref: &str, id: &str) {
    let Some(r) = d.record_mut(xref) else { return };
    let has = r.children.iter().any(|c| c.tag == "REFN" && c.child_value("TYPE").eq_ignore_ascii_case("wikitree"));
    if !has {
        let pos = r.children.iter().position(|c| matches!(c.tag.as_str(), "FAMC" | "FAMS" | "CHAN")).unwrap_or(r.children.len());
        r.children.insert(pos, Node::new("REFN", id).with_child(Node::new("TYPE", "WikiTree")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(xref: &str, given: &str, surname: &str, birth: i32, death: Option<i32>) -> TreePerson {
        TreePerson { xref: xref.into(), given: given.into(), surname: surname.into(), birth: Some(birth), death, wikitree: None }
    }

    fn profile(id: &str, first: &str, surname: &str, birth: i32, death: Option<i32>) -> Profile {
        Profile { id: id.into(), first: first.into(), surnames: vec![surname.into()], birth: Some(birth), death, has_photo: true, display: String::new() }
    }

    #[test]
    fn matches_only_when_unambiguous() {
        let people = vec![
            person("I1", "Eugene Jr", "Henshaw", 1866, Some(1960)),
            person("I2", "Mary Mann", "Mitchell", 1868, None),
            // Two John Smiths born 1800: neither can be matched safely.
            person("I3", "John", "Smith", 1800, None),
            person("I4", "John", "Smith", 1800, None),
            person("I5", "Susan", "Chenault", 1839, Some(1927)),
        ];
        let profiles = vec![
            profile("Henshaw-367", "eugene", "henshaw", 1866, Some(1960)),
            profile("Mitchell-13353", "mary", "mitchell", 1868, Some(1931)),
            profile("Smith-1", "john", "smith", 1800, None),
            // Same name and birth year but a different death year: not her.
            profile("Chenault-9", "susan", "chenault", 1839, Some(1901)),
        ];
        let m = match_people(&people, &profiles);
        assert_eq!(m.get("I1").map(|p| p.id.as_str()), Some("Henshaw-367"));
        assert_eq!(m.get("I2").map(|p| p.id.as_str()), Some("Mitchell-13353"));
        assert!(!m.contains_key("I3") && !m.contains_key("I4"));
        assert!(!m.contains_key("I5"));
    }

    #[test]
    fn a_recorded_id_wins() {
        let mut t = person("I9", "Bob", "Nobody", 1900, None);
        t.wikitree = Some("Henshaw-367".into());
        let m = match_people(&[t], &[profile("Henshaw-367", "eugene", "henshaw", 1866, None)]);
        assert_eq!(m["I9"].id, "Henshaw-367");
    }

    #[test]
    fn full_size_image_urls() {
        assert_eq!(
            original_url("/photo.php/thumb/c/c0/Henshaw-367.jpg/300px-Henshaw-367.jpg").as_deref(),
            Some("https://www.wikitree.com/photo.php/c/c0/Henshaw-367.jpg")
        );
        assert_eq!(original_url(""), None);
        assert_eq!(wikitree_id_in("see https://www.wikitree.com/wiki/Henshaw-1012 for more").as_deref(), Some("Henshaw-1012"));
    }

    #[test]
    fn import_attaches_notes_and_records_the_id() {
        let dir = std::env::temp_dir().join(format!("genie-wt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dl")).unwrap();
        let tree = dir.join("T.ged");
        std::fs::write(&tree, "0 HEAD\n0 TRLR\n").unwrap();
        std::fs::write(dir.join("dl/wikitree-X-1.jpg"), b"img").unwrap();
        let (mut doc, _) = Document::from_bytes(include_bytes!("sample.ged"));
        let photo = FoundPhoto {
            xref: "I1".into(),
            profile: "Hartwell-1".into(),
            title: "Thomas at the yard".into(),
            kind: "photo".into(),
            width: 400,
            height: 500,
            page_url: format!("{SITE}/photo/jpg/X-1"),
            file: dir.join("dl/wikitree-X-1.jpg"),
        };
        let (n, failed) = import(&mut doc, &tree, &[&photo]);
        assert_eq!((n, failed.len()), (1, 0));
        let item = doc.media_items().pop().unwrap();
        assert_eq!(item.note, "From WikiTree profile Hartwell-1: https://www.wikitree.com/photo/jpg/X-1");
        assert_eq!(doc.person("I1").unwrap().photos.first().map(String::as_str), Some("T media/wikitree-X-1.jpg"));
        assert!(doc.to_gedcom().contains("1 REFN Hartwell-1\r\n2 TYPE WikiTree\r\n"));
        // Next time: offered as a starting point, and not offered again.
        assert_eq!(suggested_starts(&doc)[0].0, "Hartwell-1");
        assert!(imported_pages(&doc).contains(&photo.page_url));
        assert!(doc.undo());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
