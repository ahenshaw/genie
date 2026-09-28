//! Accounts: roles, passwords, and session tokens.
//!
//! A token is `v1.{user}.{epoch}.{expires}.{hmac}`, signed with the server's
//! session secret, so checking one needs no session table. The account is
//! still read on every request: its role decides what the request may do, and
//! a token only counts while its epoch matches the account's. Changing a role
//! or password, disabling the account, or signing out bumps the epoch and
//! every earlier token stops working at once.

use std::net::{IpAddr, SocketAddr};

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::header;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

use crate::State;
use crate::error::{ApiError, ApiResult};

/// How long a sign-in lasts.
pub const TOKEN_LIFETIME: u64 = 30 * 24 * 60 * 60;
pub const MIN_PASSWORD: usize = 10;

/// What an account may do, from least to most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Sees people who have died; living people appear as "Private".
    Guest,
    /// Sees everything; changes nothing.
    Family,
    /// Edits the tree.
    Editor,
    /// Edits, and manages accounts.
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Guest => "guest",
            Role::Family => "family",
            Role::Editor => "editor",
            Role::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "guest" => Some(Role::Guest),
            "family" => Some(Role::Family),
            "editor" => Some(Role::Editor),
            "admin" => Some(Role::Admin),
            _ => None,
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---- passwords ------------------------------------------------------------------------------

/// An argon2id hash of `password`. Slow on purpose: call it off the async threads.
pub fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(password.as_bytes(), &salt).expect("argon2 accepts any password").to_string()
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash).is_ok_and(|h| Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// Checked against when the username doesn't exist, so a wrong name takes as
/// long to refuse as a wrong password.
pub fn dummy_hash() -> &'static str {
    static DUMMY: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DUMMY.get_or_init(|| hash_password("not anyone's password"))
}

pub fn check_new_password(password: &str) -> ApiResult<()> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(ApiError::bad_request(format!("Use at least {MIN_PASSWORD} characters.")));
    }
    Ok(())
}

// ---- tokens ------------------------------------------------------------------------------------

fn sign(secret: &[u8], body: &str) -> String {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256>>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(body.as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn mint_token(secret: &[u8], user: i32, epoch: i32, expires: u64) -> String {
    let body = format!("v1.{user}.{epoch}.{expires}");
    let sig = sign(secret, &body);
    format!("{body}.{sig}")
}

/// (user, epoch) from a well-formed, correctly signed, unexpired token. The
/// signature is checked before the clock, so a forgery can't be told from an
/// expired token by timing.
pub fn verify_token(secret: &[u8], token: &str) -> Option<(i32, i32)> {
    use subtle::ConstantTimeEq;
    let (body, sig) = token.rsplit_once('.')?;
    if sign(secret, body).as_bytes().ct_eq(sig.as_bytes()).unwrap_u8() != 1 {
        return None;
    }
    let mut parts = body.split('.');
    if parts.next()? != "v1" {
        return None;
    }
    let user = parts.next()?.parse().ok()?;
    let epoch = parts.next()?.parse().ok()?;
    let expires: u64 = parts.next()?.parse().ok()?;
    (parts.next().is_none() && expires > now()).then_some((user, epoch))
}

// ---- the signed-in account, per request --------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct CurrentUser {
    pub id: i32,
    pub username: String,
    pub display_name: String,
    pub role: Role,
}

impl CurrentUser {
    pub fn require(&self, role: Role) -> ApiResult<()> {
        if self.role >= role { Ok(()) } else { Err(ApiError::forbidden()) }
    }

    /// The name to show for their changes.
    pub fn shown_name(&self) -> &str {
        if self.display_name.is_empty() { &self.username } else { &self.display_name }
    }
}

pub const COOKIE: &str = "genie_session";

fn token_from(parts: &Parts) -> Option<&str> {
    if let Some(v) = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok())
        && let Some(t) = v.strip_prefix("Bearer ")
    {
        return Some(t.trim());
    }
    // The browser app (later) holds the token in an HttpOnly cookie instead.
    parts
        .headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())?
        .split(';')
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v)
}

impl FromRequestParts<State> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &State) -> Result<Self, Self::Rejection> {
        let token = token_from(parts).ok_or_else(ApiError::unauthorized)?;
        let (id, epoch) = verify_token(&state.config.session_secret, token).ok_or_else(ApiError::unauthorized)?;
        let row: Option<(String, String, String, bool, i32)> =
            sqlx::query_as("SELECT username, display_name, CAST(role AS CHAR), disabled, session_epoch FROM users WHERE id = ?")
                .bind(id)
                .fetch_optional(&state.pool)
                .await?;
        let Some((username, display_name, role, disabled, current_epoch)) = row else { return Err(ApiError::unauthorized()) };
        if disabled || current_epoch != epoch {
            return Err(ApiError::unauthorized());
        }
        let role = Role::parse(&role).ok_or_else(|| ApiError::internal(format!("unknown role {role}")))?;
        Ok(CurrentUser { id, username, display_name, role })
    }
}

/// The caller's address: the proxy's `X-Forwarded-For` when the connection
/// comes from this machine (Apache), otherwise the peer.
pub struct ClientIp(pub Option<IpAddr>);

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        let forwarded = || {
            parts.headers.get("x-forwarded-for")?.to_str().ok()?.rsplit(',').next()?.trim().parse::<IpAddr>().ok()
        };
        let ip = match peer {
            Some(p) if p.is_loopback() => forwarded().or(Some(p)),
            other => other,
        };
        Ok(ClientIp(ip))
    }
}

/// Creates an account. Used by the admin API and by `genie-server create-admin`.
pub async fn create_user(pool: &sqlx::MySqlPool, username: &str, display_name: &str, password: &str, role: Role) -> ApiResult<i32> {
    let username = username.trim().to_lowercase();
    if !(2..=64).contains(&username.len()) || !username.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(ApiError::bad_request("Usernames are 2–64 letters, digits, dots, dashes or underscores."));
    }
    check_new_password(password)?;
    let password = password.to_string();
    let hash = tokio::task::spawn_blocking(move || hash_password(&password)).await.map_err(ApiError::internal)?;
    let result = sqlx::query("INSERT INTO users (username, display_name, password_hash, role) VALUES (?, ?, ?, ?)")
        .bind(&username)
        .bind(display_name.trim())
        .bind(&hash)
        .bind(role.as_str())
        .execute(pool)
        .await;
    match result {
        Ok(r) => Ok(r.last_insert_id() as i32),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(ApiError::new(axum::http::StatusCode::CONFLICT, "That username is taken.")),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"a-test-secret-that-is-32-bytes-long!";

    #[test]
    fn tokens_round_trip_and_resist_tampering() {
        let t = mint_token(SECRET, 7, 3, now() + 60);
        assert_eq!(verify_token(SECRET, &t), Some((7, 3)));
        assert_eq!(verify_token(b"another-secret-of-at-least-32-bytes", &t), None);
        let forged = t.replacen("v1.7.", "v1.1.", 1);
        assert_eq!(verify_token(SECRET, &forged), None);
        assert_eq!(verify_token(SECRET, &mint_token(SECRET, 7, 3, now() - 1)), None);
    }

    #[test]
    fn passwords_verify() {
        let h = hash_password("correct horse battery");
        assert!(verify_password(&h, "correct horse battery"));
        assert!(!verify_password(&h, "wrong"));
        assert!(!verify_password("not a hash", "anything"));
    }

    #[test]
    fn roles_are_ordered() {
        assert!(Role::Admin > Role::Editor && Role::Editor > Role::Family && Role::Family > Role::Guest);
    }
}
