//! The wizard's look — a line-for-line port of `macos/Onboarding/Sources/Theme.swift`.
//!
//! Same four brand colours, same corner radii, same paddings, same type scale,
//! so the Windows/Linux wizard is the macOS wizard, not a lookalike. When one
//! side changes, change the other: the numbers here are the numbers there.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Response, Sense, Stroke, StrokeKind,
    Ui, Vec2,
};

// ── PADDOCK brand palette (brandkit/README.md) — the entire allowed set ───────
/// #0D2E25 Racing Green — every piece of text and every outline.
pub const GREEN: Color32 = Color32::from_rgb(0x0D, 0x2E, 0x25);
/// #CEDC00 Signal Citron — the sole accent (CTAs, checkmarks, progress).
pub const CITRON: Color32 = Color32::from_rgb(0xCE, 0xDC, 0x00);
/// #EFEAD8 Chamois Cream — the wizard's ground.
pub const CREAM: Color32 = Color32::from_rgb(0xEF, 0xEA, 0xD8);
/// White, used for unselected plan cards (matches SwiftUI's `Color.white`).
pub const WHITE: Color32 = Color32::from_rgb(0xFF, 0xFF, 0xFF);

/// Racing green at `a` (0..=1) opacity over the cream ground. SwiftUI composites
/// `Color.brandGreen.opacity(x)` the same way; egui needs it pre-multiplied
/// because we paint text with an opaque colour.
pub fn green_a(a: f32) -> Color32 {
    blend(GREEN, CREAM, a)
}

/// Linear blend of `fg` over `bg` at `a` (0..=1).
pub fn blend(fg: Color32, bg: Color32, a: f32) -> Color32 {
    let a = a.clamp(0.0, 1.0);
    let m = |f: u8, b: u8| (f as f32 * a + b as f32 * (1.0 - a)).round() as u8;
    Color32::from_rgb(m(fg.r(), bg.r()), m(fg.g(), bg.g()), m(fg.b(), bg.b()))
}

// ── Type scale ───────────────────────────────────────────────────────────────
// SwiftUI's `.system(size:weight:)`. egui has one weight per family, so the
// bold faces are registered as separate families (see `install_fonts`).

/// Font family name for the semibold/bold face.
pub const BOLD: &str = "wp-bold";
/// Font family name for the regular face.
pub const REGULAR: &str = "wp-regular";
/// Font family name for the monospace face (license keys, sizes, percentages).
pub const MONO: &str = "wp-mono";

pub fn font(size: f32, bold: bool) -> FontId {
    FontId::new(
        size,
        egui::FontFamily::Name(if bold { BOLD.into() } else { REGULAR.into() }),
    )
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name(MONO.into()))
}

/// Load the platform's UI font so the wizard reads as a native app on each OS
/// (Segoe UI on Windows, the fontconfig `sans-serif` on Linux, SF on macOS)
/// rather than egui's bundled Ubuntu. Falls back to egui's own faces for any
/// slot we can't resolve — a missing font must never leave the wizard blank.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    let mut register = |name: &str, candidates: &[String], fallback: &egui::FontFamily| {
        let loaded = candidates
            .iter()
            .find_map(|p| std::fs::read(p).ok())
            .map(|bytes| std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        match loaded {
            Some(data) => {
                fonts.font_data.insert(name.to_owned(), data);
                fonts
                    .families
                    .insert(egui::FontFamily::Name(name.into()), vec![name.to_owned()]);
            }
            // Reuse egui's own family under our name — a box with no readable
            // system font still gets a legible wizard.
            None => {
                let names = fonts.families.get(fallback).cloned().unwrap_or_default();
                fonts
                    .families
                    .insert(egui::FontFamily::Name(name.into()), names);
            }
        }
    };

    register(
        REGULAR,
        &system_font(false),
        &egui::FontFamily::Proportional,
    );
    register(BOLD, &system_font(true), &egui::FontFamily::Proportional);
    register(MONO, &system_mono(), &egui::FontFamily::Monospace);

    ctx.set_fonts(fonts);
}

/// Candidate paths for the platform UI font, best first.
fn system_font(bold: bool) -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        let dir =
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
        let files: &[&str] = if bold {
            &["seguisb.ttf", "segoeuib.ttf", "segoeui.ttf", "arialbd.ttf"]
        } else {
            &["segoeui.ttf", "arial.ttf"]
        };
        files.iter().map(|f| format!("{dir}{f}")).collect()
    }
    #[cfg(target_os = "macos")]
    {
        let _ = bold;
        // SF Pro ships as one variable file; egui renders its default instance,
        // so bold falls through to the heavier Display face when present.
        if bold {
            vec![
                "/System/Library/Fonts/SFNSDisplay.ttf".into(),
                "/System/Library/Fonts/SFNS.ttf".into(),
                "/System/Library/Fonts/Helvetica.ttc".into(),
            ]
        } else {
            vec![
                "/System/Library/Fonts/SFNS.ttf".into(),
                "/System/Library/Fonts/Helvetica.ttc".into(),
            ]
        }
    }
    #[cfg(target_os = "linux")]
    {
        // Ask fontconfig for the desktop's own sans face; hard-coded paths are a
        // fallback for a box without `fc-match`.
        let pattern = if bold {
            "sans-serif:bold"
        } else {
            "sans-serif"
        };
        let mut out: Vec<String> = fc_match(pattern).into_iter().collect();
        out.extend(
            if bold {
                [
                    "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
                ]
            } else {
                [
                    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
                    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
                ]
            }
            .iter()
            .map(|s| s.to_string()),
        );
        out
    }
}

fn system_mono() -> Vec<String> {
    #[cfg(target_os = "windows")]
    {
        let dir =
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
        ["consola.ttf", "cour.ttf"]
            .iter()
            .map(|f| format!("{dir}{f}"))
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        vec![
            "/System/Library/Fonts/SFNSMono.ttf".into(),
            "/System/Library/Fonts/Menlo.ttc".into(),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        let mut out: Vec<String> = fc_match("monospace").into_iter().collect();
        out.push("/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf".into());
        out
    }
}

#[cfg(target_os = "linux")]
fn fc_match(pattern: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", pattern])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!path.is_empty() && std::path::Path::new(&path).is_file()).then_some(path)
}

// ── Widgets ──────────────────────────────────────────────────────────────────

/// Citron-filled primary CTA (`BrandPrimaryButtonStyle`): 15 pt semibold racing
/// green on a 12 pt-radius citron squircle, full width, 12 pt vertical padding.
/// Disabled dims the fill to 35 % and the label to 50 %.
pub fn primary_button(ui: &mut Ui, label: &str, enabled: bool) -> Response {
    let width = ui.available_width();
    let (rect, mut resp) = ui.allocate_exact_size(
        Vec2::new(width, 41.0),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let pressed = enabled && resp.is_pointer_button_down_on();
    let rect = if pressed {
        shrink_scaled(rect, 0.97)
    } else {
        rect
    };
    let fill = if enabled {
        CITRON
    } else {
        blend(CITRON, CREAM, 0.35)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(12), fill);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        font(15.0, true),
        if enabled {
            GREEN
        } else {
            blend(GREEN, fill, 0.5)
        },
    );
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if !enabled {
        resp.flags -= egui::response::Flags::CLICKED;
    }
    resp
}

/// Compact per-row button (`BrandRowButtonStyle`): 12 pt semibold, 8 pt radius,
/// citron when prominent, cream with a hairline outline otherwise.
pub fn row_button(ui: &mut Ui, label: &str, prominent: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font(12.0, true),
        if prominent { GREEN } else { green_a(0.85) },
    );
    let size = Vec2::new(galley.size().x + 24.0, galley.size().y + 12.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let rect = if resp.is_pointer_button_down_on() {
        shrink_scaled(rect, 0.96)
    } else {
        rect
    };
    let fill = if prominent { CITRON } else { CREAM };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    if !prominent {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.0, green_a(0.15)),
            StrokeKind::Inside,
        );
    }
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, GREEN);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Quiet "Granted ✓" pill (`BrandRowBadge`) — a status, not a control.
pub fn badge(ui: &mut Ui, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(12.0, true), GREEN);
    let size = Vec2::new(
        galley.size().x + 10.0 + 14.0 + 4.0 + 10.0,
        galley.size().y + 10.0,
    );
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(8), blend(CITRON, CREAM, 0.55));
    let check = Rect::from_center_size(
        Pos2::new(rect.left() + 17.0, rect.center().y),
        Vec2::splat(10.0),
    );
    checkmark(ui.painter(), check, GREEN, 2.0);
    ui.painter().galley(
        Pos2::new(rect.left() + 28.0, rect.center().y - galley.size().y / 2.0),
        galley,
        GREEN,
    );
}

/// Rounded-square checkbox (`BrandCheckbox`): citron fill + green tick when on,
/// hairline green outline when off. Returns the click response.
pub fn checkbox(ui: &mut Ui, on: bool, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if on {
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), CITRON);
        checkmark(
            ui.painter(),
            rect.shrink(size * 0.26),
            GREEN,
            (size * 0.11).max(1.5),
        );
    } else {
        ui.painter().rect_stroke(
            rect,
            CornerRadius::same(8),
            Stroke::new(1.5, green_a(0.35)),
            StrokeKind::Inside,
        );
    }
    resp
}

/// A ✓ drawn to fill `rect` — SF Symbols' `checkmark`, same proportions.
pub fn checkmark(painter: &egui::Painter, rect: Rect, color: Color32, width: f32) {
    let p = |x: f32, y: f32| {
        Pos2::new(
            rect.left() + rect.width() * x,
            rect.top() + rect.height() * y,
        )
    };
    painter.add(egui::Shape::line(
        vec![p(0.08, 0.52), p(0.38, 0.82), p(0.94, 0.18)],
        Stroke::new(width, color),
    ));
}

/// A text link — 12/13 pt semibold racing green, underlined on hover.
pub fn link(ui: &mut Ui, label: &str, size: f32, alpha: f32) -> Response {
    let color = green_a(alpha);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font(size, true), color);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::click());
    ui.painter().galley(rect.left_top(), galley, color);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        ui.painter()
            .hline(rect.x_range(), rect.bottom() - 1.0, Stroke::new(1.0, color));
    }
    resp
}

/// Brand key cap (`KeyCap`) — cream fill, hairline outline, 12 pt semibold.
pub fn key_cap(ui: &mut Ui, text: &str) {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font(12.0, true), GREEN);
    let size = Vec2::new(galley.size().x + 20.0, galley.size().y + 8.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(6), CREAM);
    ui.painter().rect_stroke(
        rect,
        CornerRadius::same(6),
        Stroke::new(1.0, green_a(0.2)),
        StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() / 2.0, galley, GREEN);
}

/// Plain centred text, the wizard's workhorse.
pub fn label(ui: &mut Ui, text: &str, size: f32, bold: bool, color: Color32) {
    let galley = ui.painter().layout(
        text.to_owned(),
        font(size, bold),
        color,
        ui.available_width(),
    );
    let (rect, _) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.left_top(), galley, color);
}

/// Scale a rect about its centre — SwiftUI's `.scaleEffect` on press.
fn shrink_scaled(rect: Rect, factor: f32) -> Rect {
    Rect::from_center_size(rect.center(), rect.size() * factor)
}

/// The brand app icon (the squircle), decoded once at startup and handed to
/// egui as a texture. The brand master lives with the SwiftUI wizard because
/// SwiftPM can only bundle resources from inside its own target directory —
/// one file, two readers.
pub const APP_ICON_PNG: &[u8] =
    include_bytes!("../../macos/Onboarding/Sources/Resources/AppIcon.png");

/// Decode the app icon to RGBA at `size`x`size`. Used for the in-window logo and
/// for the window/taskbar icon.
pub fn app_icon_rgba(size: u32) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::load_from_memory(APP_ICON_PNG).ok()?;
    let img = img
        .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let (w, h) = img.dimensions();
    Some((w, h, img.into_raw()))
}

/// Upload the app icon as an egui texture (once per context).
pub fn load_logo(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    let (w, h, rgba) = app_icon_rgba(256)?;
    Some(ctx.load_texture(
        "wp-logo",
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba),
        egui::TextureOptions::LINEAR,
    ))
}

/// Draw the logo centred at the cursor, `size` points wide, with the soft
/// racing-green drop shadow the SwiftUI `LogoSquircle` carries.
pub fn draw_logo(ui: &mut Ui, tex: Option<&egui::TextureHandle>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    // Shadow: a few translucent rounded rects under the icon, standing in for
    // SwiftUI's `.shadow(radius: 12, y: 6)`.
    for (i, a) in [(6.0_f32, 0.05_f32), (4.0, 0.06), (2.0, 0.07)] {
        let r = rect.translate(Vec2::new(0.0, i)).shrink(i * 0.5);
        ui.painter().rect_filled(
            r,
            CornerRadius::same((size * 0.22) as u8),
            blend(GREEN, CREAM, a),
        );
    }
    match tex {
        Some(tex) => {
            egui::Image::new(tex)
                .fit_to_exact_size(rect.size())
                .paint_at(ui, rect);
        }
        None => {
            ui.painter()
                .rect_filled(rect, CornerRadius::same((size * 0.22) as u8), GREEN);
        }
    }
}
