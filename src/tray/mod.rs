#[cfg(target_os = "windows")]
mod windows_shell;

use crate::config::Config;
use crate::state::{AppState, Event, State};
use crate::util::LockSafe;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};

/// The ONE menu-bar glyph (three brand sound-waves). Every tray state renders
/// this exact geometry — only the colour changes — so the icon never shifts
/// size or shape between states. Idle draws it as a macOS template (auto
/// black/white); the active states recolour it (see `set_tray_icon`).
const ICON_GLYPH: &[u8] = include_bytes!("../../resources/icons/icon-glyph.png");

/// Signal citron — the single brand accent, used only for the "live" state.
const TINT_RECORDING: [u8; 3] = [0xCE, 0xDC, 0x00];
/// Opacity (0–255) of the dimmed busy glyph. ~43% reads clearly as "working,
/// not ready yet" while staying visible on any menu bar.
const BUSY_OPACITY: u8 = 110;

/// How to render the master glyph for a given state.
enum GlyphStyle {
    /// Monochrome macOS template (auto black/white) at the given opacity:
    /// 255 = crisp (idle), lower = dimmed (busy). Visible on any background.
    Template(u8),
    /// Solid brand colour, fully opaque (recording).
    Tint([u8; 3]),
}

/// The process's Windows AppUserModelID — see `windows_shell::APP_ID`.
#[cfg(target_os = "windows")]
pub fn windows_app_id() -> &'static str {
    windows_shell::APP_ID
}

/// Claim our AppUserModelID for this process. Called at the very start of
/// `app::run`, before onboarding: the wizard and guided setup both notify, and a
/// toast sent before the id is registered is attributed to PowerShell.
#[cfg(target_os = "windows")]
pub fn register_windows_app_id() {
    windows_shell::register_app_id();
}

/// Racing green — the badge ground on Windows/Linux (see `badge_icon`).
const BRAND_GREEN: [u8; 3] = [0x0D, 0x2E, 0x25];

/// Build the tray icon for `style`.
///
/// **macOS** gets the bare glyph: the menu bar wants a template image, which the
/// OS recolours to contrast with whatever is behind it.
///
/// **Windows and Linux** get the brand badge — the glyph on a rounded racing-
/// green square, i.e. the app icon. A template image is exactly wrong there:
/// nothing recolours it, so the pure-black glyph this ships is invisible on the
/// (default) dark Windows taskbar and on most Linux panels. That is what "it
/// doesn't appear in the tray" was. A badge carries its own background, so it
/// reads on any panel colour, light or dark, with no theme guessing — and it's
/// what every other Windows app in that area does. Recording inverts it (citron
/// ground, green waves) so "live" is unmistakable at 16 px.
fn glyph_icon(style: GlyphStyle) -> Option<Icon> {
    #[cfg(target_os = "macos")]
    {
        template_icon(style)
    }
    #[cfg(not(target_os = "macos"))]
    {
        badge_icon(style)
    }
}

/// The glyph alone, recoloured — macOS menu-bar form.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn template_icon(style: GlyphStyle) -> Option<Icon> {
    let mut img = image::load_from_memory(ICON_GLYPH).ok()?.into_rgba8();
    match style {
        GlyphStyle::Tint([r, g, b]) => {
            for px in img.pixels_mut() {
                if px[3] > 0 {
                    px[0] = r;
                    px[1] = g;
                    px[2] = b;
                }
            }
        }
        GlyphStyle::Template(opacity) if opacity < 255 => {
            for px in img.pixels_mut() {
                px[3] = (px[3] as u16 * opacity as u16 / 255) as u8;
            }
        }
        GlyphStyle::Template(_) => {} // full opacity — leave the glyph untouched
    }
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// The glyph on a rounded brand square — Windows/Linux notification-area form.
/// Rendered at the platform's icon size so the 3 px strokes survive: handing
/// Shell_NotifyIcon a 128 px image and letting it downscale to 16 px washed the
/// waves out to almost nothing.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn badge_icon(style: GlyphStyle) -> Option<Icon> {
    let img = badge_image(style)?;
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// The badge's pixels — split from `badge_icon` so the invariants that make it
/// visible (opaque ground, rounded corners) can be asserted in a test;
/// `tray_icon::Icon` hands back nothing to inspect.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn badge_image(style: GlyphStyle) -> Option<image::RgbaImage> {
    let size = platform_icon_size();
    let (ground, ink, alpha) = match style {
        // Live: inverted, so the state reads at a glance.
        GlyphStyle::Tint(c) => (c, BRAND_GREEN, 255u8),
        // Busy: the brand badge, dimmed.
        GlyphStyle::Template(o) if o < 255 => (BRAND_GREEN, TINT_RECORDING, o),
        GlyphStyle::Template(_) => (BRAND_GREEN, TINT_RECORDING, 255),
    };

    // Ground: a rounded square with the app icon's corner ratio (22%).
    let mut img = image::RgbaImage::new(size, size);
    let r = (size as f32 * 0.22).round();
    let (w, h) = (size as f32, size as f32);
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        // Distance outside the rounded rect, for a 1 px antialiased edge.
        let dx = (r - fx).max(fx - (w - r)).max(0.0);
        let dy = (r - fy).max(fy - (h - r)).max(0.0);
        let d = (dx * dx + dy * dy).sqrt() - r;
        let cover = (0.5 - d).clamp(0.0, 1.0);
        *px = image::Rgba([
            ground[0],
            ground[1],
            ground[2],
            (cover * alpha as f32) as u8,
        ]);
    }

    // Ink: the one master glyph, scaled to ~66% and composited on top.
    let glyph_px = (size as f32 * 0.66).round().max(8.0) as u32;
    let glyph = image::load_from_memory(ICON_GLYPH)
        .ok()?
        .resize_exact(glyph_px, glyph_px, image::imageops::FilterType::Lanczos3)
        .into_rgba8();
    let off = ((size - glyph_px) / 2) as i64;
    for (gx, gy, gp) in glyph.enumerate_pixels() {
        let a = gp[3] as f32 / 255.0;
        if a <= 0.0 {
            continue;
        }
        let (x, y) = (gx as i64 + off, gy as i64 + off);
        if x < 0 || y < 0 || x >= size as i64 || y >= size as i64 {
            continue;
        }
        let dst = img.get_pixel_mut(x as u32, y as u32);
        for i in 0..3 {
            dst[i] = (ink[i] as f32 * a + dst[i] as f32 * (1.0 - a)) as u8;
        }
        dst[3] = dst[3].max((a * alpha as f32) as u8);
    }

    Some(img)
}

/// Pixel size to render the tray icon at. Windows tells us exactly what the
/// notification area wants (16 at 100 % DPI, 20/24 when scaled); elsewhere 32 is
/// a safe source size for panels that scale it themselves.
fn platform_icon_size() -> u32 {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSMICON};
        let n = unsafe { GetSystemMetrics(SM_CXSMICON) };
        // A tiny icon would look ragged and a huge one is a scaling artefact:
        // clamp to the range the shell actually uses.
        return (n as u32).clamp(16, 64);
    }
    #[cfg(not(target_os = "windows"))]
    {
        32
    }
}

/// Submenu title showing the current device selection, e.g. "Input: Auto".
fn device_title(label: &str, value: &str) -> String {
    if value == "auto" {
        format!("{label}: Auto")
    } else {
        format!("{label}: {value}")
    }
}

// Built-in hotkey presets. Combos ("cmd+shift+space") work on every platform now
// that the Linux (evdev) and Windows (WH_KEYBOARD_LL) listeners parse and match
// them through `hotkey::combo`, so the list no longer forks per OS — only the
// modifier NAMES do, because ⌘/⌥ are Command/Option on a Mac and Win/Alt
// elsewhere, and a preset must read like the user's own keyboard.
#[cfg(target_os = "macos")]
const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold: Control", "ctrl", "hold"),
    ("Hold: Right Control", "rctrl", "hold"),
    ("Hold: Right Command", "rcmd", "hold"),
    ("Hold: Right Option", "ralt", "hold"),
    ("Toggle: \u{2318}\u{21e7}Space", "cmd+shift+space", "toggle"),
    (
        "Toggle: \u{2303}\u{21e7}Space",
        "ctrl+shift+space",
        "toggle",
    ),
];
#[cfg(not(target_os = "macos"))]
const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold: Control", "ctrl", "hold"),
    ("Hold: Right Control", "rctrl", "hold"),
    ("Hold: Right Alt", "ralt", "hold"),
    ("Hold: Right Windows", "rcmd", "hold"),
    ("Toggle: Ctrl+Shift+Space", "ctrl+shift+space", "toggle"),
    ("Toggle: Alt+Space", "alt+space", "toggle"),
];

/// User events forwarded into winit's event loop.
#[derive(Debug)]
#[allow(dead_code)]
enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    App(Event),
    /// Carries nothing: sending it just wakes the loop so `about_to_wait` drains
    /// the crossbeam channels now instead of at the next tick. See `wake_main`.
    Wake,
}

/// Permissions submenu title. Carries the count itself, so the greyed
/// "⚠ N permission(s) missing" line that used to sit above it — unclickable and
/// saying the same thing — is gone.
fn perms_title(status: &crate::permissions::PermissionStatus) -> String {
    if status.all_granted() {
        "Permissions \u{2713}".into()
    } else {
        format!("\u{26a0} Permissions: {} to grant", status.missing_count())
    }
}

/// One permission row: "✓ Microphone │ Granted". The `│` (never an em dash)
/// joins the two facts — see the menu-label rule in CLAUDE.md.
fn perm_label(kind: crate::permissions::PermKind, state: crate::permissions::PermState) -> String {
    format!(
        "{} {} \u{2502} {}",
        state.symbol(),
        kind.title(),
        state.label()
    )
}

/// How long "Set Custom Hotkey…" stays armed before giving up, so the menu
/// never sits in "Press your shortcut now…" forever.
const HOTKEY_CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);

/// Resting label of the custom-hotkey item: the binding when there is one (it
/// then also carries the checkmark), otherwise the call to action.
fn custom_hotkey_label(current: Option<String>) -> String {
    match current {
        Some(disp) => format!("Custom: {disp}"),
        None => "Set Custom Hotkey\u{2026}".to_string(),
    }
}

/// Suffix of the update-item text for "newer release, but no asset for this
/// platform". `Event::UpdateStatus` carries only display text, so the
/// `UpdateStatus` arm recognizes this exact suffix to arm
/// `pending_release_page` (click then opens the releases page).
const OPEN_RELEASES_HINT: &str = "open download page";

/// Menu text for "a newer version exists, but this platform installs it by
/// hand". The trailing hint is load-bearing: `Event::UpdateStatus` matches on it
/// to arm the click that opens the download page instead of an in-app install.
pub fn manual_update_label(version: &str) -> String {
    format!("\u{2b06} v{version} available \u{2502} {OPEN_RELEASES_HINT}")
}

/// The application struct that implements winit's ApplicationHandler.
struct App {
    state: AppState,
    config: Arc<Mutex<Config>>,
    rx: Receiver<Event>,
    tray: Option<TrayIcon>,
    pipeline_tx: Option<crossbeam_channel::Sender<Event>>,
    // Menu items (created in init, kept alive)
    menu_items: Option<MenuItems>,
    // Pending update info (version, download_url)
    pending_update: Option<(String, String)>,
    // Releases page to open on update-item click when a newer release has no
    // asset for this platform. Mutually exclusive with `pending_update`.
    pending_release_page: Option<String>,
    // "Set Custom Hotkey…" is armed and waiting for a combo. The generation
    // counter lets a timeout tell its own capture from a later one.
    capturing: bool,
    capture_gen: u64,
}

struct MenuItems {
    /// The root menu, kept so the head (CTA / status line) can be inserted and
    /// removed as state changes — a greyed-out line nobody can click is noise,
    /// so we show nothing rather than something dead.
    menu: Menu,
    /// Non-clickable state line. Only in the menu while it says something the
    /// clickable items don't already: loading, recording, transcribing.
    status_item: MenuItem,
    /// Separator under the head; in the menu only when the head has content.
    head_separator: PredefinedMenuItem,
    #[allow(dead_code)]
    notifications_item: CheckMenuItem,
    #[allow(dead_code)]
    sound_item: CheckMenuItem,
    #[allow(dead_code)]
    debug_item: CheckMenuItem,
    quit_id: String,
    notif_id: String,
    sound_id: String,
    debug_id: String,
    uninstall_id: String,
    update_item: MenuItem,
    update_id: String,
    #[allow(dead_code)]
    report_item: MenuItem,
    report_id: String,
    hotkey_ids: Vec<(String, String, String)>,
    hotkey_items: Vec<(CheckMenuItem, String, String)>,
    hotkey_submenu: Submenu,
    /// One item doing three jobs, so a custom binding is visible in the same
    /// list as the presets: "Set Custom Hotkey…" when idle, "Press your shortcut
    /// now…" while armed, and a *checked* "Custom: ⌘⇧D" once one is bound.
    custom_hotkey_item: Option<CheckMenuItem>,
    custom_hotkey_id: String,
    input_ids: Vec<(String, String)>,
    input_device_items: Vec<(CheckMenuItem, String)>,
    input_submenu: Submenu,
    output_ids: Vec<(String, String)>,
    output_device_items: Vec<(CheckMenuItem, String)>,
    output_submenu: Submenu,
    /// One row per permission THIS platform gates (see `permissions::tracked`),
    /// paired with the kind it opens Settings for. macOS has three, Windows one,
    /// Linux one — the submenu is built from the list, never hard-coded.
    perm_items: Vec<(MenuItem, crate::permissions::PermKind)>,
    perms_submenu: Submenu,

    setup_id: String,
    model_items: Vec<(MenuItem, String)>, // (item, model name = config.model value)
    // Dictionary (adaptive correction)
    dict_submenu: Submenu,
    #[allow(dead_code)]
    dict_enabled_item: CheckMenuItem,
    dict_correct_last_id: String,
    dict_add_id: String,
    dict_open_id: String,
    dict_reload_id: String,
    dict_enabled_id: String,
    dict_forget_voice_id: String,
    /// One (item, term) per listed word; rebuilt on every dictionary change.
    /// A placeholder/"more" line has an empty term.
    dict_entry_items: Vec<(MenuItem, String)>,
    // Templates (voice snippets — say the trigger, paste the content)
    templates_submenu: Submenu,
    template_add_id: String,
    template_open_id: String,
    template_reload_id: String,
    /// One disabled label per template trigger; rebuilt on change.
    template_items: Vec<(MenuItem, String)>,
    // History (recent dictations — click an entry to copy it)
    history_submenu: Submenu,
    history_open_id: String,
    history_clear_id: String,
    /// One (item, full text) per recent run; rebuilt on change.
    history_entry_items: Vec<(MenuItem, String)>,
    // License (Lemon Squeezy)
    license_submenu: Submenu,
    license_status_item: MenuItem,
    /// "Subscribe…" while unlicensed / "Manage License…" once a key is active.
    license_subscription_item: MenuItem,
    license_subscription_id: String,
    /// "Enter License Key…" — straight to the key screen; only present while
    /// unlicensed (once you have a key there is nothing to enter).
    license_activate_item: MenuItem,
    license_activate_id: String,
    /// "Deactivate this device…" — only present while licensed.
    license_deactivate_item: MenuItem,
    license_deactivate_id: String,
    /// Buy-forward CTA, in the menu only while unlicensed. It carries its own
    /// urgency ("Unlock Whisper Push | 3 days left"), so there is no second,
    /// greyed-out line saying the same thing; once licensed it goes away
    /// entirely and the License submenu is the one place to manage it.
    unlock_item: MenuItem,
    unlock_id: String,
}

impl App {
    fn new(state: AppState, rx: Receiver<Event>) -> Self {
        let config = Arc::new(Mutex::new(state.config.clone()));
        Self {
            state,
            config,
            rx,
            tray: None,
            pipeline_tx: None,
            menu_items: None,
            pending_update: None,
            pending_release_page: None,
            capturing: false,
            capture_gen: 0,
        }
    }

    /// Disarm capture and put the menu item back to its resting label (the
    /// current custom binding, if any). The one exit from capture mode.
    fn end_hotkey_capture(&mut self) {
        crate::hotkey::cancel_capture();
        self.capturing = false;
        self.capture_gen += 1;
        let cfg = self.config.lock_safe().clone();
        let is_custom = !HOTKEY_PRESETS
            .iter()
            .any(|(_, hk, m)| *hk == cfg.hotkey && *m == cfg.hotkey_mode);
        if let Some(it) = self
            .menu_items
            .as_ref()
            .and_then(|mi| mi.custom_hotkey_item.as_ref())
        {
            it.set_text(custom_hotkey_label(
                is_custom.then(|| format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode)),
            ));
            it.set_checked(is_custom);
        }
    }

    fn create_tray(&mut self) {
        // Before anything is drawn: a device pinned in the config that no longer
        // exists must not stay pinned (see `reconcile_device_pins`).
        self.reconcile_device_pins();
        let cfg = self.config.lock_safe().clone();

        // Build menu
        let is_ready = self.state.current() == State::Idle;
        let disp = format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode);
        let status_text = if is_ready {
            format!("Whisper Push ({disp})")
        } else {
            "Whisper Push: \u{231b} Loading model\u{2026}".into()
        };
        let status_item = MenuItem::new(&status_text, false, None);

        // Hotkey submenu (titled with the current binding)
        let hotkey_submenu = Submenu::new(
            &format!(
                "Hotkey: {}",
                format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode)
            ),
            true,
        );
        let mut hotkey_items = Vec::new();
        for (label, hotkey, mode) in HOTKEY_PRESETS {
            let checked = *hotkey == cfg.hotkey && *mode == cfg.hotkey_mode;
            let item = CheckMenuItem::new(*label, true, checked, None);
            let _ = hotkey_submenu.append(&item);
            hotkey_items.push((item, hotkey.to_string(), mode.to_string()));
        }
        // "Set Custom Hotkey…" — live key-combo capture, on every platform
        // (`hotkey::combo::Capture`, wired into all three listeners).
        let (custom_hotkey_item, custom_hotkey_id) = {
            let _ = hotkey_submenu.append(&PredefinedMenuItem::separator());
            // A binding that matches no preset is a custom one: show it here,
            // checked, so the menu always says what is actually bound.
            let is_custom = !HOTKEY_PRESETS
                .iter()
                .any(|(_, hk, m)| *hk == cfg.hotkey && *m == cfg.hotkey_mode);
            let item = CheckMenuItem::new(
                custom_hotkey_label(
                    is_custom.then(|| format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode)),
                ),
                true,
                is_custom,
                None,
            );
            let _ = hotkey_submenu.append(&item);
            let id = item.id().0.clone();
            (Some(item), id)
        };

        // Permissions (computed once here; reused for the Permissions section).
        let perms = crate::permissions::check_all();

        // Apply the configured output device to the playback module up front.
        crate::audio::playback::set_output_device(&cfg.output_device);

        // Device pickers are real submenus (the old Tahoe hover-close bug was a
        // muda 0.16 issue, fixed by the 0.19 upgrade). Device *enumeration* needs
        // no microphone permission on macOS — TCC only gates capture — so both
        // pickers are always populated; mic usability is shown in Permissions.
        let input_submenu = Submenu::new(&device_title("Input", &cfg.input_device), true);
        let mut input_device_items: Vec<(CheckMenuItem, String)> = Vec::new();
        let input_auto = CheckMenuItem::new("Auto", true, cfg.input_device == "auto", None);
        let _ = input_submenu.append(&input_auto);
        input_device_items.push((input_auto, "auto".to_string()));
        if let Ok(devices) = crate::audio::list_devices() {
            for name in devices {
                let checked = cfg.input_device == name;
                let item = CheckMenuItem::new(&name, true, checked, None);
                let _ = input_submenu.append(&item);
                input_device_items.push((item, name));
            }
        }
        // If the mic is explicitly denied, recording won't work — hint the user.
        if perms.microphone() == crate::permissions::PermState::Denied {
            let _ = input_submenu.append(&PredefinedMenuItem::separator());
            let _ = input_submenu.append(&MenuItem::new(
                "\u{26a0} Microphone denied: grant to record",
                false,
                None,
            ));
        }

        // Output device picker (no permission needed).
        let output_submenu = Submenu::new(&device_title("Output", &cfg.output_device), true);
        let mut output_device_items: Vec<(CheckMenuItem, String)> = Vec::new();
        let output_auto = CheckMenuItem::new("Auto", true, cfg.output_device == "auto", None);
        let _ = output_submenu.append(&output_auto);
        output_device_items.push((output_auto, "auto".to_string()));
        if let Ok(devices) = crate::audio::list_output_devices() {
            for name in devices {
                let checked = cfg.output_device == name;
                let item = CheckMenuItem::new(&name, true, checked, None);
                let _ = output_submenu.append(&item);
                output_device_items.push((item, name));
            }
        }

        // Model selector
        let models = crate::model_manager::list_models();
        // Engine submenu — one entry per model, mirroring the onboarding picker
        // (model_manager::list_models is the shared source of truth). ● marks the
        // active model; ⤓ marks one not yet downloaded — clicking it downloads it
        // on the pipeline thread (LoadModel), then loads it.
        let backend_submenu = Submenu::new("Engine", true);
        let mut model_items: Vec<(MenuItem, String)> = Vec::new();
        for m in &models {
            let active = if m.name == cfg.model {
                "\u{25CF} "
            } else {
                "    "
            };
            let dl = if m.is_downloaded { "" } else { " \u{2913}" };
            let item = MenuItem::new(format!("{active}{}{dl}", m.label), true, None);
            let _ = backend_submenu.append(&item);
            model_items.push((item, m.name.to_string()));
        }

        // Toggles
        let notifications_item = CheckMenuItem::new("Notifications", true, cfg.notifications, None);
        let sound_item = CheckMenuItem::new("Sound Feedback", true, cfg.sound_feedback, None);
        let debug_item = CheckMenuItem::new("Debug Logging", true, cfg.debug, None);
        let update_item = MenuItem::new("Check for Updates\u{2026}", true, None);
        let report_item = MenuItem::new("Report a Problem\u{2026}", true, None);
        let uninstall_item = MenuItem::new("Uninstall...", true, None);
        let quit_item = MenuItem::new("Quit Whisper Push", true, None);

        // Permissions (perms already computed above for the input picker gate)
        let perms_submenu = Submenu::new(perms_title(&perms), true);
        let perm_items: Vec<(MenuItem, crate::permissions::PermKind)> = perms
            .items
            .iter()
            .map(|p| {
                let item = MenuItem::new(
                    perm_label(p.kind, p.state),
                    p.state != crate::permissions::PermState::Granted,
                    None,
                );
                let _ = perms_submenu.append(&item);
                (item, p.kind)
            })
            .collect();
        let _ = perms_submenu.append(&PredefinedMenuItem::separator());
        let setup_item = MenuItem::new("\u{2699} Run Guided Setup\u{2026}", true, None);
        let _ = perms_submenu.append(&setup_item);

        // Dictionary submenu — see & edit your words live (hot-reloaded).
        let dict_count = crate::dictionary::entry_count();
        let dict_submenu = Submenu::new(&format!("Dictionary ({dict_count})"), true);
        let dict_correct_last_item = MenuItem::new("Correct Last Dictation\u{2026}", true, None);
        let dict_add_item = MenuItem::new("Add Word\u{2026}", true, None);
        let dict_open_item = MenuItem::new("Open dictionary.toml\u{2026}", true, None);
        let dict_reload_item = MenuItem::new("Reload from Disk", true, None);
        let dict_enabled_item =
            CheckMenuItem::new("Adaptive Correction", true, cfg.dictionary_enabled, None);
        let voiceprints = crate::acoustic::len();
        let dict_forget_voice_item = MenuItem::new(
            &format!(
                "Forget {voiceprints} voiceprint{}",
                if voiceprints == 1 { "" } else { "s" }
            ),
            voiceprints > 0,
            None,
        );
        let _ = dict_submenu.append(&dict_correct_last_item);
        let _ = dict_submenu.append(&dict_add_item);
        let _ = dict_submenu.append(&dict_open_item);
        let _ = dict_submenu.append(&dict_reload_item);
        let _ = dict_submenu.append(&dict_enabled_item);
        let _ = dict_submenu.append(&dict_forget_voice_item);
        let _ = dict_submenu.append(&PredefinedMenuItem::separator());
        let _ = dict_submenu.append(&MenuItem::new("Your words (click to remove):", false, None));
        // One removable item per word — kept at the end so we can refresh just
        // these without disturbing the stable action items above.
        let dict_entry_items = populate_dict_entries(&dict_submenu);
        let dict_correct_last_id = dict_correct_last_item.id().0.clone();
        let dict_add_id = dict_add_item.id().0.clone();
        let dict_open_id = dict_open_item.id().0.clone();
        let dict_reload_id = dict_reload_item.id().0.clone();
        let dict_enabled_id = dict_enabled_item.id().0.clone();
        let dict_forget_voice_id = dict_forget_voice_item.id().0.clone();

        // License submenu (Lemon Squeezy). All state/text comes from license.rs.
        // Items are created once; `refresh_license_submenu` retitles/enables
        // them as the state moves between trial ↔ licensed (both directions).
        // Only the actions that apply are in this submenu at any time — an item
        // greyed out because it can't apply is noise (you can't "enter a key"
        // when you already have one). `sync_license_submenu` swaps them.
        let license_submenu = Submenu::new(&crate::license::submenu_title(), true);
        let license_status_item = MenuItem::new(&crate::license::status_text(), false, None);
        let license_subscription_item = MenuItem::new("Subscribe\u{2026}", true, None);
        let license_activate_item = MenuItem::new("Enter License Key\u{2026}", true, None);
        let license_deactivate_item = MenuItem::new("Deactivate this device\u{2026}", true, None);
        let _ = license_submenu.append(&license_status_item);
        let _ = license_submenu.append(&PredefinedMenuItem::separator());
        let license_subscription_id = license_subscription_item.id().0.clone();
        let license_activate_id = license_activate_item.id().0.clone();
        let license_deactivate_id = license_deactivate_item.id().0.clone();

        // Templates submenu (voice snippets). Triggers are disabled labels; the
        // live actions are Add / Open. The trigger list refreshes on change.
        let templates_submenu =
            Submenu::new(&format!("Templates ({})", crate::templates::count()), true);
        let template_add_item = MenuItem::new("Add Template\u{2026}", true, None);
        let template_open_item = MenuItem::new("Open templates.toml\u{2026}", true, None);
        let template_reload_item = MenuItem::new("Reload from Disk", true, None);
        let _ = templates_submenu.append(&template_add_item);
        let _ = templates_submenu.append(&template_open_item);
        let _ = templates_submenu.append(&template_reload_item);
        let _ = templates_submenu.append(&PredefinedMenuItem::separator());
        let _ = templates_submenu.append(&MenuItem::new(
            "Your templates (click to edit/delete):",
            false,
            None,
        ));
        let template_items = populate_template_items(&templates_submenu);
        let template_add_id = template_add_item.id().0.clone();
        let template_open_id = template_open_item.id().0.clone();
        let template_reload_id = template_reload_item.id().0.clone();

        // History submenu (recent dictations). Clicking an entry copies it.
        // Entries come first (inserted right after the header on refresh);
        // the file/clear actions sit at the bottom.
        let history_submenu = Submenu::new("History", true);
        let _ = history_submenu.append(&MenuItem::new("Recent (click to copy):", false, None));
        let history_entry_items = populate_history_entries(&history_submenu);
        let history_open_item = MenuItem::new("Open history.txt\u{2026}", true, None);
        let history_clear_item = MenuItem::new("Clear History", true, None);
        let _ = history_submenu.append(&PredefinedMenuItem::separator());
        let _ = history_submenu.append(&history_open_item);
        let _ = history_submenu.append(&history_clear_item);
        let history_open_id = history_open_item.id().0.clone();
        let history_clear_id = history_clear_item.id().0.clone();

        // Settings submenu — low-frequency toggles + uninstall (moved off the
        // top level to keep the menu uncluttered).
        let settings_submenu = Submenu::new("Settings", true);
        let _ = settings_submenu.append(&notifications_item);
        let _ = settings_submenu.append(&sound_item);
        let _ = settings_submenu.append(&debug_item);
        let _ = settings_submenu.append(&PredefinedMenuItem::separator());
        let _ = settings_submenu.append(&uninstall_item);

        // Assemble — flat menu (submenus crash on macOS Tahoe)
        let menu = Menu::new();

        // While unlicensed (trial included), a BUY-FORWARD CTA is pinned to the
        // VERY TOP of the menu: one clickable line carrying its own urgency
        // ("Unlock Whisper Push | 3 days left") that opens the PLANS directly —
        // leading with purchase, not key entry. Entering an existing key stays
        // available inside that modal and in the License submenu. It is inserted
        // by `sync_menu_head`, not appended here, so it can come and go: once
        // licensed there is nothing at the top at all.
        let unlock_item = MenuItem::new(
            crate::license::cta_text(&crate::license::status()),
            true,
            None,
        );
        let unlock_id = unlock_item.id().0.clone();
        let head_separator = PredefinedMenuItem::separator();

        // Permissions submenu — only shown when something is actually missing
        // (when everything's granted it's just noise). Its title carries the
        // count, so no separate warning line is needed.
        if !perms.all_granted() {
            let _ = menu.append(&perms_submenu);
        }

        let _ = menu.append(&PredefinedMenuItem::separator());

        // Daily-use group first, then configuration dropdowns.
        let _ = menu.append(&history_submenu);
        let _ = menu.append(&dict_submenu);
        let _ = menu.append(&templates_submenu);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&hotkey_submenu);
        let _ = menu.append(&backend_submenu);
        let _ = menu.append(&input_submenu);
        let _ = menu.append(&output_submenu);
        let _ = menu.append(&license_submenu);

        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&settings_submenu);
        let _ = menu.append(&update_item);
        let _ = menu.append(&report_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit_item);

        // Collect IDs
        let hotkey_ids: Vec<_> = hotkey_items
            .iter()
            .map(|(i, h, m)| (i.id().0.clone(), h.clone(), m.clone()))
            .collect();
        let input_ids: Vec<_> = input_device_items
            .iter()
            .map(|(i, n)| (i.id().0.clone(), n.clone()))
            .collect();
        let output_ids: Vec<_> = output_device_items
            .iter()
            .map(|(i, n)| (i.id().0.clone(), n.clone()))
            .collect();

        self.menu_items = Some(MenuItems {
            update_id: update_item.id().0.clone(),
            report_id: report_item.id().0.clone(),
            uninstall_id: uninstall_item.id().0.clone(),
            quit_id: quit_item.id().0.clone(),
            notif_id: notifications_item.id().0.clone(),
            sound_id: sound_item.id().0.clone(),
            debug_id: debug_item.id().0.clone(),
            setup_id: setup_item.id().0.clone(),
            perm_items,
            perms_submenu,
            model_items,
            update_item,
            report_item,
            dict_submenu,
            dict_enabled_item,
            dict_correct_last_id,
            dict_add_id,
            dict_open_id,
            dict_reload_id,
            dict_enabled_id,
            dict_forget_voice_id,
            dict_entry_items,
            license_submenu,
            license_status_item,
            license_subscription_item,
            license_subscription_id,
            license_activate_item,
            license_activate_id,
            license_deactivate_item,
            license_deactivate_id,
            unlock_item,
            unlock_id,
            menu: menu.clone(),
            status_item,
            head_separator,
            notifications_item,
            sound_item,
            debug_item,
            hotkey_ids,
            hotkey_items,
            hotkey_submenu,
            custom_hotkey_item,
            custom_hotkey_id,
            input_ids,
            input_device_items,
            input_submenu,
            output_ids,
            output_device_items,
            output_submenu,
            templates_submenu,
            template_add_id,
            template_open_id,
            template_reload_id,
            template_items,
            history_submenu,
            history_open_id,
            history_clear_id,
            history_entry_items,
        });
        // Apply the enabled/title state of the license items for the current state.
        self.refresh_license_submenu();

        // Build tray
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Whisper Push");
        if let Some(icon) = glyph_icon(GlyphStyle::Template(255)) {
            builder = builder.with_icon(icon);
        }
        #[cfg(target_os = "macos")]
        {
            builder = builder.with_icon_as_template(true);
        }
        // Don't panic the whole daemon if the status item can't be created
        // (transient ControlCenter/XPC pressure, locked screen at launch): the
        // app still works headless via the hotkey + paste path. Degrade, warn.
        match builder.build() {
            Ok(tray) => self.tray = Some(tray),
            Err(e) => {
                warn!("Failed to create tray icon ({e}) — running without a menu bar item");
                self.tray = None;
                // On Linux this is usually a desktop with no StatusNotifier host
                // (vanilla GNOME without the AppIndicator extension). The app
                // still dictates, but its only UI is gone — say so, or it looks
                // like nothing launched.
                #[cfg(target_os = "linux")]
                crate::notify::app(
                    "Whisper Push is running, but your desktop shows no tray icon. \
                     On GNOME, install the AppIndicator extension \
                     (gnome-shell-extension-appindicator) and log back in. \
                     Dictation works meanwhile: hold your hotkey and speak.",
                );
            }
        }
        // Windows 11 files every new notification-area icon into the overflow
        // flyout until the user drags it out. `promote` does that for them once,
        // on its own thread — Explorer writes the entry we need some time after
        // the icon registers, so it retries with a backoff.
        #[cfg(target_os = "windows")]
        if self.tray.is_some() {
            windows_shell::promote();
        }

        // Prompt permissions after a short delay
        if !perms.all_granted() {
            let tx = self.state.tx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let _ = tx.send(Event::PromptPermissions);
            });
        }

        info!("Tray icon created");
    }

    /// Rebuild just the listed word items (after add/remove/correct/reload).
    /// Action items above keep their stable IDs; only the trailing entries are
    /// removed and re-appended. Runs on the main thread (menu is closed).
    fn refresh_dict_submenu(&mut self) {
        let Some(mi) = self.menu_items.as_mut() else {
            return;
        };
        let old = std::mem::take(&mut mi.dict_entry_items);
        for (it, _) in old {
            let _ = mi.dict_submenu.remove(&it);
        }
        mi.dict_entry_items = populate_dict_entries(&mi.dict_submenu);
        let n = crate::dictionary::entry_count();
        mi.dict_submenu.set_text(format!("Dictionary ({n})"));
    }

    /// Rebuild the recent-dictation entries in the History submenu.
    fn refresh_history_submenu(&mut self) {
        let Some(mi) = self.menu_items.as_mut() else {
            return;
        };
        let old = std::mem::take(&mut mi.history_entry_items);
        for (it, _) in old {
            let _ = mi.history_submenu.remove(&it);
        }
        mi.history_entry_items = populate_history_entries(&mi.history_submenu);
    }

    /// Rebuild the trigger list + count in the Templates submenu.
    fn refresh_templates_submenu(&mut self) {
        let Some(mi) = self.menu_items.as_mut() else {
            return;
        };
        let old = std::mem::take(&mut mi.template_items);
        for (it, _) in old {
            let _ = mi.templates_submenu.remove(&it);
        }
        mi.template_items = populate_template_items(&mi.templates_submenu);
        mi.templates_submenu
            .set_text(format!("Templates ({})", crate::templates::count()));
    }

    /// Refresh every license-dependent menu item from `license::status()` — in
    /// BOTH directions (activation, deactivation, expiry), cheap, no rebuild.
    fn refresh_license_submenu(&mut self) {
        let Some(mi) = self.menu_items.as_ref() else {
            return;
        };
        let st = crate::license::status();
        let licensed = matches!(st, crate::license::LicenseStatus::Licensed(_));
        mi.license_status_item
            .set_text(crate::license::status_text());
        mi.license_submenu.set_text(crate::license::submenu_title());
        mi.license_subscription_item.set_text(if licensed {
            "Manage License\u{2026}"
        } else {
            "Subscribe\u{2026}"
        });
        // Licensed → [Manage License…, Deactivate this device…]
        // Unlicensed → [Subscribe…, Enter License Key…]
        // Never both sets with half of them greyed out.
        let sub = &mi.license_submenu;
        let _ = sub.remove(&mi.license_subscription_item);
        let _ = sub.remove(&mi.license_activate_item);
        let _ = sub.remove(&mi.license_deactivate_item);
        let _ = sub.append(&mi.license_subscription_item);
        let _ = sub.append(if licensed {
            &mi.license_deactivate_item
        } else {
            &mi.license_activate_item
        });
        mi.unlock_item.set_text(crate::license::cta_text(&st));
        self.sync_menu_head();
    }

    /// Fall back to Auto for any device pinned in the config that is no longer
    /// present.
    ///
    /// A stale pin is a lie the whole UI then repeats: nothing is ticked in the
    /// picker (the pinned name has no row), the submenu is titled after a device
    /// that is gone, and recording quietly uses the system default anyway
    /// (`find_input_device` falls back). Switching to Auto makes the menu agree
    /// with what actually happens, and the user can re-pick once the device is
    /// back. Only acts on a list we actually got: when enumeration fails
    /// entirely we keep the pin rather than throw a preference away over a
    /// CoreAudio stall.
    fn reconcile_device_pins(&mut self) {
        let (input, output) = {
            let c = self.config.lock_safe();
            (c.input_device.clone(), c.output_device.clone())
        };
        let gone = |pinned: &str, list: Vec<String>| {
            pinned != "auto" && !list.is_empty() && !list.iter().any(|n| n == pinned)
        };
        let drop_input = gone(&input, crate::audio::list_devices().unwrap_or_default());
        let drop_output = gone(
            &output,
            crate::audio::list_output_devices().unwrap_or_default(),
        );
        if !drop_input && !drop_output {
            return;
        }
        let mut c = self.config.lock_safe();
        if drop_input {
            warn!("Input device '{input}' is no longer present \u{2014} falling back to Auto");
            c.input_device = "auto".into();
            // A pin the user no longer has can't outrank the auto-fallback.
            crate::audio::set_input_override("");
        }
        if drop_output {
            warn!("Output device '{output}' is no longer present \u{2014} falling back to Auto");
            c.output_device = "auto".into();
        }
        let _ = c.save();
        crate::audio::playback::set_output_device(&c.output_device);
    }

    /// Put exactly the head items that carry information into the menu, in
    /// order, and take out the ones that don't.
    ///
    /// The rule: **nothing at the top that you cannot click**. The buy CTA is
    /// there only while unlicensed (once licensed, managing happens in the
    /// License submenu — a second entry point at the top was just clutter), and
    /// the state line only while the app is doing something the rest of the menu
    /// doesn't already say. When both are gone the separator goes too, so the
    /// menu opens straight onto its real contents.
    fn sync_menu_head(&mut self) {
        let Some(mi) = self.menu_items.as_ref() else {
            return;
        };
        let licensed = matches!(
            crate::license::status(),
            crate::license::LicenseStatus::Licensed(_)
        );
        let busy = self.state.current() != State::Idle;

        // Remove first (harmless if absent), then re-insert what applies: the
        // order of a menu is its indices, so rebuilding the head wholesale is
        // simpler and less error-prone than patching positions.
        let _ = mi.menu.remove(&mi.unlock_item);
        let _ = mi.menu.remove(&mi.status_item);
        let _ = mi.menu.remove(&mi.head_separator);

        let mut at = 0;
        if !licensed {
            let _ = mi.menu.insert(&mi.unlock_item, at);
            at += 1;
        }
        if busy {
            let _ = mi.menu.insert(&mi.status_item, at);
            at += 1;
        }
        if at > 0 {
            let _ = mi.menu.insert(&mi.head_separator, at);
        }
    }

    /// The one way to open the license / subscription modal, from every entry
    /// point (Unlock, Subscribe/Manage, Enter License Key, the blocked-dictation
    /// notification). Runs on the main thread so the helper gets the activation
    /// hand-off, then blocks on its own thread until the window closes and
    /// refreshes the menu from the (possibly rewritten) license.json. Dev builds
    /// without the helper bundle fall back to a native key dialog.
    fn open_license_window(&self, start_activate: bool) {
        // Hand foreground activation to the helper FIRST, from here: this runs on
        // the main thread (it is called out of the event loop) and the yield is
        // main-thread-only. Doing it inside the worker below silently did
        // nothing, and the modal opened without keyboard focus.
        crate::onboarding::yield_activation_to_wizard();
        let tx = self.state.tx.clone();
        std::thread::Builder::new()
            .name("license-window".into())
            .spawn(move || {
                // No file watching here: `license::start_background_sync` polls
                // license.json for the whole run, so an activation or
                // deactivation done *inside* the modal retitles the menu while
                // the window is still open — and the CLI path is covered too.
                let opened = crate::onboarding::run_license_window(start_activate);
                if opened {
                    let _ = tx.send(Event::LicenseChanged);
                } else {
                    license_activate_dialog(tx);
                }
            })
            .ok();
    }

    fn process_event(&mut self, event: Event) {
        if matches!(event, Event::DictChanged) {
            self.refresh_dict_submenu();
            return;
        }
        if matches!(event, Event::LicenseChanged) {
            self.refresh_license_submenu();
            return;
        }
        if let Event::OpenLicenseWindow { start_activate } = event {
            self.open_license_window(start_activate);
            return;
        }
        let mi = match &self.menu_items {
            Some(m) => m,
            None => return,
        };

        match event {
            Event::ModelReady => {
                self.state.set(State::Idle);
                let disp = format_hotkey_display(
                    &self.state.config.hotkey,
                    &self.state.config.hotkey_mode,
                );
                mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                self.sync_menu_head(); // no longer loading → the state line goes
                set_tray_icon(&self.tray, State::Idle);
                if self.config.lock_safe().notifications {
                    crate::notify::app("Model loaded and ready!");
                }
                info!("Ready");
            }

            Event::MenuClicked(ref id) => {
                if id == &mi.quit_id {
                    crate::util::exit_clean();
                }
                if id == &mi.uninstall_id {
                    // This deletes the downloaded models, the learned
                    // dictionary and the license activation — several GB and a
                    // device slot — so it asks first. It used to go on one
                    // click, from a menu item sitting next to "Quit".
                    std::thread::spawn(uninstall_dialog);
                    return;
                }
                if id == &mi.template_add_id {
                    // osascript dialogs block → run off the UI thread. The list
                    // refreshes via templates::take_dirty() in about_to_wait.
                    std::thread::spawn(add_template_dialog);
                    return;
                }
                if id == &mi.template_open_id {
                    open_path(&crate::templates::ensure_file());
                    return;
                }
                if id == &mi.template_reload_id {
                    crate::templates::reload(); // sets the dirty flag → submenu refresh
                    crate::notify::app(&format!(
                        "Templates reloaded \u{2014} {} template(s).",
                        crate::templates::count()
                    ));
                    return;
                }
                // Click a template trigger → edit (open file) or delete.
                if let Some((_, trigger)) = mi
                    .template_items
                    .iter()
                    .find(|(it, t)| !t.is_empty() && id == &it.id().0)
                {
                    let trigger = trigger.clone();
                    std::thread::spawn(move || template_action_dialog(&trigger));
                    return;
                }
                if id == &mi.history_open_id {
                    open_path(&crate::history::file_path());
                    return;
                }
                if id == &mi.history_clear_id {
                    crate::history::clear(); // sets the dirty flag → submenu refresh
                    crate::notify::app("History cleared.");
                    return;
                }
                // Click a history entry → copy that dictation to the clipboard.
                if let Some((_, text)) = mi
                    .history_entry_items
                    .iter()
                    .find(|(it, t)| !t.is_empty() && id == &it.id().0)
                {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        if cb.set_text(text.clone()).is_ok() {
                            crate::notify::app("Copied to clipboard.");
                        }
                    }
                    return;
                }
                // A permission row: take the user where it is granted. On
                // Linux that IS the grant (a polkit `usermod`), hence
                // `request_one` rather than only opening Settings.
                if let Some((_, kind)) = mi.perm_items.iter().find(|(it, _)| id == &it.id().0) {
                    crate::permissions::request_one(*kind);
                    crate::permissions::open_settings_for(*kind);
                    return;
                }
                if id == &mi.setup_id {
                    crate::permissions::guided_setup(); // self-spawns + guards re-entry
                    return;
                }
                if id == &mi.update_id {
                    if let Some(page) = self.pending_release_page.take() {
                        open_path(std::path::Path::new(&page));
                        // UpdateFailed("") is the one reset path for the item.
                        let _ = self.state.tx.send(Event::UpdateFailed(String::new()));
                    } else if let Some((version, url)) = self.pending_update.clone() {
                        mi.update_item
                            .set_text(&format!("Downloading v{version}\u{2026}"));
                        mi.update_item.set_enabled(false);
                        std::thread::Builder::new()
                            .name("update-install".into())
                            .spawn(move || {
                                if let Err(e) = crate::updater::install::download_and_install(&url)
                                {
                                    tracing::error!("Update failed: {e}");
                                    // Can't send event here because process may exit on success
                                    crate::notify::app(&format!("Update failed: {e}"));
                                }
                            })
                            .ok();
                    } else {
                        // Manual check
                        mi.update_item.set_text("Checking\u{2026}");
                        mi.update_item.set_enabled(false);
                        let tx = self.state.tx.clone();
                        std::thread::Builder::new()
                            .name("update-manual-check".into())
                            .spawn(move || {
                                use crate::updater::UpdateCheck;
                                match crate::updater::check_for_update() {
                                    Ok(UpdateCheck::Available { version, url }) => {
                                        let _ = tx.send(Event::UpdateAvailable(version, url));
                                    }
                                    Ok(UpdateCheck::UpToDate) => {
                                        let v = env!("CARGO_PKG_VERSION");
                                        crate::notify::app(&format!(
                                            "You are on the latest version (v{v})."
                                        ));
                                        // Show the result in the menu too — the
                                        // notification can be silently invisible —
                                        // then reset the item after a beat.
                                        let _ = tx.send(Event::UpdateStatus(format!(
                                            "\u{2713} Up to date (v{v})"
                                        )));
                                        std::thread::sleep(std::time::Duration::from_secs(8));
                                        let _ = tx.send(Event::UpdateFailed(String::new()));
                                    }
                                    Ok(UpdateCheck::NoAsset { version }) => {
                                        crate::notify::app(&format!(
                                            "v{version} is out. Whisper Push updates itself \
                                             only on macOS \u{2014} use the menu to open the \
                                             download page."
                                        ));
                                        let _ = tx.send(Event::UpdateStatus(manual_update_label(
                                            &version,
                                        )));
                                    }
                                    Err(e) => {
                                        tracing::error!("Update check failed: {e}");
                                        crate::notify::app(&format!("Update check failed: {e}"));
                                        let _ = tx.send(Event::UpdateFailed(e.to_string()));
                                    }
                                }
                            })
                            .ok();
                    }
                    return;
                }
                if id == &mi.report_id {
                    crate::report::open_report();
                    return;
                }
                if id == &mi.notif_id {
                    let mut c = self.config.lock_safe();
                    c.notifications = !c.notifications;
                    let _ = c.save();
                    return;
                }
                if id == &mi.sound_id {
                    let mut c = self.config.lock_safe();
                    c.sound_feedback = !c.sound_feedback;
                    let _ = c.save();
                    return;
                }
                if id == &mi.debug_id {
                    let mut c = self.config.lock_safe();
                    c.debug = !c.debug;
                    let _ = c.save();
                    return;
                }
                if id == &mi.dict_correct_last_id {
                    // osascript blocks until the user answers → run off the UI thread.
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || correct_last_dialog(tx));
                    return;
                }
                if id == &mi.dict_add_id {
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || add_word_dialog(tx));
                    return;
                }
                if id == &mi.dict_open_id {
                    open_path(&crate::dictionary::ensure_file());
                    return;
                }
                if id == &mi.dict_reload_id {
                    let _ = crate::dictionary::reload();
                    crate::notify::app(&format!(
                        "Dictionary reloaded \u{2014} {} word(s).",
                        crate::dictionary::entry_count()
                    ));
                    let _ = self.state.tx.send(Event::DictChanged);
                    return;
                }
                if id == &mi.dict_forget_voice_id {
                    crate::acoustic::clear();
                    crate::notify::app("Forgot all learned voiceprints.");
                    let _ = self.state.tx.send(Event::DictChanged);
                    return;
                }
                if id == &mi.dict_enabled_id {
                    let mut c = self.config.lock_safe();
                    c.dictionary_enabled = !c.dictionary_enabled;
                    let on = c.dictionary_enabled;
                    let _ = c.save();
                    drop(c);
                    crate::dictionary::init(on);
                    crate::notify::app(if on {
                        "Adaptive correction ON"
                    } else {
                        "Adaptive correction OFF"
                    });
                    return;
                }
                if (!mi.unlock_id.is_empty() && id == &mi.unlock_id)
                    || id == &mi.license_subscription_id
                {
                    // Buy-forward (plans screen; a licensed user gets the
                    // "manage" screen instead — the modal reads the state).
                    self.open_license_window(false);
                    return;
                }
                if id == &mi.license_activate_id {
                    // Straight to "enter your key".
                    self.open_license_window(true);
                    return;
                }
                if id == &mi.license_deactivate_id {
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || license_deactivate_dialog(tx));
                    return;
                }
                // Click a listed word → edit (open the file) or delete.
                if let Some((_, term)) = mi
                    .dict_entry_items
                    .iter()
                    .find(|(it, t)| !t.is_empty() && id == &it.id().0)
                {
                    let term = term.clone();
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || dict_action_dialog(&term, tx));
                    return;
                }
                for (item_id, hotkey, mode) in &mi.hotkey_ids {
                    if id == item_id {
                        let mut c = self.config.lock_safe();
                        c.hotkey = hotkey.clone();
                        c.hotkey_mode = mode.clone();
                        let _ = c.save();
                        for (item, hk, m) in &mi.hotkey_items {
                            item.set_checked(hk == hotkey && m == mode);
                        }
                        let disp = format_hotkey_display(hotkey, mode);
                        mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                        mi.hotkey_submenu.set_text(format!("Hotkey: {disp}"));
                        // Live on every platform: the listeners hold a mutable
                        // matcher, so no restart is needed anywhere.
                        crate::hotkey::rebind(hotkey, mode);
                        crate::notify::app(&format!("Hotkey set to {disp}"));
                        return;
                    }
                }
                if !mi.custom_hotkey_id.is_empty() && id == &mi.custom_hotkey_id {
                    if self.capturing {
                        self.end_hotkey_capture(); // clicked again = cancel
                        return;
                    }
                    crate::hotkey::start_capture(self.state.tx.clone());
                    self.capturing = true;
                    self.capture_gen += 1;
                    // The prompt goes in the MENU: the notification below rides
                    // on the deprecated NSUserNotification path, which recent
                    // macOS often delivers invisibly — the item looked dead.
                    if let Some(it) = &mi.custom_hotkey_item {
                        it.set_text("\u{2328} Press your shortcut now\u{2026} (click to cancel)");
                        it.set_checked(false);
                    }
                    crate::notify::app(
                        "Press your shortcut now: tap a modifier (e.g. Right \u{2318}) to hold, or a combo like \u{2318}\u{21e7}D to toggle.",
                    );
                    // Don't stay armed forever if they walk away.
                    let (tx, generation) = (self.state.tx.clone(), self.capture_gen);
                    std::thread::spawn(move || {
                        std::thread::sleep(HOTKEY_CAPTURE_TIMEOUT);
                        let _ = tx.send(Event::HotkeyCaptureTimeout(generation));
                    });
                    return;
                }
                for (item_id, name) in &mi.input_ids {
                    if id == item_id {
                        let mut c = self.config.lock_safe();
                        c.input_device = name.clone();
                        let _ = c.save();
                        // An explicit pick overrides any silent auto-fallback.
                        crate::audio::set_input_override("");
                        crate::audio::clear_dead_mics();
                        for (item, n) in &mi.input_device_items {
                            item.set_checked(n == name);
                        }
                        mi.input_submenu.set_text(device_title("Input", name));
                        return;
                    }
                }
                for (item_id, name) in &mi.output_ids {
                    if id == item_id {
                        let mut c = self.config.lock_safe();
                        c.output_device = name.clone();
                        let _ = c.save();
                        crate::audio::playback::set_output_device(name);
                        for (item, n) in &mi.output_device_items {
                            item.set_checked(n == name);
                        }
                        mi.output_submenu.set_text(device_title("Output", name));
                        return;
                    }
                }
                // Model selection — `id` matches a model row in the Engine submenu.
                for (item, model_name) in &mi.model_items {
                    if id == &item.id().0 {
                        {
                            let mut c = self.config.lock_safe();
                            c.model = model_name.clone();
                            let _ = c.save();
                        }
                        // Re-render every row: ● on the picked model, ⤓ on any not
                        // (yet) downloaded — recomputed from the live model list.
                        let models = crate::model_manager::list_models();
                        for (bi, bv) in &mi.model_items {
                            if let Some(m) = models.iter().find(|m| m.name == bv.as_str()) {
                                let active = if bv == model_name {
                                    "\u{25CF} "
                                } else {
                                    "    "
                                };
                                let dl = if m.is_downloaded { "" } else { " \u{2913}" };
                                bi.set_text(format!("{active}{}{dl}", m.label));
                            }
                        }
                        // Send LoadModel to the pipeline thread — it unloads the old
                        // model and loads (downloading if needed) the new one on its
                        // own thread (WGPU/Metal same-thread constraint).
                        if let Some(ref tx) = self.pipeline_tx {
                            let _ = tx.send(Event::LoadModel(model_name.clone()));
                        }
                        let label = models
                            .iter()
                            .find(|m| m.name == model_name.as_str())
                            .map(|m| m.label)
                            .unwrap_or(model_name.as_str());
                        crate::notify::app(&format!("Loading {label}..."));
                        return;
                    }
                }
            }

            Event::StateChanged(State::Recording) => {
                // Reached from BOTH the menu toggle and the physical hotkey
                // (the pipeline thread now emits this so the icon turns citron
                // regardless of how recording started). The start sound is
                // played at each trigger point, never here, to avoid doubling.
                self.state.set(State::Recording);
                set_tray_icon(&self.tray, State::Recording);
                crate::overlay::set_state(crate::overlay::OverlayState::Recording);
            }

            // Pill-only events (the tray icon stays on StateChanged). ShowOverlay
            // fires on key-down so the pill appears with the start sound, ahead of
            // the hold-delay gate + mic open; HideOverlay covers the early exits.
            Event::ShowOverlay => {
                crate::overlay::set_state(crate::overlay::OverlayState::Recording);
            }
            Event::HideOverlay => {
                crate::overlay::set_state(crate::overlay::OverlayState::Idle);
            }

            Event::HotkeyCaptured(hotkey, mode) => {
                info!("Custom hotkey captured: '{hotkey}' ({mode})");
                {
                    let mut c = self.config.lock_safe();
                    c.hotkey = hotkey.clone();
                    c.hotkey_mode = mode.clone();
                    let _ = c.save();
                }
                self.capturing = false;
                self.capture_gen += 1; // any pending timeout is now stale
                // Tap already rebound the live listener; just sync the UI.
                let mut matched_preset = false;
                for (item, hk, m) in &mi.hotkey_items {
                    let on = hk == &hotkey && m == &mode;
                    matched_preset |= on;
                    item.set_checked(on);
                }
                let disp = format_hotkey_display(&hotkey, &mode);
                // A combo that is none of the presets now appears in the list as
                // a checked "Custom: …" entry, instead of vanishing into the
                // submenu title.
                if let Some(it) = &mi.custom_hotkey_item {
                    it.set_text(custom_hotkey_label((!matched_preset).then(|| disp.clone())));
                    it.set_checked(!matched_preset);
                }
                mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                mi.hotkey_submenu.set_text(format!("Hotkey: {disp}"));
                crate::notify::app(&format!("Custom hotkey set: {disp}"));
            }

            // A timeout from an earlier capture (a newer one started, or this one
            // already completed) carries a stale generation and falls through.
            Event::HotkeyCaptureTimeout(generation)
                if generation == self.capture_gen && self.capturing =>
            {
                self.end_hotkey_capture();
                crate::notify::app("No shortcut captured \u{2014} nothing changed.");
            }

            Event::PromptPermissions => {
                info!("Checking/prompting permissions...");
                let status = crate::permissions::check_all();
                if !status.all_granted() {
                    // Guided flow: prompts + opens panes + polls + restarts. It
                    // self-spawns a worker thread and returns immediately, so this
                    // never blocks the winit main thread.
                    crate::permissions::guided_setup();
                }
                // Schedule a re-check to update the menu
                let tx = self.state.tx.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    let _ = tx.send(Event::RefreshPermissions);
                });
            }

            Event::RefreshPermissions => {
                let status = crate::permissions::check_all();
                if let Some(mi) = &self.menu_items {
                    for (item, kind) in &mi.perm_items {
                        let state = status.state(*kind);
                        item.set_text(perm_label(*kind, state));
                        item.set_enabled(state != crate::permissions::PermState::Granted);
                    }
                    mi.perms_submenu.set_text(perms_title(&status));
                }
                info!("Permissions refreshed: {} missing", status.missing_count());
                // Re-check again in 5s if still not all granted
                if !status.all_granted() {
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let _ = tx.send(Event::RefreshPermissions);
                    });
                }
            }

            Event::UpdateAvailable(ref version, ref url) => {
                mi.update_item
                    .set_text(&format!("\u{2b06} Update to v{version}"));
                mi.update_item.set_enabled(true);
                self.pending_update = Some((version.clone(), url.clone()));
                // An installable update supersedes any "open the releases page"
                // state — the two click behaviours must never both be armed.
                self.pending_release_page = None;
                if self.config.lock_safe().notifications {
                    crate::notify::app(&format!(
                        "Version {version} available! Click the menu to update."
                    ));
                }
                info!("Update available: v{version}");
            }

            Event::UpdateStatus(ref text) => {
                // Reliable, in-menu feedback for a manual check — the toast route
                // (deprecated NSUserNotification) can be delivered invisibly.
                mi.update_item.set_text(text);
                mi.update_item.set_enabled(true);
                if text.ends_with(OPEN_RELEASES_HINT) {
                    // "Newer release, no asset for this platform" → clicking the
                    // item opens the releases page instead of downloading.
                    self.pending_release_page = Some(crate::updater::releases_page());
                    self.pending_update = None;
                } else {
                    self.pending_release_page = None;
                }
            }

            Event::UpdateFailed(ref msg) => {
                mi.update_item.set_text("Check for Updates\u{2026}");
                mi.update_item.set_enabled(true);
                // The one reset path for the item — clear both click behaviours.
                self.pending_release_page = None;
                if !msg.is_empty() {
                    warn!("Update failed: {msg}");
                }
            }

            Event::RefreshTrayIcon => {
                // Debounce timer fired — push the icon if the state has settled.
                flush_tray_icon(&self.tray);
            }

            Event::InputSwitched(ref name) => {
                // A dead mic was auto-replaced: show the live device in the
                // submenu title. Checkmarks are left alone — they reflect the
                // *saved* config, and an explicit user pick (handler above)
                // resets the title via the plain `device_title`.
                mi.input_submenu
                    .set_text(format!("{} (auto-switched)", device_title("Input", name)));
            }

            Event::StateChanged(s) => {
                self.state.set(s);
                self.sync_menu_head(); // busy ⇄ idle decides the state line
                set_tray_icon(&self.tray, s); // also refreshes the tooltip
                crate::overlay::set_state(match s {
                    State::Processing => crate::overlay::OverlayState::Processing,
                    _ => crate::overlay::OverlayState::Idle, // Idle / Loading
                });
            }

            _ => {}
        }
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // WaitUntil(500ms) — needed for NSEvent monitors to fire (they require
        // the run loop to pump events). 500ms is slow enough to not close menus
        // on most interactions, but fast enough for hotkey responsiveness.
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(500),
        ));

        if cause == winit::event::StartCause::Init {
            // Load model SYNCHRONOUSLY before creating the tray,
            // so the menu starts in "Ready" state and never needs updating.
            // This avoids modifying menu items after creation (which closes
            // the menu on macOS Tahoe).
            // Load Whisper on main thread (always needed as fallback).
            self.state.set(State::Idle);
            self.create_tray();
            let startup_model = self.state.config.model.clone();

            // Wake the macOS run loop so the icon appears immediately.
            wake_main();

            // Start hotkey listener + autonomous pipeline thread.
            // The pipeline runs entirely in background threads — it never
            // touches the winit event loop, so the tray menu stays open.
            let hotkey_cfg = self.state.config.hotkey.clone();
            let hotkey_mode = self.state.config.hotkey_mode.clone();
            let pipeline_cfg = self.config.clone();
            let (ptx, prx) = crossbeam_channel::unbounded();
            self.pipeline_tx = Some(ptx.clone());
            // Publish for the state watchdog (stuck-Recording recovery).
            let _ = PIPELINE_TX.set(ptx.clone());
            // The pipeline keeps a sender to its own channel so it can re-queue an
            // event it must not drop (e.g. a model switch that lands during the
            // hold-delay gate).
            let self_tx = ptx.clone();
            // A failed listener (unparseable hotkey, missing 'input' group on
            // Linux, hook install error) means dictation is dead — don't swallow
            // it. Tell the user instead of looking silently broken.
            if let Err(e) = crate::hotkey::start_listener(&hotkey_cfg, &hotkey_mode, ptx) {
                warn!("Hotkey listener failed to start: {e}");
                crate::notify::app(&format!(
                    "Couldn't start the {hotkey_cfg} hotkey ({e}). Pick another in the menu."
                ));
            }

            // Pipeline thread: hotkey events + model load → capture → transcribe → paste
            let ui_tx = self.state.tx.clone();
            std::thread::spawn(move || {
                pipeline_loop(prx, pipeline_cfg, ui_tx, self_tx);
            });

            // Load model on the pipeline thread (all backends, including Voxtral/WGPU)
            if let Some(ref tx) = self.pipeline_tx {
                let _ = tx.send(Event::LoadModel(startup_model));
            }

            // Background update check (waits 10s before hitting GitHub API)
            let update_tx = self.state.tx.clone();
            let check_updates = self.state.config.check_updates;
            crate::updater::spawn_check(update_tx, check_updates);

            info!("Ready! Hold Control to dictate.");
        }

        // Events arrive via user_event() from the EventLoopProxy forwarder.
        // No polling needed — ControlFlow::Wait keeps the run loop clean.
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Poll all event sources. No EventLoopProxy used — avoids macOS Tahoe
        // menu closing bug. This is called every ~100ms via WaitUntil.

        // 1. Menu events (clicked items)
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            self.process_event(Event::MenuClicked(event.id().0.to_string()));
        }

        // 2. App events (hotkey, transcription, model loading, etc.)
        while let Ok(event) = self.rx.try_recv() {
            self.process_event(event);
        }

        // 3. Auto-capture learned a word off the UI thread → refresh the list.
        if crate::dictionary::take_menu_dirty() {
            self.refresh_dict_submenu();
        }
        // 4. History / templates changed (a dictation, a clear, an add) → refresh.
        if crate::history::take_dirty() {
            self.refresh_history_submenu();
        }
        if crate::templates::take_dirty() {
            self.refresh_templates_submenu();
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(menu_event) => {
                self.process_event(Event::MenuClicked(menu_event.id().0.to_string()));
            }
            UserEvent::Tray(_) => {}
            UserEvent::App(app_event) => {
                self.process_event(app_event);
            }
            // A bare wake: the work happens in `about_to_wait`, which winit runs
            // right after dispatching this.
            UserEvent::Wake => {}
        }
    }
}

pub fn run(state: AppState, rx: Receiver<Event>) -> Result<()> {
    info!("Starting system tray...");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;

    // Menu and tray events are NOT forwarded through the proxy: a winit user
    // event closes the tray menu on macOS Tahoe (Apple bug), so they are polled
    // in `about_to_wait` instead, driven by the WaitUntil tick.
    //
    // Off macOS the proxy is kept for one thing only — waking that tick early so
    // a state change (the recording icon) shows immediately. See `wake_main`.
    #[cfg(not(target_os = "macos"))]
    let _ = EVENT_PROXY.set(event_loop.create_proxy());

    // Sender for the coalesced trailing tray-icon refresh (see set_tray_icon).
    let _ = TRAY_TX.set(state.tx.clone());

    // Safety net against a wedged state machine (a lost Processing→Idle
    // transition leaves the app silently refusing dictations — observed in the
    // wild). Force Idle if Processing persists far longer than any real
    // transcription could take.
    spawn_state_watchdog(state.tx.clone());

    // Floating "listening" pill — created hidden on the main thread now; shown
    // with a live citron waveform while recording.
    crate::overlay::set_enabled(state.config.overlay_enabled);
    crate::overlay::init();

    let mut app = App::new(state, rx);

    event_loop.run_app(&mut app)?;

    Ok(())
}

/// Poll interval for the state watchdog.
const WATCHDOG_TICK: Duration = Duration::from_secs(10);
/// Force Idle if `Processing` has lasted longer than this — a real transcription
/// (even a cold-start page-in) finishes in well under 10 s, so this only ever
/// fires on a genuine wedge, never on a legitimately slow dictation.
const WATCHDOG_MAX_PROCESSING: u64 = 30;
/// End a recording that has lasted longer than this. No real push-to-talk hold
/// runs for minutes; this only trips when a `HotkeyUp` was lost (e.g. the
/// CGEventTap died) and the mic is stuck open. We end it via the *normal* stop
/// path so whatever was captured is still transcribed and pasted.
const WATCHDOG_MAX_RECORDING: u64 = 300;

/// Recover from a wedged state machine:
/// - stuck in `Processing` (a lost Processing→Idle transition) → force `Idle`,
///   so the app can't silently refuse all further dictations;
/// - stuck in `Recording` (a dropped `HotkeyUp` left the mic open) → inject a
///   `HotkeyUp` into the pipeline, which stops + transcribes + returns to Idle.
fn spawn_state_watchdog(tx: Sender<Event>) {
    std::thread::Builder::new()
        .name("state-watchdog".into())
        .spawn(move || {
            loop {
                std::thread::sleep(WATCHDOG_TICK);
                if let Some(secs) = crate::state::processing_stuck_secs() {
                    if secs >= WATCHDOG_MAX_PROCESSING {
                        warn!("state watchdog: stuck in Processing for {secs}s — forcing Idle");
                        let _ = tx.send(Event::StateChanged(State::Idle));
                    }
                }
                if let Some(secs) = crate::state::recording_stuck_secs() {
                    if secs >= WATCHDOG_MAX_RECORDING {
                        warn!(
                            "state watchdog: stuck in Recording for {secs}s — \
                             ending it (likely a dropped HotkeyUp)"
                        );
                        // Route through the pipeline so the mic actually closes and
                        // the captured audio is transcribed, not just the UI reset.
                        if let Some(ptx) = PIPELINE_TX.get() {
                            let _ = ptx.send(Event::HotkeyUp);
                        }
                    }
                }
            }
        })
        .ok();
}

/// Autonomous pipeline: listens for hotkey events, captures audio,
/// transcribes, and pastes — all in background threads.
/// Never touches the winit event loop or tray menu.
fn pipeline_loop(
    rx: Receiver<Event>,
    config: Arc<Mutex<Config>>,
    ui_tx: Sender<Event>,
    self_tx: Sender<Event>,
) {
    // Everything here runs on this one thread — no Arc/Mutex/atomics needed.
    let mut recording = false;
    let mut capture: Option<crate::audio::capture::AudioCapture> = None;
    // Generation counter for mid-recording mic health checks: bumped when a
    // recording commits (and on a mid-recording input swap), so a check that
    // arrives late — for a recording that already ended — is ignored as stale.
    let mut mic_check_gen: u64 = 0;

    loop {
        // recv() is the one place the loop ends (channel closed at shutdown).
        let event = match rx.recv() {
            Ok(e) => e,
            Err(_) => break,
        };
        // Contain panics per-event. A fault handling one event — a bad audio
        // device, a model load failure, a transcription crash — must not tear
        // down this thread: it is the *only* pipeline worker, so its death would
        // stop all dictation silently with no recovery short of an app restart.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            handle_pipeline_event(
                event,
                &rx,
                &config,
                &ui_tx,
                &self_tx,
                &mut recording,
                &mut capture,
                &mut mic_check_gen,
            )
        }))
        .is_err()
        {
            tracing::error!("Pipeline event handler panicked — recovering to idle");
            // Reset to a known-good state so the next hotkey works again.
            recording = false;
            capture = None;
            let _ = ui_tx.send(Event::StateChanged(State::Idle));
        }
    }
}

/// Wake the macOS main run loop so a UI event sent from a worker thread drains
/// now, instead of after the ~500 ms `WaitUntil` tick (the crossbeam channel
/// doesn't itself wake winit). No-op off macOS. A bare `wake_up` creates no winit
/// UserEvent, so it does NOT trip the Tahoe menu-close bug — do not reintroduce
/// `EventLoopProxy` for this.
fn wake_main() {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_foundation::CFRunLoop;
        if let Some(rl) = CFRunLoop::main() {
            rl.wake_up();
        }
    }
    // Windows and Linux have no equivalent of "poke the run loop" outside winit,
    // so wake it the sanctioned way. The Tahoe menu-close bug that rules the
    // proxy out is macOS-only — and there the CFRunLoop poke above already does
    // the job without creating a winit event. Without this the recording icon,
    // which off macOS is the ONLY sign the app heard you (there is no overlay
    // pill), lagged the 500 ms WaitUntil tick on every single dictation.
    #[cfg(not(target_os = "macos"))]
    if let Some(proxy) = EVENT_PROXY.get() {
        let _ = proxy.send_event(UserEvent::Wake);
    }
}

/// Proxy used by `wake_main` off macOS. Set once, when the loop is built.
#[cfg(not(target_os = "macos"))]
static EVENT_PROXY: OnceLock<winit::event_loop::EventLoopProxy<UserEvent>> = OnceLock::new();

/// Send a UI event from the pipeline thread and wake the main loop so the icon /
/// overlay pill react immediately rather than lagging the WaitUntil tick.
fn notify_ui(ui_tx: &Sender<Event>, ev: Event) {
    let _ = ui_tx.send(ev);
    wake_main();
}

/// Wait out the hold-delay gate. Returns `true` if the hold was cancelled before
/// `delay` elapsed — a quick tap (`HotkeyUp`), a model switch (re-queued via
/// `self_tx` so it isn't dropped), or a dead channel; `false` means a genuine
/// hold was confirmed. Shared by the entitled (pre-roll) and not-entitled
/// (nudge) `HotkeyDown` paths so the quick-tap filter lives in one place.
fn hold_gate(rx: &Receiver<Event>, self_tx: &Sender<Event>, delay: f64) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(delay);
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        if remaining.is_zero() {
            return false;
        }
        match rx.recv_timeout(remaining) {
            Ok(Event::HotkeyUp) => return true,
            Ok(Event::LoadModel(m)) => {
                let _ = self_tx.send(Event::LoadModel(m));
                return true;
            }
            Ok(_) => {} // ignore a duplicate HotkeyDown/Toggle here
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => return false,
            Err(_) => return true,
        }
    }
    false
}

/// Delay before the first mid-recording mic health probe — long enough for a
/// slow (Bluetooth) mic to open its link and start delivering on a healthy one.
const MIC_CHECK_DELAY: Duration = Duration::from_millis(1200);
/// Extra grace when a check finds too few samples to judge the peak yet.
const MIC_CHECK_RETRY_DELAY: Duration = Duration::from_millis(700);
/// Minimum captured audio (~0.9 s at 16 kHz) before a health check trusts the
/// peak — under this the stream may simply still be warming up.
const MIC_CHECK_MIN_SAMPLES: usize = crate::audio::SAMPLE_RATE as usize * 9 / 10;

/// Send `MicHealthCheck(generation)` back into the pipeline channel after
/// `delay`, from a detached thread (the pipeline thread must keep draining).
fn schedule_mic_check(self_tx: &Sender<Event>, generation: u64, delay: Duration) {
    let tx = self_tx.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = tx.send(Event::MicHealthCheck(generation));
    });
}

/// Handle one pipeline event. Split out of `pipeline_loop` so each event runs
/// inside a `catch_unwind` at the call site — a panic here is contained and the
/// loop resets to idle rather than the worker thread dying.
#[allow(clippy::too_many_arguments)]
fn handle_pipeline_event(
    event: Event,
    rx: &Receiver<Event>,
    config: &Arc<Mutex<Config>>,
    ui_tx: &Sender<Event>,
    self_tx: &Sender<Event>,
    recording: &mut bool,
    capture: &mut Option<crate::audio::capture::AudioCapture>,
    mic_check_gen: &mut u64,
) {
    match event {
        Event::HotkeyDown => {
            if *recording {
                return;
            }
            // Entitlement is read here but enforced only AFTER the hold is
            // confirmed (below), so an incidental Ctrl tap (e.g. Ctrl+C) never
            // triggers the "subscribe" nudge — only a genuine dictation hold does.
            let entitled = crate::license::is_entitled();
            let cfg = config.lock_safe();
            let device = crate::audio::effective_input_device(&cfg.input_device);
            let delay = cfg.hold_delay;
            let sound_feedback = cfg.sound_feedback;
            let language = cfg.language.clone();
            drop(cfg);

            // Immediate audio acknowledgement — a 70 ms blip the moment the
            // key is pressed. Only when we'll actually record: a blocked
            // (trial-over) user shouldn't hear a "start" blip with no transcription.
            if entitled && sound_feedback {
                crate::audio::playback::play_sound("start");
            }

            // Not entitled: filter quick taps with the gate, then nudge on a
            // genuine hold. Never open the mic.
            if !entitled {
                if !hold_gate(rx, self_tx, delay) {
                    crate::license::on_blocked();
                }
                return;
            }

            // PRE-ROLL — open the mic the instant the key goes down (at the
            // "start" cue), NOT after the hold-delay gate, so the beginning of
            // speech is captured instead of lost to the open latency. The stream
            // buffers from the moment it's ready; we keep it only if the hold is
            // confirmed, else discard it (a quick tap). cpal streams are `!Send`,
            // so the open must run here on the pipeline thread. Trade-off: a quick
            // Ctrl tap (e.g. Ctrl+click) now briefly opens+closes the mic, so the
            // macOS mic indicator blinks — harmless on a built-in/wired mic; on a
            // Bluetooth mic it also blips the audio mode, so prefer a local mic.
            let cap = match crate::audio::capture::AudioCapture::start(&device) {
                Ok(c) => c,
                Err(e) => {
                    warn!("Capture failed: {e}");
                    crate::notify::app(
                        "Couldn't start recording — check your microphone is connected.",
                    );
                    return;
                }
            };
            // Hold-delay gate, with the mic already capturing. A release within
            // hold_delay is a quick tap → discard the pre-rolled audio and close.
            if hold_gate(rx, self_tx, delay) {
                debug!("Quick tap — discarding pre-roll");
                drop(cap); // stops + closes the stream
                return;
            }
            // Confirmed hold — commit. Show the pill + turn the icon citron only
            // now, so a quick tap / Ctrl-click never flashes the pill.
            notify_ui(ui_tx, Event::ShowOverlay);
            *capture = Some(cap);
            *recording = true;
            // Arm the mid-recording dead-mic check: if this mic turns out to be
            // flatline we swap it while the user is still speaking (see the
            // MicHealthCheck arm), salvaging the dictation instead of losing it.
            *mic_check_gen += 1;
            schedule_mic_check(self_tx, *mic_check_gen, MIC_CHECK_DELAY);
            notify_ui(ui_tx, Event::StateChanged(State::Recording));
            // Harvest on-screen names off the pipeline thread so the AX reads
            // can't add latency between key-up and transcription (#7).
            let lang = language.clone();
            std::thread::spawn(move || crate::dictionary::update_session_context(&lang));
            info!("Recording…");
        }

        Event::HotkeyUp => {
            // A quick-tap HotkeyUp is consumed by the hold_delay gate in
            // the HotkeyDown arm; if we receive one here it means hold was
            // confirmed and the mic is open.
            if !*recording {
                return;
            }
            *recording = false;
            notify_ui(ui_tx, Event::StateChanged(State::Processing));
            stop_and_transcribe(config, capture, ui_tx);
            notify_ui(ui_tx, Event::StateChanged(State::Idle));
        }

        Event::HotkeyToggle => {
            if !*recording {
                if !crate::license::gate() {
                    return;
                }
                let (configured, language) = {
                    let c = config.lock_safe();
                    (c.input_device.clone(), c.language.clone())
                };
                let device = crate::audio::effective_input_device(&configured);
                // Pill up before the (synchronous) mic open, like the hold path.
                notify_ui(ui_tx, Event::ShowOverlay);
                match crate::audio::capture::AudioCapture::start(&device) {
                    Ok(cap) => {
                        *capture = Some(cap);
                        *recording = true;
                        // Same mid-recording dead-mic rescue as the hold path.
                        *mic_check_gen += 1;
                        schedule_mic_check(self_tx, *mic_check_gen, MIC_CHECK_DELAY);
                        notify_ui(ui_tx, Event::StateChanged(State::Recording));
                        if config.lock_safe().sound_feedback {
                            crate::audio::playback::play_sound("start");
                        }
                        // Harvest on-screen names off-thread (#7), as in hold mode.
                        std::thread::spawn(move || {
                            crate::dictionary::update_session_context(&language)
                        });
                        info!("Recording (toggle)...");
                    }
                    Err(e) => {
                        warn!("Capture failed: {e}");
                        notify_ui(ui_tx, Event::HideOverlay);
                        crate::notify::app(
                            "Couldn't start recording — check your microphone is connected.",
                        );
                    }
                }
            } else {
                *recording = false;
                notify_ui(ui_tx, Event::StateChanged(State::Processing));
                stop_and_transcribe(config, capture, ui_tx);
                notify_ui(ui_tx, Event::StateChanged(State::Idle));
            }
        }

        Event::LoadModel(model_name) => {
            let start = std::time::Instant::now();
            info!("Loading model '{model_name}' on pipeline thread...");

            // Tell the UI we're loading (icon changes, hotkeys ignored)
            notify_ui(ui_tx, Event::StateChanged(State::Loading));

            // Unload all backends
            crate::transcribe::unload_model();
            crate::transcribe::parakeet::unload_model();
            crate::transcribe::voxtral_local::unload_model();

            let backend = crate::model_manager::resolve_backend(&model_name);

            // Check if this specific model needs downloading and notify user.
            if let Some(info) = crate::model_manager::find_model(&model_name) {
                if !info.is_downloaded {
                    crate::notify::app(&format!(
                        "Downloading {} (~{}MB)... This may take a few minutes.",
                        info.label, info.size_mb
                    ));
                }
            }

            // One dispatch table — the Backend enum, via `ensure_loaded` — so a
            // new backend is handled in a single place (the CLI uses the same
            // path). Voxtral is the lone exception: loaded eagerly here to
            // pre-warm on switch (ensure_loaded leaves it lazy), and it MUST load
            // on this pipeline thread (WGPU/Metal thread-affinity), which is where
            // we already are.
            let load_result = match backend {
                crate::transcribe::Backend::VoxtralLocal => {
                    let dir = crate::config::voxtral_dir();
                    crate::transcribe::voxtral_local::load_model(dir.to_str().unwrap_or(""))
                }
                ref b => crate::transcribe::ensure_loaded(b, &model_name),
            };

            let elapsed = start.elapsed();
            let name = backend.name();
            match load_result {
                Ok(()) => {
                    info!("{name} model loaded ({:.1}s)", elapsed.as_secs_f64());
                    crate::notify::app(&format!("{name} ready! ({:.0}s)", elapsed.as_secs_f64()));
                }
                Err(e) => {
                    tracing::error!("{name} load failed: {e}");
                    crate::notify::app(&format!("Failed to load {name}: {e}"));
                }
            }

            // Back to idle — hotkeys work again
            notify_ui(ui_tx, Event::StateChanged(State::Idle));
        }

        Event::MicHealthCheck(generation) => {
            // Stale (a newer recording started) or the recording already ended.
            if generation != *mic_check_gen || !*recording {
                return;
            }
            let Some(cap) = capture.as_ref() else { return };
            if cap.live_len() < MIC_CHECK_MIN_SAMPLES {
                // Too little audio to judge the peak — Bluetooth mics can be
                // slow to deliver first samples. Look again shortly, same gen.
                schedule_mic_check(self_tx, generation, MIC_CHECK_RETRY_DELAY);
                return;
            }
            if cap.live_peak() >= crate::audio::DEAD_MIC_PEAK {
                return; // signal present — the mic is healthy
            }
            // Dead mid-recording: swap to a verified-working mic NOW so the rest
            // of the utterance is salvaged instead of lost. cpal streams are
            // `!Send`, so the swap must happen here on the pipeline thread.
            let dead = cap.device_name().to_string();
            warn!("No signal from '{dead}' mid-recording — looking for a working mic");
            match crate::audio::best_working_mic(&dead) {
                Some(next) => match crate::audio::capture::AudioCapture::start(&next) {
                    Ok(new_cap) => {
                        // Replacing the capture drops the old one → its stream closes.
                        *capture = Some(new_cap);
                        crate::audio::set_input_override(&next);
                        info!("Mid-recording input switch: '{dead}' → '{next}'");
                        crate::notify::app(&format!(
                            "“{dead}” went silent — now recording on “{next}”. \
                             Please restart your sentence."
                        ));
                        notify_ui(ui_tx, Event::InputSwitched(next));
                        // Watch the replacement too (fresh generation).
                        *mic_check_gen += 1;
                        schedule_mic_check(self_tx, *mic_check_gen, MIC_CHECK_DELAY);
                    }
                    // Couldn't open the replacement / none available: keep the
                    // current stream; the post-hoc path in stop_and_transcribe
                    // still recovers at key-up.
                    Err(e) => warn!("Couldn't switch to '{next}' mid-recording: {e}"),
                },
                None => warn!("No replacement mic for dead '{dead}' — deferring to key-up"),
            }
        }

        _ => {}
    }
}

/// Icon updates are **debounced**: we push to the status item only once the
/// state has stopped changing for `TRAY_DEBOUNCE`. This (a) protects the macOS
/// ControlCenter status-item XPC, which aborts the whole process (SIGABRT in
/// `_xpc_serializer_pack`, queue `com.apple.controlcenter.statusitems`) when
/// flooded with rapid updates, and (b) means a state that flaps quickly — e.g.
/// after a long uptime + wake-from-sleep — never flickers the menu-bar icon; it
/// just settles on the final state. The menu-bar icon is a secondary cue (the
/// pill + sounds are instant), so the small settle delay is unnoticeable.
const TRAY_DEBOUNCE: Duration = Duration::from_millis(140);

struct TrayThrottle {
    desired: Option<State>,     // most recently requested state
    pushed: Option<State>,      // what's currently on the status item
    settle_at: Option<Instant>, // when `desired` is stable enough to push
    scheduled: bool,            // a flush is already pending
}
static TRAY_THROTTLE: Mutex<TrayThrottle> = Mutex::new(TrayThrottle {
    desired: None,
    pushed: None,
    settle_at: None,
    scheduled: false,
});
/// Event sender used to fire the debounced flush on the main thread (set once in
/// `run`). The push itself always happens on the main thread.
static TRAY_TX: OnceLock<Sender<Event>> = OnceLock::new();

/// Sender into the pipeline thread's own channel (set once when the pipeline is
/// created). Lets the state watchdog inject a `HotkeyUp` to end a recording that
/// got stranded by a dropped key-up — the watchdog itself is spawned before the
/// pipeline exists, so it can't be handed the sender directly.
static PIPELINE_TX: OnceLock<Sender<Event>> = OnceLock::new();

/// Post an event to the UI (main) thread from anywhere — e.g. the notification
/// delegate asking for the license window. Dropped silently before `run`.
pub fn post(event: Event) {
    if let Some(tx) = TRAY_TX.get() {
        let _ = tx.send(event);
    }
}

fn schedule_tray_flush(after: Duration) {
    if let Some(tx) = TRAY_TX.get() {
        let tx = tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let _ = tx.send(Event::RefreshTrayIcon);
        });
    }
}

fn set_tray_icon(tray: &Option<TrayIcon>, state: State) {
    let _ = tray; // the actual push happens in flush_tray_icon (debounced)
    let mut t = TRAY_THROTTLE.lock_safe();
    if t.desired == Some(state) {
        return; // already heading there
    }
    t.desired = Some(state);
    t.settle_at = Some(Instant::now() + TRAY_DEBOUNCE);
    if !t.scheduled {
        t.scheduled = true;
        schedule_tray_flush(TRAY_DEBOUNCE);
    }
}

/// Debounced flush (main thread): push `desired` only once it has been stable
/// for the debounce window; otherwise wait out the remaining time.
fn flush_tray_icon(tray: &Option<TrayIcon>) {
    let (push, reschedule) = {
        let mut t = TRAY_THROTTLE.lock_safe();
        match t.settle_at {
            Some(dl) => {
                let now = Instant::now();
                if now >= dl {
                    t.scheduled = false;
                    let d = t.desired;
                    if t.pushed != d {
                        t.pushed = d;
                        (d, None)
                    } else {
                        (None, None)
                    }
                } else {
                    (None, Some(dl - now)) // changed again mid-window → wait more
                }
            }
            None => {
                t.scheduled = false;
                (None, None)
            }
        }
    };
    if let Some(state) = push {
        set_tray_icon_now(tray, state);
    }
    if let Some(wait) = reschedule {
        schedule_tray_flush(wait);
    }
}

fn set_tray_icon_now(tray: &Option<TrayIcon>, state: State) {
    // One glyph, four states — only colour/opacity change (see `glyph_icon`),
    // so the icon never shifts size or shape. All states except Recording are
    // macOS templates (monochrome, auto-adapt: white on a dark menu bar, black
    // on a light one), so they stay visible on any background:
    //   - Idle                 → crisp template          (ready)
    //   - Loading / Processing → dimmed template (~43%)   (busy, not ready yet)
    //   - Recording            → signal citron #CEDC00    (user speaking)
    // The tooltip echoes the state in plain text on hover.
    let (style, is_template, tooltip) = match state {
        State::Loading => (
            GlyphStyle::Template(BUSY_OPACITY),
            true,
            "Whisper Push: Loading model\u{2026}",
        ),
        State::Processing => (
            GlyphStyle::Template(BUSY_OPACITY),
            true,
            "Whisper Push: Transcribing\u{2026}",
        ),
        State::Recording => (
            GlyphStyle::Tint(TINT_RECORDING),
            false,
            "Whisper Push: Recording",
        ),
        State::Idle => (GlyphStyle::Template(255), true, "Whisper Push: Ready"),
    };
    if let Some(tray) = tray {
        if let Some(icon) = glyph_icon(style) {
            // Set the image AND its template flag atomically. macOS renders a
            // template image (the glyph is pure black + alpha) in the menu bar's
            // contrasting label colour automatically — black on a light bar,
            // white on a dark one — exactly like native menu-bar icons. Doing it
            // in one call avoids the stale-flag bug of `set_icon` followed by a
            // separate `set_icon_as_template` (where switching to the coloured
            // Recording icon left the template state inconsistent). Recording
            // passes `is_template = false` so it keeps its citron colour.
            #[cfg(target_os = "macos")]
            let _ = tray.set_icon_with_as_template(Some(icon), is_template);
            #[cfg(not(target_os = "macos"))]
            {
                let _ = is_template; // template is a macOS-only concept
                let _ = tray.set_icon(Some(icon));
            }
        }
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

/// The "every mic is dead" notification fires at most once per silent stretch —
/// set when shown, re-armed by the next recording with real signal (see
/// `stop_and_transcribe`) so a fixed permission doesn't leave it muted forever.
static SYSTEMIC_MIC_NOTIFIED: AtomicBool = AtomicBool::new(false);

/// Every usable input has failed — almost always a denied Microphone permission,
/// not N broken devices in a row. Point the user at the fix.
fn notify_systemic_mic_failure() {
    if SYSTEMIC_MIC_NOTIFIED.swap(true, Ordering::Relaxed) {
        return; // already told this silent stretch
    }
    let body = "No microphone is delivering any audio — this usually means Whisper Push \
                is missing the Microphone permission. Open Settings and enable it, then dictate again.";
    #[cfg(target_os = "macos")]
    crate::notify::app_action(body, "Open Settings", || {
        crate::permissions::open_settings_for(crate::permissions::PermKind::Microphone)
    });
    #[cfg(not(target_os = "macos"))]
    crate::notify::app(body);
}

/// Dead-mic recovery choke point: find a verified replacement for `dead`, make
/// it the session input, retitle the Input submenu, and recap to the user —
/// `what_happened` already names the device and duration (e.g. `No sound from
/// “X” (2.3 s recorded)`). Escalates to the systemic notification when no
/// candidate exists. Probes devices (~1–2 s worst case) — run it off the
/// pipeline thread when a transcription is waiting.
fn recover_dead_mic(what_happened: &str, dead: &str, ui_tx: &Sender<Event>) {
    match crate::audio::best_working_mic(dead) {
        Some(next) => {
            crate::audio::set_input_override(&next);
            warn!("Input auto-switch: '{dead}' → '{next}'");
            crate::notify::app(&format!(
                "{what_happened} — switched to “{next}”. Press your key and dictate again."
            ));
            notify_ui(ui_tx, Event::InputSwitched(next));
        }
        None => notify_systemic_mic_failure(),
    }
}

/// Stop capture, transcribe audio, and paste result.
fn stop_and_transcribe(
    config: &Arc<Mutex<Config>>,
    capture: &mut Option<crate::audio::capture::AudioCapture>,
    ui_tx: &Sender<Event>,
) {
    let cfg = config.lock_safe().clone();
    if cfg.sound_feedback {
        crate::audio::playback::play_sound("stop");
    }

    let cap = capture.take();
    // Did the device drop out mid-recording (AirPods/Bluetooth)? Check before we
    // consume the capture, so we can tell the user instead of failing silently.
    let device_lost = cap.as_ref().is_some_and(|c| c.device_lost());
    let used_device = cap.as_ref().map(|c| c.device_name().to_string());
    let audio = cap.map(|c| c.stop()).unwrap_or_default();
    let secs = audio.len() as f32 / crate::audio::SAMPLE_RATE as f32;

    if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
        if device_lost {
            crate::notify::app("Recording stopped — the microphone disconnected.");
            // Line up a verified replacement in the background so the NEXT press
            // just works. Probing takes ~1–2 s — never on this thread, where the
            // user may already be re-pressing the key.
            if let Some(dead) = used_device {
                let ui_tx = ui_tx.clone();
                std::thread::spawn(move || {
                    recover_dead_mic(
                        &format!("“{dead}” disconnected ({secs:.1} s captured)"),
                        &dead,
                        &ui_tx,
                    )
                });
            }
        }
        info!("Too short, skipping");
        return;
    }

    // Auto-fallback: enough audio was captured but it's flatline silence — the
    // signature of a connected-but-not-working mic (AirPods whose mic link never
    // opened, a muted USB interface), not a quiet room, which always has some
    // ambient peak. This utterance is unrecoverable (the mid-recording health
    // check only rescues holds longer than its delay), but switch the live input
    // to a probe-verified mic so the next press just works.
    let peak = audio.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak < crate::audio::DEAD_MIC_PEAK {
        if let Some(dead) = used_device.as_deref() {
            warn!("No signal from '{dead}' (peak={peak:.6})");
            recover_dead_mic(
                &format!("No sound from “{dead}” ({secs:.1} s recorded)"),
                dead,
                ui_tx,
            );
        } else {
            notify_systemic_mic_failure();
        }
        return; // nothing to transcribe — the audio is flatline
    }
    // Good signal — forget any earlier dead-mic memory so devices that recover
    // become eligible again, and re-arm the systemic notification.
    crate::audio::clear_dead_mics();
    SYSTEMIC_MIC_NOTIFIED.store(false, Ordering::Relaxed);

    let rms = crate::util::rms(&audio);
    let backend = crate::model_manager::resolve_backend(&cfg.model);
    info!(
        "Processing {:.1}s of audio with backend '{}' (RMS={:.4})...",
        audio.len() as f32 / crate::audio::SAMPLE_RATE as f32,
        backend.name(),
        rms
    );

    // (The session-context harvest — focused field / selection / clipboard — now
    // runs on a detached thread at record-start, so its AX reads never sit
    // between key-up and transcription. See the HotkeyDown / HotkeyToggle arms.)

    let start = std::time::Instant::now();
    // Panics are already caught inside transcribe_with_backend (the choke point)
    // and returned as Err, so no extra catch_unwind is needed here.
    let result = crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend);
    // Did we actually produce text? Drives the wording of the device-lost recap
    // below, so it never claims "transcribed" when nothing came out.
    let mut transcribed = false;
    match result {
        Ok(text) if !text.is_empty() => {
            transcribed = true;
            // Record the run so the user can find/re-copy it (History submenu +
            // history.txt). Records what was *recognised*, not the expansion.
            crate::history::record(&text);
            // If the whole dictation matches a template trigger, paste its
            // expansion instead (e.g. say "signature" → paste your signature).
            let to_paste = crate::templates::expand(&text).unwrap_or_else(|| text.clone());
            info!(
                "Pasting ({:.2}s): '{}'",
                start.elapsed().as_secs_f64(),
                // char-based, not byte-based: `&s[..80]` panics if byte 80
                // lands mid-codepoint (French accents, CJK — all in scope).
                to_paste.chars().take(80).collect::<String>()
            );
            if let Err(e) = crate::paste::paste_text(&to_paste) {
                tracing::error!("Paste failed: {e}");
            }
            // No per-dictation notification (noise).
        }
        Ok(_) => {
            info!("No speech detected");
            // Empty text from a *quiet* recording is worth explaining; a decent
            // RMS means the user likely just said nothing — stay silent then.
            // (When the device dropped, the disconnect recap below covers it.)
            if !device_lost
                && rms < crate::audio::LOW_SIGNAL_RMS
                && let Some(dev) = used_device.as_deref()
            {
                crate::notify::app(&format!(
                    "Heard {secs:.1} s from “{dev}” but it was too quiet to transcribe — \
                     speak closer to the mic or pick another one in the menu."
                ));
            }
        }
        Err(e) => {
            tracing::error!("Transcription: {e}");
            crate::notify::app(&format!("Error: {e}"));
        }
    }

    // The mic died partway through: recap what actually happened so a truncated
    // (or missing) paste isn't a mystery. Word it by whether text came out.
    if device_lost && let Some(dev) = used_device.as_deref() {
        crate::notify::app(&if transcribed {
            format!(
                "“{dev}” disconnected mid-dictation — transcribed the {secs:.1} s \
                 captured before the drop."
            )
        } else {
            format!(
                "“{dev}” disconnected mid-dictation — too little was captured to \
                 transcribe. Reconnect it or pick another mic in the menu."
            )
        });
    }
}

/// Open a file with the OS default handler.
fn open_path(path: &std::path::Path) {
    crate::util::open_external(path);
}

/// Build the recent-dictation entries for the History submenu. Returns
/// (item, full text) so a click can copy the full (possibly multi-line) text;
/// the label is a one-line preview. A disabled placeholder (empty text) shows
/// when there's no history yet.
fn populate_history_entries(submenu: &Submenu) -> Vec<(MenuItem, String)> {
    const MAX: usize = 12;
    const PREVIEW: usize = 48;
    // Entries live between the header (index 0) and the trailing separator +
    // actions, so they're inserted right after the header — refresh removes the
    // old entries and re-inserts here without touching the stable items.
    let mut pos = 1;
    let recent = crate::history::recent();
    let mut items = Vec::new();
    if recent.is_empty() {
        let ph = MenuItem::new("  (empty: your dictations will appear here)", false, None);
        let _ = submenu.insert(&ph, pos);
        items.push((ph, String::new()));
        return items;
    }
    for text in recent.iter().take(MAX) {
        let mut preview = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if preview.chars().count() > PREVIEW {
            preview = format!(
                "{}\u{2026}",
                preview.chars().take(PREVIEW).collect::<String>()
            );
        }
        let it = MenuItem::new(&format!("  {preview}"), true, None);
        let _ = submenu.insert(&it, pos);
        pos += 1;
        items.push((it, text.clone()));
    }
    items
}

/// Build the trigger labels for the Templates submenu (disabled — the live
/// actions are Add / Open). Returns (item, trigger).
fn populate_template_items(submenu: &Submenu) -> Vec<(MenuItem, String)> {
    const MAX: usize = 30;
    let triggers = crate::templates::triggers();
    let mut items = Vec::new();
    if triggers.is_empty() {
        let ph = MenuItem::new("  (none yet: use Add Template\u{2026})", false, None);
        let _ = submenu.append(&ph);
        items.push((ph, String::new()));
        return items;
    }
    for t in triggers.iter().take(MAX) {
        // Enabled so a click opens the edit/delete dialog.
        let it = MenuItem::new(&format!("  \u{201c}{t}\u{201d}"), true, None);
        let _ = submenu.append(&it);
        items.push((it, t.clone()));
    }
    if triggers.len() > MAX {
        let more = MenuItem::new(
            &format!("  \u{2026} +{} more (use Open file)", triggers.len() - MAX),
            false,
            None,
        );
        let _ = submenu.append(&more);
        items.push((more, String::new()));
    }
    items
}

/// "Add Template…" dialog: ask for the trigger, then the content, then save.
fn add_template_dialog() {
    let Some(trigger) = crate::dialog::text_input(
        "Add a template \u{2014} the word/phrase you'll say to paste it. \
         (For long or multi-line content, edit templates.toml instead.)",
        "",
    ) else {
        return;
    };
    let trigger = trigger.trim().to_string();
    if trigger.is_empty() {
        return;
    }
    let Some(content) = crate::dialog::text_input(
        &format!("Text to paste when you say \u{201c}{trigger}\u{201d}:"),
        "",
    ) else {
        return;
    };
    match crate::templates::add(&trigger, &content) {
        Ok(()) => crate::notify::app(&format!("Template \u{201c}{trigger}\u{201d} saved.")),
        Err(e) => crate::notify::app(&format!("Couldn't save template: {e}")),
    }
}

/// Per-template menu click → Edit (open the file for full multi-line/formatting
/// control) or Delete.
fn template_action_dialog(trigger: &str) {
    match crate::dialog::choice(
        &format!("Template \u{201c}{trigger}\u{201d}"),
        &["Cancel", "Edit", "Delete"],
    )
    .as_deref()
    {
        Some("Delete") => {
            if crate::templates::remove(trigger) {
                crate::notify::app(&format!("Deleted template \u{201c}{trigger}\u{201d}."));
            }
        }
        // "Edit" (and any non-Delete) opens the file — multi-line content with
        // the user's own formatting is edited there (TOML triple-quotes).
        Some("Edit") => open_path(&crate::templates::ensure_file()),
        _ => {}
    }
}

/// Per-word dictionary menu click → Edit (open the file) or Delete.
fn dict_action_dialog(term: &str, tx: crossbeam_channel::Sender<Event>) {
    match crate::dialog::choice(
        &format!("Dictionary word \u{201c}{term}\u{201d}"),
        &["Cancel", "Edit", "Delete"],
    )
    .as_deref()
    {
        Some("Delete") => {
            if let Ok(true) = crate::dictionary::remove_entry(term) {
                crate::notify::app(&format!("Removed \u{201c}{term}\u{201d} from dictionary"));
                let _ = tx.send(Event::DictChanged);
            }
        }
        Some("Edit") => open_path(&crate::dictionary::ensure_file()),
        _ => {}
    }
}

/// shown when the dictionary is empty or truncated.
fn populate_dict_entries(submenu: &Submenu) -> Vec<(MenuItem, String)> {
    const MAX: usize = 40;
    let entries = crate::dictionary::list_entries();
    let mut items = Vec::new();
    if entries.is_empty() {
        let ph = MenuItem::new("  (empty: your corrections will appear here)", false, None);
        let _ = submenu.append(&ph);
        items.push((ph, String::new()));
        return items;
    }
    for e in entries.iter().take(MAX) {
        let star = if e.starred { "\u{2605} " } else { "" };
        let label = if e.variants.is_empty() {
            format!("  {star}{}", e.term)
        } else {
            format!("  {star}{}  \u{2190}  {}", e.term, e.variants.join(", "))
        };
        let it = MenuItem::new(&label, true, None);
        let _ = submenu.append(&it);
        items.push((it, e.term.clone()));
    }
    if entries.len() > MAX {
        let more = MenuItem::new(
            &format!("  \u{2026} +{} more (use Open file)", entries.len() - MAX),
            false,
            None,
        );
        let _ = submenu.append(&more);
        items.push((more, String::new()));
    }
    items
}

/// Native dialog prefilled with the last dictation; on Save, learn from the
/// user's correction. Runs on its own thread (the dialog blocks on input).
fn correct_last_dialog(tx: crossbeam_channel::Sender<Event>) {
    let Some(last) = crate::dictionary::last_dictation() else {
        crate::notify::app("No recent dictation to correct.");
        return;
    };
    let corrected = match crate::dialog::text_input(
        "Edit the last dictation — fix any wrong words:",
        &last.finalized,
    ) {
        Some(t) if !t.trim().is_empty() => t,
        _ => return, // cancelled or empty
    };
    if corrected.trim() == last.finalized.trim() {
        return; // nothing changed
    }
    use crate::dictionary::Correction;
    match crate::dictionary::correct_last(&corrected) {
        Correction::Done(report) => {
            // Learn the SOUND of each corrected word too (acoustic dictionary),
            // so it's recovered next time whatever the model's spelling; and
            // (if opted in) check its canonical spelling online.
            for (heard, term) in &report.learned {
                crate::acoustic::learn_word(heard, term);
                crate::enrich::maybe_suggest(term, &last.lang);
            }
            let msg = if !report.learned.is_empty() {
                let pairs: Vec<String> = report
                    .learned
                    .iter()
                    .map(|(h, t)| format!("\u{201c}{h}\u{201d} \u{2192} \u{201c}{t}\u{201d}"))
                    .collect();
                format!("Learned {}", pairs.join(", "))
            } else if !report.demoted.is_empty() {
                format!("Unlearned {}", report.demoted.join(", "))
            } else {
                "Noted — nothing to learn (rewrite / everyday words ignored).".to_string()
            };
            crate::notify::app(&msg);
            let _ = tx.send(Event::DictChanged);
        }
        Correction::NoLast => crate::notify::app("Nothing to correct yet."),
        Correction::NotReady => crate::notify::app("Dictionary is off."),
        Correction::SaveError(e) => crate::notify::app(&format!("Save failed: {e}")),
    }
}

/// Native dialog to add a word manually: "Correct = heard1, heard2".
fn add_word_dialog(tx: crossbeam_channel::Sender<Event>) {
    let Some(input) = crate::dialog::text_input(
        "Add a word — just type the correct spelling; the app catches misheard \
         versions by sound. (Optional: Word = misheard1, misheard2)",
        "",
    ) else {
        return;
    };
    let input = input.trim();
    if input.is_empty() {
        return;
    }
    let (term, variants) = match input.split_once('=') {
        Some((t, vs)) => (
            t.trim().to_string(),
            vs.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>(),
        ),
        None => (input.to_string(), Vec::new()),
    };
    if term.is_empty() {
        return;
    }
    match crate::dictionary::add_entry(&term, &variants, false, None) {
        Ok(()) => {
            crate::notify::app(&format!("Added \u{201c}{term}\u{201d}"));
            let _ = tx.send(Event::DictChanged);
        }
        Err(e) => crate::notify::app(&format!("Add failed: {e}")),
    }
}

/// Two-step dialog (key, then email) → activate. Runs off the UI thread.
fn license_activate_dialog(tx: crossbeam_channel::Sender<Event>) {
    let Some(key) = crate::dialog::text_input("Enter your Whisper Push license key:", "") else {
        return;
    };
    if key.trim().is_empty() {
        return;
    }
    use crate::license::ActivateOutcome::*;
    let msg = match crate::license::activate(&key) {
        Activated => "License activated \u{2014} thank you!".to_string(),
        Rejected(r) => format!("Activation failed: {r}"),
        Offline => "Couldn't reach the license server. Check your connection and retry.".into(),
    };
    crate::notify::app(&msg);
    let _ = tx.send(Event::LicenseChanged);
}

/// Confirm, then free this device's slot. Runs off the UI thread.
fn license_deactivate_dialog(tx: crossbeam_channel::Sender<Event>) {
    if !crate::dialog::confirm(
        "Deactivate Whisper Push on this device? This frees one of your device slots; you can re-activate anytime.",
        "Deactivate",
    ) {
        return;
    }
    use crate::license::DeactivateOutcome::*;
    let msg = match crate::license::deactivate() {
        Done => "This device has been deactivated.".to_string(),
        Offline => {
            "Couldn't reach the server \u{2014} deactivate from your account page instead.".into()
        }
    };
    crate::notify::app(&msg);
    let _ = tx.send(Event::LicenseChanged);
}

/// Wipe local state after confirming, then quit. Runs off the UI thread (the
/// dialog blocks) — never on the winit main thread.
fn uninstall_dialog() {
    // Where the app itself lives differs per platform, and that is the part the
    // user has to finish by hand.
    let removal = if cfg!(target_os = "macos") {
        "Afterwards, drag Whisper Push out of Applications."
    } else if cfg!(target_os = "windows") {
        "Afterwards, remove Whisper Push from Settings \u{203a} Apps."
    } else {
        "Afterwards, remove the package (sudo apt remove whisper-push)."
    };
    if !crate::dialog::confirm(
        &format!(
            "Remove Whisper Push's data? This deletes your downloaded models, your \
             learned dictionary and this device's license activation. {removal}"
        ),
        "Remove data",
    ) {
        return;
    }
    // Free the server-side device slot before wiping local state.
    let _ = crate::license::deactivate();
    let data_dir = crate::config::data_dir();
    if data_dir.exists() {
        let _ = std::fs::remove_dir_all(&data_dir);
        info!("Removed data dir: {}", data_dir.display());
    }
    crate::autostart::disable();
    crate::notify::app(&format!("Whisper Push data removed. {removal}"));
    crate::util::exit_clean();
}

/// Render a binding for the menu. macOS writes modifiers as its own glyphs
/// (⌃⌥⇧⌘, which is what every Mac menu shows); Windows and Linux spell them out,
/// because ⌘ means nothing on a keyboard whose key says "Ctrl".
pub fn format_hotkey_display(hotkey: &str, mode: &str) -> String {
    #[cfg(not(target_os = "macos"))]
    let symbols: &[(&str, &str)] = &[
        ("cmd", "Win"),
        ("shift", "Shift"),
        ("alt", "Alt"),
        ("ctrl", "Ctrl"),
        ("rctrl", "Right Ctrl"),
        ("rcmd", "Right Win"),
        ("ralt", "Right Alt"),
        ("rshift", "Right Shift"),
        ("lctrl", "Left Ctrl"),
        ("lcmd", "Left Win"),
        ("lalt", "Left Alt"),
        ("lshift", "Left Shift"),
        ("space", "Space"),
    ];
    #[cfg(target_os = "macos")]
    let symbols: &[(&str, &str)] = &[
        ("cmd", "\u{2318}"),
        ("shift", "\u{21e7}"),
        ("alt", "\u{2325}"),
        ("ctrl", "\u{2303}"),
        ("rctrl", "\u{2303}R"),
        ("rcmd", "\u{2318}R"),
        ("ralt", "\u{2325}R"),
        ("rshift", "\u{21e7}R"),
        ("lctrl", "\u{2303}L"),
        ("lcmd", "\u{2318}L"),
        ("lalt", "\u{2325}L"),
        ("lshift", "\u{21e7}L"),
        ("space", "Space"),
    ];
    let mut r = if mode == "hold" {
        "Hold ".into()
    } else {
        String::new()
    };
    for (i, p) in hotkey.to_lowercase().split('+').enumerate() {
        let p = p.trim();
        if i > 0 {
            r.push('+');
        }
        if let Some((_, s)) = symbols.iter().find(|(k, _)| *k == p) {
            r.push_str(s);
        } else {
            r.push_str(&p.to_uppercase());
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows/Linux tray icon must carry its own background: a template
    /// (black + alpha) glyph is invisible on the default dark Windows taskbar,
    /// which is exactly the bug this badge exists to fix. Assert the real
    /// composed image: opaque brand ground in the middle, corners rounded away,
    /// and citron ink somewhere inside.
    #[test]
    fn badge_icon_is_opaque_with_rounded_corners() {
        let img = badge_image(GlyphStyle::Template(255)).expect("badge builds");
        let size = img.width();
        assert_eq!(img.height(), size, "the badge is square");
        assert_eq!(
            img.get_pixel(size / 2, size / 2)[3],
            255,
            "the centre of the badge must be fully opaque"
        );
        assert_eq!(
            img.get_pixel(0, 0)[3],
            0,
            "the corners must be rounded away"
        );
        // The ground is racing green…
        assert_eq!(
            [img.get_pixel(size / 2, 2)[0], img.get_pixel(size / 2, 2)[1]],
            [BRAND_GREEN[0], BRAND_GREEN[1]],
            "the ground must be the brand green"
        );
        // …and the waves are citron: at least one pixel is clearly yellower.
        let inked = img
            .pixels()
            .any(|p| p[1] > BRAND_GREEN[1] + 60 && p[3] > 200);
        assert!(inked, "the glyph must be drawn on the badge");
    }

    /// Recording inverts the badge (citron ground) so "live" reads at 16 px.
    #[test]
    fn recording_badge_inverts() {
        let img = badge_image(GlyphStyle::Tint(TINT_RECORDING)).expect("badge builds");
        let size = img.width();
        let ground = img.get_pixel(size / 2, 2);
        assert_eq!(
            [ground[0], ground[1], ground[2]],
            TINT_RECORDING,
            "recording paints the citron ground"
        );
    }

    /// Every state must produce an icon of the SAME size, so the tray item never
    /// resizes as the app moves between idle / busy / recording.
    #[test]
    fn every_state_icon_has_one_size() {
        for style in [
            GlyphStyle::Template(255),
            GlyphStyle::Template(BUSY_OPACITY),
            GlyphStyle::Tint(TINT_RECORDING),
        ] {
            assert!(glyph_icon(style).is_some());
        }
    }
}
