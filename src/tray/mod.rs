use crate::audio::capture::AudioCapture;
use crate::config::Config;
use crate::state::{AppState, Event, State};
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use std::sync::{Arc, Mutex};
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
    capture: Arc<Mutex<Option<AudioCapture>>>,
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
    backend_items: Vec<(MenuItem, String)>, // (item, config value)
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
            capture: Arc::new(Mutex::new(None)),
            pipeline_tx: None,
            menu_items: None,
            pending_update: None,
        }
    }

    fn create_tray(&mut self) {
        let cfg = self.config.lock().unwrap().clone();

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
        let _ = hotkey_submenu.append(&PredefinedMenuItem::separator());
        let custom_hotkey_item = MenuItem::new("Set Custom Hotkey\u{2026}", true, None);
        let _ = hotkey_submenu.append(&custom_hotkey_item);
        let custom_hotkey_id = custom_hotkey_item.id().0.clone();

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
        let current_backend = crate::model_manager::backend_for_model(&cfg.model);
        let parakeet_status = models
            .iter()
            .find(|m| m.backend == "parakeet")
            .map(|m| m.is_downloaded)
            .unwrap_or(false);
        let voxtral_status = models
            .iter()
            .find(|m| m.backend == "voxtral-local")
            .map(|m| m.is_downloaded)
            .unwrap_or(false);
        let whisper_status = models
            .iter()
            .find(|m| m.backend == "whisper")
            .map(|m| m.is_downloaded)
            .unwrap_or(false);

        let engine_label =
            |name: &str, backend_key: &str, downloaded: bool, current: &str| -> String {
                let active = if backend_key == current {
                    "\u{25CF} "
                } else {
                    "    "
                }; // ● or spaces
                let dl = if downloaded { "" } else { " \u{2913}" }; // ⤓ if not downloaded
                format!("{active}{name}{dl}")
            };

        let backend_parakeet = MenuItem::new(
            &engine_label(
                "Parakeet TDT v3 (600 MB)",
                "parakeet",
                parakeet_status,
                current_backend,
            ),
            true,
            None,
        );
        let backend_voxtral_local = MenuItem::new(
            &engine_label(
                "Voxtral Realtime 2602 (2.3 GB, streaming)",
                "voxtral-local",
                voxtral_status,
                current_backend,
            ),
            true,
            None,
        );
        let backend_whisper = MenuItem::new(
            &engine_label(
                "Whisper large-v3-turbo (550 MB)",
                "whisper",
                whisper_status,
                current_backend,
            ),
            true,
            None,
        );

        // Engine submenu (compact dropdown).
        let backend_submenu = Submenu::new("Engine", true);
        let _ = backend_submenu.append(&backend_parakeet);
        let _ = backend_submenu.append(&backend_voxtral_local);
        let _ = backend_submenu.append(&backend_whisper);

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
            backend_items: vec![
                (backend_parakeet, "parakeet".into()),
                (backend_voxtral_local, "voxtral-local".into()),
                (backend_whisper, "whisper".into()),
            ],
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
        self.tray = Some(builder.build().expect("failed to create tray icon"));

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
                if self.config.lock().unwrap().notifications {
                    crate::notify::send("Whisper Push", "Model loaded and ready!");
                }
                info!("Ready");
            }

            Event::MenuClicked(ref id) => {
                if id == &mi.quit_id {
                    std::process::exit(0);
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
                    crate::notify::send(
                        "Whisper Push",
                        "Uninstalled. You can delete the app from Applications.",
                    );
                    std::process::exit(0);
                }
                if id == &mi.toggle_id {
                    self.process_event(Event::HotkeyToggle);
                    return;
                }
                if id == &mi.test_id {
                    // Test: record 3 seconds + transcribe + show result
                    let cfg = self.config.lock().unwrap().clone();
                    std::thread::spawn(move || {
                        info!("=== TEST: Recording 3 seconds... ===");
                        crate::notify::send("Whisper Push", "Recording 3 seconds...");

                        match crate::audio::capture::AudioCapture::start(&cfg.input_device) {
                            Ok(cap) => {
                                std::thread::sleep(std::time::Duration::from_secs(3));
                                let audio = cap.stop();
                                let rms: f32 = (audio.iter().map(|s| s * s).sum::<f32>()
                                    / audio.len().max(1) as f32)
                                    .sqrt();
                                info!(
                                    "=== TEST: Captured {:.1}s, RMS={:.4} ===",
                                    audio.len() as f32 / crate::audio::SAMPLE_RATE as f32,
                                    rms
                                );

                                if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
                                    crate::notify::send(
                                        "Whisper Push",
                                        "Test failed: audio too short",
                                    );
                                    return;
                                }
                                if rms < crate::audio::capture::SILENCE_RMS_THRESHOLD {
                                    crate::notify::send(
                                        "Whisper Push",
                                        "Test failed: silence (check mic permission)",
                                    );
                                    return;
                                }

                                let bk = crate::model_manager::backend_for_model(&cfg.model);
                                info!("=== TEST: Transcribing with '{}' ===", bk);
                                crate::notify::send(
                                    "Whisper Push",
                                    &format!("Transcribing with {}...", bk),
                                );

                                let backend = crate::model_manager::resolve_backend(&cfg.model);

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
                                        crate::notify::send(
                                            "Whisper Push",
                                            &format!(
                                                "Test OK ({:.1}s): {}",
                                                elapsed.as_secs_f64(),
                                                text
                                            ),
                                        );
                                    }
                                    Ok(_) => {
                                        info!("=== TEST: No speech detected ===");
                                        crate::notify::send(
                                            "Whisper Push",
                                            "Test: no speech detected",
                                        );
                                    }
                                    Err(e) => {
                                        info!("=== TEST ERROR: {e} ===");
                                        crate::notify::send(
                                            "Whisper Push",
                                            &format!("Test error: {e}"),
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                info!("=== TEST: Capture failed: {e} ===");
                                crate::notify::send("Whisper Push", &format!("Test failed: {e}"));
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
                    crate::permissions::guided_setup();
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
                                    crate::notify::send(
                                        "Whisper Push",
                                        &format!("Update failed: {e}"),
                                    );
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
                                    crate::notify::send(
                                        "Whisper Push",
                                        "You\u{2019}re on the latest version.",
                                    );
                                    let _ = tx.send(Event::UpdateFailed(String::new()));
                                }
                                Err(e) => {
                                    tracing::error!("Update check failed: {e}");
                                    crate::notify::send(
                                        "Whisper Push",
                                        &format!("Update check failed: {e}"),
                                    );
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
                    let mut c = self.config.lock().unwrap();
                    c.notifications = !c.notifications;
                    let _ = c.save();
                    return;
                }
                if id == &mi.sound_id {
                    let mut c = self.config.lock().unwrap();
                    c.sound_feedback = !c.sound_feedback;
                    let _ = c.save();
                    return;
                }
                if id == &mi.debug_id {
                    let mut c = self.config.lock().unwrap();
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
                    crate::notify::send(
                        "Whisper Push",
                        &format!(
                            "Dictionary reloaded \u{2014} {} word(s).",
                            crate::dictionary::entry_count()
                        ),
                    );
                    let _ = self.state.tx.send(Event::DictChanged);
                    return;
                }
                if id == &mi.dict_forget_voice_id {
                    crate::acoustic::clear();
                    crate::notify::send("Whisper Push", "Forgot all learned voiceprints.");
                    let _ = self.state.tx.send(Event::DictChanged);
                    return;
                }
                if id == &mi.dict_enabled_id {
                    let mut c = self.config.lock().unwrap();
                    c.dictionary_enabled = !c.dictionary_enabled;
                    let on = c.dictionary_enabled;
                    let _ = c.save();
                    drop(c);
                    crate::dictionary::init(on);
                    crate::notify::send(
                        "Whisper Push",
                        if on {
                            "Adaptive correction ON"
                        } else {
                            "Adaptive correction OFF"
                        },
                    );
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
                            crate::notify::send(
                                "Whisper Push",
                                &format!("Removed \u{201c}{term}\u{201d} from dictionary"),
                            );
                        }
                        let _ = self.state.tx.send(Event::DictChanged);
                        return;
                    }
                }
                for (item_id, hotkey, mode) in &mi.hotkey_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap();
                        c.hotkey = hotkey.clone();
                        c.hotkey_mode = mode.clone();
                        let _ = c.save();
                        for (item, hk, m) in &mi.hotkey_items {
                            item.set_checked(hk == hotkey && m == mode);
                        }
                        let disp = format_hotkey_display(hotkey, mode);
                        mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                        mi.hotkey_submenu.set_text(format!("Hotkey: {disp}"));
                        crate::hotkey::rebind(hotkey, mode); // live — no restart
                        crate::notify::send("Whisper Push", &format!("Hotkey set to {disp}"));
                        return;
                    }
                }
                if id == &mi.custom_hotkey_id {
                    crate::hotkey::start_capture(self.state.tx.clone());
                    crate::notify::send(
                        "Whisper Push",
                        "Press your shortcut now: tap a modifier (e.g. Right \u{2318}) to hold, or a combo like \u{2318}\u{21e7}D to toggle.",
                    );
                    return;
                }
                for (item_id, name) in &mi.input_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap();
                        c.input_device = name.clone();
                        let _ = c.save();
                        for (item, n) in &mi.input_device_items {
                            item.set_checked(n == name);
                        }
                        mi.input_submenu.set_text(device_title("Input", name));
                        return;
                    }
                }
                for (item_id, name) in &mi.output_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap();
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
                // Backend selection
                for (item, backend_value) in &mi.backend_items {
                    if id == &item.id().0 {
                        // Save model in config (backend is derived automatically)
                        let model_name = crate::model_manager::model_for_backend(backend_value);
                        let mut c = self.config.lock().unwrap();
                        c.model = model_name.to_string();
                        let _ = c.save();
                        drop(c);
                        // Update ● indicator and remove ⤓ download icon
                        for (bi, bv) in &mi.backend_items {
                            let current_text = bi.text();
                            let stripped = current_text
                                .trim_start_matches('\u{25CF}')
                                .trim_start()
                                .trim_end_matches('\u{2913}')
                                .trim_end();
                            if bv == backend_value {
                                bi.set_text(&format!("\u{25CF} {stripped}"));
                            } else {
                                bi.set_text(&format!("    {stripped}"));
                            }
                        }

                        // Send LoadModel to pipeline thread — it handles unloading
                        // old models and loading the new one on its own thread
                        // (required for WGPU/Metal same-thread constraint).
                        if let Some(ref tx) = self.pipeline_tx {
                            let _ = tx.send(Event::LoadModel(model_name.to_string()));
                        }
                        crate::notify::send(
                            "Whisper Push",
                            &format!("Loading {}...", backend_value),
                        );
                        return;
                    }
                }
            }

            Event::HotkeyToggle => match self.state.current() {
                State::Idle => {
                    if !crate::license::is_entitled() {
                        crate::license::on_blocked();
                        return;
                    }
                    let device = self.config.lock().unwrap().input_device.clone();
                    match AudioCapture::start(&device) {
                        Ok(cap) => {
                            *self.capture.lock().unwrap() = Some(cap);
                            self.state.set(State::Recording);
                            mi.toggle_item.set_text("Stop & Transcribe");
                            set_tray_icon(&self.tray, State::Recording);
                            if self.config.lock().unwrap().sound_feedback {
                                crate::audio::playback::play_sound("start");
                            }
                        }
                        Err(e) => warn!("Capture failed: {e}"),
                    }
                }
                State::Recording => self.finish_recording(),
                _ => {}
            },

            Event::StateChanged(State::Recording) => {
                // Reached from BOTH the menu toggle and the physical hotkey
                // (the pipeline thread now emits this so the icon turns citron
                // regardless of how recording started). The start sound is
                // played at each trigger point, never here, to avoid doubling.
                self.state.set(State::Recording);
                mi.toggle_item.set_text("Recording\u{2026}");
                set_tray_icon(&self.tray, State::Recording);
            }

            Event::Transcribed(text) => {
                if let Err(e) = crate::paste::paste_text(&text) {
                    tracing::error!("Paste failed: {e}");
                }
                // No per-dictation notification — it's noise. Notifications are
                // reserved for meaningful events (learned a word, errors).
                self.state.set(State::Idle);
                mi.toggle_item.set_text("Start Recording");
                mi.toggle_item.set_enabled(true);
                set_tray_icon(&self.tray, State::Idle);
            }

            Event::HotkeyCaptured(hotkey, mode) => {
                info!("Custom hotkey captured: '{hotkey}' ({mode})");
                {
                    let mut c = self.config.lock().unwrap();
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
                crate::notify::send("Whisper Push", &format!("Custom hotkey set: {disp}"));
            }

            Event::PromptPermissions => {
                info!("Checking/prompting permissions...");
                let status = crate::permissions::check_all();
                if !status.all_granted() {
                    // Guided flow: prompts + opens panes + polls + restarts.
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
                if self.config.lock().unwrap().notifications {
                    crate::notify::send(
                        "Whisper Push",
                        &format!("Version {version} available! Click the menu to update."),
                    );
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

            Event::StateChanged(s) => {
                self.state.set(s);
                set_tray_icon(&self.tray, s); // also refreshes the tooltip
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

    fn finish_recording(&mut self) {
        let mi = self.menu_items.as_ref().unwrap();
        self.state.set(State::Processing);
        mi.toggle_item.set_text("Processing...");
        mi.toggle_item.set_enabled(false);
        set_tray_icon(&self.tray, State::Processing);

        if self.config.lock().unwrap().sound_feedback {
            crate::audio::playback::play_sound("stop");
        }

        let audio = self
            .capture
            .lock()
            .unwrap()
            .take()
            .map(|c| c.stop())
            .unwrap_or_default();
        if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
            self.state.set(State::Idle);
            mi.toggle_item.set_text("Start Recording");
            mi.toggle_item.set_enabled(true);
            set_tray_icon(&self.tray, State::Idle);
            return;
        }

        let tx = self.state.tx.clone();
        let cfg = self.config.lock().unwrap().clone();
        let backend = crate::model_manager::resolve_backend(&cfg.model);
        std::thread::spawn(move || {
            crate::dictionary::update_session_context(&cfg.language);
            match crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend) {
                Ok(text) if !text.is_empty() => {
                    let _ = tx.send(Event::Transcribed(text));
                }
                Ok(_) => {
                    let _ = tx.send(Event::StateChanged(State::Idle));
                }
                Err(e) => {
                    tracing::error!("Transcription: {e}");
                    let _ = tx.send(Event::StateChanged(State::Idle));
                }
            }
        });
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

            // Wake the macOS run loop so the icon appears immediately
            #[cfg(target_os = "macos")]
            {
                // CFRunLoop API
                use objc2_core_foundation::CFRunLoop;
                let rl = CFRunLoop::main().unwrap();
                rl.wake_up();
            }

            // Start hotkey listener + autonomous pipeline thread.
            // The pipeline runs entirely in background threads — it never
            // touches the winit event loop, so the tray menu stays open.
            let hotkey_cfg = self.state.config.hotkey.clone();
            let hotkey_mode = self.state.config.hotkey_mode.clone();
            let pipeline_cfg = self.config.clone();
            let (ptx, prx) = crossbeam_channel::unbounded();
            self.pipeline_tx = Some(ptx.clone());
            let _ = crate::hotkey::start_listener(&hotkey_cfg, &hotkey_mode, ptx);

            // Pipeline thread: hotkey events + model load → capture → transcribe → paste
            let ui_tx = self.state.tx.clone();
            std::thread::spawn(move || {
                pipeline_loop(prx, pipeline_cfg, ui_tx);
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

    let mut app = App::new(state, rx);

    event_loop.run_app(&mut app)?;

    Ok(())
}

/// Autonomous pipeline: listens for hotkey events, captures audio,
/// transcribes, and pastes — all in background threads.
/// Never touches the winit event loop or tray menu.
fn pipeline_loop(rx: Receiver<Event>, config: Arc<Mutex<Config>>, ui_tx: Sender<Event>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let recording = Arc::new(AtomicBool::new(false));
    let capture: Arc<Mutex<Option<crate::audio::capture::AudioCapture>>> =
        Arc::new(Mutex::new(None));

    loop {
        match rx.recv() {
            Ok(Event::HotkeyDown) => {
                if !crate::license::is_entitled() {
                    crate::license::on_blocked();
                    continue;
                }
                if recording.load(Ordering::Relaxed) {
                    continue;
                }
                let cfg = config.lock().unwrap();
                let device = cfg.input_device.clone();
                let delay = cfg.hold_delay;
                let sound_feedback = cfg.sound_feedback;
                // Voxtral streaming hangs on first use (GPU shader compilation blocks
                // the feed_chunk loop). Use batch mode for all backends — record first,
                // then transcribe after release.
                let is_voxtral_streaming = false;
                drop(cfg);

                // Immediate audio acknowledgement — a 70 ms blip the moment the
                // key is pressed, before hold_delay. The user gets a clear cue
                // that the key was heard; hold_delay still gates recording.
                if sound_feedback {
                    crate::audio::playback::play_sound("start");
                }

                if is_voxtral_streaming {
                    // Streaming mode: use StreamingCapture + StreamingSession
                    recording.store(true, Ordering::Relaxed);
                    let rec = recording.clone();
                    let cfg2 = config.clone();
                    let rx_clone = rx.clone();

                    // Run streaming in this thread (needs same thread for WGPU)
                    info!("Starting streaming transcription...");
                    // (start sound already played immediately on HotkeyDown above)

                    // Ensure Voxtral model is loaded on this thread
                    if !crate::transcribe::voxtral_local::is_loaded() {
                        let _ = ui_tx.send(Event::StateChanged(State::Loading));
                        info!("Loading Voxtral Q4 on first use...");
                        let load_start = std::time::Instant::now();
                        let dir = crate::config::data_dir().join("models").join("voxtral");
                        if let Err(e) =
                            crate::transcribe::voxtral_local::load_model(dir.to_str().unwrap_or(""))
                        {
                            tracing::error!("Voxtral load failed: {e}");
                            crate::notify::send("Whisper Push", &format!("Error: {e}"));
                            let _ = ui_tx.send(Event::StateChanged(State::Idle));
                            rec.store(false, Ordering::Relaxed);
                            continue;
                        }
                        info!("Voxtral ready ({:.1}s)", load_start.elapsed().as_secs_f64());
                        let _ = ui_tx.send(Event::StateChanged(State::Idle));
                    }

                    // Start streaming session
                    match crate::transcribe::voxtral_local::streaming::start() {
                        Ok(mut session) => {
                            // Start streaming capture (500ms chunks)
                            match crate::audio::stream::StreamingCapture::start(&device, 500) {
                                Ok(stream_capture) => {
                                    info!("Streaming: capture + session started");

                                    // Feed chunks until HotkeyUp
                                    loop {
                                        // Check for HotkeyUp (non-blocking)
                                        if let Ok(Event::HotkeyUp) = rx_clone.try_recv() {
                                            break;
                                        }

                                        // Get next audio chunk (with timeout)
                                        match stream_capture
                                            .chunk_rx
                                            .recv_timeout(std::time::Duration::from_millis(100))
                                        {
                                            Ok(chunk) => {
                                                match crate::transcribe::voxtral_local::streaming::feed_chunk(
                                                    &mut session, &chunk.samples
                                                ) {
                                                    Ok(words) if !words.is_empty() => {
                                                        let text = words.join(" ");
                                                        info!("Streaming → '{text}'");
                                                        if let Err(e) = crate::paste::type_text(&text) {
                                                            tracing::error!("Type failed: {e}");
                                                        }
                                                    }
                                                    Ok(_) => {} // no new words yet
                                                    Err(e) => {
                                                        tracing::error!("Streaming feed error: {e}");
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                                                continue;
                                            }
                                            Err(_) => break,
                                        }
                                    }

                                    // Stop capture and feed remaining
                                    drop(stream_capture);
                                    if cfg2.lock().unwrap().sound_feedback {
                                        crate::audio::playback::play_sound("stop");
                                    }

                                    // Finish and paste any remaining text
                                    match crate::transcribe::voxtral_local::streaming::finish(
                                        session,
                                    ) {
                                        Ok(final_text) if !final_text.is_empty() => {
                                            info!("Streaming final: '{final_text}'");
                                        }
                                        Ok(_) => info!("Streaming: no final text"),
                                        Err(e) => tracing::error!("Streaming finish: {e}"),
                                    }
                                }
                                Err(e) => warn!("Stream capture failed: {e}"),
                            }
                        }
                        Err(e) => {
                            tracing::error!("Streaming session failed: {e}");
                            crate::notify::send("Whisper Push", &format!("Streaming error: {e}"));
                        }
                    }

                    rec.store(false, Ordering::Relaxed);
                    continue; // Don't fall through to batch mode
                }

                // Batch mode (Whisper, Parakeet) — do NOT pre-roll the mic.
                // Wait for hold_delay synchronously while peeking for an early
                // HotkeyUp; the microphone only opens once a genuine hold is
                // confirmed. Privacy: with the previous pre-roll, every Ctrl
                // tap (e.g. Ctrl+C) briefly opened the mic, which made the
                // macOS "mic in use" indicator flicker / stay lit.
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs_f64(delay);
                let mut cancelled = false;
                while let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                {
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(Event::HotkeyUp) => {
                            cancelled = true;
                            break;
                        }
                        Ok(_) => {} // ignore other events during the gate
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => break,
                        Err(_) => {
                            cancelled = true;
                            break;
                        }
                    }
                }
                if cancelled {
                    debug!("Quick tap — mic never opened");
                    continue;
                }
                match crate::audio::capture::AudioCapture::start(&device) {
                    Ok(cap) => {
                        *capture.lock().unwrap() = Some(cap);
                        recording.store(true, Ordering::Relaxed);
                        // Tell the UI thread so the menu-bar icon turns citron —
                        // without this the icon only updated on menu-driven recording.
                        let _ = ui_tx.send(Event::StateChanged(State::Recording));
                        info!("Recording…");
                    }
                    Err(e) => warn!("Capture failed: {e}"),
                }
            }

            Ok(Event::HotkeyUp) => {
                // A quick-tap HotkeyUp is consumed by the hold_delay gate in
                // the HotkeyDown arm; if we receive one here it means hold was
                // confirmed and the mic is open.
                if !recording.load(Ordering::Relaxed) {
                    continue;
                }
                recording.store(false, Ordering::Relaxed);
                let _ = ui_tx.send(Event::StateChanged(State::Processing));
                stop_and_transcribe(&config, &capture);
                let _ = ui_tx.send(Event::StateChanged(State::Idle));
            }

            Ok(Event::HotkeyToggle) => {
                if !recording.load(Ordering::Relaxed) {
                    if !crate::license::is_entitled() {
                        crate::license::on_blocked();
                        continue;
                    }
                    let device = config.lock().unwrap().input_device.clone();
                    match crate::audio::capture::AudioCapture::start(&device) {
                        Ok(cap) => {
                            *capture.lock().unwrap() = Some(cap);
                            recording.store(true, Ordering::Relaxed);
                            let _ = ui_tx.send(Event::StateChanged(State::Recording));
                            if config.lock().unwrap().sound_feedback {
                                crate::audio::playback::play_sound("start");
                            }
                            info!("Recording (toggle)...");
                        }
                        Err(e) => warn!("Capture failed: {e}"),
                    }
                } else {
                    recording.store(false, Ordering::Relaxed);
                    let _ = ui_tx.send(Event::StateChanged(State::Processing));
                    stop_and_transcribe(&config, &capture);
                    let _ = ui_tx.send(Event::StateChanged(State::Idle));
                }
            }

            Ok(Event::LoadModel(model_name)) => {
                let start = std::time::Instant::now();
                info!("Loading model '{model_name}' on pipeline thread...");

                // Tell the UI we're loading (icon changes, hotkeys ignored)
                let _ = ui_tx.send(Event::StateChanged(State::Loading));

                // Unload all backends
                crate::transcribe::unload_model();
                crate::transcribe::parakeet::unload_model();
                crate::transcribe::voxtral_local::unload_model();

                let backend = crate::model_manager::backend_for_model(&model_name);

                // Check if model needs downloading and notify user
                let needs_download = !crate::model_manager::is_model_downloaded(backend);
                if needs_download {
                    let size = crate::model_manager::model_size_mb(backend);
                    crate::notify::send(
                        "Whisper Push",
                        &format!(
                            "Downloading {} (~{}MB)... This may take a few minutes.",
                            backend, size
                        ),
                    );
                }

                let load_result = match backend {
                    "voxtral-local" => {
                        let dir = crate::config::data_dir().join("models").join("voxtral");
                        crate::transcribe::voxtral_local::load_model(dir.to_str().unwrap_or(""))
                    }
                    "parakeet" => crate::transcribe::parakeet::load_model(),
                    _ => crate::transcribe::load_model(&model_name),
                };

                let elapsed = start.elapsed();
                match load_result {
                    Ok(()) => {
                        info!("{backend} model loaded ({:.1}s)", elapsed.as_secs_f64());
                        crate::notify::send(
                            "Whisper Push",
                            &format!("{backend} ready! ({:.0}s)", elapsed.as_secs_f64()),
                        );
                    }
                    Err(e) => {
                        tracing::error!("{backend} load failed: {e}");
                        crate::notify::send(
                            "Whisper Push",
                            &format!("Failed to load {backend}: {e}"),
                        );
                    }
                }

                // Back to idle — hotkeys work again
                let _ = ui_tx.send(Event::StateChanged(State::Idle));
            }

            Ok(_) => {}
            Err(_) => break,
        }
    }
}

fn set_tray_icon(tray: &Option<TrayIcon>, state: State) {
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
            let _ = tray.set_icon(Some(icon));
        }
        let _ = tray.set_tooltip(Some(tooltip));
        #[cfg(target_os = "macos")]
        tray.set_icon_as_template(is_template);
    }
}

/// Stop capture, transcribe audio, and paste result.
fn stop_and_transcribe(
    config: &Arc<Mutex<Config>>,
    capture: &Arc<Mutex<Option<crate::audio::capture::AudioCapture>>>,
) {
    let cfg = config.lock().unwrap().clone();
    if cfg.sound_feedback {
        crate::audio::playback::play_sound("stop");
    }

    let audio = capture
        .lock()
        .unwrap()
        .take()
        .map(|c| c.stop())
        .unwrap_or_default();

    if audio.len() < crate::audio::MIN_AUDIO_SAMPLES {
        info!("Too short, skipping");
        return;
    }

    let rms: f32 = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
    let backend = crate::model_manager::resolve_backend(&cfg.model);
    info!(
        "Processing {:.1}s of audio with backend '{:?}' (RMS={:.4})...",
        audio.len() as f32 / crate::audio::SAMPLE_RATE as f32,
        backend,
        rms
    );

    // Semantic layer: harvest names from the current screen/clipboard so the
    // dictation can recognize them even if never explicitly taught.
    crate::dictionary::update_session_context(&cfg.language);

    let start = std::time::Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend)
    }));
    let result = match result {
        Ok(r) => r,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown panic".into()
            };
            Err(anyhow::anyhow!("Transcription panicked: {msg}"))
        }
    };
    match result {
        Ok(text) if !text.is_empty() => {
            info!(
                "Pasting ({:.2}s): '{}'",
                start.elapsed().as_secs_f64(),
                if text.len() > 80 { &text[..80] } else { &text }
            );
            if let Err(e) = crate::paste::paste_text(&text) {
                tracing::error!("Paste failed: {e}");
            }
            // No per-dictation notification (noise) — see Transcribed handler.
        }
        Ok(_) => info!("No speech detected"),
        Err(e) => {
            tracing::error!("Transcription: {e}");
            crate::notify::send("Whisper Push", &format!("Error: {e}"));
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
        crate::notify::send("Whisper Push", "No recent dictation to correct.");
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
            crate::notify::send("Whisper Push", &msg);
            let _ = tx.send(Event::DictChanged);
        }
        Correction::NoLast => crate::notify::send("Whisper Push", "Nothing to correct yet."),
        Correction::NotReady => crate::notify::send("Whisper Push", "Dictionary is off."),
        Correction::SaveError(e) => {
            crate::notify::send("Whisper Push", &format!("Save failed: {e}"))
        }
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
            crate::notify::send("Whisper Push", &format!("Added \u{201c}{term}\u{201d}"));
            let _ = tx.send(Event::DictChanged);
        }
        Err(e) => crate::notify::send("Whisper Push", &format!("Add failed: {e}")),
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
    crate::notify::send("Whisper Push", &msg);
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
    crate::notify::send("Whisper Push", &msg);
    let _ = tx.send(Event::LicenseChanged);
}

/// Native yes/no dialog (macOS). Returns true if the confirm button was clicked.
#[cfg(target_os = "macos")]
fn osascript_confirm(message: &str, confirm_btn: &str) -> bool {
    let script = format!(
        "display dialog \"{}\" with title \"Whisper Push\" buttons {{\"Cancel\", \"{}\"}} \
         default button \"Cancel\" cancel button \"Cancel\"",
        applescript_escape(message),
        applescript_escape(confirm_btn),
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
        applescript_escape(message),
        applescript_escape(prefill)
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
    crate::notify::send(
        "Whisper Push",
        "In-app dictionary editing is macOS-only for now — use `whisper-push dict`.",
    );
    None
}

/// Escape a string for embedding inside an AppleScript double-quoted literal.
#[cfg(target_os = "macos")]
fn applescript_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\r' => out.push(' '),
            other => out.push(other),
        }
    }
    out
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
