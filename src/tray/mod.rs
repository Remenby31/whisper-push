use crate::config::Config;
use crate::state::{AppState, Event, State};
use crate::util::LockSafe;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
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

/// Build a tray icon from the one master glyph, applying `style`. The geometry
/// is always identical — only colour/opacity change — so the icon never shifts
/// size or shape between states.
fn glyph_icon(style: GlyphStyle) -> Option<Icon> {
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

/// Submenu title showing the current device selection, e.g. "Input: Auto".
fn device_title(label: &str, value: &str) -> String {
    if value == "auto" {
        format!("{label}: Auto")
    } else {
        format!("{label}: {value}")
    }
}

// Built-in hotkey presets, per platform. The toggle entries are key *combos*
// (`cmd+shift+space`), which only the macOS listener parses today — the Linux
// (evdev) and Windows (WH_KEYBOARD_LL) parsers accept a single token, so a combo
// preset there would make `start_listener` fail and silently kill dictation.
// Until the non-macOS parsers learn combos (#6 full), those platforms get
// single-key presets only.
#[cfg(target_os = "macos")]
const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold \u{2014} Control", "ctrl", "hold"),
    ("Hold \u{2014} Right Control", "rctrl", "hold"),
    ("Hold \u{2014} Right Command", "rcmd", "hold"),
    ("Hold \u{2014} Right Option", "ralt", "hold"),
    (
        "Toggle \u{2014} \u{2318}\u{21e7}Space",
        "cmd+shift+space",
        "toggle",
    ),
    (
        "Toggle \u{2014} \u{2303}\u{21e7}Space",
        "ctrl+shift+space",
        "toggle",
    ),
];
#[cfg(not(target_os = "macos"))]
const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold \u{2014} Control", "ctrl", "hold"),
    ("Hold \u{2014} Right Control", "rctrl", "hold"),
    ("Hold \u{2014} Right Alt", "ralt", "hold"),
    ("Hold \u{2014} Right Super", "rcmd", "hold"),
];

/// User events forwarded into winit's event loop.
#[derive(Debug)]
#[allow(dead_code)]
enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    App(Event),
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
}

struct MenuItems {
    status_item: MenuItem,
    toggle_item: MenuItem,
    #[allow(dead_code)]
    notifications_item: CheckMenuItem,
    #[allow(dead_code)]
    sound_item: CheckMenuItem,
    #[allow(dead_code)]
    debug_item: CheckMenuItem,
    toggle_id: String,
    quit_id: String,
    notif_id: String,
    sound_id: String,
    debug_id: String,
    test_id: String,
    uninstall_id: String,
    update_item: MenuItem,
    update_id: String,
    #[allow(dead_code)]
    report_item: MenuItem,
    report_id: String,
    hotkey_ids: Vec<(String, String, String)>,
    hotkey_items: Vec<(CheckMenuItem, String, String)>,
    hotkey_submenu: Submenu,
    custom_hotkey_id: String,
    input_ids: Vec<(String, String)>,
    input_device_items: Vec<(CheckMenuItem, String)>,
    input_submenu: Submenu,
    output_ids: Vec<(String, String)>,
    output_device_items: Vec<(CheckMenuItem, String)>,
    output_submenu: Submenu,
    mic_perm_item: MenuItem,
    acc_perm_item: MenuItem,
    input_mon_perm_item: MenuItem,
    perms_submenu: Submenu,
    warn_item: Option<MenuItem>,
    mic_perm_id: String,
    acc_perm_id: String,
    input_mon_perm_id: String,
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
    // License (Lemon Squeezy)
    license_submenu: Submenu,
    license_status_item: MenuItem,
    license_subscription_id: String,
    license_deactivate_id: String,
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
        }
    }

    fn create_tray(&mut self) {
        let cfg = self.config.lock_safe().clone();

        // Build menu
        let is_ready = self.state.current() == State::Idle;
        let disp = format_hotkey_display(&cfg.hotkey, &cfg.hotkey_mode);
        let status_text = if is_ready {
            format!("Whisper Push ({disp})")
        } else {
            "Whisper Push \u{2014} \u{231b} Loading model\u{2026}".into()
        };
        let status_item = MenuItem::new(&status_text, false, None);
        let toggle_item = MenuItem::new(
            if is_ready {
                "Start Recording"
            } else {
                "\u{231b} Loading model\u{2026} (unavailable)"
            },
            is_ready,
            None,
        );

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
        // "Set Custom Hotkey…" relies on live key-combo capture, which today only
        // exists on macOS. Offer it there; elsewhere the presets cover it and the
        // item would be a dead end (no HotkeyCaptured ever arrives) — so hide it.
        #[cfg(target_os = "macos")]
        let custom_hotkey_id = {
            let _ = hotkey_submenu.append(&PredefinedMenuItem::separator());
            let custom_hotkey_item = MenuItem::new("Set Custom Hotkey\u{2026}", true, None);
            let _ = hotkey_submenu.append(&custom_hotkey_item);
            custom_hotkey_item.id().0.clone()
        };
        #[cfg(not(target_os = "macos"))]
        let custom_hotkey_id = String::new();

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
        if perms.microphone == crate::permissions::PermState::Denied {
            let _ = input_submenu.append(&PredefinedMenuItem::separator());
            let _ = input_submenu.append(&MenuItem::new(
                "\u{26a0} Microphone denied \u{2014} grant to record",
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
        let test_item = MenuItem::new("Test (record 3s + transcribe)", true, None);
        let update_item = MenuItem::new("Check for Updates\u{2026}", true, None);
        let report_item = MenuItem::new("Report a Problem\u{2026}", true, None);
        let uninstall_item = MenuItem::new("Uninstall...", true, None);
        let quit_item = MenuItem::new("Quit Whisper Push", true, None);

        // Permissions (perms already computed above for the input picker gate)
        let mic_label = format!(
            "{} Microphone \u{2014} {}",
            perms.microphone.symbol(),
            perms.microphone.label()
        );
        let acc_label = format!(
            "{} Accessibility \u{2014} {}",
            perms.accessibility.symbol(),
            perms.accessibility.label()
        );
        let input_mon_label = format!(
            "{} Input Monitoring \u{2014} {}",
            perms.input_monitoring.symbol(),
            perms.input_monitoring.label()
        );
        let mic_perm_item = MenuItem::new(&mic_label, true, None);
        let acc_perm_item = MenuItem::new(&acc_label, true, None);
        let input_mon_perm_item = MenuItem::new(&input_mon_label, true, None);
        let perms_submenu = Submenu::new(
            if perms.all_granted() {
                "Permissions \u{2713}"
            } else {
                "\u{26a0} Permissions"
            },
            true,
        );
        let _ = perms_submenu.append(&mic_perm_item);
        let _ = perms_submenu.append(&acc_perm_item);
        let _ = perms_submenu.append(&input_mon_perm_item);
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
        let license_submenu = Submenu::new(&crate::license::submenu_title(), true);
        let license_status_item = MenuItem::new(&crate::license::status_text(), false, None);
        let license_subscription_item = MenuItem::new("Subscription\u{2026}", true, None);
        let license_deactivate_item = MenuItem::new("Deactivate this device\u{2026}", true, None);
        let _ = license_submenu.append(&license_status_item);
        let _ = license_submenu.append(&PredefinedMenuItem::separator());
        let _ = license_submenu.append(&license_subscription_item);
        let _ = license_submenu.append(&PredefinedMenuItem::separator());
        let _ = license_submenu.append(&license_deactivate_item);
        let license_subscription_id = license_subscription_item.id().0.clone();
        let license_deactivate_id = license_deactivate_item.id().0.clone();

        // Assemble — flat menu (submenus crash on macOS Tahoe)
        let menu = Menu::new();

        let _ = menu.append(&status_item);
        let warn_item = if !perms.all_granted() {
            let w = MenuItem::new(
                &format!("\u{26a0} {} permission(s) missing", perms.missing_count()),
                false,
                None,
            );
            let _ = menu.append(&w);
            Some(w)
        } else {
            None
        };
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&toggle_item);

        // Permissions submenu (always available; titled ✓ or ⚠).
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&perms_submenu);

        let _ = menu.append(&PredefinedMenuItem::separator());

        // Settings (dropdowns to keep the menu compact)
        let _ = menu.append(&hotkey_submenu);
        let _ = menu.append(&backend_submenu);
        let _ = menu.append(&input_submenu);
        let _ = menu.append(&output_submenu);
        let _ = menu.append(&dict_submenu);
        let _ = menu.append(&license_submenu);

        let _ = menu.append(&PredefinedMenuItem::separator());

        let _ = menu.append(&notifications_item);
        let _ = menu.append(&sound_item);

        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&test_item);
        let _ = menu.append(&update_item);
        let _ = menu.append(&report_item);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&uninstall_item);
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
            toggle_id: toggle_item.id().0.clone(),
            test_id: test_item.id().0.clone(),
            update_id: update_item.id().0.clone(),
            report_id: report_item.id().0.clone(),
            uninstall_id: uninstall_item.id().0.clone(),
            quit_id: quit_item.id().0.clone(),
            notif_id: notifications_item.id().0.clone(),
            sound_id: sound_item.id().0.clone(),
            debug_id: debug_item.id().0.clone(),
            mic_perm_id: mic_perm_item.id().0.clone(),
            acc_perm_id: acc_perm_item.id().0.clone(),
            input_mon_perm_id: input_mon_perm_item.id().0.clone(),
            setup_id: setup_item.id().0.clone(),
            mic_perm_item,
            acc_perm_item,
            input_mon_perm_item,
            perms_submenu,
            warn_item,
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
            license_subscription_id,
            license_deactivate_id,
            status_item,
            toggle_item,
            notifications_item,
            sound_item,
            debug_item,
            hotkey_ids,
            hotkey_items,
            hotkey_submenu,
            custom_hotkey_id,
            input_ids,
            input_device_items,
            input_submenu,
            output_ids,
            output_device_items,
            output_submenu,
        });

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
            }
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

    /// Refresh the License submenu title + status line (cheap, no rebuild).
    fn refresh_license_submenu(&mut self) {
        if let Some(mi) = self.menu_items.as_ref() {
            mi.license_status_item
                .set_text(crate::license::status_text());
            mi.license_submenu.set_text(crate::license::submenu_title());
        }
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
        let mi = match &self.menu_items {
            Some(m) => m,
            None => return,
        };

        match event {
            Event::ModelReady => {
                self.state.set(State::Idle);
                mi.toggle_item.set_text("Start Recording");
                mi.toggle_item.set_enabled(true);
                let disp = format_hotkey_display(
                    &self.state.config.hotkey,
                    &self.state.config.hotkey_mode,
                );
                mi.status_item.set_text(&format!("Whisper Push ({disp})"));
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
                    // Free the server-side device slot before wiping local state.
                    let _ = crate::license::deactivate();
                    // Uninstall: remove data dir, autostart, and notify
                    let data_dir = crate::config::data_dir();
                    if data_dir.exists() {
                        let _ = std::fs::remove_dir_all(&data_dir);
                        info!("Removed data dir: {}", data_dir.display());
                    }
                    crate::autostart::disable();
                    crate::notify::app("Uninstalled. You can delete the app from Applications.");
                    crate::util::exit_clean();
                }
                if id == &mi.toggle_id {
                    // Recording lives entirely on the pipeline thread (the single
                    // chokepoint, off the UI thread — AudioCapture calls into
                    // CoreAudio which can block for seconds on Bluetooth). The menu
                    // just forwards the toggle.
                    match &self.pipeline_tx {
                        Some(tx) => {
                            let _ = tx.send(Event::HotkeyToggle);
                        }
                        None => warn!("toggle clicked before pipeline was ready"),
                    }
                    return;
                }
                if id == &mi.test_id {
                    // Test: record 3 seconds + transcribe + show result
                    let cfg = self.config.lock_safe().clone();
                    std::thread::spawn(move || {
                        info!("=== TEST: Recording 3 seconds... ===");
                        crate::notify::app("Recording 3 seconds...");

                        match crate::audio::capture::AudioCapture::start(&cfg.input_device) {
                            Ok(cap) => {
                                std::thread::sleep(std::time::Duration::from_secs(3));
                                let audio = cap.stop();
                                let rms = crate::util::rms(&audio);
                                info!(
                                    "=== TEST: Captured {:.1}s, RMS={:.4} ===",
                                    audio.len() as f32 / crate::audio::SAMPLE_RATE as f32,
                                    rms
                                );

                                if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
                                    crate::notify::app("Test failed: audio too short");
                                    return;
                                }
                                if rms < crate::audio::capture::SILENCE_RMS_THRESHOLD {
                                    crate::notify::app(
                                        "Test failed: silence (check mic permission)",
                                    );
                                    return;
                                }

                                let backend = crate::model_manager::resolve_backend(&cfg.model);
                                info!("=== TEST: Transcribing with '{}' ===", backend.name());
                                crate::notify::app(&format!(
                                    "Transcribing with {}...",
                                    backend.name()
                                ));

                                let start = std::time::Instant::now();
                                match crate::transcribe::transcribe_with_backend(
                                    &audio,
                                    &cfg.language,
                                    &backend,
                                ) {
                                    Ok(text) if !text.is_empty() => {
                                        let elapsed = start.elapsed();
                                        info!(
                                            "=== TEST OK ({:.2}s): '{}' ===",
                                            elapsed.as_secs_f64(),
                                            text
                                        );
                                        crate::notify::app(&format!(
                                            "Test OK ({:.1}s): {}",
                                            elapsed.as_secs_f64(),
                                            text
                                        ));
                                    }
                                    Ok(_) => {
                                        info!("=== TEST: No speech detected ===");
                                        crate::notify::app("Test: no speech detected");
                                    }
                                    Err(e) => {
                                        info!("=== TEST ERROR: {e} ===");
                                        crate::notify::app(&format!("Test error: {e}"));
                                    }
                                }
                            }
                            Err(e) => {
                                info!("=== TEST: Capture failed: {e} ===");
                                crate::notify::app(&format!("Test failed: {e}"));
                            }
                        }
                    });
                    return;
                }
                if id == &mi.mic_perm_id {
                    #[cfg(target_os = "macos")]
                    crate::permissions::open_settings("Privacy_Microphone");
                    return;
                }
                if id == &mi.acc_perm_id {
                    #[cfg(target_os = "macos")]
                    crate::permissions::open_settings("Privacy_Accessibility");
                    return;
                }
                if id == &mi.input_mon_perm_id {
                    #[cfg(target_os = "macos")]
                    crate::permissions::open_settings("Privacy_ListenEvent");
                    return;
                }
                if id == &mi.setup_id {
                    crate::permissions::guided_setup(); // self-spawns + guards re-entry
                    return;
                }
                if id == &mi.update_id {
                    if let Some((version, url)) = self.pending_update.clone() {
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
                            .spawn(move || match crate::updater::check_for_update() {
                                Ok(Some((version, url))) => {
                                    let _ = tx.send(Event::UpdateAvailable(version, url));
                                }
                                Ok(None) => {
                                    crate::notify::app("You\u{2019}re on the latest version.");
                                    let _ = tx.send(Event::UpdateFailed(String::new()));
                                }
                                Err(e) => {
                                    tracing::error!("Update check failed: {e}");
                                    crate::notify::app(&format!("Update check failed: {e}"));
                                    let _ = tx.send(Event::UpdateFailed(e.to_string()));
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
                    let path = crate::dictionary::ensure_file();
                    let _ = std::process::Command::new("open").arg(&path).spawn();
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
                if id == &mi.license_subscription_id {
                    // Open the in-app payment / activation modal. Falls back to a
                    // text dialog in dev builds where the wizard isn't bundled.
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || {
                        if crate::onboarding::run_license_window() {
                            let _ = tx.send(Event::LicenseChanged);
                        } else {
                            license_activate_dialog(tx);
                        }
                    });
                    return;
                }
                if id == &mi.license_deactivate_id {
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || license_deactivate_dialog(tx));
                    return;
                }
                // Click a listed word to remove it from the dictionary.
                for (it, term) in &mi.dict_entry_items {
                    if !term.is_empty() && id == &it.id().0 {
                        if let Ok(true) = crate::dictionary::remove_entry(term) {
                            crate::notify::app(&format!(
                                "Removed \u{201c}{term}\u{201d} from dictionary"
                            ));
                        }
                        let _ = self.state.tx.send(Event::DictChanged);
                        return;
                    }
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
                        crate::hotkey::rebind(hotkey, mode); // live on macOS
                        // The live rebind only takes effect immediately on macOS;
                        // the Linux/Windows listeners read their key once at start,
                        // so be honest that a restart is needed there (#3).
                        #[cfg(target_os = "macos")]
                        crate::notify::app(&format!("Hotkey set to {disp}"));
                        #[cfg(not(target_os = "macos"))]
                        crate::notify::app(&format!(
                            "Hotkey saved ({disp}) \u{2014} restart Whisper Push to apply."
                        ));
                        return;
                    }
                }
                if id == &mi.custom_hotkey_id {
                    crate::hotkey::start_capture(self.state.tx.clone());
                    crate::notify::app(
                        "Press your shortcut now: tap a modifier (e.g. Right \u{2318}) to hold, or a combo like \u{2318}\u{21e7}D to toggle.",
                    );
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
                mi.toggle_item.set_text("Recording\u{2026}");
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
                // Tap already rebound the live listener; just sync the UI.
                for (item, hk, m) in &mi.hotkey_items {
                    item.set_checked(hk == &hotkey && m == &mode);
                }
                let disp = format_hotkey_display(&hotkey, &mode);
                mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                mi.hotkey_submenu.set_text(format!("Hotkey: {disp}"));
                crate::notify::app(&format!("Custom hotkey set: {disp}"));
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
                    let mic_label = format!(
                        "{} Microphone \u{2014} {}",
                        status.microphone.symbol(),
                        status.microphone.label()
                    );
                    let acc_label = format!(
                        "{} Accessibility \u{2014} {}",
                        status.accessibility.symbol(),
                        status.accessibility.label()
                    );
                    let input_mon_label = format!(
                        "{} Input Monitoring \u{2014} {}",
                        status.input_monitoring.symbol(),
                        status.input_monitoring.label()
                    );
                    mi.mic_perm_item.set_text(&mic_label);
                    mi.mic_perm_item
                        .set_enabled(status.microphone != crate::permissions::PermState::Granted);
                    mi.acc_perm_item.set_text(&acc_label);
                    mi.acc_perm_item.set_enabled(
                        status.accessibility != crate::permissions::PermState::Granted,
                    );
                    mi.input_mon_perm_item.set_text(&input_mon_label);
                    mi.input_mon_perm_item.set_enabled(
                        status.input_monitoring != crate::permissions::PermState::Granted,
                    );
                    mi.perms_submenu.set_text(if status.all_granted() {
                        "Permissions \u{2713}"
                    } else {
                        "\u{26a0} Permissions"
                    });
                    if let Some(ref w) = mi.warn_item {
                        if status.all_granted() {
                            w.set_text("\u{2713} All permissions granted");
                        } else {
                            w.set_text(&format!(
                                "\u{26a0} {} permission(s) missing",
                                status.missing_count()
                            ));
                        }
                    }
                }
                info!(
                    "Permissions refreshed: mic={:?} acc={:?}",
                    status.microphone, status.accessibility
                );
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
                if self.config.lock_safe().notifications {
                    crate::notify::app(&format!(
                        "Version {version} available! Click the menu to update."
                    ));
                }
                info!("Update available: v{version}");
            }

            Event::UpdateFailed(ref msg) => {
                mi.update_item.set_text("Check for Updates\u{2026}");
                mi.update_item.set_enabled(true);
                if !msg.is_empty() {
                    warn!("Update failed: {msg}");
                }
            }

            Event::RefreshTrayIcon => {
                // Debounce timer fired — push the icon if the state has settled.
                flush_tray_icon(&self.tray);
            }

            Event::StateChanged(s) => {
                self.state.set(s);
                set_tray_icon(&self.tray, s); // also refreshes the tooltip
                crate::overlay::set_state(match s {
                    State::Processing => crate::overlay::OverlayState::Processing,
                    _ => crate::overlay::OverlayState::Idle, // Idle / Loading
                });
                // Keep the toggle item in sync for every state (Recording is
                // owned by its dedicated arm above). Matters most when the
                // hotkey — not the menu — drives the transition.
                if let Some(mi) = &self.menu_items {
                    match s {
                        State::Idle => {
                            mi.toggle_item.set_text("Start Recording");
                            mi.toggle_item.set_enabled(true);
                        }
                        State::Processing => {
                            mi.toggle_item.set_text("Processing\u{2026}");
                            mi.toggle_item.set_enabled(false);
                        }
                        State::Loading => {
                            mi.toggle_item
                                .set_text("\u{231b} Loading model\u{2026} (unavailable)");
                            mi.toggle_item.set_enabled(false);
                        }
                        State::Recording => {}
                    }
                }
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
        }
    }
}

pub fn run(state: AppState, rx: Receiver<Event>) -> Result<()> {
    info!("Starting system tray...");

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;

    // Don't use EventLoopProxy for menu/tray events — it causes macOS Tahoe
    // to close the tray menu. Instead, poll MenuEvent::receiver() in about_to_wait().

    // No proxy forwarding — all crossbeam events are polled in about_to_wait().
    // EventLoopProxy wake-ups cause macOS Tahoe to close the tray menu (Apple bug).
    // Instead, we use WaitUntil(100ms) so about_to_wait is called periodically.

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
}

/// Send a UI event from the pipeline thread and wake the main loop so the icon /
/// overlay pill react immediately rather than lagging the WaitUntil tick.
fn notify_ui(ui_tx: &Sender<Event>, ev: Event) {
    let _ = ui_tx.send(ev);
    wake_main();
}

/// Handle one pipeline event. Split out of `pipeline_loop` so each event runs
/// inside a `catch_unwind` at the call site — a panic here is contained and the
/// loop resets to idle rather than the worker thread dying.
fn handle_pipeline_event(
    event: Event,
    rx: &Receiver<Event>,
    config: &Arc<Mutex<Config>>,
    ui_tx: &Sender<Event>,
    self_tx: &Sender<Event>,
    recording: &mut bool,
    capture: &mut Option<crate::audio::capture::AudioCapture>,
) {
    match event {
        Event::HotkeyDown => {
            if !crate::license::gate() {
                return;
            }
            if *recording {
                return;
            }
            let cfg = config.lock_safe();
            let device = crate::audio::effective_input_device(&cfg.input_device);
            let delay = cfg.hold_delay;
            let sound_feedback = cfg.sound_feedback;
            let language = cfg.language.clone();
            drop(cfg);

            // Immediate audio acknowledgement — a 70 ms blip the moment the
            // key is pressed, before hold_delay. The user gets a clear cue
            // that the key was heard; hold_delay still gates recording.
            if sound_feedback {
                crate::audio::playback::play_sound("start");
            }

            // Batch mode (Whisper, Parakeet) — do NOT pre-roll the mic.
            // Wait for hold_delay synchronously while peeking for an early
            // HotkeyUp; the microphone only opens once a genuine hold is
            // confirmed. Privacy: with the previous pre-roll, every Ctrl
            // tap (e.g. Ctrl+C) briefly opened the mic, which made the
            // macOS "mic in use" indicator flicker / stay lit.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(delay);
            let mut cancelled = false;
            while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                if remaining.is_zero() {
                    break;
                }
                match rx.recv_timeout(remaining) {
                    Ok(Event::HotkeyUp) => {
                        cancelled = true;
                        break;
                    }
                    // A model switch must not be silently dropped by the gate.
                    // Re-queue it and cancel this (not-yet-started) recording —
                    // safe because the mic isn't open yet, so LoadModel is then
                    // processed cleanly with nothing recording.
                    Ok(Event::LoadModel(m)) => {
                        let _ = self_tx.send(Event::LoadModel(m));
                        cancelled = true;
                        break;
                    }
                    Ok(_) => {} // ignore a duplicate HotkeyDown/Toggle here
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                    Err(_) => {
                        cancelled = true;
                        break;
                    }
                }
            }
            if cancelled {
                debug!("Quick tap — mic never opened");
                return;
            }
            // Hold confirmed — show the listening pill now, *before* the
            // synchronous mic open (50–200 ms), so it appears the instant
            // recording begins instead of lagging it ~500 ms behind the poll.
            // Showing it only here (not on key-down) means a quick tap / Ctrl-
            // click never flashes the pill. Hidden again if the mic fails to open.
            notify_ui(ui_tx, Event::ShowOverlay);
            match crate::audio::capture::AudioCapture::start(&device) {
                Ok(cap) => {
                    *capture = Some(cap);
                    *recording = true;
                    // Tell the UI thread so the menu-bar icon turns citron —
                    // without this the icon only updated on menu-driven recording.
                    notify_ui(ui_tx, Event::StateChanged(State::Recording));
                    // Harvest on-screen names off the pipeline thread so the AX
                    // reads can't add latency between key-up and transcription
                    // (#7). Runs during the recording; consumed at transcribe end.
                    let lang = language.clone();
                    std::thread::spawn(move || crate::dictionary::update_session_context(&lang));
                    info!("Recording…");
                }
                Err(e) => {
                    warn!("Capture failed: {e}");
                    notify_ui(ui_tx, Event::HideOverlay);
                    crate::notify::app(
                        "Couldn't start recording — check your microphone is connected.",
                    );
                }
            }
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
            stop_and_transcribe(config, capture);
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
                stop_and_transcribe(config, capture);
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
            "Whisper Push \u{2014} Loading model\u{2026}",
        ),
        State::Processing => (
            GlyphStyle::Template(BUSY_OPACITY),
            true,
            "Whisper Push \u{2014} Transcribing\u{2026}",
        ),
        State::Recording => (
            GlyphStyle::Tint(TINT_RECORDING),
            false,
            "Whisper Push \u{2014} Recording",
        ),
        State::Idle => (
            GlyphStyle::Template(255),
            true,
            "Whisper Push \u{2014} Ready",
        ),
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

/// Stop capture, transcribe audio, and paste result.
fn stop_and_transcribe(
    config: &Arc<Mutex<Config>>,
    capture: &mut Option<crate::audio::capture::AudioCapture>,
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

    if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
        if device_lost {
            crate::notify::app("Recording stopped — the microphone disconnected.");
        }
        info!("Too short, skipping");
        return;
    }

    // Auto-fallback: enough audio was captured but it's flatline silence — the
    // signature of a connected-but-not-working mic (AirPods whose mic link never
    // opened, a muted USB interface), not a quiet room, which always has some
    // ambient peak. We can't recover this utterance, but switch the live input
    // to a known-good mic (built-in if the lid's open, else any other) so the
    // next press just works.
    let peak = audio.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    if peak < crate::audio::DEAD_MIC_PEAK {
        if let Some(dead) = used_device {
            if let Some(next) = crate::audio::next_working_mic(&dead) {
                crate::audio::set_input_override(&next);
                warn!("No signal from '{dead}' (peak={peak:.6}) — switching input to '{next}'");
                crate::notify::app(&format!(
                    "No sound from {dead} — switched to {next}. Press your shortcut again."
                ));
                return;
            }
        }
        // Every input is silent → systemic (likely Microphone permission). Fall
        // through; the empty transcription is harmless and the log shows why.
    } else {
        // Good signal — forget any earlier dead-mic memory so devices that
        // recover become eligible again.
        crate::audio::clear_dead_mics();
    }

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
    match result {
        Ok(text) if !text.is_empty() => {
            info!(
                "Pasting ({:.2}s): '{}'",
                start.elapsed().as_secs_f64(),
                // char-based, not byte-based: `&text[..80]` panics if byte 80
                // lands mid-codepoint (French accents, CJK — all in scope).
                text.chars().take(80).collect::<String>()
            );
            if let Err(e) = crate::paste::paste_text(&text) {
                tracing::error!("Paste failed: {e}");
            }
            // No per-dictation notification (noise).
        }
        Ok(_) => info!("No speech detected"),
        Err(e) => {
            tracing::error!("Transcription: {e}");
            crate::notify::app(&format!("Error: {e}"));
        }
    }
}

/// Append one (removable) menu item per dictionary entry; returns them paired
/// with their term so clicks map back. A disabled placeholder (empty term) is
/// shown when the dictionary is empty or truncated.
fn populate_dict_entries(submenu: &Submenu) -> Vec<(MenuItem, String)> {
    const MAX: usize = 40;
    let entries = crate::dictionary::list_entries();
    let mut items = Vec::new();
    if entries.is_empty() {
        let ph = MenuItem::new(
            "  (empty \u{2014} your corrections will appear here)",
            false,
            None,
        );
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
    let corrected = match osascript_input(
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
    let Some(input) = osascript_input(
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
    let Some(key) = osascript_input("Enter your Whisper Push license key:", "") else {
        return;
    };
    if key.trim().is_empty() {
        return;
    }
    let Some(email) = osascript_input("Enter the email used for your purchase:", "") else {
        return;
    };
    use crate::license::ActivateOutcome::*;
    let msg = match crate::license::activate(&key, &email) {
        Activated => "License activated \u{2014} thank you!".to_string(),
        Rejected(r) => format!("Activation failed: {r}"),
        Offline => "Couldn't reach the license server. Check your connection and retry.".into(),
    };
    crate::notify::app(&msg);
    let _ = tx.send(Event::LicenseChanged);
}

/// Confirm, then free this device's slot. Runs off the UI thread.
fn license_deactivate_dialog(tx: crossbeam_channel::Sender<Event>) {
    if !osascript_confirm(
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

/// Native yes/no dialog (macOS). Returns true if the confirm button was clicked.
#[cfg(target_os = "macos")]
fn osascript_confirm(message: &str, confirm_btn: &str) -> bool {
    let script = format!(
        "display dialog \"{}\" with title \"Whisper Push\" buttons {{\"Cancel\", \"{}\"}} \
         default button \"Cancel\" cancel button \"Cancel\"",
        crate::notify::applescript_escape(message),
        crate::notify::applescript_escape(confirm_btn),
    );
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn osascript_confirm(_message: &str, _confirm_btn: &str) -> bool {
    false
}

/// Native text-input dialog (macOS). Returns `None` if the user cancels.
#[cfg(target_os = "macos")]
fn osascript_input(message: &str, prefill: &str) -> Option<String> {
    let script = format!(
        "display dialog \"{}\" default answer \"{}\" with title \"Whisper Push\" \
         buttons {{\"Cancel\", \"Save\"}} default button \"Save\" cancel button \"Cancel\"\n\
         text returned of result",
        crate::notify::applescript_escape(message),
        crate::notify::applescript_escape(prefill)
    );
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None; // Cancel → osascript exits non-zero
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches(['\n', '\r'])
            .to_string(),
    )
}

#[cfg(not(target_os = "macos"))]
fn osascript_input(_message: &str, _prefill: &str) -> Option<String> {
    crate::notify::app(
        "In-app dictionary editing is macOS-only for now — use `whisper-push dict`.",
    );
    None
}

pub fn format_hotkey_display(hotkey: &str, mode: &str) -> String {
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
