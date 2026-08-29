//! The branded dialog window behind `crate::dialog` on Windows and Linux —
//! "Add Word…", "Edit / Delete", "Are you sure?". Same palette and buttons as
//! the wizard, so a menu action doesn't drop the user into a system message box
//! that looks like it belongs to another app.
//!
//! It prints the answer on stdout and exits; an empty stdout means cancelled.

use super::theme;
use crate::dialog::{Kind, Spec};
use eframe::egui::{self, Rect};

/// Show the dialog and block until it is answered or closed. `shot` renders one
/// frame to that PNG and exits instead — this window only ever runs on Windows
/// and Linux, so a way to look at it from a Mac is the difference between
/// reviewing it and hoping.
pub fn run(spec: Spec, shot: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    // Sized to the content: a message, maybe a field, and one row of buttons.
    let (w, h) = match spec.kind {
        Kind::Text => (420.0, 172.0),
        _ => (420.0, 132.0),
    };
    let icon = theme::app_icon_rgba(64).map(|(iw, ih, rgba)| egui::IconData {
        rgba,
        width: iw,
        height: ih,
    });
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([w, h])
        .with_min_inner_size([w, h])
        .with_resizable(false)
        // A dialog fired from the tray must land in front of whatever the user
        // is doing — that's the whole point of asking them something.
        .with_always_on_top()
        .with_title("Whisper Push");
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    eframe::run_native(
        "Whisper Push",
        eframe::NativeOptions {
            viewport,
            centered: true,
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(DialogApp::new(cc, spec, shot)))),
    )
    .map_err(|e| anyhow::anyhow!("dialog failed: {e}"))
}

struct DialogApp {
    spec: Spec,
    value: String,
    done: bool,
    /// Screenshot mode: where to write, and how many frames to let settle first
    /// (fonts upload on frame 1).
    shot: Option<std::path::PathBuf>,
    frames: u32,
}

impl DialogApp {
    fn new(cc: &eframe::CreationContext<'_>, spec: Spec, shot: Option<std::path::PathBuf>) -> Self {
        theme::apply(&cc.egui_ctx);
        Self {
            value: spec.prefill.clone(),
            spec,
            done: false,
            shot,
            frames: 0,
        }
    }

    /// Answer and close. An empty answer is a cancel.
    fn answer(&mut self, ctx: &egui::Context, value: Option<String>) {
        if let Some(v) = value {
            println!("{v}");
        }
        self.done = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for DialogApp {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        let c = theme::CREAM;
        [
            c.r() as f32 / 255.0,
            c.g() as f32 / 255.0,
            c.b() as f32 / 255.0,
            1.0,
        ]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.done {
            return;
        }
        let ctx = ui.ctx().clone();
        if self.shot.is_some() {
            self.drive_screenshot(&ctx);
        }
        // Escape always cancels, in every kind.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.answer(&ctx, None);
            return;
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(theme::CREAM).inner_margin(20))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                theme::label(ui, &self.spec.message.clone(), 13.0, false, theme::GREEN);
                ui.add_space(14.0);

                let mut submit: Option<Option<String>> = None;
                match self.spec.kind {
                    Kind::Text => {
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.value)
                                .font(theme::font(13.0, false))
                                .desired_width(f32::INFINITY)
                                .margin(egui::Margin::symmetric(8, 6))
                                .background_color(theme::WHITE),
                        );
                        if !ctx.memory(|m| m.focused().is_some()) {
                            resp.request_focus();
                        }
                        // Return submits, like the SwiftUI activate field.
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            submit = Some(Some(self.value.trim().to_string()));
                        }
                    }
                    Kind::Choice | Kind::Confirm => {}
                }

                // Buttons sit on one row at the bottom: cancel on the left as a
                // quiet link, the actions on the right.
                let row = Rect::from_min_max(
                    egui::pos2(ui.max_rect().left(), ui.max_rect().bottom() - 34.0),
                    ui.max_rect().max,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(row), |ui| {
                    ui.horizontal(|ui| {
                        let cancel = self
                            .spec
                            .buttons
                            .first()
                            .cloned()
                            .unwrap_or_else(|| "Cancel".into());
                        if theme::link(ui, &cancel, 13.0, 0.6).clicked() {
                            submit = Some(None);
                        }
                        let actions: Vec<String> = match self.spec.kind {
                            Kind::Text => vec!["Save".into()],
                            _ => self.spec.buttons.iter().skip(1).cloned().collect(),
                        };
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // Rightmost = the default action, so it is the
                            // last one drawn in a right-to-left layout.
                            for (i, label) in actions.iter().rev().enumerate() {
                                let prominent = i == 0;
                                if theme::row_button(ui, label, prominent).clicked() {
                                    submit = Some(Some(match self.spec.kind {
                                        Kind::Text => self.value.trim().to_string(),
                                        _ => label.clone(),
                                    }));
                                }
                                ui.add_space(8.0);
                            }
                        });
                    });
                });

                if let Some(value) = submit {
                    // An empty text answer is a cancel: saving "" would create a
                    // blank dictionary entry nobody asked for.
                    let value = value.filter(|v| !v.is_empty() || self.spec.kind != Kind::Text);
                    self.answer(&ctx, value);
                }
            });
    }
}

impl DialogApp {
    /// Grab one frame and quit — see `run`'s `shot`.
    fn drive_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.shot.clone() else {
            return;
        };
        let image = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let rgba: Vec<u8> = image
                .pixels
                .iter()
                .flat_map(|p| [p.r(), p.g(), p.b(), p.a()])
                .collect();
            if let Some(img) =
                image::RgbaImage::from_raw(image.width() as u32, image.height() as u32, rgba)
            {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = img.save(&path);
                println!("{}", path.display());
            }
            self.done = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        self.frames += 1;
        if self.frames >= 3 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}
