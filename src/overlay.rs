//! Floating "listening" pill (Wispr-Flow style).
//!
//! A small, fully-rounded pill pinned to the bottom-centre of the screen, just
//! above the Dock, whose citron bars react to the live mic level while
//! recording — a discreet "you're being heard" cue — and that turns into a
//! small citron spinner + "Loading" while the model is (re)loading, so a cold
//! start never looks like the app ignored the key. macOS only; a no-op
//! elsewhere. Visual tunables live as consts in the macOS impl so the look is
//! easy to iterate.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Latest mic RMS as f32 bits. Written by the audio capture thread (cheap,
/// lock-free), read ~60 fps by the pill's animation tick on the main thread.
static LEVEL: AtomicU32 = AtomicU32::new(0);
/// User toggle (config `overlay_enabled`).
static ENABLED: AtomicBool = AtomicBool::new(true);

/// What the pill is currently showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OverlayState {
    /// Hidden.
    Idle,
    /// Recording — animated citron waveform.
    Recording,
    /// Transcribing — the pill scales out (the stop sound is the cue).
    Processing,
    /// Model loading (startup, engine switch, or a cold page-in mid-dictation)
    /// — spinner + "Loading", the pill widened to fit the label.
    Loading,
}

/// Report the current mic level (0.0–~1.0). Called from the capture callback.
pub fn feed_level(rms: f32) {
    LEVEL.store(rms.to_bits(), Ordering::Relaxed);
}

/// The smoothed level the animation should target right now.
#[allow(dead_code)]
fn level() -> f32 {
    f32::from_bits(LEVEL.load(Ordering::Relaxed))
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Enable/disable the pill (config `overlay_enabled`, applied at startup).
/// Disabling hides it immediately.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !on {
        set_state(OverlayState::Idle);
    }
}

// ─── Platform dispatch ────────────────────────────────────────────────────────

/// Create the (hidden) pill at startup. Must run on the main thread.
pub fn init() {
    #[cfg(target_os = "macos")]
    macos::init();
}

/// Drive the pill from the app state machine (main thread).
pub fn set_state(state: OverlayState) {
    // Respect the user's overlay setting (config `overlay_enabled`): when
    // disabled, the pill must never appear, whatever the app state.
    let state = if is_enabled() {
        state
    } else {
        OverlayState::Idle
    };
    #[cfg(target_os = "macos")]
    macos::set_state(state);
    #[cfg(not(target_os = "macos"))]
    let _ = state;
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{OverlayState, level};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSBackingStoreType, NSBox, NSBoxType, NSColor, NSEvent, NSFont, NSFontWeightMedium,
        NSPanel, NSScreen, NSTextField, NSTitlePosition, NSView, NSWindowCollectionBehavior,
        NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSTimer};
    use std::cell::RefCell;

    // Compact, fully-rounded, discreet — just a "you're being heard" cue.
    const PILL_W: f64 = 72.0;
    const PILL_H: f64 = 24.0;
    const BAR_COUNT: usize = 5;
    const BAR_W: f64 = 4.0;
    const BAR_GAP: f64 = 5.0;
    const DOCK_PAD: f64 = 26.0; // clear float above the Dock
    const DEFAULT_DOCK: f64 = 70.0; // assumed Dock height when it auto-hides
    const PAD_V: f64 = 4.0; // vertical inset inside the pill (smaller = taller bars)
    const BAR_MIN: f64 = 0.16; // idle bar height (fraction of usable height)
    const GAIN: f64 = 28.0; // mic RMS → amplitude
    const AMP_CURVE: f64 = 0.6; // <1 compresses: normal speech already fills the bars
    const APPEAR_EASE: f64 = 0.34; // scale in/out (and widen) speed (per 60 fps frame)
    const FPS: f64 = 60.0;
    // Loading: a ring of citron dots with a fading "comet" tail, then the label.
    const SPIN_DOTS: usize = 8;
    const SPIN_R: f64 = 5.0; // ring radius (dot centres)
    const SPIN_DOT: f64 = 2.6; // dot diameter
    const SPIN_REV: f64 = 0.9; // seconds per revolution
    const SPIN_TAIL: f64 = 0.15; // alpha of the dimmest dot
    const LABEL: &str = "Loading";
    const LABEL_PT: f64 = 11.0;
    const LABEL_GAP: f64 = 6.0; // spinner ↔ label
    const PAD_H: f64 = 10.0; // horizontal inset of the loading layout

    struct Pill {
        panel: Retained<NSPanel>,
        bg: Retained<NSBox>,
        bars: Vec<Retained<NSBox>>,
        dots: Vec<Retained<NSBox>>,
        label: Retained<NSTextField>,
        label_size: NSSize,
        /// Loading layout width (spinner + label); the panel is this wide so the
        /// pill can widen inside it without resizing the window.
        wide: f64,
        smooth: Vec<f64>,
        phase: f64,
        state: OverlayState,
        /// Spinner content (Loading) vs bars — kept through the scale-out so the
        /// pill doesn't swap faces while it disappears.
        loading: bool,
        timer: Option<Retained<NSTimer>>,
        /// Current scale (0 = hidden, 1 = full) and where it's easing toward.
        appear: f64,
        target: f64,
        /// Current pill width, eased toward PILL_W (bars) or `wide` (Loading).
        width: f64,
    }

    thread_local! {
        static PILL: RefCell<Option<Pill>> = const { RefCell::new(None) };
    }

    fn srgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)
    }

    fn make_box(mtm: MainThreadMarker, color: &NSColor, radius: f64) -> Retained<NSBox> {
        let b = NSBox::initWithFrame(mtm.alloc(), NSRect::ZERO);
        b.setBoxType(NSBoxType::Custom);
        b.setTitlePosition(NSTitlePosition::NoTitle);
        b.setBorderWidth(0.0);
        b.setBorderColor(&NSColor::clearColor());
        b.setCornerRadius(radius);
        b.setFillColor(color);
        b.setContentViewMargins(NSSize::new(0.0, 0.0));
        b
    }

    /// Horizontal offset of bar `i`'s centre from the pill centre.
    fn bar_dx(i: usize) -> f64 {
        (i as f64 - (BAR_COUNT as f64 - 1.0) / 2.0) * (BAR_W + BAR_GAP)
    }

    pub fn init() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let pill = build(mtm);
        PILL.with(|c| *c.borrow_mut() = Some(pill));
    }

    fn build(mtm: MainThreadMarker) -> Pill {
        // The label sizes itself to its text; the loading layout (and so the
        // panel) is derived from it rather than from a guessed width.
        let label = NSTextField::labelWithString(&NSString::from_str(LABEL), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize_weight(LABEL_PT, unsafe {
            NSFontWeightMedium
        })));
        label.setTextColor(Some(&srgb(
            0xEF as f64 / 255.0,
            0xEA as f64 / 255.0,
            0xD8 as f64 / 255.0,
            1.0,
        ))); // cream
        label.sizeToFit();
        let label_size = label.frame().size;
        let wide = (2.0 * PAD_H + 2.0 * SPIN_R + SPIN_DOT + LABEL_GAP + label_size.width).ceil();
        let panel_w = wide.max(PILL_W);

        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(panel_w, PILL_H));
        let style = NSWindowStyleMask::NonactivatingPanel | NSWindowStyleMask::Borderless;
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe {
            panel.setReleasedWhenClosed(false);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            // NO window shadow: the pill's contents are reshaped ~60×/s while
            // recording, and a shadowed window recomputes `_setShadowParameters`
            // (AX-contrast + CFPreferences lookups) on every reshape. That pegged
            // the main thread so hard the event loop couldn't drain the
            // stop-recording event → permanent 100% CPU / "Not Responding".
            // The pill already has its own dark rounded background.
            panel.setHasShadow(false);
            panel.setIgnoresMouseEvents(true);
            panel.setLevel(25); // NSStatusWindowLevel — floats above normal windows
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::IgnoresCycle,
            );
            panel.setFloatingPanel(true);
            panel.setBecomesKeyOnlyIfNeeded(true);
        }

        let root = NSView::initWithFrame(mtm.alloc(), rect);
        let dark = srgb(0.07, 0.07, 0.08, 0.78);
        let bg = make_box(mtm, &dark, PILL_H / 2.0);
        root.addSubview(&bg);

        // Every child is placed by `tick` before the first visible frame.
        let citron = srgb(0xCE as f64 / 255.0, 0xDC as f64 / 255.0, 0.0, 1.0);
        let add = |radius: f64| {
            let b = make_box(mtm, &citron, radius);
            root.addSubview(&b);
            b
        };
        let bars: Vec<_> = (0..BAR_COUNT).map(|_| add(BAR_W / 2.0)).collect();
        let dots: Vec<_> = (0..SPIN_DOTS).map(|_| add(SPIN_DOT / 2.0)).collect();
        root.addSubview(&label);
        panel.setContentView(Some(&root));

        let mut p = Pill {
            panel,
            bg,
            bars,
            dots,
            label,
            label_size,
            wide,
            smooth: vec![BAR_MIN; BAR_COUNT],
            phase: 0.0,
            state: OverlayState::Idle,
            loading: false,
            timer: None,
            appear: 0.0,
            target: 0.0,
            width: PILL_W,
        };
        show_content(&mut p, false);
        p
    }

    /// Swap the pill's face: spinner + label (Loading) or the level bars.
    fn show_content(p: &mut Pill, loading: bool) {
        p.loading = loading;
        p.bars.iter().for_each(|b| b.setHidden(loading));
        p.dots.iter().for_each(|d| d.setHidden(!loading));
        p.label.setHidden(!loading);
    }

    pub fn set_state(state: OverlayState) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        PILL.with(|c| {
            let mut g = c.borrow_mut();
            let Some(p) = g.as_mut() else {
                return;
            };
            if p.state == state {
                return;
            }
            p.state = state;
            match state {
                OverlayState::Recording | OverlayState::Loading => {
                    let loading = state == OverlayState::Loading;
                    if p.appear < 0.02 {
                        // Appearing from nothing: start at the final width (it
                        // only eases when morphing a pill that's on screen).
                        p.width = if loading { p.wide } else { PILL_W };
                    }
                    show_content(p, loading);
                    // Scale in (on the start sound, for Recording).
                    reposition(p, mtm);
                    p.target = 1.0;
                    p.panel.orderFrontRegardless();
                    ensure_timer(p);
                    tracing::debug!("overlay: showing pill ({state:?})");
                }
                OverlayState::Processing | OverlayState::Idle => {
                    // Scale out immediately (the stop-sound moment). The running
                    // timer eases it down, then hides + stops itself once shrunk.
                    p.target = 0.0;
                    ensure_timer(p);
                }
            }
        });
    }

    fn ensure_timer(p: &mut Pill) {
        if p.timer.is_none() {
            // `tick` runs through an objc2 block invoked by the main-thread
            // CFRunLoop ~60×/s; a panic there would unwind across the Obj-C frame
            // = UB → process abort. Contain it (a dropped frame is harmless).
            let block = block2::RcBlock::new(|_t: core::ptr::NonNull<NSTimer>| {
                let _ = std::panic::catch_unwind(tick);
            });
            let t = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / FPS, true, &block)
            };
            p.timer = Some(t);
        }
    }

    /// The screen the user is actually working on.
    ///
    /// A menu-bar app has no key window of its own, so `mainScreen` resolves to
    /// the menu-bar screen — which in a multi-monitor / clamshell setup (lid
    /// closed, working on an external display) is not necessarily the one the
    /// user is looking at. The mouse cursor is the reliable proxy: pick the
    /// screen whose frame contains it. `mouseLocation` and `frame` share the
    /// same global coordinate space (origin = bottom-left of the primary
    /// display), so containment is a direct test. Falls back to `mainScreen`
    /// then the first attached screen so the pill is always placed.
    fn active_screen(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
        let mouse = NSEvent::mouseLocation();
        let screens = NSScreen::screens(mtm);
        for screen in screens.iter() {
            let f = screen.frame();
            if mouse.x >= f.origin.x
                && mouse.x < f.origin.x + f.size.width
                && mouse.y >= f.origin.y
                && mouse.y < f.origin.y + f.size.height
            {
                return Some(screen);
            }
        }
        NSScreen::mainScreen(mtm).or_else(|| screens.firstObject())
    }

    fn reposition(p: &Pill, mtm: MainThreadMarker) {
        let Some(screen) = active_screen(mtm) else {
            return;
        };
        let frame = screen.frame();
        let vis = screen.visibleFrame();
        // visibleFrame already excludes the Dock, so its bottom edge is the top
        // of the Dock. If the Dock auto-hides (visibleFrame reaches the screen
        // bottom), reserve a default height so the pill still clears it.
        let dock_top = if vis.origin.y - frame.origin.y > 4.0 {
            vis.origin.y
        } else {
            frame.origin.y + DEFAULT_DOCK
        };
        let x = frame.origin.x + (frame.size.width - p.panel.frame().size.width) / 2.0;
        let y = dock_top + DOCK_PAD;
        p.panel.setFrameOrigin(NSPoint::new(x, y));
    }

    fn tick() {
        PILL.with(|c| {
            let mut g = c.borrow_mut();
            let Some(p) = g.as_mut() else {
                return;
            };

            // Ease the scale toward its target; once fully shrunk, hide + stop.
            p.appear += (p.target - p.appear) * APPEAR_EASE;
            if p.target < 0.5 && p.appear < 0.02 {
                p.appear = 0.0;
                if let Some(t) = p.timer.take() {
                    t.invalidate();
                }
                p.panel.orderOut(None);
                tracing::debug!("overlay: hidden");
                return;
            }
            let s = p.appear; // current scale, 0..1
            let want_w = if p.loading { p.wide } else { PILL_W };
            p.width += (want_w - p.width) * APPEAR_EASE;

            // Whole pill scales from its centre (genie in/out). The panel stays
            // full-size + transparent; we draw the dark bg + content at scale `s`.
            let cx = p.panel.frame().size.width / 2.0;
            let cy = PILL_H / 2.0;
            let (bw, bh) = (p.width * s, PILL_H * s);
            p.bg.setFrame(NSRect::new(
                NSPoint::new(cx - bw / 2.0, cy - bh / 2.0),
                NSSize::new(bw, bh),
            ));
            p.bg.setCornerRadius(bh / 2.0);

            if p.loading {
                tick_spinner(p, cx, cy, s);
            } else {
                tick_bars(p, cx, cy, s);
            }
        });
    }

    /// Level bars: compressed amplitude — a power curve (<1) lifts low/normal
    /// speech so the bars are lively without shouting; quiet ≠ flat, loud
    /// saturates.
    fn tick_bars(p: &mut Pill, cx: f64, cy: f64, s: f64) {
        p.phase += 0.22;
        let amp = (level() as f64 * GAIN).clamp(0.0, 1.0).powf(AMP_CURVE);
        let usable = (PILL_H - 2.0 * PAD_V) * s;
        let center = (BAR_COUNT as f64 - 1.0) / 2.0;
        for i in 0..p.bars.len() {
            let prox = 1.0 - (i as f64 - center).abs() / (center + 1.0); // centre taller
            let wobble = 0.6 + 0.4 * (p.phase + i as f64 * 0.9).sin();
            let target = (BAR_MIN + (1.0 - BAR_MIN) * amp * (0.55 + 0.45 * prox) * wobble)
                .clamp(BAR_MIN, 1.0);
            p.smooth[i] += (target - p.smooth[i]) * 0.35;
            let h = (p.smooth[i] * usable).max(BAR_W * s);
            let w = BAR_W * s;
            // bar centre, scaled around the pill centre
            let bx = cx + bar_dx(i) * s - w / 2.0;
            p.bars[i].setFrame(NSRect::new(
                NSPoint::new(bx, cy - h / 2.0),
                NSSize::new(w, h),
            ));
            p.bars[i].setCornerRadius(w / 2.0);
        }
    }

    /// Spinner + label, laid out left→right and centred in the pill. The ring
    /// stands still; only each dot's opacity moves (a bright head with a fading
    /// tail running clockwise), which reads as rotation at no layout cost.
    fn tick_spinner(p: &mut Pill, cx: f64, cy: f64, s: f64) {
        let n = SPIN_DOTS as f64;
        p.phase = (p.phase + n / (SPIN_REV * FPS)) % n; // head position, in dots
        let content = 2.0 * SPIN_R + SPIN_DOT + LABEL_GAP + p.label_size.width;
        let left = -content / 2.0; // content's left edge, relative to cx
        let (sx, d) = (cx + (left + SPIN_R + SPIN_DOT / 2.0) * s, SPIN_DOT * s);
        for (i, dot) in p.dots.iter().enumerate() {
            // Clockwise from 12 o'clock (AppKit's y axis points up).
            let a = std::f64::consts::FRAC_PI_2 - i as f64 / n * std::f64::consts::TAU;
            let (x, y) = (sx + SPIN_R * s * a.cos(), cy + SPIN_R * s * a.sin());
            dot.setFrame(NSRect::new(
                NSPoint::new(x - d / 2.0, y - d / 2.0),
                NSSize::new(d, d),
            ));
            dot.setCornerRadius(d / 2.0);
            let behind = (p.phase - i as f64).rem_euclid(n) / n; // 0 = head
            dot.setAlphaValue(SPIN_TAIL + (1.0 - SPIN_TAIL) * (1.0 - behind).powi(2));
        }
        // Text can't scale cheaply, so it rides the genie by position + fade.
        let lx = cx + (left + 2.0 * SPIN_R + SPIN_DOT + LABEL_GAP) * s;
        p.label
            .setFrameOrigin(NSPoint::new(lx, cy - p.label_size.height / 2.0));
        p.label.setAlphaValue(s * s);
    }
}
