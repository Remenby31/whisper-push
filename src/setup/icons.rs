//! [Lucide](https://lucide.dev) icons (ISC licence) drawn as vector paths, the
//! same choice — and for the same reason — as `LucideIcon.swift`: no asset
//! catalogue, nothing to bundle, nothing to go missing at runtime, and crisp at
//! any size. Geometry is transcribed from the official 24×24 sources, keeping
//! Lucide's 2 pt round-capped stroke scaled with the icon.
//!
//! Curves are emitted as short polylines (`arc`, `quad`): egui paints line
//! strips, not paths, and at 14–24 pt a 12-segment arc is indistinguishable
//! from the real thing.

use eframe::egui::{Color32, Painter, Pos2, Rect, Shape, Stroke, Vec2};
use std::f32::consts::PI;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
    /// `shield-check` — "100 % local, nothing leaves your machine".
    ShieldCheck,
    /// `zap` — GPU-accelerated.
    Zap,
    /// `keyboard` — hold a key.
    Keyboard,
    /// `mic` — microphone permission.
    Mic,
    /// `accessibility` — the macOS Accessibility permission.
    Accessibility,
    /// `arrow-right` — the Ready screen's flow.
    ArrowRight,
    /// `clipboard-paste` — paste the license key.
    ClipboardPaste,
    /// `lock` — secure checkout.
    Lock,
    /// `triangle-alert` — a model that needs more RAM than this box has.
    Alert,
    /// `users` — the Linux `input` group.
    Users,
}

/// Draw `glyph` centred in `rect`, stroked in `color`.
pub fn draw(painter: &Painter, glyph: Glyph, rect: Rect, color: Color32) {
    let size = rect.width().min(rect.height());
    let s = size / 24.0;
    let o = rect.center() - Vec2::splat(size / 2.0);
    let stroke = Stroke::new((2.0 * s).max(1.0), color);
    let p = |x: f32, y: f32| Pos2::new(o.x + x * s, o.y + y * s);

    let line = |pts: Vec<(f32, f32)>| {
        painter.add(Shape::line(
            pts.into_iter().map(|(x, y)| p(x, y)).collect(),
            stroke,
        ));
    };

    match glyph {
        // <path d="M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z"/>
        // <path d="m9 12 2 2 4-4"/>
        Glyph::ShieldCheck => {
            let mut pts = vec![(4.0, 13.0), (4.0, 6.0)];
            pts.extend(arc(5.0, 6.0, 1.0, PI, PI * 1.5));
            pts.push((12.0, 2.4));
            pts.push((19.0, 5.0));
            pts.extend(arc(19.0, 6.0, 1.0, -PI * 0.5, 0.0));
            pts.push((20.0, 13.0));
            pts.push((16.0, 20.0));
            pts.push((12.0, 21.9));
            pts.push((8.0, 20.0));
            pts.push((4.0, 13.0));
            line(pts);
            line(vec![(9.0, 12.0), (11.0, 14.0), (15.0, 10.0)]);
        }
        // <path d="M4 14a1 1 0 0 1-.78-1.63l9.9-10.2a.5.5 0 0 1 .86.46l-1.92 6.02A1 1 0 0 0 13 10h7a1 1 0 0 1 .78 1.63l-9.9 10.2a.5.5 0 0 1-.86-.46l1.92-6.02A1 1 0 0 0 11 14z"/>
        Glyph::Zap => line(vec![
            (13.0, 2.0),
            (4.0, 14.0),
            (11.0, 14.0),
            (11.0, 22.0),
            (20.0, 10.0),
            (13.0, 10.0),
            (13.0, 2.0),
        ]),
        // <rect width="20" height="16" x="2" y="4" rx="2"/> + key dots.
        // The dots are Lucide zero-length round-capped strokes; a zero-length
        // polyline draws nothing here, so they are small filled squares of the
        // same visual weight.
        Glyph::Keyboard => {
            rounded_rect(painter, p(2.0, 4.0), p(22.0, 20.0), 2.0 * s, stroke);
            let dot = 1.1 * s;
            for (x, y) in [
                (6.0, 9.0),
                (10.0, 9.0),
                (14.0, 9.0),
                (18.0, 9.0),
                (6.0, 13.0),
                (18.0, 13.0),
            ] {
                painter.circle_filled(p(x, y), dot, color);
            }
            line(vec![(9.5, 16.0), (14.5, 16.0)]);
        }
        // <path d="M12 19v3"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/>
        // <rect x="9" y="2" width="6" height="13" rx="3"/>
        Glyph::Mic => {
            rounded_rect(painter, p(9.0, 2.0), p(15.0, 15.0), 3.0 * s, stroke);
            // The cup is the BOTTOM half of a r=7 circle: 0→π runs
            // (19,12) → (12,19) → (5,12).
            let mut cup = vec![(19.0, 10.0)];
            cup.extend(arc(12.0, 12.0, 7.0, 0.0, PI));
            cup.push((5.0, 10.0));
            line(cup);
            line(vec![(12.0, 19.0), (12.0, 22.0)]);
        }
        // The ISO access symbol (head, arms, body, legs) rather than Lucide's
        // `accessibility`: that one is two partial arcs of a running figure,
        // which turns to mush below ~20 pt — and this row is 24 pt. Same
        // meaning, legible at the size it's actually drawn.
        Glyph::Accessibility => {
            painter.circle_stroke(p(12.0, 4.5), 2.2 * s, stroke);
            line(vec![(4.5, 9.5), (19.5, 9.5)]);
            line(vec![(12.0, 8.0), (12.0, 14.0)]);
            line(vec![(12.0, 14.0), (8.0, 20.5)]);
            line(vec![(12.0, 14.0), (16.0, 20.5)]);
        }
        // <path d="M5 12h14"/><path d="m12 5 7 7-7 7"/>
        Glyph::ArrowRight => {
            line(vec![(5.0, 12.0), (19.0, 12.0)]);
            line(vec![(12.0, 5.0), (19.0, 12.0), (12.0, 19.0)]);
        }
        // clipboard-paste (see LucideIcon.swift for the same transcription)
        Glyph::ClipboardPaste => {
            line(vec![(11.0, 14.0), (21.0, 14.0)]);
            let mut top = vec![(16.0, 4.0), (18.0, 4.0)];
            top.extend(arc(18.0, 6.0, 2.0, -PI * 0.5, 0.0));
            top.push((20.0, 7.34));
            line(top);
            line(vec![(17.0, 18.0), (21.0, 14.0), (17.0, 10.0)]);
            let mut body = vec![(8.0, 4.0), (6.0, 4.0)];
            body.extend(arc(6.0, 6.0, 2.0, PI, PI * 1.5).into_iter().rev());
            body.push((4.0, 20.0));
            body.extend(arc(6.0, 20.0, 2.0, PI * 0.5, PI).into_iter().rev());
            body.push((18.0, 22.0));
            line(body);
            rounded_rect(painter, p(8.0, 2.0), p(16.0, 6.0), 1.0 * s, stroke);
        }
        // <rect width="18" height="11" x="3" y="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>
        Glyph::Lock => {
            rounded_rect(painter, p(3.0, 11.0), p(21.0, 22.0), 2.0 * s, stroke);
            let mut shackle = vec![(7.0, 11.0)];
            shackle.extend(arc(12.0, 7.0, 5.0, PI, 2.0 * PI));
            shackle.push((17.0, 11.0));
            line(shackle);
        }
        // <path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/>
        // <path d="M12 9v4"/><path d="M12 17h.01"/>
        Glyph::Alert => {
            line(vec![(12.0, 3.0), (21.7, 20.0), (2.3, 20.0), (12.0, 3.0)]);
            line(vec![(12.0, 9.0), (12.0, 13.5)]);
            line(vec![(12.0, 17.0), (12.0, 17.01)]);
        }
        // <path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/>
        // <path d="M22 21v-2a4 4 0 0 0-3-3.87"/><path d="M16 3.13a4 4 0 0 1 0 7.75"/>
        Glyph::Users => {
            painter.circle_stroke(p(9.0, 7.0), 4.0 * s, stroke);
            let mut body = vec![(2.0, 21.0), (2.0, 19.0)];
            body.extend(arc(9.0, 19.0, 7.0, PI, 2.0 * PI).into_iter().rev());
            body.push((16.0, 21.0));
            line(body);
            line(vec![(19.0, 15.5), (21.5, 18.0), (21.5, 21.0)]);
            line(vec![(16.0, 3.6), (18.5, 7.0), (16.0, 10.4)]);
        }
    }
}

/// Points along a circular arc in the 24×24 icon space, `from`→`to` radians
/// (0 = east, growing clockwise in screen coordinates).
fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 8;
    (0..=STEPS)
        .map(|i| {
            let t = from + (to - from) * i as f32 / STEPS as f32;
            (cx + r * t.cos(), cy + r * t.sin())
        })
        .collect()
}

/// A rounded rectangle outline between two already-scaled corners.
fn rounded_rect(painter: &Painter, min: Pos2, max: Pos2, radius: f32, stroke: Stroke) {
    painter.rect_stroke(
        Rect::from_min_max(min, max),
        eframe::egui::CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8),
        stroke,
        eframe::egui::StrokeKind::Middle,
    );
}
