//! Embervale's launcher: check, patch, play.
//!
//! Deliberately small. It is the first thing a player runs and the thing they
//! run every time after that, so it has no runtime to install, no webview to
//! depend on, and nothing to configure before it works.

// No console window behind the launcher on Windows. Kept out of debug builds so
// that println! during development still goes somewhere visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod manifest;
mod sync;
mod theme;

fn main() -> eframe::Result<()> {
    // Fixed and small, the size of the updaters this shape of launcher comes
    // from. A launcher is a doorway, not a place to spend time: it should sit in
    // a corner of the screen rather than take it over, and at one size the
    // layout can be composed instead of made to survive being stretched.
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([app::WINDOW.x, app::WINDOW.y])
        .with_resizable(false)
        .with_maximize_button(false)
        // Its own title bar, so the window reads as part of the game rather
        // than as a dialog the OS drew a frame around.
        .with_decorations(false)
        .with_title("Embervale");

    if let Some(icon) = app::icon() {
        viewport = viewport.with_icon(icon);
    }

    eframe::run_native(
        "Embervale Launcher",
        eframe::NativeOptions {
            viewport,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::Launcher::new(cc)))),
    )
}
