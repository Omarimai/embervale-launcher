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
    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1000.0, 620.0])
        .with_min_inner_size([860.0, 540.0])
        .with_resizable(true)
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
