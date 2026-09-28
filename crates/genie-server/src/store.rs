//! The tree in the database: its head revision (cached), saving a new
//! revision with a row per changed record, and what each role may see of it.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex, OnceLock};

use genie_core::gedcom::{self, Node};
use genie_core::model::Document;
use genie_core::privacy;
use genie_core::sync::{Action, Change};
use sqlx::MySqlConnection;

use crate::auth::Role;
use crate::error::{ApiError, ApiResult};
use crate::AppState;

pub fn gzip(text: &str) -> Vec<u8> {
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(text.as_bytes()).expect("writing to memory");
    enc.finish().expect("writing to memory")
}

pub fn gunzip(bytes: &[u8]) -> ApiResult<String> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes).read_to_string(&mut text).map_err(ApiError::internal)?;
    Ok(text)
}

/// The same GEDCOM text Genie itself would save for these records.
pub fn canonical(records: Vec<Node>) -> (Document, String) {
    let doc = Document::from_records(records);
    let text = doc.to_gedcom();
    (doc, text)
}

pub fn parse(text: &str) -> Vec<Node> {
    gedcom::parse(text).0
}

pub fn this_year() -> i32 {
    time::OffsetDateTime::now_utc().year()
}

/// A revision, parsed, with what guests may see of it worked out once.
pub struct Head {
    pub id: i64,
    pub text: String,
    pub records: Vec<Node>,
    guest: OnceLock<GuestView>,
}

pub struct GuestView {
    pub text: String,
    pub records: Vec<Node>,
    pub living: HashSet<String>,
}

impl Head {
    pub fn new(id: i64, text: String) -> Self {
        let records = parse(&text);
        Self { id, text, records, guest: OnceLock::new() }
    }

    pub fn guest(&self, living_years: i32) -> &GuestView {
        self.guest.get_or_init(|| {
            let year = this_year();
            let records = privacy::guest_view(&self.records, living_years, year);
            GuestView { text: gedcom::write(&records), living: privacy::living_people(&self.records, living_years, year), records }
        })
    }

    /// The text `role` is sent.
    pub fn text_for(&self, role: Role, living_years: i32) -> &str {
        if role == Role::Guest { &self.guest(living_years).text } else { &self.text }
    }

    /// The records `role` sees.
    pub fn records_for(&self, role: Role, living_years: i32) -> &[Node] {
        if role == Role::Guest { &self.guest(living_years).records } else { &self.records }
    }

    /// Whether `role` may see the history of `xref`: for guests, only
    /// records they see in full.
    pub fn history_visible(&self, role: Role, living_years: i32, xref: &str) -> bool {
        if role != Role::Guest {
            return true;
        }
        let g = self.guest(living_years);
        let Some(r) = g.records.iter().find(|r| r.xref.as_deref() == Some(xref)) else { return false };
        match r.tag.as_str() {
            "INDI" => !g.living.contains(xref),
            "FAM" => !r.children.iter().any(|c| matches!(c.tag.as_str(), "HUSB" | "WIFE") && c.pointer().is_some_and(|p| g.living.contains(p))),
            _ => true,
        }
    }
}

#[derive(Default)]
pub struct HeadCache(Mutex<Option<Arc<Head>>>);

pub async fn head_id(pool: &sqlx::MySqlPool) -> ApiResult<Option<i64>> {
    let row: Option<(Option<i64>,)> = sqlx::query_as("SELECT head_revision_id FROM tree WHERE id = 1").fetch_optional(pool).await?;
    Ok(row.and_then(|r| r.0))
}

pub async fn revision_text(pool: &sqlx::MySqlPool, id: i64) -> ApiResult<Option<String>> {
    let row: Option<(Vec<u8>,)> = sqlx::query_as("SELECT gedcom FROM revisions WHERE id = ?").bind(id).fetch_optional(pool).await?;
    row.map(|r| gunzip(&r.0)).transpose()
}

/// The current revision, from the cache when it's still current.
pub async fn head(state: &AppState) -> ApiResult<Option<Arc<Head>>> {
    let Some(id) = head_id(&state.pool).await? else { return Ok(None) };
    if let Some(h) = state.head.0.lock().unwrap().as_ref().filter(|h| h.id == id) {
        return Ok(Some(h.clone()));
    }
    let text = revision_text(&state.pool, id).await?.ok_or_else(|| ApiError::internal(format!("revision {id} is missing")))?;
    let head = Arc::new(tokio::task::spawn_blocking(move || Head::new(id, text)).await.map_err(ApiError::internal)?);
    *state.head.0.lock().unwrap() = Some(head.clone());
    Ok(Some(head))
}

/// What a record was called, for the history list.
pub fn label(doc: &Document, xref: &str) -> String {
    let Some(r) = doc.record(xref) else { return String::new() };
    let name = |x: &str| doc.person(x).map(|p| p.display.clone());
    let text = match r.tag.as_str() {
        "INDI" => name(xref).unwrap_or_default(),
        "FAM" => ["HUSB", "WIFE"].iter().filter_map(|t| r.child(t)?.pointer()).filter_map(name).collect::<Vec<_>>().join(" & "),
        "SOUR" => r.child_value("TITL").to_string(),
        "OBJE" => {
            let file = r.child("FILE");
            let title = file.map(|f| f.child_value("TITL")).filter(|t| !t.is_empty()).unwrap_or_else(|| r.child_value("TITL"));
            if title.is_empty() { genie_core::media::file_name(file.map(|f| f.value.as_str()).unwrap_or("")) } else { title.to_string() }
        }
        "NOTE" => r.value.lines().next().unwrap_or("").to_string(),
        _ => String::new(),
    };
    text.chars().take(250).collect()
}

/// Saves `text` as the new head, recording `changes` against `user`. Runs in
/// the caller's transaction, which must hold the tree row's lock.
pub async fn commit_revision(
    tx: &mut MySqlConnection,
    parent: Option<i64>,
    user: i32,
    text: &str,
    people: usize,
    note: &str,
    changes: &[(Change, String)],
) -> ApiResult<i64> {
    let rev = sqlx::query("INSERT INTO revisions (parent_id, user_id, gedcom, people_count, note) VALUES (?, ?, ?, ?, ?)")
        .bind(parent)
        .bind(user)
        .bind(gzip(text))
        .bind(people as i32)
        .bind(note)
        .execute(&mut *tx)
        .await?
        .last_insert_id() as i64;
    for (c, label) in changes {
        sqlx::query("INSERT INTO changes (revision_id, xref, record_tag, action, label, user_id) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(rev)
            .bind(&c.xref)
            .bind(&c.tag)
            .bind(c.action.as_str())
            .bind(label)
            .bind(user)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE tree SET head_revision_id = ? WHERE id = 1").bind(rev).execute(&mut *tx).await?;
    Ok(rev)
}

/// Labels for `changes`: from the new tree, or for deletions the old one.
pub fn labelled(changes: Vec<Change>, old: &Document, new: &Document) -> Vec<(Change, String)> {
    changes
        .into_iter()
        .map(|c| {
            let l = if c.action == Action::Delete { label(old, &c.xref) } else { label(new, &c.xref) };
            (c, l)
        })
        .collect()
}

/// Locks the tree row for the rest of the transaction, so saves apply one at a time.
pub async fn lock_tree(tx: &mut MySqlConnection) -> ApiResult<Option<i64>> {
    let row: (Option<i64>,) = sqlx::query_as("SELECT head_revision_id FROM tree WHERE id = 1 FOR UPDATE").fetch_one(&mut *tx).await?;
    Ok(row.0)
}

/// Every document path the records link to, as stored in the media table.
pub fn linked_files(records: &[Node]) -> HashSet<String> {
    fn walk(n: &Node, out: &mut HashSet<String>) {
        for c in &n.children {
            if c.tag == "FILE" && n.tag == "OBJE" {
                out.insert(normalize_path(&c.value));
            }
            walk(c, out);
        }
    }
    let mut out = HashSet::new();
    for r in records {
        walk(r, &mut out);
    }
    out.remove("");
    out
}

/// A FILE value the way the media table keys it.
pub fn normalize_path(file: &str) -> String {
    file.trim().trim_start_matches("file://").replace('\\', "/").trim_start_matches("./").to_string()
}
