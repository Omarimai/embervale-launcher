//! The launcher window.
//!
//! Everything slow happens on one worker thread; the UI thread only ever reads
//! messages off a channel. An immediate-mode UI redraws from scratch every
//! frame, so a download that blocked the UI thread would freeze the window --
//! including the progress bar meant to show it was working.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use eframe::egui::{self, Align, Color32, Layout, RichText, CornerRadius, Stroke, Vec2};

use crate::config::Config;
use crate::manifest::{self, Manifest};
use crate::sync::{self, Update};
use crate::theme;

const BACKGROUND_PNG: &[u8] = include_bytes!("../assets/background.png");

/// What the worker sends back.
enum Msg {
    Status(String),
    Progress { done: u64, total: u64 },
    Manifest(Box<Manifest>),
    Ready,
    Failed(String),
}

#[derive(PartialEq)]
enum Phase {
    /// Talking to the update server, or hashing what is already installed.
    Working,
    /// Everything matches the manifest; PLAY is live.
    Ready,
    /// Nothing can be done until the launcher is restarted.
    Failed,
}

pub struct Launcher {
    config: Config,
    rx: Receiver<Msg>,
    /// Kept so the worker can be restarted by Retry.
    ctx_for_worker: Option<egui::Context>,

    phase: Phase,
    status: String,
    error: Option<String>,
    progress: Option<(u64, u64)>,
    manifest: Option<Manifest>,

    background: Option<egui::TextureHandle>,
}

impl Launcher {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let (config, config_error) = Config::load();
        let (tx, rx) = channel();

        let mut app = Self {
            config,
            rx,
            ctx_for_worker: Some(cc.egui_ctx.clone()),
            phase: Phase::Working,
            status: "Starting…".to_string(),
            error: None,
            progress: None,
            manifest: None,
            background: None,
        };

        // A bad launcher.toml is worth stopping for. Carrying on would check a
        // different channel than the file says, and the player would have no
        // way to tell.
        if let Some(message) = config_error {
            app.phase = Phase::Failed;
            app.error = Some(message);
            return app;
        }

        app.spawn_worker(tx);
        app
    }

    fn spawn_worker(&mut self, tx: Sender<Msg>) {
        let manifest_url = self.config.manifest_url.clone();
        let install_dir = self.config.install_dir.clone();
        let ctx = self.ctx_for_worker.clone();

        std::thread::spawn(move || {
            // Every send is followed by a repaint request: without it the UI
            // sleeps until the next input event and the progress bar only moves
            // when the mouse does.
            let send = |msg: Msg| {
                let _ = tx.send(msg);
                if let Some(ctx) = &ctx {
                    ctx.request_repaint();
                }
            };

            send(Msg::Status("Checking for updates…".into()));
            let manifest = match manifest::fetch(&manifest_url) {
                Ok(m) => m,
                Err(e) => {
                    send(Msg::Failed(e));
                    return;
                }
            };
            send(Msg::Manifest(Box::new(manifest.clone())));

            if let Err(e) = std::fs::create_dir_all(&install_dir) {
                send(Msg::Failed(format!(
                    "cannot create {}: {e}",
                    install_dir.display()
                )));
                return;
            }

            let mut report = |u: Update| match u {
                Update::Status(s) => send(Msg::Status(s)),
                Update::Progress { done, total } => send(Msg::Progress { done, total }),
            };

            let plan = match sync::plan(&manifest, &install_dir, &mut report) {
                Ok(p) => p,
                Err(e) => {
                    send(Msg::Failed(e));
                    return;
                }
            };

            if plan.is_empty() {
                send(Msg::Status(format!("Up to date — {}", manifest.version)));
                send(Msg::Ready);
                return;
            }

            send(Msg::Status(format!(
                "Downloading {} ({})",
                plural(plan.missing.len(), "file", "files"),
                human_bytes(plan.bytes)
            )));

            if let Err(e) = sync::apply(&plan, &install_dir, &mut report) {
                send(Msg::Failed(e));
                return;
            }

            send(Msg::Status(format!("Ready — {}", manifest.version)));
            send(Msg::Ready);
        });
    }

    fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Msg::Status(s)) => self.status = s,
                Ok(Msg::Progress { done, total }) => self.progress = Some((done, total)),
                Ok(Msg::Manifest(m)) => self.manifest = Some(*m),
                Ok(Msg::Ready) => {
                    self.phase = Phase::Ready;
                    self.progress = None;
                }
                Ok(Msg::Failed(e)) => {
                    self.phase = Phase::Failed;
                    self.error = Some(e);
                    self.progress = None;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    fn retry(&mut self) {
        let (tx, rx) = channel();
        self.rx = rx;
        self.phase = Phase::Working;
        self.error = None;
        self.progress = None;
        self.status = "Retrying…".into();
        self.spawn_worker(tx);
    }

    /// Starts the game and closes the launcher.
    fn play(&mut self, ctx: &egui::Context) {
        let Some(manifest) = &self.manifest else {
            return;
        };
        let target = match sync::launch_target(manifest, &self.config.install_dir) {
            Ok(t) => t,
            Err(e) => {
                self.phase = Phase::Failed;
                self.error = Some(e);
                return;
            }
        };

        match std::process::Command::new(&target)
            .current_dir(&self.config.install_dir)
            .spawn()
        {
            Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Err(e) => {
                self.phase = Phase::Failed;
                self.error = Some(format!("cannot start {}: {e}", target.display()));
            }
        }
    }

    fn background(&mut self, ctx: &egui::Context) -> Option<egui::TextureHandle> {
        if self.background.is_none() {
            let decoded = image::load_from_memory(BACKGROUND_PNG).ok()?.to_rgba8();
            let size = [decoded.width() as usize, decoded.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
            self.background =
                Some(ctx.load_texture("background", image, egui::TextureOptions::LINEAR));
        }
        self.background.clone()
    }
}

impl eframe::App for Launcher {
    // eframe hands over the root Ui, which has no margin and no background of
    // its own -- so the art below is the window's background, not something
    // drawn over one.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();

        let ctx = ui.ctx().clone();

        // The window, not ui.max_rect(): the root Ui is handed a rect larger
        // than the viewport, so laying the bottom bar out against max_rect puts
        // it below the bottom edge, where the art hides that anything is wrong.
        //
        // Art fills the viewport; controls stay inside the content rect, which
        // on a desktop is the same rectangle and elsewhere is the part not
        // behind a notch or status bar.
        let background = self.background(&ctx);
        paint_background(ui, ctx.viewport_rect(), background);

        // Bottom bar is a fixed height so the art above it is never partly
        // covered by a control bar that grew.
        let (art, bar) = split_bottom(ctx.content_rect(), 108.0);

        self.header(ui, art);
        self.news(ui, art);
        self.bar(ui, bar, &ctx);
    }
}

impl Launcher {
    fn header(&self, ui: &mut egui::Ui, art: egui::Rect) {
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(art.shrink(28.0)));
        ui.vertical(|ui| {
            ui.label(
                RichText::new("EMBERVALE")
                    .size(44.0)
                    .strong()
                    .color(theme::TEXT),
            );
            let version = self
                .manifest
                .as_ref()
                .map(|m| m.version.clone())
                .unwrap_or_else(|| "—".into());
            ui.label(
                RichText::new(format!("Version {version}"))
                    .size(14.0)
                    .color(theme::TEXT_DIM),
            );
        });
    }

    /// Release notes down the right-hand side, the way every launcher of this
    /// shape puts them. Skipped entirely when the manifest carries none, rather
    /// than leaving an empty card over the art.
    fn news(&self, ui: &mut egui::Ui, art: egui::Rect) {
        let Some(manifest) = &self.manifest else {
            return;
        };
        if manifest.news.is_empty() {
            return;
        }

        let width = (art.width() * 0.36).clamp(240.0, 380.0);
        let panel = egui::Rect::from_min_max(
            egui::pos2(art.right() - width - 28.0, art.top() + 120.0),
            egui::pos2(art.right() - 28.0, art.bottom() - 20.0),
        );

        ui.painter()
            .rect_filled(panel, CornerRadius::same(6), theme::panel(214));

        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink(16.0)));
        ui.label(
            RichText::new("LATEST NEWS")
                .size(12.0)
                .strong()
                .color(theme::EMBER),
        );
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut ui, |ui| {
                for item in &manifest.news {
                    ui.label(RichText::new(&item.title).size(15.0).strong().color(theme::TEXT));
                    if !item.date.is_empty() {
                        ui.label(RichText::new(&item.date).size(11.0).color(theme::TEXT_DIM));
                    }
                    if !item.body.is_empty() {
                        ui.add_space(2.0);
                        ui.label(RichText::new(&item.body).size(13.0).color(theme::TEXT_DIM));
                    }
                    ui.add_space(12.0);
                }
            });
    }

    fn bar(&mut self, ui: &mut egui::Ui, bar: egui::Rect, ctx: &egui::Context) {
        ui.painter().rect_filled(bar, CornerRadius::ZERO, theme::HAZE);
        ui.painter().hline(
            bar.x_range(),
            bar.top(),
            Stroke::new(1.0, theme::scrim(120)),
        );

        let inner = bar.shrink2(Vec2::new(28.0, 18.0));
        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner));

        ui.horizontal(|ui| {
            let button_width = 190.0;
            let left = ui.available_width() - button_width - 20.0;

            ui.allocate_ui_with_layout(
                Vec2::new(left.max(120.0), inner.height()),
                Layout::top_down(Align::LEFT),
                |ui| {
                    let (text, colour) = match (&self.phase, &self.error) {
                        (Phase::Failed, Some(e)) => (e.clone(), theme::EMBER_BRIGHT),
                        _ => (self.status.clone(), theme::TEXT_DIM),
                    };
                    ui.label(RichText::new(text).size(13.0).color(colour));
                    ui.add_space(6.0);

                    if let Some((done, total)) = self.progress {
                        let fraction = if total == 0 {
                            0.0
                        } else {
                            done as f32 / total as f32
                        };
                        ui.add(
                            egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                                .desired_height(10.0)
                                .fill(theme::ARCANE)
                                .text(
                                    RichText::new(format!(
                                        "{} / {}",
                                        human_bytes(done),
                                        human_bytes(total)
                                    ))
                                    .size(11.0),
                                ),
                        );
                    }
                },
            );

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (label, enabled) = match self.phase {
                    Phase::Ready => ("PLAY", true),
                    Phase::Failed => ("RETRY", true),
                    Phase::Working => ("PLEASE WAIT", false),
                };

                let button = egui::Button::new(
                    RichText::new(label)
                        .size(20.0)
                        .strong()
                        .color(Color32::from_rgb(0x1a, 0x12, 0x05)),
                )
                .fill(if enabled { theme::EMBER } else { theme::scrim(90) })
                .min_size(Vec2::new(190.0, 46.0))
                .corner_radius(CornerRadius::same(4));

                if ui.add_enabled(enabled, button).clicked() {
                    match self.phase {
                        Phase::Ready => self.play(ctx),
                        Phase::Failed => self.retry(),
                        Phase::Working => {}
                    }
                }
            });
        });
    }
}

/// Draws the art so it covers the window without distorting: scale to the
/// larger ratio and centre, cropping the overflow.
fn paint_background(ui: &egui::Ui, rect: egui::Rect, texture: Option<egui::TextureHandle>) {
    let Some(texture) = texture else {
        ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::NIGHT);
        return;
    };

    let size = texture.size_vec2();
    let scale = (rect.width() / size.x).max(rect.height() / size.y);
    let drawn = size * scale;

    // How much of the source is visible on each axis, as a 0..1 fraction.
    let u = (rect.width() / drawn.x).min(1.0);
    let v = (rect.height() / drawn.y).min(1.0);
    let uv = egui::Rect::from_min_max(
        egui::pos2((1.0 - u) * 0.5, (1.0 - v) * 0.5),
        egui::pos2(1.0 - (1.0 - u) * 0.5, 1.0 - (1.0 - v) * 0.5),
    );

    ui.painter()
        .image(texture.id(), rect, uv, Color32::WHITE);
    ui.painter().rect_filled(rect, CornerRadius::ZERO, theme::scrim(110));
}

fn split_bottom(rect: egui::Rect, height: f32) -> (egui::Rect, egui::Rect) {
    let split = rect.bottom() - height;
    (
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), split)),
        egui::Rect::from_min_max(egui::pos2(rect.left(), split), rect.max),
    )
}

pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The window icon, decoded at startup.
pub fn icon() -> Option<egui::IconData> {
    let decoded = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = decoded.dimensions();
    Some(egui::IconData {
        rgba: decoded.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_the_way_people_say_them() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
    }

    #[test]
    fn plurals_agree_with_their_number() {
        assert_eq!(plural(1, "file", "files"), "1 file");
        assert_eq!(plural(2, "file", "files"), "2 files");
        assert_eq!(plural(0, "file", "files"), "0 files");
    }
}
