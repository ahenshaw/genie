//! The HTTP API, all under `/api`.

use std::collections::HashMap;

use axum::extract::{DefaultBodyLimit, Path, Query, State as AxState};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use genie_core::model::Document;
use genie_core::sync::{self, ConflictKind, Side};
use serde::{Deserialize, Serialize};

use crate::auth::{self, ClientIp, CurrentUser, Role};
use crate::error::{ApiError, ApiResult};
use crate::{State, media, store};

/// Largest tree accepted in one save.
const MAX_TREE: usize = 64 * 1024 * 1024;

pub fn router(state: State) -> Router {
    let web = state.config.web_dir.clone();
    let api = Router::new()
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/me", get(me))
        .route("/api/password", post(change_password))
        .route("/api/tree", get(get_tree).post(save_tree).layer(DefaultBodyLimit::max(MAX_TREE)))
        .route("/api/changes", get(changes))
        .route("/api/users", get(list_users).post(add_user))
        .route("/api/users/{id}", patch(update_user))
        .route("/api/media", get(media::manifest).put(media::upload).layer(DefaultBodyLimit::disable()))
        .route("/api/media/{sha}", get(media::download))
        .route("/api/import", post(media::import).layer(DefaultBodyLimit::disable()))
        .with_state(state);
    // The browser app, when it's installed: everything that isn't the API.
    let app = match web {
        Some(dir) => api.fallback_service(tower_http::services::ServeDir::new(dir)),
        None => api,
    };
    app.layer(axum::middleware::from_fn(response_headers))
}

/// Security headers on everything, and caching for the app's files: its
/// page is checked every time, while trunk names the rest by content hash.
async fn response_headers(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let path = req.uri().path().to_string();
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(header::X_CONTENT_TYPE_OPTIONS, header::HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, header::HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, header::HeaderValue::from_static("same-origin"));
    if !path.starts_with("/api/") && !h.contains_key(header::CACHE_CONTROL) {
        let cache = if path == "/" || path.ends_with(".html") { "no-cache" } else { "public, max-age=31536000, immutable" };
        h.insert(header::CACHE_CONTROL, header::HeaderValue::from_static(cache));
    }
    resp
}

// ---- sessions ----------------------------------------------------------------------------

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
    /// A browser: also set the session cookie.
    #[serde(default)]
    cookie: bool,
}

#[derive(Serialize)]
struct Session {
    token: String,
    expires_at: u64,
    user: CurrentUser,
}

async fn login(AxState(st): AxState<State>, ip: ClientIp, Json(req): Json<Login>) -> ApiResult<Response> {
    let username = req.username.trim().to_lowercase();
    let mut keys = vec![format!("u:{username}")];
    if let Some(ip) = ip.0 {
        keys.push(format!("ip:{ip}"));
    }
    if let Some(wait) = st.throttle.locked(&keys) {
        let mins = wait.as_secs().div_ceil(60);
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, format!("Too many wrong passwords. Try again in {mins} minute{}.", if mins == 1 { "" } else { "s" })));
    }
    let row: Option<(i32, String, String, String, bool, i32)> = sqlx::query_as(
        "SELECT id, password_hash, display_name, CAST(role AS CHAR), disabled, session_epoch FROM users WHERE username = ?",
    )
    .bind(&username)
    .fetch_optional(&st.pool)
    .await?;
    let hash = row.as_ref().map(|r| r.1.clone()).unwrap_or_else(|| auth::dummy_hash().to_string());
    let password = req.password;
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&hash, &password)).await.map_err(ApiError::internal)?;
    // A disabled account gets the same answer as a wrong password.
    let Some((id, _, display_name, role, _, epoch)) = row.filter(|r| ok && !r.4) else {
        st.throttle.failed(&keys);
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Wrong username or password."));
    };
    st.throttle.succeeded(&keys);
    sqlx::query("UPDATE users SET last_login_at = CURRENT_TIMESTAMP WHERE id = ?").bind(id).execute(&st.pool).await?;
    let role = Role::parse(&role).ok_or_else(|| ApiError::internal("unknown role"))?;
    let expires_at = auth::now() + auth::TOKEN_LIFETIME;
    let token = auth::mint_token(&st.config.session_secret, id, epoch, expires_at);
    let cookie = req.cookie.then(|| auth::session_cookie(Some(&token)));
    let session = Json(Session { token, expires_at, user: CurrentUser { id, username, display_name, role } });
    Ok(match cookie {
        Some(c) => ([(header::SET_COOKIE, c)], session).into_response(),
        None => session.into_response(),
    })
}

/// Signs the account out everywhere: every token it holds stops working.
async fn logout(AxState(st): AxState<State>, user: CurrentUser) -> ApiResult<Response> {
    sqlx::query("UPDATE users SET session_epoch = session_epoch + 1 WHERE id = ?").bind(user.id).execute(&st.pool).await?;
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, auth::session_cookie(None))]).into_response())
}

async fn me(user: CurrentUser) -> Json<CurrentUser> {
    Json(user)
}

#[derive(Deserialize)]
struct PasswordChange {
    current: String,
    new: String,
}

/// Changes your own password; other sessions end, and this one gets a new token.
async fn change_password(AxState(st): AxState<State>, user: CurrentUser, headers: HeaderMap, Json(req): Json<PasswordChange>) -> ApiResult<Response> {
    auth::check_new_password(&req.new)?;
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(user.id).fetch_one(&st.pool).await?;
    let current = req.current;
    let new = req.new;
    let (ok, new_hash) = tokio::task::spawn_blocking(move || (auth::verify_password(&hash, &current), auth::hash_password(&new)))
        .await
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "Your current password is wrong."));
    }
    sqlx::query("UPDATE users SET password_hash = ?, session_epoch = session_epoch + 1 WHERE id = ?").bind(&new_hash).bind(user.id).execute(&st.pool).await?;
    let (epoch,): (i32,) = sqlx::query_as("SELECT session_epoch FROM users WHERE id = ?").bind(user.id).fetch_one(&st.pool).await?;
    let expires_at = auth::now() + auth::TOKEN_LIFETIME;
    let token = auth::mint_token(&st.config.session_secret, user.id, epoch, expires_at);
    // A browser's cookie held the old token, which just stopped working.
    let cookie = headers.contains_key(header::COOKIE).then(|| auth::session_cookie(Some(&token)));
    let session = Json(Session { token, expires_at, user });
    Ok(match cookie {
        Some(c) => ([(header::SET_COOKIE, c)], session).into_response(),
        None => session.into_response(),
    })
}

// ---- accounts (admin) -----------------------------------------------------------------------

#[derive(Serialize, sqlx::FromRow)]
struct Account {
    id: i32,
    username: String,
    display_name: String,
    role: String,
    disabled: bool,
    created_at: String,
    last_login_at: Option<String>,
}

async fn list_users(AxState(st): AxState<State>, user: CurrentUser) -> ApiResult<Json<Vec<Account>>> {
    user.require(Role::Admin)?;
    let rows = sqlx::query_as::<_, Account>(
        "SELECT id, username, display_name, CAST(role AS CHAR) AS role, disabled,
                DATE_FORMAT(created_at, '%Y-%m-%dT%H:%i:%sZ') AS created_at,
                DATE_FORMAT(last_login_at, '%Y-%m-%dT%H:%i:%sZ') AS last_login_at
         FROM users ORDER BY username",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize)]
struct NewAccount {
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
    role: Role,
}

async fn add_user(AxState(st): AxState<State>, user: CurrentUser, Json(req): Json<NewAccount>) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    user.require(Role::Admin)?;
    let id = auth::create_user(&st.pool, &req.username, &req.display_name, &req.password, req.role).await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

#[derive(Deserialize)]
struct AccountUpdate {
    display_name: Option<String>,
    role: Option<Role>,
    disabled: Option<bool>,
    password: Option<String>,
}

/// Changes an account. A new role, password or disabling signs it out everywhere.
async fn update_user(AxState(st): AxState<State>, user: CurrentUser, Path(id): Path<i32>, Json(req): Json<AccountUpdate>) -> ApiResult<StatusCode> {
    user.require(Role::Admin)?;
    let row: Option<(String, bool)> = sqlx::query_as("SELECT CAST(role AS CHAR), disabled FROM users WHERE id = ?").bind(id).fetch_optional(&st.pool).await?;
    let (role, disabled) = row.ok_or_else(ApiError::not_found)?;
    let was_active_admin = role == "admin" && !disabled;
    let stays_active_admin = req.role.map_or(role == "admin", |r| r == Role::Admin) && !req.disabled.unwrap_or(disabled);
    if was_active_admin && !stays_active_admin {
        let (admins,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users WHERE role = 'admin' AND NOT disabled").fetch_one(&st.pool).await?;
        if admins <= 1 {
            return Err(ApiError::new(StatusCode::CONFLICT, "That's the last administrator; make someone else one first."));
        }
    }
    let mut tx = st.pool.begin().await?;
    let mut revoke = false;
    if let Some(name) = &req.display_name {
        sqlx::query("UPDATE users SET display_name = ? WHERE id = ?").bind(name.trim()).bind(id).execute(&mut *tx).await?;
    }
    if let Some(role) = req.role {
        sqlx::query("UPDATE users SET role = ? WHERE id = ?").bind(role.as_str()).bind(id).execute(&mut *tx).await?;
        revoke = true;
    }
    if let Some(d) = req.disabled {
        sqlx::query("UPDATE users SET disabled = ? WHERE id = ?").bind(d).bind(id).execute(&mut *tx).await?;
        revoke = true;
    }
    if let Some(p) = req.password {
        auth::check_new_password(&p)?;
        let hash = tokio::task::spawn_blocking(move || auth::hash_password(&p)).await.map_err(ApiError::internal)?;
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?").bind(&hash).bind(id).execute(&mut *tx).await?;
        revoke = true;
    }
    if revoke {
        sqlx::query("UPDATE users SET session_epoch = session_epoch + 1 WHERE id = ?").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- the tree ---------------------------------------------------------------------------------

fn etag(rev: i64) -> String {
    format!("\"rev-{rev}\"")
}

#[derive(Serialize)]
struct TreeResponse<'a> {
    /// `None` until the first revision is saved.
    revision: Option<i64>,
    role: Role,
    gedcom: &'a str,
}

/// The current tree as the caller may see it. Answers 304 when `If-None-Match`
/// names the current revision, so checking for news is cheap.
async fn get_tree(AxState(st): AxState<State>, user: CurrentUser, headers: HeaderMap) -> ApiResult<Response> {
    let Some(head) = store::head(&st).await? else {
        let empty = Document::new_empty().to_gedcom();
        return Ok(Json(TreeResponse { revision: None, role: user.role, gedcom: &empty }).into_response());
    };
    let tag = etag(head.id);
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(tag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, tag)]).into_response());
    }
    let body = TreeResponse { revision: Some(head.id), role: user.role, gedcom: head.text_for(user.role, st.config.living_years) };
    Ok(([(header::ETAG, tag.clone()), (header::CACHE_CONTROL, "no-store".to_string())], Json(body)).into_response())
}

#[derive(Deserialize)]
struct Save {
    /// The revision the edits were made on; `None` only for the very first save.
    base_revision: Option<i64>,
    gedcom: String,
    /// For records that conflicted last time: whose version to keep.
    #[serde(default)]
    resolve: HashMap<String, Resolution>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Resolution {
    Mine,
    Theirs,
}

#[derive(Serialize)]
struct Saved {
    revision: i64,
    /// Nothing differed from the current tree, so no revision was made.
    unchanged: bool,
    /// Other people's changes were merged in: `gedcom` is the result, which
    /// the client should adopt.
    merged: bool,
    /// New records of yours renumbered because someone else had used the number.
    renamed: Vec<(String, String)>,
    changes: usize,
    gedcom: Option<String>,
}

#[derive(Serialize)]
struct ConflictOut {
    xref: String,
    tag: String,
    kind: &'static str,
    /// For a link to a deleted record: the record it pointed at.
    target: Option<String>,
    label: String,
    /// Who else changed it since your base revision.
    changed_by: Vec<String>,
}

/// Saves an edited tree. Changes others made since `base_revision` are
/// merged in; records both sides changed come back as 409 conflicts, to be
/// resubmitted with `resolve`.
async fn save_tree(AxState(st): AxState<State>, user: CurrentUser, Json(req): Json<Save>) -> ApiResult<Response> {
    user.require(Role::Editor)?;
    let _serial = st.write_lock.lock().await;
    let mut tx = st.pool.begin().await?;
    let head_id = store::lock_tree(&mut tx).await?;

    let submitted = tokio::task::spawn_blocking(move || {
        let (doc, _) = Document::from_bytes(req.gedcom.as_bytes());
        store::parse(&doc.to_gedcom())
    })
    .await
    .map_err(ApiError::internal)?;

    // The records the edit was made on, and the current ones.
    let (base_records, head_records) = match (req.base_revision, head_id) {
        (_, None) => {
            // The first save creates the tree: only an administrator may.
            user.require(Role::Admin)?;
            (Vec::new(), Vec::new())
        }
        (None, Some(_)) => return Err(ApiError::new(StatusCode::CONFLICT, "The tree already exists; fetch it first.")),
        (Some(base), Some(head)) => {
            let head_text = store::revision_text(&st.pool, head).await?.ok_or_else(|| ApiError::internal("head missing"))?;
            let base_text = if base == head {
                head_text.clone()
            } else {
                store::revision_text(&st.pool, base).await?.ok_or_else(|| ApiError::bad_request(format!("There's no revision {base}.")))?
            };
            (store::parse(&base_text), store::parse(&head_text))
        }
    };

    let resolve = req.resolve;
    let merge_needed = head_id.is_some() && req.base_revision != head_id;
    let outcome = tokio::task::spawn_blocking(move || {
        let merged = if merge_needed {
            sync::merge_resolving(&base_records, &submitted, &head_records, |x| {
                resolve.get(x).map(|r| match r {
                    Resolution::Mine => Side::Ours,
                    Resolution::Theirs => Side::Theirs,
                })
            })
            .map(|m| (m.records, m.renamed))
        } else {
            Ok((submitted, Vec::new()))
        };
        merged.map(|(records, renamed)| {
            let (new_doc, text) = store::canonical(records);
            let old_doc = Document::from_records(head_records.clone());
            let changes = sync::diff(&head_records, &store::parse(&text));
            let changes = store::labelled(changes, &old_doc, &new_doc);
            (text, new_doc.people().len(), changes, renamed, old_doc)
        })
        .map_err(|conflicts| (conflicts, Document::from_records(head_records), Document::from_records(base_records)))
    })
    .await
    .map_err(ApiError::internal)?;

    match outcome {
        Err((conflicts, head_doc, base_doc)) => {
            let base = req.base_revision.unwrap_or(0);
            let mut out = Vec::new();
            for c in conflicts {
                let names: Vec<(String, String)> = sqlx::query_as(
                    "SELECT DISTINCT u.username, u.display_name FROM changes c JOIN users u ON u.id = c.user_id
                     WHERE c.xref = ? AND c.revision_id > ?",
                )
                .bind(&c.xref)
                .bind(base)
                .fetch_all(&mut *tx)
                .await?;
                let label = Some(store::label(&head_doc, &c.xref)).filter(|l| !l.is_empty()).unwrap_or_else(|| store::label(&base_doc, &c.xref));
                let (kind, target) = match c.kind {
                    ConflictKind::BothChanged => ("both_changed", None),
                    ConflictKind::ChangedAndDeleted => ("changed_and_deleted", None),
                    ConflictKind::DanglingLink { target } => ("dangling_link", Some(target)),
                };
                out.push(ConflictOut {
                    xref: c.xref,
                    tag: c.tag,
                    kind,
                    target,
                    label,
                    changed_by: names.into_iter().map(|(u, d)| if d.is_empty() { u } else { d }).collect(),
                });
            }
            let body = serde_json::json!({ "error": "Someone else changed the same records.", "head_revision": head_id, "conflicts": out });
            Ok((StatusCode::CONFLICT, Json(body)).into_response())
        }
        Ok((text, people, changes, renamed, _)) => {
            if changes.is_empty() && head_id.is_some() {
                let rev = head_id.expect("checked");
                return Ok(Json(Saved { revision: rev, unchanged: true, merged: merge_needed, renamed, changes: 0, gedcom: merge_needed.then_some(text) }).into_response());
            }
            let rev = store::commit_revision(&mut tx, head_id, user.id, &text, people, "", &changes).await?;
            tx.commit().await?;
            let send_back = merge_needed || !renamed.is_empty();
            Ok(Json(Saved { revision: rev, unchanged: false, merged: merge_needed, changes: changes.len(), renamed, gedcom: send_back.then_some(text) }).into_response())
        }
    }
}

// ---- history ------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChangesQuery {
    /// Only changes made after this revision.
    since: Option<i64>,
    /// Only changes to this record.
    xref: Option<String>,
}

#[derive(Serialize, sqlx::FromRow)]
struct ChangeOut {
    revision: i64,
    xref: String,
    tag: String,
    action: String,
    label: String,
    user: Option<String>,
    at: String,
}

/// Who changed what: since a revision (for "3 changes by Alice"), or for one
/// record (its history). Newest first. Guests see only records they see in full.
async fn changes(AxState(st): AxState<State>, user: CurrentUser, Query(q): Query<ChangesQuery>) -> ApiResult<Json<Vec<ChangeOut>>> {
    let rows = sqlx::query_as::<_, ChangeOut>(
        "SELECT c.revision_id AS revision, c.xref, c.record_tag AS tag, CAST(c.action AS CHAR) AS action, c.label,
                COALESCE(NULLIF(u.display_name, ''), u.username) AS user,
                DATE_FORMAT(c.created_at, '%Y-%m-%dT%H:%i:%sZ') AS at
         FROM changes c LEFT JOIN users u ON u.id = c.user_id
         WHERE c.revision_id > ? AND (? IS NULL OR c.xref = ?)
         ORDER BY c.revision_id DESC, c.id DESC
         LIMIT 500",
    )
    .bind(q.since.unwrap_or(0))
    .bind(&q.xref)
    .bind(&q.xref)
    .fetch_all(&st.pool)
    .await?;
    if user.role != Role::Guest {
        return Ok(Json(rows));
    }
    let Some(head) = store::head(&st).await? else { return Ok(Json(Vec::new())) };
    let visible = rows.into_iter().filter(|r| head.history_visible(Role::Guest, st.config.living_years, &r.xref)).collect();
    Ok(Json(visible))
}
