//! What differs between the desktop app and the browser build.
//!
//! On the desktop Genie reads and writes files directly. In the browser
//! (served by `genie --serve`) the tree is loaded from and saved to the
//! server, media files are fetched from it, and anything that needs a
//! native file dialog isn't available. Both halves offer the same API.

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;

#[cfg(target_arch = "wasm32")]
mod web;
#[cfg(target_arch = "wasm32")]
pub use web::*;

/// Running in a browser, against a Genie server.
pub const WEB: bool = cfg!(target_arch = "wasm32");

/// Something the server finished doing.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))] // Only the browser has a server.
pub enum ServerEvent {
    /// The tree, with the server's path to it.
    Loaded { path: std::path::PathBuf, bytes: Vec<u8> },
    Saved(Result<(), String>),
    LoadFailed(String),
}

/// Shown when a desktop-only feature is asked for in the browser.
pub fn desktop_only(ctx: &egui::Context, what: &str) {
    elegance::Toast::new(format!("{what} is only available in the desktop app"))
        .description("The browser can't reach files on the server's disk.")
        .show(ctx);
}
