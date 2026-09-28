//! `genie-server`: serves the shared tree's API.
//!
//! ```text
//! genie-server                               serve (the default)
//! genie-server migrate                       apply the schema and exit
//! genie-server create-admin <username>       add an administrator (asks for a password,
//!                                            or takes GENIE_NEW_PASSWORD)
//! genie-server reset-password <username>     set a new password (asks for it)
//! ```
//!
//! Settings come from the environment, or a `.env` file beside it:
//!
//! | Variable | Meaning |
//! |---|---|
//! | `DATABASE_URL` | `mysql://genie:…@127.0.0.1:3306/genie` |
//! | `SESSION_SECRET` | at least 32 characters; signs sign-in tokens |
//! | `BIND_ADDR` | default `127.0.0.1:3100` |
//! | `MEDIA_DIR` | default `/var/lib/genie/media` |
//! | `LIVING_YEARS` | default 100: born that long ago counts as deceased for guests |

use std::net::SocketAddr;
use std::path::PathBuf;

use genie_server::auth::{self, Role};
use genie_server::{AppState, Config, MIGRATOR};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("genie-server: {msg}");
    std::process::exit(1);
}

fn ask_password(prompt: &str) -> String {
    // For scripted setups (and tests); otherwise it's asked for.
    if let Some(p) = env("GENIE_NEW_PASSWORD") {
        return p;
    }
    let p = rpassword::prompt_password(prompt).unwrap_or_else(|e| fail(format!("couldn't read a password: {e}")));
    if p.chars().count() < auth::MIN_PASSWORD {
        fail(format!("passwords need at least {} characters", auth::MIN_PASSWORD));
    }
    if rpassword::prompt_password("Again: ").unwrap_or_default() != p {
        fail("the passwords didn't match");
    }
    p
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    let args: Vec<String> = std::env::args().skip(1).collect();

    let url = env("DATABASE_URL").unwrap_or_else(|| fail("DATABASE_URL must be set"));
    let pool = sqlx::mysql::MySqlPoolOptions::new()
        .max_connections(10)
        .connect(&url)
        .await
        .unwrap_or_else(|e| fail(format!("can't connect to the database: {e}")));
    MIGRATOR.run(&pool).await.unwrap_or_else(|e| fail(format!("couldn't apply the schema: {e}")));

    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => {}
        ["migrate"] => {
            println!("Schema is up to date.");
            return;
        }
        ["create-admin", username] => {
            let password = ask_password("Password: ");
            match auth::create_user(&pool, username, "", &password, Role::Admin).await {
                Ok(id) => println!("Created administrator {username} (id {id})."),
                Err(e) => fail(e.message),
            }
            return;
        }
        ["reset-password", username] => {
            let password = ask_password("New password: ");
            let hash = auth::hash_password(&password);
            let done = sqlx::query("UPDATE users SET password_hash = ?, session_epoch = session_epoch + 1, disabled = FALSE WHERE username = ?")
                .bind(&hash)
                .bind(username.to_lowercase())
                .execute(&pool)
                .await
                .unwrap_or_else(|e| fail(e));
            if done.rows_affected() == 0 {
                fail(format!("there's no user {username}"));
            }
            println!("Password changed for {username}; their other sign-ins have ended.");
            return;
        }
        _ => fail("usage: genie-server [serve | migrate | create-admin <username> | reset-password <username>]"),
    }

    let secret = env("SESSION_SECRET").unwrap_or_else(|| fail("SESSION_SECRET must be set (run `openssl rand -hex 32` and paste the output)"));
    if secret.len() < 32 {
        fail("SESSION_SECRET must be at least 32 characters");
    }
    let media_dir = PathBuf::from(env("MEDIA_DIR").unwrap_or_else(|| "/var/lib/genie/media".into()));
    std::fs::create_dir_all(&media_dir).unwrap_or_else(|e| fail(format!("can't use MEDIA_DIR {}: {e}", media_dir.display())));
    let living_years = env("LIVING_YEARS").map(|v| v.parse().unwrap_or_else(|_| fail("LIVING_YEARS must be a number"))).unwrap_or(genie_core::privacy::LIVING_YEARS);
    let bind = env("BIND_ADDR").unwrap_or_else(|| "127.0.0.1:3100".into());

    let state = AppState::new(pool, Config { session_secret: secret.into_bytes(), media_dir, living_years });
    let app = genie_server::router(state);
    let listener = tokio::net::TcpListener::bind(&bind).await.unwrap_or_else(|e| fail(format!("can't listen on {bind}: {e}")));
    println!("genie-server listening on {}", listener.local_addr().map(|a| a.to_string()).unwrap_or(bind));
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .unwrap_or_else(|e| fail(e));
}
