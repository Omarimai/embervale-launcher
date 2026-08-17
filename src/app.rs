//! The launcher window.
//!
//! Everything slow happens on one worker thread; the UI thread only ever reads
//! messages off a channel. An immediate-mode UI redraws from scratch every
//! frame, so a download that blocked the UI thread would freeze the window --
//! including the progress bar meant to show it was working.
//!
//! The window is laid out as fixed horizontal bands rather than with nested
//! layouts: at one window size the bands can simply be measured off, and a
//! band that is a known height cannot be pushed off the bottom edge by a
//! neighbour that grew.

use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, StrokeKind, Vec2,
};

use crate::config::Config;
use crate::manifest::{self, Manifest};
use crate::sync::{self, Update};
use crate::theme;

const BACKGROUND_PNG: &[u8] = include_bytes!("../assets/background.png");
const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

/// Outer size of the window.
pub const WINDOW: Vec2 = Vec2::new(580.0, 400.0);

const CHROME_H: f32 = 28.0;
const BANNER_H: f32 = 128.0;
const TABS_H: f32 = 26.0;
const STATUS_H: f32 = 44.0;
/// The column holding PLAY and the settings button.
const RIGHT_W: f32 = 132.0;

/// Status lines kept for the Logs tab. Enough to cover a whole patch, small
/// enough that a launcher left open for a week cannot grow without bound.
const LOG_LIMIT: usize = 200;

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

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    News,
    Logs,
}

/// The two title-bar buttons, drawn as strokes rather than characters.
#[derive(Clone, Copy)]
enum Glyph {
    Minimise,
    Close,
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

    tab: Tab,
    /// Every status line the worker has sent, in order.
    log: Vec<String>,
    settings_open: bool,

    background: Option<egui::TextureHandle>,
    icon: Option<egui::TextureHandle>,
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
            tab: Tab::News,
            log: Vec::new(),
            settings_open: false,
            background: None,
            icon: None,
        };

        // A bad launcher.toml is worth stopping for. Carrying on would check a
        // different channel than the file says, and the player would have no
        // way to tell.
        if let Some(message) = config_error {
            app.phase = Phase::Failed;
            app.error = Some(message.clone());
            app.log.push(message);
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
                Ok(Msg::Status(s)) => {
                    self.note(s.clone());
                    self.status = s;
                }
                Ok(Msg::Progress { done, total }) => self.progress = Some((done, total)),
                Ok(Msg::Manifest(m)) => self.manifest = Some(*m),
                Ok(Msg::Ready) => {
                    self.phase = Phase::Ready;
                    self.progress = None;
                }
                Ok(Msg::Failed(e)) => {
                    self.phase = Phase::Failed;
                    self.note(e.clone());
                    self.error = Some(e);
                    self.progress = None;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// Records a line for the Logs tab, dropping the oldest once full.
    ///
    /// Repeats are not recorded: `plan` reports progress against the same file
    /// many times over, and a log that is one line repeated eighty times hides
    /// the line before it that says what went wrong.
    fn note(&mut self, line: String) {
        if self.log.last() == Some(&line) {
            return;
        }
        self.log.push(line);
        if self.log.len() > LOG_LIMIT {
            self.log.remove(0);
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

    fn texture(
        slot: &mut Option<egui::TextureHandle>,
        ctx: &egui::Context,
        name: &str,
        bytes: &[u8],
    ) -> Option<egui::TextureHandle> {
        if slot.is_none() {
            let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
            let size = [decoded.width() as usize, decoded.height() as usize];
            let image = egui::ColorImage::from_rgba_unmultiplied(size, decoded.as_raw());
            *slot = Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR));
        }
        slot.clone()
    }
}

impl eframe::App for Launcher {
    // eframe hands over the root Ui, which has no margin and no background of
    // its own -- so what is painted here is the window itself, not something
    // drawn over one.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain();

        let ctx = ui.ctx().clone();

        // The viewport, not ui.max_rect(): the root Ui is handed a rect larger
        // than the window, so measuring the bands off max_rect puts the last of
        // them below the bottom edge.
        let root = ctx.viewport_rect();
        ui.painter().rect_filled(root, CornerRadius::ZERO, theme::NIGHT);

        let (chrome, rest) = split_top(root, CHROME_H);
        let (banner, rest) = split_top(rest, BANNER_H);
        let (tabs, rest) = split_top(rest, TABS_H);
        let (body, status) = split_bottom(rest, STATUS_H);

        self.chrome(ui, chrome, &ctx);
        self.banner(ui, banner, &ctx);
        self.tabs(ui, tabs);
        self.list(ui, split_right(body, RIGHT_W).0);
        self.actions(ui, split_right(body, RIGHT_W).1, &ctx);
        self.status_strip(ui, status);

        if self.settings_open {
            self.settings(ui, body);
        }

        // The window has no decorations, so without this it has no edge: a dark
        // launcher on a dark desktop would end nowhere in particular.
        ui.painter().rect_stroke(
            root,
            CornerRadius::ZERO,
            Stroke::new(1.0, theme::scrim(220)),
            StrokeKind::Inside,
        );
    }
}

impl Launcher {
    /// Title bar: icon, name, and the two buttons the OS would have drawn.
    fn chrome(&mut self, ui: &mut egui::Ui, rect: egui::Rect, ctx: &egui::Context) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::scrim(235));

        // Dragging anywhere on the bar moves the window, which is the one piece
        // of behaviour lost by drawing our own.
        let drag = ui.interact(rect, ui.id().with("drag_chrome"), Sense::click_and_drag());
        if drag.is_pointer_button_down_on() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        if let Some(icon) = Self::texture(&mut self.icon, ctx, "icon", ICON_PNG) {
            let side = 16.0;
            let at = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, rect.center().y - side / 2.0),
                Vec2::splat(side),
            );
            ui.painter().image(
                icon.id(),
                at,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        ui.painter().text(
            egui::pos2(rect.left() + 32.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            "Embervale",
            egui::FontId::proportional(12.0),
            theme::TEXT_DIM,
        );

        let side = 22.0;
        let close = egui::Rect::from_min_size(
            egui::pos2(rect.right() - side - 4.0, rect.center().y - side / 2.0),
            Vec2::splat(side),
        );
        let minimise = close.translate(Vec2::new(-side - 2.0, 0.0));

        if self
            .chrome_button(ui, minimise, Glyph::Minimise, theme::HAZE)
            .clicked()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }
        // Red on hover, because closing the launcher mid-download throws the
        // download away and the button should not look like the other one.
        if self
            .chrome_button(ui, close, Glyph::Close, Color32::from_rgb(0xb4, 0x3a, 0x2a))
            .clicked()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// Drawn rather than typeset. The obvious characters for these (`✕`, `−`)
    /// are not in the default font, and a missing glyph does not fail loudly --
    /// it renders as a tofu box that looks like a third button.
    fn chrome_button(
        &self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        glyph: Glyph,
        hover: Color32,
    ) -> egui::Response {
        let salt = match glyph {
            Glyph::Minimise => "minimise",
            Glyph::Close => "close",
        };
        let response = ui.interact(rect, ui.id().with(salt), Sense::click());
        if response.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(3), hover);
        }

        let stroke = Stroke::new(1.2, theme::TEXT);
        let c = rect.center();
        let r = 4.0;
        match glyph {
            Glyph::Minimise => {
                ui.painter().hline((c.x - r)..=(c.x + r), c.y + r, stroke);
            }
            Glyph::Close => {
                ui.painter()
                    .line_segment([egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)], stroke);
                ui.painter()
                    .line_segment([egui::pos2(c.x + r, c.y - r), egui::pos2(c.x - r, c.y + r)], stroke);
            }
        }
        response
    }

    /// The art, with the wordmark over it. The only place the key art appears
    /// now: at this size a full-bleed background would leave no unbusy pixels
    /// for the text that has to be read.
    fn banner(&mut self, ui: &mut egui::Ui, rect: egui::Rect, ctx: &egui::Context) {
        let art = Self::texture(&mut self.background, ctx, "background", BACKGROUND_PNG);
        paint_cover(ui, rect, art);

        // Darkened towards the bottom, where the wordmark sits.
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::scrim(70));
        let lower = egui::Rect::from_min_max(
            egui::pos2(rect.left(), rect.center().y),
            rect.max,
        );
        ui.painter()
            .rect_filled(lower, CornerRadius::ZERO, theme::scrim(90));

        // Dragging the art moves the window too -- the title bar is 28px, and
        // aiming for it is the kind of thing only the person who built the
        // window finds easy.
        let drag = ui.interact(rect, ui.id().with("drag_banner"), Sense::click_and_drag());
        if drag.is_pointer_button_down_on() {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        ui.painter().text(
            egui::pos2(rect.left() + 20.0, rect.bottom() - 34.0),
            egui::Align2::LEFT_BOTTOM,
            "EMBERVALE",
            egui::FontId::proportional(34.0),
            theme::TEXT,
        );
        ui.painter().text(
            egui::pos2(rect.left() + 22.0, rect.bottom() - 16.0),
            egui::Align2::LEFT_BOTTOM,
            "Launcher",
            egui::FontId::proportional(13.0),
            theme::EMBER,
        );

        let version = self
            .manifest
            .as_ref()
            .map(|m| m.version.clone())
            .unwrap_or_else(|| "—".into());
        ui.painter().text(
            egui::pos2(rect.right() - 20.0, rect.bottom() - 16.0),
            egui::Align2::RIGHT_BOTTOM,
            format!("Version {version}"),
            egui::FontId::proportional(12.0),
            theme::TEXT_DIM,
        );

        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            Stroke::new(1.0, theme::EMBER.gamma_multiply(0.5)),
        );
    }

    fn tabs(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::HAZE);

        let mut x = rect.left() + 12.0;
        for (tab, label) in [(Tab::News, "News"), (Tab::Logs, "Logs")] {
            let width = 74.0;
            let at = egui::Rect::from_min_size(
                egui::pos2(x, rect.top() + 4.0),
                Vec2::new(width, rect.height() - 4.0),
            );
            let selected = self.tab == tab;
            let response = ui.interact(at, ui.id().with(label), Sense::click());

            if selected {
                ui.painter().rect_filled(
                    at,
                    CornerRadius {
                        nw: 4,
                        ne: 4,
                        sw: 0,
                        se: 0,
                    },
                    theme::panel(235),
                );
            } else if response.hovered() {
                ui.painter()
                    .rect_filled(at, CornerRadius::same(4), theme::scrim(80));
            }

            ui.painter().text(
                at.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(12.5),
                if selected { theme::EMBER } else { theme::TEXT_DIM },
            );

            if response.clicked() {
                self.tab = tab;
            }
            x += width + 4.0;
        }
    }

    /// News or logs, depending on the tab. One scrolling panel either way, so
    /// the window never changes height for its content.
    fn list(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        let panel = rect.shrink2(Vec2::new(12.0, 8.0));
        ui.painter()
            .rect_filled(panel, CornerRadius::same(4), theme::panel(235));
        ui.painter().rect_stroke(
            panel,
            CornerRadius::same(4),
            Stroke::new(1.0, theme::scrim(160)),
            StrokeKind::Inside,
        );

        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(panel.shrink(8.0)));
        ui.spacing_mut().item_spacing.y = 4.0;

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(&mut ui, |ui| match self.tab {
                Tab::News => self.news_items(ui),
                Tab::Logs => self.log_lines(ui),
            });
    }

    fn news_items(&self, ui: &mut egui::Ui) {
        let items = self.manifest.as_ref().map(|m| &m.news);
        let empty = items.map(|n| n.is_empty()).unwrap_or(true);
        if empty {
            ui.label(
                RichText::new("No news yet.")
                    .size(12.0)
                    .color(theme::TEXT_DIM),
            );
            return;
        }

        for item in items.into_iter().flatten() {
            ui.horizontal_top(|ui| {
                // The lantern dot standing in for the reference's per-item
                // icon: one warm mark to start the row on.
                let (dot, _) = ui.allocate_exact_size(Vec2::new(12.0, 16.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 3.0, theme::EMBER);

                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(&item.title)
                            .size(12.5)
                            .strong()
                            .color(theme::TEXT),
                    );
                    if !item.date.is_empty() {
                        ui.label(
                            RichText::new(format!("Posted on: {}", item.date))
                                .size(10.5)
                                .color(theme::TEXT_DIM),
                        );
                    }
                    if !item.body.is_empty() {
                        ui.label(
                            RichText::new(&item.body)
                                .size(11.5)
                                .color(theme::TEXT_DIM),
                        );
                    }
                });
            });
            ui.add_space(6.0);
        }
    }

    fn log_lines(&self, ui: &mut egui::Ui) {
        if self.log.is_empty() {
            ui.label(RichText::new("Nothing yet.").size(12.0).color(theme::TEXT_DIM));
            return;
        }
        for line in &self.log {
            ui.label(
                RichText::new(line)
                    .size(11.0)
                    .monospace()
                    .color(theme::TEXT_DIM),
            );
        }
    }

    /// PLAY, and the settings button under it.
    fn actions(&mut self, ui: &mut egui::Ui, rect: egui::Rect, ctx: &egui::Context) {
        let (label, enabled) = match self.phase {
            Phase::Ready => ("PLAY", true),
            Phase::Failed => ("RETRY", true),
            Phase::Working => ("WAIT", false),
        };

        let play_at = egui::Rect::from_min_size(
            egui::pos2(rect.left(), rect.top() + 10.0),
            Vec2::new(rect.width() - 14.0, 62.0),
        );

        let play = egui::Button::new(
            RichText::new(label)
                .size(19.0)
                .strong()
                .color(Color32::from_rgb(0x1a, 0x12, 0x05)),
        )
        .fill(if enabled { theme::EMBER } else { theme::scrim(90) })
        .corner_radius(CornerRadius::same(5))
        .stroke(Stroke::new(
            1.0,
            if enabled {
                theme::EMBER_BRIGHT
            } else {
                theme::scrim(0)
            },
        ));

        if ui.put(play_at, play).clicked() && enabled {
            match self.phase {
                Phase::Ready => self.play(ctx),
                Phase::Failed => self.retry(),
                Phase::Working => {}
            }
        }

        let gear_at = egui::Rect::from_min_size(
            egui::pos2(play_at.center().x - 16.0, play_at.bottom() + 12.0),
            Vec2::splat(32.0),
        );
        let gear = egui::Button::new(RichText::new("⚙").size(15.0).color(theme::TEXT))
            .fill(theme::HAZE)
            .corner_radius(CornerRadius::same(16))
            .stroke(Stroke::new(1.0, theme::scrim(160)));
        if ui.put(gear_at, gear).on_hover_text("Settings").clicked() {
            self.settings_open = !self.settings_open;
        }
    }

    /// What the launcher is pointed at. Read-only: the file is the place to
    /// change it, and a settings panel that silently disagreed with
    /// `launcher.toml` would be worse than none.
    fn settings(&mut self, ui: &mut egui::Ui, body: egui::Rect) {
        let card = body.shrink2(Vec2::new(24.0, 14.0));
        ui.painter()
            .rect_filled(card, CornerRadius::same(5), theme::panel(250));
        ui.painter().rect_stroke(
            card,
            CornerRadius::same(5),
            Stroke::new(1.0, theme::EMBER.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );

        // Swallows clicks so the list behind the card cannot be scrolled or
        // hovered through it.
        ui.interact(card, ui.id().with("settings_modal"), Sense::click_and_drag());

        let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink(12.0)));
        ui.spacing_mut().item_spacing.y = 3.0;

        ui.label(
            RichText::new("SETTINGS")
                .size(11.0)
                .strong()
                .color(theme::EMBER),
        );
        ui.add_space(4.0);

        ui.label(RichText::new("Update channel").size(10.5).color(theme::TEXT_DIM));
        ui.label(
            RichText::new(&self.config.manifest_url)
                .size(10.5)
                .monospace()
                .color(theme::TEXT),
        );
        ui.add_space(4.0);
        ui.label(RichText::new("Installed to").size(10.5).color(theme::TEXT_DIM));
        ui.label(
            RichText::new(self.config.install_dir.display().to_string())
                .size(10.5)
                .monospace()
                .color(theme::TEXT),
        );

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui
                .add(
                    egui::Button::new(RichText::new("Open folder").size(11.0).color(theme::TEXT))
                        .fill(theme::HAZE)
                        .corner_radius(CornerRadius::same(3)),
                )
                .clicked()
            {
                open_folder(&self.config.install_dir);
            }
            if ui
                .add(
                    egui::Button::new(RichText::new("Close").size(11.0).color(theme::TEXT))
                        .fill(theme::HAZE)
                        .corner_radius(CornerRadius::same(3)),
                )
                .clicked()
            {
                self.settings_open = false;
            }
        });
    }

    /// The one line saying what is happening, and the bar underneath it.
    fn status_strip(&self, ui: &mut egui::Ui, rect: egui::Rect) {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::HAZE);
        ui.painter()
            .hline(rect.x_range(), rect.top(), Stroke::new(1.0, theme::scrim(140)));

        let inner = rect.shrink2(Vec2::new(14.0, 7.0));
        let mut ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::top_down(Align::LEFT)),
        );
        ui.spacing_mut().item_spacing.y = 3.0;

        let (text, colour) = match (&self.phase, &self.error) {
            (Phase::Failed, Some(e)) => (e.as_str(), theme::EMBER_BRIGHT),
            _ => (self.status.as_str(), theme::TEXT_DIM),
        };
        ui.label(RichText::new(text).size(11.5).color(colour));

        if let Some((done, total)) = self.progress {
            let fraction = if total == 0 {
                0.0
            } else {
                done as f32 / total as f32
            };
            ui.add(
                egui::ProgressBar::new(fraction.clamp(0.0, 1.0))
                    .desired_height(8.0)
                    .corner_radius(CornerRadius::same(4))
                    .fill(theme::ARCANE)
                    .text(
                        RichText::new(format!(
                            "{} / {}",
                            human_bytes(done),
                            human_bytes(total)
                        ))
                        .size(9.5),
                    ),
            );
        }
    }
}

#[cfg(target_os = "windows")]
fn open_folder(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

#[cfg(not(target_os = "windows"))]
fn open_folder(_path: &std::path::Path) {}

/// Draws the art so it covers the rect without distorting: scale to the larger
/// ratio and centre, cropping the overflow.
fn paint_cover(ui: &egui::Ui, rect: egui::Rect, texture: Option<egui::TextureHandle>) {
    let Some(texture) = texture else {
        ui.painter()
            .rect_filled(rect, CornerRadius::ZERO, theme::NIGHT);
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

    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
}

fn split_top(rect: egui::Rect, height: f32) -> (egui::Rect, egui::Rect) {
    let split = rect.top() + height;
    (
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), split)),
        egui::Rect::from_min_max(egui::pos2(rect.left(), split), rect.max),
    )
}

fn split_bottom(rect: egui::Rect, height: f32) -> (egui::Rect, egui::Rect) {
    let split = rect.bottom() - height;
    (
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), split)),
        egui::Rect::from_min_max(egui::pos2(rect.left(), split), rect.max),
    )
}

fn split_right(rect: egui::Rect, width: f32) -> (egui::Rect, egui::Rect) {
    let split = rect.right() - width;
    (
        egui::Rect::from_min_max(rect.min, egui::pos2(split, rect.bottom())),
        egui::Rect::from_min_max(egui::pos2(split, rect.top()), rect.max),
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
    let decoded = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
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

    #[test]
    fn the_bands_tile_the_window_exactly() {
        let root = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), WINDOW);
        let (chrome, rest) = split_top(root, CHROME_H);
        let (banner, rest) = split_top(rest, BANNER_H);
        let (tabs, rest) = split_top(rest, TABS_H);
        let (body, status) = split_bottom(rest, STATUS_H);

        // No gaps and no overlaps: each band starts where the last one ended.
        assert_eq!(chrome.bottom(), banner.top());
        assert_eq!(banner.bottom(), tabs.top());
        assert_eq!(tabs.bottom(), body.top());
        assert_eq!(body.bottom(), status.top());
        assert_eq!(status.bottom(), root.bottom());
        // And the body is left with room to be worth drawing.
        assert!(body.height() > 100.0, "body was {}", body.height());
    }
}
