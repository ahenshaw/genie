//! The API against a real MySQL: each test makes a scratch database, and
//! drops it at the end.
//!
//! Set `GENIE_TEST_MYSQL` to a server URL with rights to create databases,
//! e.g. `mysql://root:genie-test@127.0.0.1:33306`. Without it these tests
//! pass without doing anything, so a plain `cargo test` works anywhere.

use std::sync::atomic::{AtomicU32, Ordering};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use genie_server::auth::{self, Role};
use genie_server::{AppState, Config, MIGRATOR};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::mysql::MySqlPoolOptions;
use sqlx::{Executor, MySqlPool};
use tower::ServiceExt;

const PASSWORD: &str = "password-123";

struct Env {
    app: Router,
    pool: MySqlPool,
    root: MySqlPool,
    db: String,
    media: std::path::PathBuf,
}

impl Env {
    async fn new() -> Option<Env> {
        Self::with_web(None).await
    }

    async fn with_web(web_dir: Option<std::path::PathBuf>) -> Option<Env> {
        let Ok(url) = std::env::var("GENIE_TEST_MYSQL") else {
            eprintln!("GENIE_TEST_MYSQL not set; skipping");
            return None;
        };
        static N: AtomicU32 = AtomicU32::new(0);
        let db = format!("genie_test_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed));
        let root = MySqlPoolOptions::new().max_connections(2).connect(&url).await.expect("test MySQL reachable");
        root.execute(format!("CREATE DATABASE {db}").as_str()).await.unwrap();
        let pool = MySqlPoolOptions::new().max_connections(5).connect(&format!("{url}/{db}")).await.unwrap();
        MIGRATOR.run(&pool).await.unwrap();
        let media = std::env::temp_dir().join(&db);
        let _ = std::fs::remove_dir_all(&media);
        let state = AppState::new(pool.clone(), Config { session_secret: b"test-secret-that-is-plenty-long-enough".to_vec(), media_dir: media.clone(), living_years: 100, web_dir });
        Some(Env { app: genie_server::router(state), pool, root, db, media })
    }

    async fn done(self) {
        self.pool.close().await;
        self.root.execute(format!("DROP DATABASE {}", self.db).as_str()).await.unwrap();
        let _ = std::fs::remove_dir_all(&self.media);
    }

    async fn user(&self, name: &str, display: &str, role: Role) -> String {
        auth::create_user(&self.pool, name, display, PASSWORD, role).await.unwrap();
        let (status, body) = self.call("POST", "/api/login", None, Some(json!({"username": name, "password": PASSWORD}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, Vec<u8>) {
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec())
    }

    async fn call(&self, method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let (status, bytes) = self.send(req).await;
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn raw(&self, method: &str, uri: &str, token: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
        let req = Request::builder().method(method).uri(uri).header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::from(body)).unwrap();
        self.send(req).await
    }

    /// Seeds the tree as `admin`; returns the revision.
    async fn seed(&self, admin: &str, gedcom: &str) -> i64 {
        let (status, body) = self.call("POST", "/api/tree", Some(admin), Some(json!({"base_revision": null, "gedcom": gedcom}))).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["revision"].as_i64().unwrap()
    }

    async fn tree(&self, token: &str) -> (i64, String) {
        let (status, body) = self.call("GET", "/api/tree", Some(token), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (body["revision"].as_i64().unwrap(), body["gedcom"].as_str().unwrap().to_string())
    }
}

fn sample() -> String {
    String::from_utf8(genie_core::SAMPLE_GED.to_vec()).unwrap()
}

#[tokio::test]
async fn roles_see_and_do_what_they_should() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "The Admin", Role::Admin).await;
    let family = env.user("fam", "", Role::Family).await;
    let guest = env.user("guest", "", Role::Guest).await;

    let (status, _) = env.call("POST", "/api/login", None, Some(json!({"username": "admin", "password": "nope-nope-nope"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = env.call("GET", "/api/tree", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Before anything is saved there's an empty tree; only an admin may create it.
    let (status, body) = env.call("GET", "/api/tree", Some(&family), None).await;
    assert_eq!((status, body["revision"].clone()), (StatusCode::OK, Value::Null));
    let rev = env.seed(&admin, &sample()).await;

    let (_, full) = env.tree(&family).await;
    assert!(full.contains("Linda /Ferris/") && full.contains("Hartford"));
    let (_, guest_view) = env.tree(&guest).await;
    assert!(!guest_view.contains("Ferris") && !guest_view.contains("Hartford") && !guest_view.contains("DATE 1934\r"));
    assert!(guest_view.contains("Thomas /Hartwell/") && guest_view.contains("NAME Private"));

    for (who, token) in [("family", &family), ("guest", &guest)] {
        let (status, _) = env.call("POST", "/api/tree", Some(token), Some(json!({"base_revision": rev, "gedcom": full}))).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who} saved");
        let (status, _) = env.call("GET", "/api/users", Some(token), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who} listed accounts");
    }

    // Nothing new: 304.
    let req = Request::builder().uri("/api/tree").header(header::AUTHORIZATION, format!("Bearer {family}")).header(header::IF_NONE_MATCH, format!("\"rev-{rev}\"")).body(Body::empty()).unwrap();
    assert_eq!(env.send(req).await.0, StatusCode::NOT_MODIFIED);
    env.done().await;
}

#[tokio::test]
async fn editors_merge_and_conflicts_name_who_changed_what() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "", Role::Admin).await;
    let alice = env.user("alice", "Alice", Role::Editor).await;
    let bob = env.user("bob", "Bob", Role::Editor).await;
    let guest = env.user("guest", "", Role::Guest).await;
    let r1 = env.seed(&admin, &sample()).await;
    let (_, base) = env.tree(&alice).await;

    // Alice renames Thomas.
    let alice_text = base.replace("NAME Thomas /Hartwell/", "NAME Tom /Hartwell/");
    let (status, body) = env.call("POST", "/api/tree", Some(&alice), Some(json!({"base_revision": r1, "gedcom": alice_text}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!((body["merged"].as_bool(), body["changes"].as_i64()), (Some(false), Some(1)));

    // Bob, still on r1, changes Margaret: merged with Alice's change.
    let bob_text = base.replace("NAME Margaret /Doyle/", "NAME Maggie /Doyle/");
    let (status, body) = env.call("POST", "/api/tree", Some(&bob), Some(json!({"base_revision": r1, "gedcom": bob_text}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["merged"].as_bool(), Some(true));
    let merged = body["gedcom"].as_str().unwrap();
    assert!(merged.contains("Tom /Hartwell/") && merged.contains("Maggie /Doyle/"));

    // Bob, still on r1, renames Thomas too: a conflict naming Alice.
    let clash = base.replace("NAME Thomas /Hartwell/", "NAME Thos /Hartwell/");
    let (status, body) = env.call("POST", "/api/tree", Some(&bob), Some(json!({"base_revision": r1, "gedcom": clash}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let c = &body["conflicts"][0];
    assert_eq!((c["xref"].as_str(), c["kind"].as_str()), (Some("I1"), Some("both_changed")));
    assert_eq!(c["changed_by"], json!(["Alice"]));

    // Resubmitted keeping his version, it goes in.
    let (status, body) = env.call("POST", "/api/tree", Some(&bob), Some(json!({"base_revision": r1, "gedcom": clash, "resolve": {"I1": "mine"}}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let merged = body["gedcom"].as_str().unwrap();
    assert!(merged.contains("Thos /Hartwell/") && merged.contains("Maggie /Doyle/"));

    // Thomas's history, newest first, tagged with who did it.
    let (_, hist) = env.call("GET", "/api/changes?xref=I1", Some(&alice), None).await;
    let who: Vec<&str> = hist.as_array().unwrap().iter().map(|c| c["user"].as_str().unwrap()).collect();
    assert_eq!(who[..2], ["Bob", "Alice"]);
    assert_eq!(hist[0]["label"], "Thos Hartwell");

    // Changes since a revision; guests only see the deceased's.
    let (_, since) = env.call("GET", &format!("/api/changes?since={r1}"), Some(&alice), None).await;
    assert_eq!(since.as_array().unwrap().len(), 3);
    let (_, seeded) = env.call("GET", "/api/changes?xref=I14", Some(&guest), None).await;
    assert_eq!(seeded, json!([]), "a living person's history reached a guest");
    let (_, dead) = env.call("GET", "/api/changes?xref=I1", Some(&guest), None).await;
    assert!(!dead.as_array().unwrap().is_empty());

    // Saving what's already there makes no revision.
    let (head, text) = env.tree(&alice).await;
    let (_, body) = env.call("POST", "/api/tree", Some(&alice), Some(json!({"base_revision": head, "gedcom": text}))).await;
    assert_eq!((body["unchanged"].as_bool(), body["revision"].as_i64()), (Some(true), Some(head)));
    env.done().await;
}

#[tokio::test]
async fn role_changes_and_sign_outs_end_sessions() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "", Role::Admin).await;
    let ed = env.user("ed", "", Role::Editor).await;
    let (_, me) = env.call("GET", "/api/me", Some(&ed), None).await;
    assert_eq!(me["role"], "editor");
    let id = me["id"].as_i64().unwrap();

    let (status, _) = env.call("PATCH", &format!("/api/users/{id}"), Some(&admin), Some(json!({"role": "family"}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(env.call("GET", "/api/me", Some(&ed), None).await.0, StatusCode::UNAUTHORIZED);

    // The last administrator can't be demoted or disabled.
    let (_, me) = env.call("GET", "/api/me", Some(&admin), None).await;
    let admin_id = me["id"].as_i64().unwrap();
    let (status, _) = env.call("PATCH", &format!("/api/users/{admin_id}"), Some(&admin), Some(json!({"disabled": true}))).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Changing your password keeps you signed in with the new token only.
    let (status, body) = env.call("POST", "/api/password", Some(&admin), Some(json!({"current": PASSWORD, "new": "another-password-1"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fresh = body["token"].as_str().unwrap().to_string();
    assert_eq!(env.call("GET", "/api/me", Some(&admin), None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(env.call("GET", "/api/me", Some(&fresh), None).await.0, StatusCode::OK);

    // Signing out ends it everywhere.
    assert_eq!(env.call("POST", "/api/logout", Some(&fresh), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(env.call("GET", "/api/me", Some(&fresh), None).await.0, StatusCode::UNAUTHORIZED);
    env.done().await;
}

#[tokio::test]
async fn repeated_wrong_passwords_lock_the_account_for_a_while() {
    let Some(env) = Env::new().await else { return };
    env.user("ann", "", Role::Guest).await;
    for _ in 0..genie_server::throttle::MAX_FAILURES {
        let (status, _) = env.call("POST", "/api/login", None, Some(json!({"username": "ann", "password": "wrong-password"}))).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let (status, body) = env.call("POST", "/api/login", None, Some(json!({"username": "ann", "password": PASSWORD}))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    env.done().await;
}

#[tokio::test]
async fn guests_get_only_documents_of_the_deceased() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "", Role::Admin).await;
    let family = env.user("fam", "", Role::Family).await;
    let guest = env.user("guest", "", Role::Guest).await;
    let tree = sample()
        .replace("0 @I1@ INDI\n", "0 @I1@ INDI\n1 OBJE @M1@\n")
        .replace("0 @I14@ INDI\n", "0 @I14@ INDI\n1 OBJE @M2@\n")
        .replace("0 TRLR", "0 @M1@ OBJE\n1 FILE media/thomas.jpg\n0 @M2@ OBJE\n1 FILE media/linda.jpg\n0 TRLR");
    env.seed(&admin, &tree).await;

    let (status, _) = env.raw("PUT", "/api/media?path=media/thomas.jpg", &guest, b"x".to_vec()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = env.raw("PUT", "/api/media?path=../etc/passwd", &admin, b"x".to_vec()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let mut shas = Vec::new();
    for (path, bytes) in [("media/thomas.jpg", &b"thomas"[..]), ("media/linda.jpg", &b"linda"[..])] {
        let (status, body) = env.raw("PUT", &format!("/api/media?path={path}"), &admin, bytes.to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        shas.push(serde_json::from_slice::<Value>(&body).unwrap()["sha256"].as_str().unwrap().to_string());
    }

    let (_, list) = env.call("GET", "/api/media", Some(&guest), None).await;
    assert_eq!(list.as_array().unwrap().iter().map(|m| m["path"].as_str().unwrap()).collect::<Vec<_>>(), ["media/thomas.jpg"]);
    let (_, list) = env.call("GET", "/api/media", Some(&family), None).await;
    assert_eq!(list.as_array().unwrap().len(), 2);

    let get = |token: &str, sha: &str| {
        let req = Request::builder().uri(format!("/api/media/{sha}")).header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).unwrap();
        env.send(req)
    };
    assert_eq!(get(&guest, &shas[0]).await, (StatusCode::OK, b"thomas".to_vec()));
    assert_eq!(get(&guest, &shas[1]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(get(&family, &shas[1]).await, (StatusCode::OK, b"linda".to_vec()));
    env.done().await;
}

#[tokio::test]
async fn a_bundle_imports_the_tree_and_its_documents() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "", Role::Admin).await;
    let ed = env.user("ed", "", Role::Editor).await;

    // A bundle made the way the app makes one.
    let dir = std::env::temp_dir().join(format!("{}-bundle", env.db));
    std::fs::create_dir_all(dir.join("Family media")).unwrap();
    std::fs::write(dir.join("Family media/scan.jpg"), b"scan bytes").unwrap();
    let text = sample().replace("0 @I1@ INDI\n", "0 @I1@ INDI\n1 OBJE @M1@\n").replace("0 TRLR", "0 @M1@ OBJE\n1 FILE Family media/scan.jpg\n0 TRLR");
    let (mut doc, _) = genie_core::model::Document::from_bytes(text.as_bytes());
    doc.path = Some(dir.join("Family.ged"));
    let mut bundle = std::io::Cursor::new(Vec::new());
    genie_core::bundle::write(&doc, &mut bundle).unwrap();
    let bundle = bundle.into_inner();

    assert_eq!(env.raw("POST", "/api/import", &ed, bundle.clone()).await.0, StatusCode::FORBIDDEN);
    let (status, body) = env.raw("POST", "/api/import", &admin, bundle.clone()).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let imported: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(imported["files"], 1);
    assert_eq!(imported["people"].as_i64(), Some(genie_core::model::Document::from_bytes(genie_core::SAMPLE_GED).0.people().len() as i64));

    let (_, list) = env.call("GET", "/api/media", Some(&ed), None).await;
    assert_eq!(list[0]["path"], "Family media/scan.jpg");
    let (_, tree) = env.tree(&ed).await;
    assert!(tree.contains("Thomas /Hartwell/"));

    // Over an existing tree only when asked to replace it.
    assert_eq!(env.raw("POST", "/api/import", &admin, bundle.clone()).await.0, StatusCode::CONFLICT);
    assert_eq!(env.raw("POST", "/api/import?replace=true", &admin, bundle).await.0, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&dir);
    env.done().await;
}

#[tokio::test]
async fn browsers_sign_in_with_a_cookie_that_only_this_site_can_use() {
    let web = std::env::temp_dir().join(format!("genie-web-test-{}", std::process::id()));
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html><title>Genie</title>").unwrap();
    std::fs::write(web.join("genie-abc123.js"), "// app").unwrap();
    let Some(env) = Env::with_web(Some(web.clone())).await else { return };
    let admin = env.user("admin", "", Role::Admin).await;
    env.seed(&admin, &sample()).await;
    auth::create_user(&env.pool, "ed", "Ed", PASSWORD, Role::Editor).await.unwrap();

    // Signing in as a browser sets an HttpOnly, SameSite=Strict cookie.
    let req = Request::post("/api/login").header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"username": "ed", "password": PASSWORD, "cookie": true}).to_string())).unwrap();
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let set = resp.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap().to_string();
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict") && set.contains("Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_string();

    let with_cookie = |method: &str, uri: &str, csrf: bool, body: Option<Value>| {
        let mut r = Request::builder().method(method).uri(uri).header(header::COOKIE, &cookie);
        if csrf {
            r = r.header("X-Genie", "1");
        }
        let r = match body {
            Some(b) => r.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())),
            None => r.body(Body::empty()),
        };
        r.unwrap()
    };
    let (status, body) = env.send(with_cookie("GET", "/api/tree", false, None)).await;
    assert_eq!(status, StatusCode::OK);
    let tree: Value = serde_json::from_slice(&body).unwrap();
    let (rev, text) = (tree["revision"].as_i64().unwrap(), tree["gedcom"].as_str().unwrap().replace("Thomas /Hartwell/", "Tom /Hartwell/"));

    // A change riding on the cookie alone is refused; with the header it goes in.
    let save = json!({"base_revision": rev, "gedcom": text});
    assert_eq!(env.send(with_cookie("POST", "/api/tree", false, Some(save.clone()))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(env.send(with_cookie("POST", "/api/tree", true, Some(save))).await.0, StatusCode::OK);

    // The app's files, with its page checked each time and the rest cached.
    let get = |uri: &str| Request::builder().uri(uri).body(Body::empty()).unwrap();
    let resp = env.app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(resp.headers()[header::X_FRAME_OPTIONS], "DENY");
    let resp = env.app.clone().oneshot(get("/genie-abc123.js")).await.unwrap();
    assert!(resp.headers()[header::CACHE_CONTROL].to_str().unwrap().contains("immutable"));

    // Signing out clears the cookie and ends the session.
    let req = with_cookie("POST", "/api/logout", true, None);
    let resp = env.app.clone().oneshot(req).await.unwrap();
    assert!(resp.headers()[header::SET_COOKIE].to_str().unwrap().contains("Max-Age=0"));
    assert_eq!(env.send(with_cookie("GET", "/api/me", false, None)).await.0, StatusCode::UNAUTHORIZED);
    let _ = std::fs::remove_dir_all(&web);
    env.done().await;
}

#[tokio::test]
async fn the_edit_report_filters_and_reverts_put_things_back() {
    let Some(env) = Env::new().await else { return };
    let admin = env.user("admin", "Admin", Role::Admin).await;
    let alice = env.user("alice", "Alice", Role::Editor).await;
    let bob = env.user("bob", "Bob", Role::Editor).await;
    env.seed(&admin, &sample()).await;

    let save = |token: &str, from: &str, to: &str| {
        let (token, from, to) = (token.to_string(), from.to_string(), to.to_string());
        let env = &env;
        async move {
            let (rev, text) = env.tree(&token).await;
            assert!(text.contains(&from), "{from}");
            let (status, body) = env.call("POST", "/api/tree", Some(&token), Some(json!({"base_revision": rev, "gedcom": text.replacen(&from, &to, 1)}))).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["revision"].as_i64().unwrap()
        }
    };
    // Alice renames Thomas; Bob, in one save, adds a child to Thomas's family.
    let r_alice = save(&alice, "NAME Thomas /Hartwell/", "NAME Tom /Hartwell/").await;
    let (_, text) = env.tree(&bob).await;
    let with_child = text
        .replacen("1 CHIL @I3@", "1 CHIL @I3@\r\n1 CHIL @I99@", 1)
        .replace("0 TRLR", "0 @I99@ INDI\r\n1 NAME New /Child/\r\n1 FAMC @F1@\r\n0 TRLR");
    let (rev, _) = env.tree(&bob).await;
    let (status, body) = env.call("POST", "/api/tree", Some(&bob), Some(json!({"base_revision": rev, "gedcom": with_child}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let r_bob = body["revision"].as_i64().unwrap();

    // The report, by account and by day.
    let (_, rows) = env.call("GET", "/api/changes?username=alice", Some(&admin), None).await;
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!((rows[0]["user"].as_str(), rows[0]["label"].as_str()), (Some("Alice"), Some("Tom Hartwell")));
    let today = time::OffsetDateTime::now_utc().date().to_string();
    let (_, rows) = env.call("GET", &format!("/api/changes?from={today}&to={today}&since=1"), Some(&admin), None).await;
    assert_eq!(rows.as_array().unwrap().len(), 3, "{rows}");
    let (_, none) = env.call("GET", "/api/changes?to=2000-01-01", Some(&admin), None).await;
    assert_eq!(none, json!([]));
    let (status, _) = env.call("GET", "/api/changes?from=yesterday", Some(&admin), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, page) = env.call("GET", &format!("/api/changes?before={r_bob}&since=1"), Some(&admin), None).await;
    assert!(page.as_array().unwrap().iter().all(|c| c["revision"].as_i64() == Some(r_alice)));

    // Only administrators revert.
    let (status, _) = env.call("POST", "/api/revert", Some(&alice), Some(json!({"revision": r_alice}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Undoing just Bob's new child would leave the family pointing at nobody.
    let (status, body) = env.call("POST", "/api/revert", Some(&admin), Some(json!({"revision": r_bob, "xref": "I99"}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body["error"].as_str().unwrap().contains("whole save"));
    // The whole save is fine.
    let (status, body) = env.call("POST", "/api/revert", Some(&admin), Some(json!({"revision": r_bob}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["undone"], 2);
    let (_, text) = env.tree(&admin).await;
    assert!(!text.contains("New /Child/") && !text.contains("@I99@"));

    // Thomas was changed again after Alice's save: listed, then forced.
    save(&bob, "NAME Tom /Hartwell/", "NAME Tommy /Hartwell/").await;
    let (status, body) = env.call("POST", "/api/revert", Some(&admin), Some(json!({"revision": r_alice, "xref": "I1"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["later"][0]["user"], "Bob");
    let (status, _) = env.call("POST", "/api/revert", Some(&admin), Some(json!({"revision": r_alice, "xref": "I1", "force": true}))).await;
    assert_eq!(status, StatusCode::OK);
    let (_, text) = env.tree(&admin).await;
    assert!(text.contains("NAME Thomas /Hartwell/"));

    // The revert is itself in the history, by the admin, saying what it was.
    let (_, hist) = env.call("GET", "/api/changes?xref=I1", Some(&admin), None).await;
    assert_eq!(hist[0]["user"], "Admin");
    assert!(hist[0]["note"].as_str().unwrap().starts_with("Reverted Tommy Hartwell"), "{}", hist[0]);
    env.done().await;
}
