//! The desktop: real files, native dialogs, no server.

use std::path::{Path, PathBuf};

use super::ServerEvent;

pub fn exists(p: &Path) -> bool {
    p.exists()
}

pub fn image_uri(p: &Path) -> String {
    format!("file://{}", p.display())
}

/// Opens a file or folder with the system's own application.
pub fn open_path(_ctx: &egui::Context, p: &Path) -> Result<(), String> {
    open::that_detached(p).map_err(|e| e.to_string())
}

pub fn open_url(_ctx: &egui::Context, url: &str) {
    let _ = open::that_detached(url);
}

pub fn set_title(ctx: &egui::Context, title: &str) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.to_string()));
}

pub fn set_unsaved(_unsaved: bool) {}

/// The page's own server; only the browser has one.
pub fn origin() -> String {
    String::new()
}

/// The browser keeps the documents its account may see; the desktop has files.
pub fn set_hosted_media(_files: Vec<(String, String)>) {}

pub fn install_loaders(_ctx: &egui::Context) {}

// ---- dialogs -------------------------------------------------------------------------------

pub fn pick_ged(dir: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new()
        .add_filter("GEDCOM or bundle", &["ged", "GED", "gdz", "GDZ"])
        .add_filter("GEDCOM", &["ged", "GED"])
        .add_filter("Bundle with documents", &["gdz", "GDZ"]);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

pub fn save_bundle(dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("Bundle with documents", &["gdz"]).set_file_name(name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

pub fn save_ged(dir: Option<&Path>, name: &str) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new().add_filter("GEDCOM", &["ged", "GED"]).set_file_name(name);
    if let Some(dir) = dir {
        dialog = dialog.set_directory(dir);
    }
    dialog.save_file()
}

pub fn pick_media_files() -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("Images and documents", &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tif", "tiff", "pdf", "txt", "doc", "docx"])
        .add_filter("All files", &["*"])
        .pick_files()
}

pub fn pick_file(title: Option<&str>) -> Option<PathBuf> {
    let mut dialog = rfd::FileDialog::new();
    if let Some(t) = title {
        dialog = dialog.set_title(t);
    }
    dialog.pick_file()
}

pub fn pick_folder(title: &str) -> Option<PathBuf> {
    rfd::FileDialog::new().set_title(title).pick_folder()
}

// ---- the server ------------------------------------------------------------------------------

/// The desktop has no server to talk to.
#[derive(Default)]
pub struct Server;

impl Server {
    pub fn load(&mut self, _ctx: &egui::Context) {}
    pub fn save(&mut self, _ctx: &egui::Context, _bytes: Vec<u8>) {}
    pub fn poll(&mut self) -> Option<ServerEvent> {
        None
    }
}
