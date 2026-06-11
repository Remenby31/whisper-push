//! Floating "listening" pill (Wispr-Flow style).
//!
//! A small, fully-rounded pill pinned to the bottom-centre of the screen, just
//! above the Dock, whose citron bars react to the live mic level while
//! recording — a discreet "you're being heard" cue. macOS only; a no-op
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
    /// Transcribing — subtle "thinking" pulse.
    Processing,
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

#[allow(dead_code)]
fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Enable/disable the pill (tray toggle). Disabling hides it immediately.
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
    #[cfg(target_os = "macos")]
    macos::set_state(state);
    #[cfg(not(target_os = "macos"))]
    let _ = state;
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{level, OverlayState};
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{
        NSBackingStoreType, NSBox, NSBoxType, NSColor, NSPanel, NSScreen, NSTitlePosition, NSView,
        NSWindowCollectionBehavior, NSWindowStyleMask,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSTimer};
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
    const APPEAR_EASE: f64 = 0.34; // scale in/out speed (per 60 fps frame)
    const FPS: f64 = 60.0;

    struct Pill {
        panel: Retained<NSPanel>,
        bg: Retained<NSBox>,
        bars: Vec<Retained<NSBox>>,
        smooth: Vec<f64>,
        phase: f64,
        state: OverlayState,
        timer: Option<Retained<NSTimer>>,
        /// Current scale (0 = hidden, 1 = full) and where it's easing toward.
        appear: f64,
        target: f64,
    }

    thread_local! {
        static PILL: RefCell<Option<Pill>> = const { RefCell::new(None) };
    }

    fn srgb(r: f64, g: f64, b: f64, a: f64) -> Retained<NSColor> {
        NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)
    }

    fn make_box(mtm: MainThreadMarker, frame: NSRect, color: &NSColor, radius: f64) -> Retained<NSBox> {
        let b = NSBox::initWithFrame(mtm.alloc(), frame);
        b.setBoxType(NSBoxType::Custom);
        b.setTitlePosition(NSTitlePosition::NoTitle);
        b.setBorderWidth(0.0);
        b.setBorderColor(&NSColor::clearColor());
        b.setCornerRadius(radius);
        b.setFillColor(color);
        b.setContentViewMargins(NSSize::new(0.0, 0.0));
        b
    }

    fn bar_x(i: usize) -> f64 {
        let total = BAR_COUNT as f64 * BAR_W + (BAR_COUNT as f64 - 1.0) * BAR_GAP;
        (PILL_W - total) / 2.0 + i as f64 * (BAR_W + BAR_GAP)
    }

    pub fn init() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let pill = build(mtm);
        PILL.with(|c| *c.borrow_mut() = Some(pill));
    }

    fn build(mtm: MainThreadMarker) -> Pill {
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PILL_W, PILL_H));
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
            panel.setHasShadow(true);
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
        let bg = make_box(mtm, rect, &dark, PILL_H / 2.0);
        root.addSubview(&bg);

        let citron = srgb(0xCE as f64 / 255.0, 0xDC as f64 / 255.0, 0.0, 1.0);
        let usable = PILL_H - 2.0 * PAD_V;
        let mut bars = Vec::with_capacity(BAR_COUNT);
        for i in 0..BAR_COUNT {
            let h = usable * BAR_MIN;
            let frame = NSRect::new(NSPoint::new(bar_x(i), (PILL_H - h) / 2.0), NSSize::new(BAR_W, h));
            let bar = make_box(mtm, frame, &citron, BAR_W / 2.0);
            root.addSubview(&bar);
            bars.push(bar);
        }
        panel.setContentView(Some(&root));

        Pill {
            panel,
            bg,
            bars,
            smooth: vec![BAR_MIN; BAR_COUNT],
            phase: 0.0,
            state: OverlayState::Idle,
            timer: None,
            appear: 0.0,
            target: 0.0,
        }
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
                OverlayState::Recording => {
                    // Scale in on the start sound.
                    reposition(p, mtm);
                    p.target = 1.0;
                    p.panel.orderFrontRegardless();
                    ensure_timer(p);
                    tracing::debug!("overlay: showing pill");
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
            let block = block2::RcBlock::new(|_t: core::ptr::NonNull<NSTimer>| tick());
            let t = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0 / FPS, true, &block)
            };
            p.timer = Some(t);
        }
    }

    fn reposition(p: &Pill, mtm: MainThreadMarker) {
        // A menu-bar app often has no key window, so `mainScreen` can be nil —
        // fall back to the primary screen so the pill is always placed (not left
        // stuck at the bottom-left origin).
        let screen = NSScreen::mainScreen(mtm).or_else(|| NSScreen::screens(mtm).firstObject());
        let Some(screen) = screen else {
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
        let x = frame.origin.x + (frame.size.width - PILL_W) / 2.0;
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

            // Whole pill scales from its centre (genie in/out). The panel stays
            // full-size + transparent; we draw the dark bg + bars at scale `s`.
            let cx = PILL_W / 2.0;
            let cy = PILL_H / 2.0;
            let (bw, bh) = (PILL_W * s, PILL_H * s);
            p.bg.setFrame(NSRect::new(
                NSPoint::new(cx - bw / 2.0, cy - bh / 2.0),
                NSSize::new(bw, bh),
            ));
            p.bg.setCornerRadius(bh / 2.0);

            // Compressed amplitude: a power curve (<1) lifts low/normal speech so
            // the bars are lively without shouting; quiet ≠ flat, loud saturates.
            p.phase += 0.22;
            let amp = (level() as f64 * GAIN).clamp(0.0, 1.0).powf(AMP_CURVE);
            let usable = (PILL_H - 2.0 * PAD_V) * s;
            let center = (BAR_COUNT as f64 - 1.0) / 2.0;
            for i in 0..p.bars.len() {
                let prox = 1.0 - (i as f64 - center).abs() / (center + 1.0); // centre taller
                let wobble = 0.6 + 0.4 * (p.phase + i as f64 * 0.9).sin();
                let target =
                    (BAR_MIN + (1.0 - BAR_MIN) * amp * (0.55 + 0.45 * prox) * wobble).clamp(BAR_MIN, 1.0);
                p.smooth[i] += (target - p.smooth[i]) * 0.35;
                let h = (p.smooth[i] * usable).max(BAR_W * s);
                let w = BAR_W * s;
                // bar centre, scaled around the pill centre
                let bx = cx + (bar_x(i) + BAR_W / 2.0 - cx) * s - w / 2.0;
                p.bars[i].setFrame(NSRect::new(NSPoint::new(bx, cy - h / 2.0), NSSize::new(w, h)));
                p.bars[i].setCornerRadius(w / 2.0);
            }
        });
    }
}
