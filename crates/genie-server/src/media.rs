//! Documents, and importing a whole tree from a `.gdz` bundle.
//!
//! A document is known by the path the tree's `FILE` line gives it, and
//! stored once per content as `media_dir/<sha256>`. Callers may list and
//! fetch only the documents linked from the part of the tree they see.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State as AxState};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::StreamExt;
use genie_core::model::Document;
use genie_core::sync;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::auth::{CurrentUser, Role};
use crate::error::{ApiError, ApiResult};
use crate::{State, store};

/// Largest single document.
const MAX_FILE: u64 = 512 * 1024 * 1024;
/// Largest bundle.
const MAX_BUNDLE: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Serialize, sqlx::FromRow)]
pub struct MediaFile {
    path: String,
    sha256: String,
    size: i64,
}

/// The documents the caller's view of the tree links to.
pub async fn manifest(AxState(st): AxState<State>, user: CurrentUser) -> ApiResult<Json<Vec<MediaFile>>> {
    let Some(head) = store::head(&st).await? else { return Ok(Json(Vec::new())) };
    let linked = store::linked_files(head.records_for(user.role, st.config.living_years));
    let rows = sqlx::query_as::<_, MediaFile>("SELECT CAST(path AS CHAR) AS path, sha256, size FROM media ORDER BY path").fetch_all(&st.pool).await?;
    Ok(Json(rows.into_iter().filter(|m| linked.contains(&m.path)).collect()))
}

/// One document, if the caller's view links to it. Anything else is 404,
/// so a guest can't even learn that a living person's photo exists.
pub async fn download(AxState(st): AxState<State>, user: CurrentUser, UrlPath(sha): UrlPath<String>) -> ApiResult<Response> {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::not_found());
    }
    let head = store::head(&st).await?.ok_or_else(ApiError::not_found)?;
    let linked = store::linked_files(head.records_for(user.role, st.config.living_years));
    let paths: Vec<(String,)> = sqlx::query_as("SELECT CAST(path AS CHAR) FROM media WHERE sha256 = ?").bind(&sha).fetch_all(&st.pool).await?;
    let Some((path,)) = paths.into_iter().find(|(p,)| linked.contains(p)) else { return Err(ApiError::not_found()) };
    let file = tokio::fs::File::open(st.config.media_dir.join(&sha)).await.map_err(|_| ApiError::not_found())?;
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok(([(header::CONTENT_TYPE, mime.essence_str().to_string()), (header::CACHE_CONTROL, "private, max-age=31536000, immutable".to_string())], body).into_response())
}

fn temp_path(dir: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    dir.join(format!(".incoming-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// Stores a stream as `dir/<sha256>`, returning (sha, size). Refuses more
/// than `limit` bytes.
async fn store_stream(dir: &Path, body: Body, limit: u64) -> ApiResult<(String, u64)> {
    tokio::fs::create_dir_all(dir).await?;
    let tmp = temp_path(dir);
    let result = async {
        let mut out = tokio::fs::File::create(&tmp).await?;
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::bad_request(format!("Upload interrupted: {e}")))?;
            size += chunk.len() as u64;
            if size > limit {
                return Err(ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "That file is too large."));
            }
            hasher.update(&chunk);
            out.write_all(&chunk).await?;
        }
        out.flush().await?;
        out.sync_all().await?;
        Ok::<_, ApiError>((hex(&hasher.finalize()), size))
    }
    .await;
    match result {
        Ok((sha, size)) => {
            let dest = dir.join(&sha);
            if tokio::fs::try_exists(&dest).await? {
                tokio::fs::remove_file(&tmp).await?;
            } else {
                tokio::fs::rename(&tmp, &dest).await?;
            }
            Ok((sha, size))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Deserialize)]
pub struct UploadQuery {
    /// The tree's `FILE` path for it.
    path: String,
}

async fn record_file(pool: &sqlx::MySqlPool, path: &str, sha: &str, size: u64, user: i32) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO media (path, sha256, size, uploaded_by) VALUES (?, ?, ?, ?)
         ON DUPLICATE KEY UPDATE sha256 = VALUES(sha256), size = VALUES(size), uploaded_by = VALUES(uploaded_by), uploaded_at = CURRENT_TIMESTAMP",
    )
    .bind(path)
    .bind(sha)
    .bind(size as i64)
    .bind(user)
    .execute(pool)
    .await?;
    Ok(())
}

/// Adds or replaces the document at `path`. Editors only.
pub async fn upload(AxState(st): AxState<State>, user: CurrentUser, Query(q): Query<UploadQuery>, body: Body) -> ApiResult<Json<MediaFile>> {
    user.require(Role::Editor)?;
    let path = genie_core::bundle::safe_relative(&q.path).ok_or_else(|| ApiError::bad_request("Documents need a relative path inside the tree's folder."))?;
    let (sha, size) = store_stream(&st.config.media_dir, body, MAX_FILE).await?;
    record_file(&st.pool, &path, &sha, size, user.id).await?;
    Ok(Json(MediaFile { path, sha256: sha, size: size as i64 }))
}

#[derive(Deserialize)]
pub struct ImportQuery {
    /// Replace an existing tree. Without it, importing over one is refused.
    #[serde(default)]
    replace: bool,
}

#[derive(Serialize)]
pub struct Imported {
    revision: i64,
    people: usize,
    files: usize,
}

/// Replaces the tree, and adds its documents, from a `.gdz` bundle. The old
/// tree stays in the history. Administrators only.
pub async fn import(AxState(st): AxState<State>, user: CurrentUser, Query(q): Query<ImportQuery>, body: Body) -> ApiResult<Json<Imported>> {
    user.require(Role::Admin)?;
    // The bundle is kept on disk while it's read, beside the documents.
    let incoming = st.config.media_dir.join(".bundles");
    let (sha, _) = store_stream(&incoming, body, MAX_BUNDLE).await?;
    let bundle = incoming.join(&sha);
    let media_dir = st.config.media_dir.clone();
    let unpacked = tokio::task::spawn_blocking(move || unpack(&bundle, &media_dir)).await.map_err(ApiError::internal);
    let _ = tokio::fs::remove_file(incoming.join(&sha)).await;
    let (text, files) = unpacked??;

    let _serial = st.write_lock.lock().await;
    let mut tx = st.pool.begin().await?;
    let head_id = store::lock_tree(&mut tx).await?;
    if head_id.is_some() && !q.replace {
        return Err(ApiError::new(StatusCode::CONFLICT, "There's already a tree; import again with replace to replace it."));
    }
    let old_records = match head_id {
        Some(h) => store::parse(&store::revision_text(&st.pool, h).await?.unwrap_or_default()),
        None => Vec::new(),
    };
    let (new_doc, text, changes) = tokio::task::spawn_blocking(move || {
        let (doc, _) = Document::from_bytes(text.as_bytes());
        let text = doc.to_gedcom();
        let changes = sync::diff(&old_records, &store::parse(&text));
        let changes = store::labelled(changes, &Document::from_records(old_records), &doc);
        (doc, text, changes)
    })
    .await
    .map_err(ApiError::internal)?;
    for (path, sha, size) in &files {
        record_file(&st.pool, path, sha, *size, user.id).await?;
    }
    let rev = store::commit_revision(&mut tx, head_id, user.id, &text, new_doc.people().len(), "Imported from a bundle", &changes).await?;
    tx.commit().await?;
    Ok(Json(Imported { revision: rev, people: new_doc.people().len(), files: files.len() }))
}

/// The tree's text and its documents (path, sha, size), stored in `media_dir`.
fn unpack(bundle: &Path, media_dir: &Path) -> ApiResult<(String, Vec<(String, String, u64)>)> {
    let file = std::fs::File::open(bundle)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| ApiError::bad_request(format!("That isn't a readable bundle: {e}")))?;
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let top_ged: Vec<&String> = names.iter().filter(|n| !n.contains('/') && n.to_lowercase().ends_with(".ged")).collect();
    let tree_entry = match names.iter().find(|n| *n == "gedcom.ged") {
        Some(t) => t.clone(),
        None if top_ged.len() == 1 => top_ged[0].clone(),
        None => return Err(ApiError::bad_request("The bundle has no gedcom.ged in it.")),
    };
    let mut text = Vec::new();
    std::io::Read::read_to_end(&mut zip.by_name(&tree_entry).map_err(ApiError::internal)?, &mut text)?;
    let text = genie_core::gedcom::decode(&text).text;

    let mut files = Vec::new();
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(ApiError::internal)?;
        if entry.is_dir() || entry.name() == tree_entry {
            continue;
        }
        let Some(rel) = entry.enclosed_name() else {
            return Err(ApiError::bad_request(format!("The bundle has an unsafe path in it: {}", entry.name())));
        };
        let path = genie_core::media::path_string(&rel);
        let tmp = temp_path(media_dir);
        let mut out = std::fs::File::create(&tmp)?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            let n = std::io::Read::read(&mut entry, &mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            std::io::Write::write_all(&mut out, &buf[..n])?;
            size += n as u64;
        }
        out.sync_all()?;
        let sha = hex(&hasher.finalize());
        let dest = media_dir.join(&sha);
        if dest.exists() {
            std::fs::remove_file(&tmp)?;
        } else {
            std::fs::rename(&tmp, &dest)?;
        }
        files.push((path, sha, size));
    }
    Ok((text, files))
}
