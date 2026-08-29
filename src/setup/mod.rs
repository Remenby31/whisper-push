//! The setup UI — the first-launch wizard and the license modal, drawn with
//! egui so **Windows and Linux get the same wizard macOS gets** from its SwiftUI
//! helper: same six screens, same brand palette, same copy, same order.
//!
//! Three things shape this module:
//!
//! * **It always runs in its own process** (`whisper-push --setup-ui`, spawned by
//!   `onboarding`). A winit event loop can only be built once per process, and
//!   the tray owns one for the daemon's whole life — so the wizard could never
//!   share it. Being a child process also means closing the window can't take
//!   the daemon down with it, and the parent learns the outcome from one JSON
//!   line on stdout, exactly as the macOS helper reports it.
//! * **It is the same binary**, so it calls `model_manager`, `permissions`,
//!   `license` and `autostart` directly. The SwiftUI wizard has to shell out for
//!   all of that and re-implements the model downloader in Swift; here there is
//!   one downloader, one model list, one permission model.
//! * **It compiles on macOS too**, though the shipped macOS app prefers the
//!   SwiftUI helper. That's deliberate: it can be run and eyeballed side by side
//!   with the real thing on a Mac (`whisper-push --setup-ui --design-preview`),
//!   which is the only practical way to keep "exactly like macOS" honest — and
//!   it gives macOS dev builds a real wizard instead of a bare popup.

pub mod dialog;
mod icons;
mod theme;

use crate::model_manager::ModelInfo;
use crate::permissions::{PermKind, PermState, PermissionStatus};
use eframe::egui::{self, Align, CornerRadius, Layout, Rect, Sense, Stroke, StrokeKind, Vec2};
use icons::Glyph;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Which UI the process was started for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Full first-launch wizard (welcome → … → ready), reports its result on stdout.
    Wizard,
    /// Standalone license modal (menu bar → License). `activate` lands on the
    /// "enter your key" screen instead of the paywall.
    License { activate: bool },
    /// One small dialog (Add Word…, Edit/Delete, confirm) — see `crate::dialog`.
    Dialog(crate::dialog::Spec),
}

/// Window size — the SwiftUI wizard's 520×440.
const W: f32 = 520.0;
const H: f32 = 440.0;
/// Primary CTA height: 15 pt label + the SwiftUI style's 12 pt vertical padding.
const BUTTON_H: f32 = 41.0;

/// Run the setup UI and block until the window closes.
///
/// `shots`: write one PNG per screen into that directory and exit. That's how
/// "the same wizard as macOS" gets checked rather than asserted — the two can be
/// put side by side, and it works headless-ish on any platform (it renders
/// through the same GL path the user sees, not a system screen grab).
pub fn run(
    mode: Mode,
    design_preview: bool,
    shots: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // A dialog is its own tiny window, not a wizard screen.
    if let Mode::Dialog(spec) = mode {
        return dialog::run(spec);
    }
    let icon = theme::app_icon_rgba(64).map(|(w, h, rgba)| egui::IconData {
        rgba,
        width: w,
        height: h,
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([W, H])
        .with_min_inner_size([W, H])
        .with_resizable(false)
        .with_title("Whisper Push");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    // The license modal is fired from the menu bar while the user is busy in
    // another app; like its macOS counterpart it floats above everything until
    // acted on. The full wizard owns the session and stays a normal window.
    if matches!(mode, Mode::License { .. }) {
        viewport = viewport.with_always_on_top();
    }
    let mode = mode.clone();

    let options = eframe::NativeOptions {
        viewport,
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "Whisper Push",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, mode, design_preview, shots)))),
    )
    .map_err(|e| anyhow::anyhow!("setup UI failed: {e}"))
}

/// Wizard steps, in the order the user walks them. Permissions come BEFORE the
/// paywall so the user is set up first, and before the model picker so grants
/// happen while nothing is running — same order as `OnboardingState.Step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    Welcome,
    Permissions,
    License,
    Model,
    Download,
    Ready,
}

impl Step {
    const ORDER: [Step; 6] = [
        Step::Welcome,
        Step::Permissions,
        Step::License,
        Step::Model,
        Step::Download,
        Step::Ready,
    ];
    fn next(self) -> Option<Step> {
        let i = Self::ORDER.iter().position(|s| *s == self)?;
        Self::ORDER.get(i + 1).copied()
    }
}

/// Which face of the license screen is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LicenseFace {
    /// The two plan cards.
    Choose,
    /// Key entry (after a purchase, or "I already have a key").
    Activate,
    /// Already licensed: plan, email, key, deactivate.
    Licensed,
}

struct App {
    mode: Mode,
    design_preview: bool,
    step: Step,
    logo: Option<egui::TextureHandle>,

    // Wizard choices
    models: Vec<ModelInfo>,
    selected: Vec<String>,
    recommended: String,
    hardware: String,
    auto_start: bool,

    // Permissions, polled while their screen is up
    perms: PermissionStatus,
    perms_polled: Instant,

    // Download job
    download: Option<Download>,

    // License screen
    face: LicenseFace,
    plan_lifetime: bool,
    key: String,
    busy: bool,
    message: Option<String>,
    activated: bool,
    license: crate::license::LicenseSnapshot,
    reveal_key: bool,
    /// Set once the user has finished; the frame after, the window closes.
    finished: bool,

    /// Screenshot mode: where to write, which screen is next, and how many
    /// frames to let settle first (fonts and the logo texture upload on frame 1).
    shots: Option<std::path::PathBuf>,
    shot_queue: Vec<(&'static str, Step, LicenseFace)>,
    shot_settle: u32,
}

/// A running model download, driven on a worker thread.
struct Download {
    rx: mpsc::Receiver<DownloadMsg>,
    fraction: f32,
    status: String,
    file: String,
    done_files: usize,
    total_files: usize,
    bytes: u64,
    total_bytes: u64,
    done: bool,
    error: Option<String>,
}

enum DownloadMsg {
    /// Started fetching this model (for the "Downloading X…" line).
    Model(String),
    /// One chunk landed. Indices are absolute across the whole job, so the bar
    /// runs 0→100 once for every model together, not once per model.
    Progress {
        file_index: usize,
        file_name: String,
        bytes: u64,
        total_bytes: u64,
    },
    Failed(String),
    Done,
}

impl App {
    fn new(
        cc: &eframe::CreationContext<'_>,
        mode: Mode,
        design_preview: bool,
        shots: Option<std::path::PathBuf>,
    ) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        // The wizard is a light, branded surface (racing green on cream) in both
        // OS themes: a dark-mode machine must not get dark-on-dark, and the
        // screens must look the same everywhere. Same call as the SwiftUI
        // wizard's `.preferredColorScheme(.light)`.
        cc.egui_ctx.all_styles_mut(|style| {
            style.visuals.panel_fill = theme::CREAM;
            style.visuals.window_fill = theme::CREAM;
            style.visuals.override_text_color = Some(theme::GREEN);
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        });

        let hw = crate::hardware::detect();
        let recommended =
            crate::model_manager::model_for_backend(crate::hardware::recommend_backend(&hw))
                .to_string();
        let models = crate::model_manager::list_models();
        let license = crate::license::snapshot();

        let step = match mode {
            Mode::License { .. } => Step::License,
            _ => Step::Welcome,
        };
        let face = if matches!(mode, Mode::License { activate: true }) {
            LicenseFace::Activate
        } else if license.licensed() {
            // A licensed user gets the "License active" screen, never the
            // paywall — and on the FIRST frame: the snapshot is read before the
            // window exists, so there is no flash of the wrong screen.
            LicenseFace::Licensed
        } else {
            LicenseFace::Choose
        };

        let mut app = Self {
            mode,
            design_preview,
            step,
            logo: theme::load_logo(&cc.egui_ctx),
            selected: default_selection(&models, &recommended),
            models,
            recommended,
            hardware: hw.gpu.label().to_string(),
            auto_start: true,
            perms: crate::permissions::check_all(),
            perms_polled: Instant::now(),
            download: None,
            face,
            plan_lifetime: false,
            // The key field starts on whatever the clipboard holds if it looks
            // like one — the user has just copied it out of the purchase email.
            key: clipboard_key(true).unwrap_or_default(),
            busy: false,
            message: None,
            activated: false,
            license,
            reveal_key: false,
            finished: false,
            shots,
            // Every screen the wizard can show, in wizard order.
            shot_queue: vec![
                ("1-welcome", Step::Welcome, LicenseFace::Choose),
                ("2-permissions", Step::Permissions, LicenseFace::Choose),
                ("3-license-plans", Step::License, LicenseFace::Choose),
                ("3b-license-activate", Step::License, LicenseFace::Activate),
                ("3c-license-active", Step::License, LicenseFace::Licensed),
                ("4-models", Step::Model, LicenseFace::Choose),
                ("5-download", Step::Download, LicenseFace::Choose),
                ("6-ready", Step::Ready, LicenseFace::Choose),
            ],
            shot_settle: 0,
        };
        if app.design_preview {
            // Show every row this platform has, all ungranted — the same thing
            // the SwiftUI poller does in preview, so the designer sees the
            // "Grant" state rather than a screen that's already done.
            app.perms = PermissionStatus {
                items: crate::permissions::tracked()
                    .iter()
                    .map(|&kind| crate::permissions::Perm {
                        kind,
                        state: PermState::NotRequested,
                    })
                    .collect(),
            };
        }
        app
    }

    /// Move to the next step, skipping Download when there's nothing to fetch.
    fn advance(&mut self) {
        let Some(mut next) = self.step.next() else {
            self.finish();
            return;
        };
        if next == Step::Download && self.pending_downloads().is_empty() {
            next = next.next().unwrap_or(Step::Ready);
        }
        self.step = next;
    }

    /// Models the user picked that aren't on disk yet.
    fn pending_downloads(&self) -> Vec<String> {
        if self.design_preview {
            return vec![];
        }
        self.selected
            .iter()
            .filter(|m| !crate::model_manager::missing_files(m).is_empty())
            .cloned()
            .collect()
    }

    /// The model the daemon should start with: the recommended one when it was
    /// kept, else whatever the user picked first.
    fn primary_model(&self) -> String {
        if self.selected.contains(&self.recommended) {
            return self.recommended.clone();
        }
        self.selected
            .first()
            .cloned()
            .unwrap_or_else(|| self.recommended.clone())
    }

    /// Report the outcome to the daemon and close. The JSON line is the same
    /// shape the SwiftUI helper prints, so `onboarding` parses one format.
    fn finish(&mut self) {
        if !self.design_preview {
            let result = serde_json::json!({
                "model": self.primary_model(),
                "download": self.selected,
                "auto_start": self.auto_start,
            });
            println!("{result}");
        }
        self.finished = true;
    }

    fn close(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = theme::CREAM;
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            1.0,
        ]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.finished {
            self.close(&ctx);
            return;
        }
        if self.shots.is_some() {
            self.drive_screenshots(&ctx);
        }
        self.pump_download(&ctx);
        self.poll_permissions(&ctx);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CREAM))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                match self.step {
                    Step::Welcome => self.welcome(ui),
                    Step::Permissions => self.permissions(ui),
                    Step::License => self.license(ui, &ctx),
                    Step::Model => self.model_picker(ui),
                    Step::Download => self.download_screen(ui),
                    Step::Ready => self.ready(ui),
                }
            });
    }
}

// ── Screens ──────────────────────────────────────────────────────────────────

impl App {
    /// The wizard's primary CTA, pinned `margin` above the bottom edge with the
    /// 60 pt side inset every SwiftUI screen uses
    /// (`.padding(.horizontal, 60).padding(.bottom, 28)`).
    fn cta(&mut self, ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
        self.cta_margin(ui, label, enabled, 28.0)
    }

    fn cta_margin(&mut self, ui: &mut egui::Ui, label: &str, enabled: bool, margin: f32) -> bool {
        let rect = Rect::from_min_max(
            egui::pos2(
                ui.max_rect().left() + 60.0,
                ui.max_rect().bottom() - margin - BUTTON_H,
            ),
            egui::pos2(
                ui.max_rect().right() - 60.0,
                ui.max_rect().bottom() - margin,
            ),
        );
        inline_at(ui, rect, |ui| {
            theme::primary_button(ui, label, enabled).clicked()
        })
    }

    /// The same button, flowing at the cursor instead of pinned — the license
    /// screens stack CTA + links under the content rather than at the edge.
    fn cta_here(&mut self, ui: &mut egui::Ui, label: &str, enabled: bool) -> bool {
        let top = ui.cursor().top();
        let rect = Rect::from_min_max(
            egui::pos2(ui.max_rect().left() + 70.0, top),
            egui::pos2(ui.max_rect().right() - 70.0, top + BUTTON_H),
        );
        // `inline_at` (scope_builder) already advances the parent cursor by the
        // space the child used — adding BUTTON_H here would double it.
        inline_at(ui, rect, |ui| {
            theme::primary_button(ui, label, enabled).clicked()
        })
    }

    /// A centred text link flowing at the cursor.
    fn link_here(&mut self, ui: &mut egui::Ui, label: &str, size: f32, alpha: f32) -> bool {
        let top = ui.cursor().top();
        let rect = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), top),
            egui::pos2(ui.max_rect().right(), top + size + 6.0),
        );
        inline_at(ui, rect, |ui| {
            let mut hit = false;
            ui.vertical_centered(|ui| hit = theme::link(ui, label, size, alpha).clicked());
            hit
        })
    }

    /// A centred text link pinned `margin` above the bottom edge.
    fn link_bottom(&mut self, ui: &mut egui::Ui, label: &str, margin: f32) -> bool {
        let rect = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), ui.max_rect().bottom() - margin - 20.0),
            egui::pos2(ui.max_rect().right(), ui.max_rect().bottom() - margin),
        );
        inline_at(ui, rect, |ui| {
            let mut hit = false;
            ui.vertical_centered(|ui| hit = theme::link(ui, label, 13.0, 0.85).clicked());
            hit
        })
    }

    fn welcome(&mut self, ui: &mut egui::Ui) {
        ui.add_space(46.0);
        ui.vertical_centered(|ui| {
            theme::draw_logo(ui, self.logo.as_ref(), 96.0);
            ui.add_space(22.0);
            theme::label(ui, "Whisper Push", 24.0, true, theme::GREEN);
            ui.add_space(6.0);
            theme::label(
                ui,
                "Push to talk voice dictation",
                13.0,
                false,
                theme::green_a(0.6),
            );
        });
        ui.add_space(22.0);

        let rows = [
            (Glyph::ShieldCheck, local_promise()),
            (Glyph::Zap, "GPU accelerated transcription."),
            (Glyph::Keyboard, "Hold a key, speak, release."),
        ];
        let inset = Rect::from_min_max(
            egui::pos2(ui.max_rect().left() + 40.0, ui.cursor().top()),
            egui::pos2(ui.max_rect().right() - 40.0, ui.max_rect().bottom()),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(inset), |ui| {
            for (glyph, text) in rows {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
                    ui.painter().circle_filled(
                        r.center(),
                        11.0,
                        theme::blend(theme::CITRON, theme::CREAM, 0.6),
                    );
                    icons::draw(ui.painter(), glyph, r.shrink(5.0), theme::GREEN);
                    ui.add_space(12.0);
                    theme::label(ui, text, 13.0, false, theme::green_a(0.85));
                });
                ui.add_space(10.0);
            }
        });

        if self.cta(ui, "Get Started", true) {
            self.advance();
        }
    }

    fn permissions(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| theme::label(ui, "Grant permissions", 24.0, true, theme::GREEN));
        ui.add_space(16.0);

        // Same 340 pt column as the model picker, so the two adjacent steps line
        // up instead of jumping width.
        let col = centered_column(ui, 340.0);
        let mut request: Option<PermKind> = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
            if self.perms.items.is_empty() {
                // Nothing to grant on this platform (or design preview).
                ui.add_space(10.0);
                theme::label(ui, no_permissions_line(), 13.0, false, theme::green_a(0.7));
                return;
            }
            for p in self.perms.items.clone() {
                ui.horizontal(|ui| {
                    ui.add_space(0.0);
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                    icons::draw(ui.painter(), perm_glyph(p.kind), r, theme::GREEN);
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.add_space(4.0);
                        theme::label(ui, p.kind.title(), 14.0, true, theme::GREEN);
                        theme::label(ui, p.kind.hint(), 11.0, false, theme::green_a(0.55));
                        ui.add_space(4.0);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if p.state == PermState::Granted {
                            theme::badge(ui, "Granted");
                        } else {
                            let label = if p.state == PermState::Denied {
                                open_settings_label()
                            } else {
                                "Grant"
                            };
                            if theme::row_button(ui, label, true).clicked() {
                                request = Some(p.kind);
                            }
                        }
                    });
                });
                ui.add_space(6.0);
            }
        });

        if let Some(kind) = request {
            // Fire and forget: the prompt can sit for 30 s waiting on the user,
            // and this is a button action. The poller below reports the outcome.
            grant(kind);
        }

        let all = self.perms.all_granted();
        let label = if all {
            "Continue"
        } else {
            "Continue without all permissions"
        };
        if self.cta(ui, label, true) {
            self.advance();
        }
    }

    fn model_picker(&mut self, ui: &mut egui::Ui) {
        ui.add_space(22.0);
        ui.vertical_centered(|ui| {
            theme::label(ui, "Choose your engines", 22.0, true, theme::GREEN);
            ui.add_space(2.0);
            theme::label(
                ui,
                &format!("Detected: {}", self.hardware),
                11.0,
                false,
                theme::green_a(0.6),
            );
        });
        ui.add_space(12.0);

        let col = centered_column(ui, 340.0);
        let mut toggle: Option<String> = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(col), |ui| {
            egui::ScrollArea::vertical()
                .max_height(ui.max_rect().height() - 120.0)
                .show(ui, |ui| {
                    for m in &self.models {
                        let installed = m.is_downloaded;
                        let checked = installed || self.selected.iter().any(|s| s == m.name);
                        let row_top = ui.cursor().top();
                        ui.horizontal(|ui| {
                            ui.add_space(0.0);
                            if theme::checkbox(ui, checked, 20.0).clicked() && !installed {
                                toggle = Some(m.name.to_string());
                            }
                            ui.add_space(12.0);
                            theme::label(ui, m.label, 13.0, false, theme::GREEN);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let size = if installed {
                                    "Installed".to_string()
                                } else {
                                    format_size(m.size_mb)
                                };
                                let galley = ui.painter().layout_no_wrap(
                                    size,
                                    theme::mono(11.0),
                                    theme::green_a(0.55),
                                );
                                let (r, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
                                ui.painter().galley(r.left_top(), galley, theme::GREEN);
                                if let Some(warning) = ram_warning(m.name) {
                                    ui.add_space(8.0);
                                    let (r, resp) =
                                        ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                    icons::draw(ui.painter(), Glyph::Alert, r, theme::green_a(0.5));
                                    resp.on_hover_text(warning);
                                }
                            });
                        });
                        // The whole row is the hit target, like the SwiftUI
                        // `.contentShape(Rectangle()).onTapGesture`.
                        let row = Rect::from_min_max(
                            egui::pos2(ui.max_rect().left(), row_top),
                            egui::pos2(ui.max_rect().right(), ui.cursor().top()),
                        );
                        if !installed
                            && ui
                                .interact(row, ui.id().with(m.name), Sense::click())
                                .clicked()
                        {
                            toggle = Some(m.name.to_string());
                        }
                        ui.add_space(8.0);
                    }
                });
        });

        if let Some(name) = toggle {
            match self.selected.iter().position(|s| *s == name) {
                // Never leave the user with nothing selected.
                Some(i) if self.selected.len() > 1 => {
                    self.selected.remove(i);
                }
                Some(_) => {}
                None => self.selected.push(name),
            }
        }

        let pending = self.pending_downloads();
        let summary = if pending.is_empty() {
            format!(
                "{} engine{} selected. All installed.",
                self.selected.len(),
                if self.selected.len() == 1 { "" } else { "s" }
            )
        } else {
            let mb: u32 = pending
                .iter()
                .filter_map(|n| self.models.iter().find(|m| m.name == n))
                .map(|m| m.size_mb)
                .sum();
            format!("{} to download \u{b7} {}", pending.len(), format_size(mb))
        };
        let bar = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), ui.max_rect().bottom() - 92.0),
            egui::pos2(ui.max_rect().right(), ui.max_rect().bottom() - 72.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            ui.vertical_centered(|ui| theme::label(ui, &summary, 11.0, false, theme::green_a(0.6)));
        });

        let label = if pending.is_empty() {
            "Continue"
        } else {
            "Download & Continue"
        };
        if self.cta(ui, label, !self.selected.is_empty()) {
            self.advance();
        }
    }

    fn download_screen(&mut self, ui: &mut egui::Ui) {
        if self.download.is_none() {
            self.start_download();
        }
        let dl = self.download.as_ref();
        let done = dl.map(|d| d.done).unwrap_or(true);

        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            theme::draw_logo(ui, self.logo.as_ref(), 96.0);
            ui.add_space(18.0);
            if let Some(err) = dl.and_then(|d| d.error.clone()) {
                theme::label(ui, "Download failed", 22.0, true, theme::GREEN);
                ui.add_space(8.0);
                theme::label(ui, &err, 11.0, false, theme::green_a(0.65));
                ui.add_space(8.0);
                theme::label(
                    ui,
                    "You can continue and retry later from the menu \u{2192} Engine.",
                    11.0,
                    false,
                    theme::green_a(0.55),
                );
            } else if done {
                theme::label(ui, "Downloads complete", 22.0, true, theme::GREEN);
            } else if let Some(d) = dl {
                theme::label(ui, &d.status, 17.0, true, theme::GREEN);
                ui.add_space(14.0);
                progress_bar(ui, d.fraction, 400.0);
                ui.add_space(10.0);
                if d.total_files > 0 {
                    theme::label(
                        ui,
                        &format!(
                            "File {} of {} \u{b7} {}",
                            (d.done_files + 1).min(d.total_files),
                            d.total_files,
                            d.file
                        ),
                        11.0,
                        false,
                        theme::green_a(0.7),
                    );
                }
                if d.total_bytes > 0 {
                    theme::label(
                        ui,
                        &format!(
                            "{} of {} for this file",
                            format_bytes(d.bytes),
                            format_bytes(d.total_bytes)
                        ),
                        10.0,
                        false,
                        theme::green_a(0.5),
                    );
                }
                ui.add_space(6.0);
                theme::label(
                    ui,
                    &format!("{}%", (d.fraction * 100.0) as u32),
                    14.0,
                    true,
                    theme::GREEN,
                );
            }
        });

        let label = if done {
            "Continue"
        } else {
            "Downloading\u{2026}"
        };
        if self.cta(ui, label, done) {
            self.advance();
        }
    }

    fn ready(&mut self, ui: &mut egui::Ui) {
        let cfg = crate::config::Config::load().unwrap_or_default();
        let hotkey = crate::tray::format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode);

        ui.add_space(44.0);
        ui.vertical_centered(|ui| {
            theme::draw_logo(ui, self.logo.as_ref(), 96.0);
            ui.add_space(22.0);
            theme::label(ui, "You're all set", 24.0, true, theme::GREEN);
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                // Centre the row by hand: egui's `horizontal` inside
                // `vertical_centered` still lays out from the left.
                let w = 250.0;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                theme::key_cap(ui, &hotkey);
                ui.add_space(8.0);
                arrow(ui);
                ui.add_space(8.0);
                theme::label(ui, "speak", 14.0, false, theme::green_a(0.85));
                ui.add_space(8.0);
                arrow(ui);
                ui.add_space(8.0);
                theme::label(ui, "release", 14.0, false, theme::green_a(0.85));
            });
            ui.add_space(20.0);
            ui.horizontal(|ui| {
                let w = 200.0;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                if theme::checkbox(ui, self.auto_start, 20.0).clicked() {
                    self.auto_start = !self.auto_start;
                }
                ui.add_space(10.0);
                theme::label(ui, "Launch at login", 14.0, false, theme::GREEN);
            });
            ui.add_space(14.0);
            // Where the app actually lives once it starts — the single most
            // asked question on Windows, where a new tray icon hides in the
            // overflow flyout until it's pinned.
            theme::label(ui, tray_hint(), 11.0, false, theme::green_a(0.6));
        });

        if self.cta(ui, "Start Whisper Push", true) {
            self.finish();
        }
    }
}

// ── License screen ───────────────────────────────────────────────────────────

/// Variant-locked, permanent Lemon Squeezy checkout links (LIVE).
const CHECKOUT_MONTHLY: &str =
    "https://whisperpush.lemonsqueezy.com/checkout/buy/2baac143-5393-465e-8d0c-66ee9bd12ab3";
const CHECKOUT_LIFETIME: &str =
    "https://whisperpush.lemonsqueezy.com/checkout/buy/04ecf078-9a78-4daf-a5a5-edf77a019c07";
/// Lemon Squeezy's hosted customer portal for this store.
const BILLING_PORTAL: &str = "https://whisperpush.lemonsqueezy.com/billing";

impl App {
    fn license(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        match self.face.clone() {
            LicenseFace::Choose => self.license_choose(ui),
            LicenseFace::Activate => self.license_activate(ui, ctx),
            LicenseFace::Licensed => self.license_licensed(ui),
        }
    }

    fn license_choose(&mut self, ui: &mut egui::Ui) {
        ui.add_space(26.0);
        ui.vertical_centered(|ui| {
            theme::label(ui, "Unlock Whisper Push", 22.0, true, theme::GREEN);
            ui.add_space(6.0);
        });
        let sub = Rect::from_min_max(
            egui::pos2(ui.max_rect().left() + 36.0, ui.cursor().top()),
            egui::pos2(ui.max_rect().right() - 36.0, ui.cursor().top() + 32.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(sub), |ui| {
            ui.vertical_centered(|ui| {
                theme::label(
                    ui,
                    "Unlimited dictation \u{b7} every engine \u{b7} up to 5 devices \u{b7} 100% on-device",
                    12.0,
                    false,
                    theme::green_a(0.65),
                )
            });
        });

        let cards = Rect::from_min_max(
            egui::pos2(ui.max_rect().center().x - 190.0, sub.bottom() + 12.0),
            egui::pos2(ui.max_rect().center().x + 190.0, sub.bottom() + 132.0),
        );
        let half = (cards.width() - 12.0) / 2.0;
        let monthly = Rect::from_min_size(cards.min, Vec2::new(half, cards.height()));
        let lifetime = Rect::from_min_size(
            egui::pos2(cards.min.x + half + 12.0, cards.min.y),
            Vec2::new(half, cards.height()),
        );
        if plan_card(
            ui,
            monthly,
            "Flexible",
            "Monthly",
            "4,99 \u{20ac}",
            "per month",
            !self.plan_lifetime,
        ) {
            self.plan_lifetime = false;
        }
        if plan_card(
            ui,
            lifetime,
            "Best value",
            "Lifetime",
            "49,99 \u{20ac}",
            "one-time",
            self.plan_lifetime,
        ) {
            self.plan_lifetime = true;
        }

        // The macOS modal embeds the Lemon Squeezy form in a WKWebView. There is
        // no in-process web view here, so checkout opens in the user's browser —
        // then they come back with the key. Say so, rather than let the window
        // seem to do nothing when the browser takes focus.
        let note = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), cards.bottom() + 14.0),
            egui::pos2(ui.max_rect().right(), cards.bottom() + 32.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(note), |ui| {
            ui.horizontal(|ui| {
                let w = 230.0;
                ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                let (r, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                icons::draw(ui.painter(), Glyph::Lock, r, theme::green_a(0.7));
                ui.add_space(5.0);
                theme::label(
                    ui,
                    "Secure checkout in your browser",
                    11.0,
                    true,
                    theme::green_a(0.7),
                );
            });
        });

        // Flowing (not pinned) so the two links sit under the button, exactly
        // like the SwiftUI VStack.
        let flow = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), note.bottom() + 18.0),
            ui.max_rect().max,
        );
        let mut go = false;
        let mut activate = false;
        ui.scope_builder(egui::UiBuilder::new().max_rect(flow), |ui| {
            go = self.cta_here(ui, "Continue", true);
            ui.add_space(10.0);
            activate = self.link_here(ui, "I already have a license key", 12.0, 0.8);
        });
        if go {
            open_url(if self.plan_lifetime {
                CHECKOUT_LIFETIME
            } else {
                CHECKOUT_MONTHLY
            });
            self.message = Some(
                "Checkout opened in your browser. Paste the key from your email below.".into(),
            );
            self.face = LicenseFace::Activate;
        }
        if activate {
            self.message = None;
            self.face = LicenseFace::Activate;
        }
        if self.link_bottom(ui, self.trial_label(), 18.0) {
            self.proceed();
        }
    }

    fn license_activate(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            theme::draw_logo(ui, self.logo.as_ref(), 52.0);
            ui.add_space(10.0);
            theme::label(
                ui,
                if self.activated {
                    "You're all set"
                } else {
                    "Activate"
                },
                22.0,
                true,
                theme::GREEN,
            );
        });

        if !self.activated {
            ui.add_space(4.0);
            ui.vertical_centered(|ui| {
                theme::label(
                    ui,
                    "Paste the license key from your purchase email.",
                    12.0,
                    false,
                    theme::green_a(0.65),
                )
            });
            ui.add_space(14.0);

            // Field + paste button together are 340 wide, so the GROUP is
            // centred (not the field, with the button hanging off the side).
            let field = Rect::from_min_size(
                egui::pos2(ui.max_rect().center().x - 170.0, ui.cursor().top()),
                Vec2::new(340.0, 30.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(field), |ui| {
                ui.horizontal(|ui| {
                    let edit = egui::TextEdit::singleline(&mut self.key)
                        .hint_text("XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX")
                        .font(theme::mono(12.0))
                        .desired_width(304.0)
                        .margin(egui::Margin::symmetric(8, 6))
                        .background_color(theme::WHITE);
                    let resp = ui.add(edit);
                    // Focus on arrival so typing and Ctrl+V just work.
                    if !self.busy && !ctx.memory(|m| m.focused().is_some()) {
                        resp.request_focus();
                    }
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.activate();
                    }
                    ui.add_space(6.0);
                    // One-click paste: keyboard focus can be flaky for a window
                    // spawned by a background agent.
                    let (r, resp) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
                    icons::draw(
                        ui.painter(),
                        Glyph::ClipboardPaste,
                        r.shrink(2.0),
                        theme::GREEN,
                    );
                    if resp.on_hover_text("Paste from clipboard").clicked()
                        && let Some(s) = clipboard_key(false)
                    {
                        self.key = s;
                    }
                });
            });
            ui.add_space(34.0);
        }

        self.show_message(ui);

        if self.activated {
            if self.cta_margin(ui, self.done_label(), true, 18.0) {
                self.proceed();
            }
            return;
        }
        let can = !self.busy && !self.key.trim().is_empty();
        let label = if self.busy {
            "Activating\u{2026}"
        } else {
            "Activate"
        };
        let mut go = false;
        let mut buy = false;
        // Right under the key field (logo 24+52, title, hint, field ≈ 190).
        let flow = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), ui.max_rect().top() + 196.0),
            ui.max_rect().max,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(flow), |ui| {
            go = self.cta_here(ui, label, can);
            ui.add_space(10.0);
            buy = self.link_here(ui, "Buy a license", 12.0, 0.8);
        });
        if go {
            self.activate();
        }
        if buy {
            self.message = None;
            self.face = LicenseFace::Choose;
        }
        if self.link_bottom(ui, self.trial_label(), 18.0) {
            self.proceed();
        }
    }

    fn license_licensed(&mut self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            theme::draw_logo(ui, self.logo.as_ref(), 52.0);
            ui.add_space(10.0);
            theme::label(ui, "License active", 22.0, true, theme::GREEN);
            ui.add_space(12.0);
            let lic = self.license.clone();
            theme::label(ui, &plan_label(&lic), 14.0, true, theme::GREEN);
            if let Some(email) = lic.email.as_deref().filter(|e| !e.is_empty()) {
                ui.add_space(4.0);
                theme::label(
                    ui,
                    &format!("Licensed to {email}"),
                    12.0,
                    false,
                    theme::green_a(0.65),
                );
            }
            // The key itself, so activating a second device doesn't mean digging
            // through the purchase email. Masked by default — the one thing here
            // worth hiding from a screen-share.
            if let Some(k) = lic.key.as_deref().filter(|k| !k.is_empty()) {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let w = 320.0;
                    ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                    let shown = if self.reveal_key {
                        k.to_string()
                    } else {
                        masked(k)
                    };
                    let galley =
                        ui.painter()
                            .layout_no_wrap(shown, theme::mono(11.0), theme::green_a(0.75));
                    let (r, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
                    ui.painter().galley(r.left_top(), galley, theme::GREEN);
                    if resp
                        .on_hover_text(if self.reveal_key {
                            "Click to hide"
                        } else {
                            "Click to reveal"
                        })
                        .clicked()
                    {
                        self.reveal_key = !self.reveal_key;
                    }
                    ui.add_space(6.0);
                    if theme::row_button(ui, "Copy", false).clicked() {
                        set_clipboard(k);
                        self.message = Some("License key copied.".into());
                    }
                });
            }
        });

        self.show_message(ui);

        let kind_is_sub = self.license.kind == "subscription";
        let billing = if kind_is_sub {
            "Manage subscription & invoices \u{2197}"
        } else {
            "Invoices & purchase details \u{2197}"
        };
        let deact = if self.busy {
            "Deactivating\u{2026}"
        } else {
            "Deactivate this device"
        };
        // Self-serve billing: the Lemon Squeezy portal covers invoices, payment
        // method, cancellation and lists the keys — it signs the customer in with
        // an emailed magic link, so the address above is all they need.
        let mut open_billing = false;
        let mut deactivate = false;
        let flow = Rect::from_min_max(
            egui::pos2(ui.max_rect().left(), ui.max_rect().bottom() - 130.0),
            ui.max_rect().max,
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(flow), |ui| {
            open_billing = self.link_here(ui, billing, 12.0, 1.0);
            ui.add_space(8.0);
            deactivate = self.link_here(ui, deact, 12.0, 0.55);
        });
        if open_billing {
            open_url(BILLING_PORTAL);
        }
        if deactivate && !self.busy {
            self.deactivate();
        }
        if self.cta_margin(ui, self.done_label(), true, 18.0) {
            self.proceed();
        }
    }

    fn show_message(&mut self, ui: &mut egui::Ui) {
        let Some(msg) = self.message.clone() else {
            return;
        };
        let rect = Rect::from_min_max(
            egui::pos2(ui.max_rect().left() + 36.0, ui.max_rect().bottom() - 150.0),
            egui::pos2(ui.max_rect().right() - 36.0, ui.max_rect().bottom() - 100.0),
        );
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.vertical_centered(|ui| theme::label(ui, &msg, 12.0, true, theme::GREEN));
        });
    }

    /// Label of the bottom escape hatch: "Close" in the standalone modal,
    /// "Start 7-day free trial" during onboarding.
    fn trial_label(&self) -> &'static str {
        if matches!(self.mode, Mode::License { .. }) {
            "Close"
        } else {
            "Start 7-day free trial"
        }
    }

    fn done_label(&self) -> &'static str {
        if matches!(self.mode, Mode::License { .. }) {
            "Done"
        } else {
            "Continue"
        }
    }

    /// Leave the license screen: close the modal, or move on in the wizard.
    fn proceed(&mut self) {
        if matches!(self.mode, Mode::License { .. }) {
            self.finished = true;
        } else {
            self.advance();
        }
    }

    fn activate(&mut self) {
        if self.busy {
            return;
        }
        let key = self.key.trim().to_string();
        if key.is_empty() {
            return;
        }
        self.busy = true;
        self.message = None;
        // In-process: the daemon binary IS this binary, so activation is a
        // direct call — no subprocess, no JSON round-trip (the SwiftUI modal has
        // to shell out because it's a separate app).
        match crate::license::activate(&key) {
            crate::license::ActivateOutcome::Activated => {
                self.activated = true;
                self.message = Some("Your license is active \u{2014} thank you!".into());
                self.license = crate::license::snapshot();
            }
            crate::license::ActivateOutcome::Offline => {
                self.message = Some("No connection \u{2014} check your internet and retry.".into())
            }
            crate::license::ActivateOutcome::Rejected(e) => {
                self.message = Some(e);
            }
        }
        self.busy = false;
    }

    fn deactivate(&mut self) {
        self.busy = true;
        self.message = None;
        if matches!(
            crate::license::deactivate(),
            crate::license::DeactivateOutcome::Done
        ) {
            self.license = crate::license::LicenseSnapshot::default();
            self.message = Some("This device has been deactivated.".into());
            self.face = LicenseFace::Choose;
        } else {
            self.message =
                Some("Couldn't reach the server \u{2014} check your connection and retry.".into());
        }
        self.busy = false;
    }
}

// ── Background work ──────────────────────────────────────────────────────────

impl App {
    /// Re-check permissions every 1.5 s while their screen is up, so a grant made
    /// in Settings flips the row without the user coming back to click anything.
    fn poll_permissions(&mut self, ctx: &egui::Context) {
        if self.step != Step::Permissions || self.design_preview {
            return;
        }
        if self.perms_polled.elapsed() >= Duration::from_millis(1500) {
            self.perms = crate::permissions::check_all();
            self.perms_polled = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }

    /// Kick off the downloads for everything the user picked that isn't on disk.
    fn start_download(&mut self) {
        // Design preview: pose a download in flight, the same numbers the
        // SwiftUI preview uses, so the screen can be reviewed at all (a preview
        // downloads nothing, so it would otherwise only ever show "complete").
        if self.design_preview {
            let (_, rx) = mpsc::channel();
            self.download = Some(Download {
                rx,
                fraction: 0.42,
                status: "Downloading Parakeet TDT v3 (int8)\u{2026}".into(),
                file: "encoder-model.onnx.data".into(),
                done_files: 1,
                total_files: 4,
                bytes: 420_000_000,
                total_bytes: 837_700_000,
                done: false,
                error: None,
            });
            return;
        }
        let models = self.pending_downloads();
        let (tx, rx) = mpsc::channel();
        let total_files: usize = models
            .iter()
            .map(|m| crate::model_manager::missing_files(m).len())
            .sum();
        self.download = Some(Download {
            rx,
            fraction: 0.0,
            status: "Preparing\u{2026}".into(),
            file: String::new(),
            done_files: 0,
            total_files,
            bytes: 0,
            total_bytes: 0,
            done: models.is_empty(),
            error: None,
        });
        if models.is_empty() {
            return;
        }
        std::thread::spawn(move || {
            let mut base = 0usize; // files completed by earlier models
            for model in models {
                let n = crate::model_manager::missing_files(&model).len();
                let _ = tx.send(DownloadMsg::Model(model.clone()));
                let tx2 = tx.clone();
                let res = crate::model_manager::download(&model, &mut |p| {
                    let _ = tx2.send(DownloadMsg::Progress {
                        file_index: base + p.file_index,
                        file_name: p.file_name.clone(),
                        bytes: p.downloaded,
                        total_bytes: p.total,
                    });
                });
                if let Err(e) = res {
                    let _ = tx.send(DownloadMsg::Failed(format!("{e}")));
                    return;
                }
                base += n;
            }
            let _ = tx.send(DownloadMsg::Done);
        });
    }

    /// Drain download progress into the UI state (called every frame).
    fn pump_download(&mut self, ctx: &egui::Context) {
        let Some(d) = self.download.as_mut() else {
            return;
        };
        if d.done && d.error.is_none() {
            return;
        }
        let mut changed = false;
        // `done_files` counts across the whole job; each model reports its own
        // file indices, so keep a running base as models complete.
        while let Ok(msg) = d.rx.try_recv() {
            changed = true;
            match msg {
                DownloadMsg::Model(name) => {
                    d.status = format!("Downloading {}\u{2026}", model_label(&name));
                }
                DownloadMsg::Progress {
                    file_index,
                    file_name,
                    bytes,
                    total_bytes,
                } => {
                    if !file_name.is_empty() {
                        d.file = file_name;
                        d.bytes = bytes;
                        d.total_bytes = total_bytes;
                    }
                    d.done_files = file_index;
                    let within = if total_bytes > 0 {
                        bytes as f32 / total_bytes as f32
                    } else {
                        0.0
                    };
                    d.fraction = if d.total_files > 0 {
                        (file_index as f32 + within) / d.total_files as f32
                    } else {
                        within
                    }
                    .clamp(0.0, 1.0);
                }
                DownloadMsg::Failed(e) => {
                    d.error = Some(e);
                    d.done = true;
                }
                DownloadMsg::Done => {
                    d.done = true;
                    d.fraction = 1.0;
                }
            }
        }
        if changed {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

impl App {
    /// Screenshot mode: pose each screen, grab it, move on, quit at the end.
    /// One screen per two frames — request on the settle frame, collect the
    /// reply on the next (egui delivers it as an input event).
    fn drive_screenshots(&mut self, ctx: &egui::Context) {
        let Some(dir) = self.shots.clone() else {
            return;
        };
        // Collect whatever the last request produced.
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot {
            if let Some((name, _, _)) = self.shot_queue.first().cloned() {
                let path = dir.join(format!("{name}.png"));
                let _ = std::fs::create_dir_all(&dir);
                let rgba: Vec<u8> = image
                    .pixels
                    .iter()
                    .flat_map(|p| [p.r(), p.g(), p.b(), p.a()])
                    .collect();
                match image::RgbaImage::from_raw(image.width() as u32, image.height() as u32, rgba)
                {
                    Some(img) => {
                        let _ = img.save(&path);
                        println!("{}", path.display());
                    }
                    None => tracing::warn!("screenshot {name}: bad buffer"),
                }
            }
            if !self.shot_queue.is_empty() {
                self.shot_queue.remove(0);
            }
            self.shot_settle = 0;
        }
        let Some((_, step, face)) = self.shot_queue.first().cloned() else {
            self.finished = true;
            return;
        };
        self.step = step;
        self.face = face.clone();
        // The "licensed" screen needs something to show even on a trial box.
        if face == LicenseFace::Licensed && !self.license.licensed() {
            self.license = crate::license::LicenseSnapshot {
                status: "licensed".into(),
                kind: "lifetime".into(),
                email: Some("you@example.com".into()),
                key: Some("C281261E-1111-2222-3333-44444444C7C4".into()),
                ..Default::default()
            };
        }
        self.shot_settle += 1;
        // Fonts and the logo texture land on frame 1; grab from frame 3.
        if self.shot_settle >= 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

// ── Small helpers ────────────────────────────────────────────────────────────

/// Which models to tick by default: the recommended engine, plus whatever else
/// the box has room for. Mirrors `OnboardingState.init`.
fn default_selection(models: &[ModelInfo], recommended: &str) -> Vec<String> {
    let mut out = vec![recommended.to_string()];
    let free_gb = free_disk_gb();
    let ram_gb = total_ram_gb();
    let add = |name: &str, out: &mut Vec<String>| {
        if !out.iter().any(|m| m == name) && models.iter().any(|m| m.name == name) {
            out.push(name.to_string());
        }
    };
    if free_gb > 10.0 {
        add("parakeet-tdt-0.6b-v3-int8", &mut out);
        add("ggml-large-v3-turbo-q5_0.bin", &mut out);
        if ram_gb > 12.0 {
            add("voxtral-q4.gguf", &mut out);
        }
    } else if free_gb > 5.0 {
        add("ggml-large-v3-turbo-q5_0.bin", &mut out);
        add("parakeet-tdt-0.6b-v3-int8", &mut out);
    }
    out
}

fn total_ram_gb() -> f64 {
    crate::hardware::total_ram_bytes() as f64 / 1e9
}

fn free_disk_gb() -> f64 {
    crate::hardware::free_disk_bytes(&crate::config::data_dir()) as f64 / 1e9
}

/// Voxtral is the only model with a hard RAM floor worth warning about.
fn ram_warning(model: &str) -> Option<&'static str> {
    (model.contains("voxtral") && total_ram_gb() <= 12.0).then_some("Needs 16 GB+ RAM")
}

fn model_label(name: &str) -> String {
    crate::model_manager::find_model(name)
        .map(|m| m.label.to_string())
        .unwrap_or_else(|| name.to_string())
}

fn format_size(mb: u32) -> String {
    if mb >= 1000 {
        format!("{:.1} GB", mb as f32 / 1000.0)
    } else {
        format!("{mb} MB")
    }
}

fn format_bytes(b: u64) -> String {
    if b >= 1_000_000_000 {
        format!("{:.1} GB", b as f64 / 1e9)
    } else if b >= 1_000_000 {
        format!("{:.0} MB", b as f64 / 1e6)
    } else {
        format!("{b} B")
    }
}

/// `C281261E-••••-••••-••••-••••••••C7C4` — enough to recognise the key, not
/// enough to use it.
fn masked(key: &str) -> String {
    if key.len() <= 12 {
        return "\u{2022}".repeat(key.chars().count());
    }
    let head: String = key.chars().take(8).collect();
    let tail: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!(
        "{head}-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}-\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}{tail}"
    )
}

fn plan_label(lic: &crate::license::LicenseSnapshot) -> String {
    match lic.kind.as_str() {
        "lifetime" => "Lifetime \u{2014} paid once, yours forever".into(),
        "subscription" => match lic.renews.as_deref().filter(|d| !d.is_empty()) {
            Some(d) => format!("Monthly \u{2014} renews {d}"),
            None => "Monthly subscription".into(),
        },
        _ => "Whisper Push".into(),
    }
}

/// Clipboard text as a candidate license key. `strict` requires the UUID shape
/// (used for the silent prefill); the paste button accepts any single line.
fn clipboard_key(strict: bool) -> Option<String> {
    let raw = arboard::Clipboard::new().ok()?.get_text().ok()?;
    let s = raw.trim().to_string();
    if s.is_empty() || s.contains('\n') {
        return None;
    }
    if !strict {
        return Some(s);
    }
    let is_uuid = s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        });
    is_uuid.then_some(s)
}

fn set_clipboard(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text.to_string());
    }
}

fn open_url(url: &str) {
    crate::util::open_external(url);
}

/// Grant a permission from the wizard. On macOS the prompt is fired by a short-
/// lived child process (`--permissions-request`) so a 30 s TCC dialog can't
/// freeze this window; elsewhere the call returns immediately.
fn grant(kind: PermKind) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe)
                .args(["--permissions-request", kind.cli_name()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
        // Accessibility / Input Monitoring are manual toggles: open the pane too.
        if kind != PermKind::Microphone {
            crate::permissions::open_settings_for(kind);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        crate::permissions::request_one(kind);
        if crate::permissions::check(kind) != PermState::Granted {
            crate::permissions::open_settings_for(kind);
        }
    }
}

fn perm_glyph(kind: PermKind) -> Glyph {
    match kind {
        PermKind::Microphone => Glyph::Mic,
        PermKind::Accessibility => Glyph::Accessibility,
        PermKind::InputMonitoring => Glyph::Keyboard,
        PermKind::InputGroup => Glyph::Users,
    }
}

/// Platform wording for the one promise that names the machine.
fn local_promise() -> &'static str {
    if cfg!(target_os = "macos") {
        "100% local. Nothing leaves your Mac."
    } else {
        "100% local. Nothing leaves your PC."
    }
}

fn open_settings_label() -> &'static str {
    if cfg!(target_os = "linux") {
        "Retry"
    } else {
        "Open Settings"
    }
}

/// Shown when the platform gates nothing (Linux with input access already, or a
/// Windows box where the mic switch is on).
fn no_permissions_line() -> &'static str {
    "Nothing to grant on this system \u{2014} you're ready to dictate."
}

/// Where the app lives once it starts, in one line, per platform.
fn tray_hint() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Whisper Push runs in the notification area (bottom-right of the taskbar)."
    }
    #[cfg(target_os = "linux")]
    {
        "Whisper Push runs in your system tray."
    }
    #[cfg(target_os = "macos")]
    {
        "Whisper Push runs in your menu bar."
    }
}

/// Run `f` inside `rect`, returning whatever it returns. egui's closures can't
/// hand a value out directly, so this wraps the usual `let mut out` dance.
fn inline_at<R: Default>(ui: &mut egui::Ui, rect: Rect, f: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let mut out = R::default();
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| out = f(ui));
    out
}

/// A rect of `width`, centred horizontally, from the cursor to the bottom.
fn centered_column(ui: &egui::Ui, width: f32) -> Rect {
    let x = ui.max_rect().center().x;
    Rect::from_min_max(
        egui::pos2(x - width / 2.0, ui.cursor().top()),
        egui::pos2(x + width / 2.0, ui.max_rect().bottom()),
    )
}

/// The `→` between the Ready screen's three beats.
fn arrow(ui: &mut egui::Ui) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    icons::draw(ui.painter(), Glyph::ArrowRight, r, theme::green_a(0.45));
}

/// Citron progress bar, `width` wide, centred.
fn progress_bar(ui: &mut egui::Ui, fraction: f32, width: f32) {
    let width = width.min(ui.available_width());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(3),
        theme::blend(theme::GREEN, theme::CREAM, 0.12),
    );
    let filled = Rect::from_min_size(
        rect.min,
        Vec2::new(rect.width() * fraction.clamp(0.0, 1.0), rect.height()),
    );
    ui.painter()
        .rect_filled(filled, CornerRadius::same(3), theme::CITRON);
}

/// One of the two plan cards on the paywall. Returns true when clicked.
fn plan_card(
    ui: &mut egui::Ui,
    rect: Rect,
    badge: &str,
    title: &str,
    price: &str,
    period: &str,
    selected: bool,
) -> bool {
    let resp = ui.interact(rect, ui.id().with(title), Sense::click());
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(14),
        if selected {
            theme::blend(theme::CITRON, theme::CREAM, 0.18)
        } else {
            theme::WHITE
        },
    );
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(14),
        Stroke::new(
            if selected { 2.0 } else { 1.0 },
            if selected {
                theme::GREEN
            } else {
                theme::green_a(0.15)
            },
        ),
        StrokeKind::Inside,
    );

    let p = ui.painter();
    let cx = rect.center().x;
    // Badge pill
    let galley = p.layout_no_wrap(badge.to_uppercase(), theme::font(9.0, true), theme::GREEN);
    let pill = Rect::from_center_size(
        egui::pos2(cx, rect.top() + 16.0 + 3.0),
        galley.size() + Vec2::new(16.0, 6.0),
    );
    p.rect_filled(pill, CornerRadius::same(9), theme::CITRON);
    p.galley(pill.center() - galley.size() / 2.0, galley, theme::GREEN);

    p.text(
        egui::pos2(cx, rect.top() + 48.0),
        egui::Align2::CENTER_CENTER,
        title,
        theme::font(14.0, true),
        theme::GREEN,
    );
    p.text(
        egui::pos2(cx, rect.top() + 74.0),
        egui::Align2::CENTER_CENTER,
        price,
        theme::font(24.0, true),
        theme::GREEN,
    );
    p.text(
        egui::pos2(cx, rect.top() + 98.0),
        egui::Align2::CENTER_CENTER,
        period,
        theme::font(11.0, false),
        theme::green_a(0.6),
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}
