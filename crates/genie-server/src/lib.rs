//! The server behind genie.henshaw.us: one shared family tree, the accounts
//! that may see and edit it, and every change they make.
//!
//! - The tree lives in MySQL as whole revisions, with a row per changed
//!   record saying who changed it ([`store`]).
//! - Editors submit the tree they edited along with the revision it was
//!   based on; other people's changes since then are merged in
//!   (`genie_core::sync`), and clashes come back as conflicts.
//! - Guests are sent `genie_core::privacy::guest_view` of the tree, never
//!   the whole of it, and only the documents that view links to.
//! - Documents are files named by their SHA-256 in `media_dir`.
//!
//! Everything is under `/api`, as JSON, authenticated with a bearer token
//! from `POST /api/login`.

pub mod api;
pub mod auth;
pub mod error;
pub mod media;
pub mod store;
pub mod throttle;

use std::path::PathBuf;
use std::sync::Arc;

use sqlx::MySqlPool;

pub use api::router;

/// The schema, applied at startup.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub struct Config {
    /// Signs session tokens; at least 32 bytes. Changing it signs everyone out.
    pub session_secret: Vec<u8>,
    /// Where documents are stored, as `<sha256>` files.
    pub media_dir: PathBuf,
    /// Born this many years ago or more counts as deceased, for guests.
    pub living_years: i32,
}

pub struct AppState {
    pub pool: MySqlPool,
    pub config: Config,
    /// Saves are applied one at a time.
    pub write_lock: tokio::sync::Mutex<()>,
    pub throttle: throttle::Throttle,
    pub head: store::HeadCache,
}

pub type State = Arc<AppState>;

impl AppState {
    pub fn new(pool: MySqlPool, config: Config) -> State {
        Arc::new(Self {
            pool,
            config,
            write_lock: tokio::sync::Mutex::new(()),
            throttle: throttle::Throttle::default(),
            head: store::HeadCache::default(),
        })
    }
}
