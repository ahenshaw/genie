#![windows_subsystem = "windows"]

mod app;
mod bundle;
mod dedup;
mod fonts;
mod gedcom;
mod graph;
mod homepath;
mod kinship;
mod media;
mod mediaview;
mod minimap;
mod model;
mod platform;
mod relation;
mod reports;
#[cfg(not(target_arch = "wasm32"))]
mod server;
mod tidy;
mod tools;
mod tree;
mod views;
mod widgets;
mod wikitree;

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let mut open = None;
    let (mut serve, mut port, mut browser) = (false, server::DEFAULT_PORT, true);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--serve") => serve = true,
            Some("--no-browser") => browser = false,
            Some("--port") => match args.next().and_then(|p| p.to_str()?.parse().ok()) {
                Some(p) => port = p,
                None => usage("--port needs a number"),
            },
            Some("-h" | "--help") => usage(""),
            _ => open = Some(std::path::PathBuf::from(a)),
        }
    }
    if serve {
        let Some(tree) = open else { usage("--serve needs a .ged file to serve") };
        if let Err(e) = server::run(tree, port, browser) {
            eprintln!("genie: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Genie")
            .with_app_id("genie")
            .with_inner_size([1320.0, 860.0])
            .with_min_inner_size([860.0, 560.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Genie", options, Box::new(|cc| Ok(Box::new(app::GenieApp::new(cc, open)))))
}

#[cfg(not(target_arch = "wasm32"))]
fn usage(problem: &str) -> ! {
    if !problem.is_empty() {
        eprintln!("genie: {problem}\n");
    }
    eprintln!("Usage: genie [FILE.ged]");
    eprintln!("       genie --serve FILE.ged [--port N] [--no-browser]");
    eprintln!();
    eprintln!("  --serve       show Genie in a web browser, at http://localhost:{}/", server::DEFAULT_PORT);
    eprintln!("  --port N      listen on port N instead");
    eprintln!("  --no-browser  don't open a browser window");
    std::process::exit(if problem.is_empty() { 0 } else { 2 });
}

/// The browser build: the page (`index.html`) provides the canvas.
#[cfg(target_arch = "wasm32")]
fn main() {
    use wasm_bindgen::JsCast;
    let canvas = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.get_element_by_id("genie_canvas"))
        .and_then(|c| c.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        .expect("index.html has a canvas with id genie_canvas");
    wasm_bindgen_futures::spawn_local(async move {
        let started = eframe::WebRunner::new()
            .start(canvas, eframe::WebOptions::default(), Box::new(|cc| Ok(Box::new(app::GenieApp::new(cc, None)))))
            .await;
        if let Err(e) = started {
            web_sys::console::error_1(&e);
        }
    });
}
