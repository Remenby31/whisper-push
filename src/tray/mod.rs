use crate::audio::capture::AudioCapture;
use crate::config::Config;
use crate::state::{AppState, Event, State};
use anyhow::Result;
use crossbeam_channel::Receiver;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tracing::{info, warn};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};

const ICON_IDLE: &[u8] = include_bytes!("../../resources/icons/icon-idle.png");
const ICON_RECORDING: &[u8] = include_bytes!("../../resources/icons/icon-recording.png");
const ICON_PROCESSING: &[u8] = include_bytes!("../../resources/icons/icon-processing.png");

fn load_icon(data: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(data).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold \u{2014} Control", "ctrl", "hold"),
    ("Hold \u{2014} Right Control", "rctrl", "hold"),
    ("Hold \u{2014} Right Command", "rcmd", "hold"),
    ("Hold \u{2014} Right Option", "ralt", "hold"),
    ("Toggle \u{2014} \u{2318}\u{21e7}Space", "cmd+shift+space", "toggle"),
    ("Toggle \u{2014} \u{2303}\u{21e7}Space", "ctrl+shift+space", "toggle"),
];

const IDLE_PRESETS: &[(&str, u32)] = &[
    ("Never", 0), ("After 5 min", 5), ("After 15 min", 15), ("After 30 min", 30),
];

/// User events forwarded into winit's event loop.
#[derive(Debug)]
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
    hold_pending: Arc<std::sync::atomic::AtomicBool>,
    hold_delay: f64,
    // Menu items (created in init, kept alive)
    menu_items: Option<MenuItems>,
}

struct MenuItems {
    status_item: MenuItem,
    toggle_item: MenuItem,
    notifications_item: CheckMenuItem,
    sound_item: CheckMenuItem,
    debug_item: CheckMenuItem,
    toggle_id: String,
    quit_id: String,
    notif_id: String,
    sound_id: String,
    debug_id: String,
    hotkey_ids: Vec<(String, String, String)>,
    hotkey_items: Vec<(CheckMenuItem, String, String)>,
    input_ids: Vec<(String, String)>,
    input_device_items: Vec<(CheckMenuItem, String)>,
    input_submenu: Submenu,
    idle_ids: Vec<(String, u32)>,
    idle_items: Vec<(CheckMenuItem, u32)>,
    mic_perm_item: MenuItem,
    acc_perm_item: MenuItem,
    perms_submenu: Submenu,
    warn_item: Option<MenuItem>,
    mic_perm_id: String,
    acc_perm_id: String,
    backend_items: Vec<(CheckMenuItem, String)>, // (item, config value)
}

impl App {
    fn new(state: AppState, rx: Receiver<Event>) -> Self {
        let hold_delay = state.config.hold_delay;
        let config = Arc::new(Mutex::new(state.config.clone()));
        Self {
            state, config, rx,
            tray: None,
            capture: Arc::new(Mutex::new(None)),
            hold_pending: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hold_delay,
            menu_items: None,
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
            "Whisper Push \u{2014} Loading...".into()
        };
        let status_item = MenuItem::new(&status_text, false, None);
        let toggle_item = MenuItem::new(
            if is_ready { "Start Recording" } else { "Loading model..." },
            is_ready,
            None,
        );

        // Hotkey submenu
        let hotkey_submenu = Submenu::new("Hotkey", true);
        let mut hotkey_items = Vec::new();
        for (label, hotkey, mode) in HOTKEY_PRESETS {
            let checked = *hotkey == cfg.hotkey && *mode == cfg.hotkey_mode;
            let item = CheckMenuItem::new(*label, true, checked, None);
            let _ = hotkey_submenu.append(&item);
            hotkey_items.push((item, hotkey.to_string(), mode.to_string()));
        }

        // Input device submenu
        let input_submenu = Submenu::new(&format!("Input: {}", &cfg.input_device), true);
        let input_auto = CheckMenuItem::new("Auto", true, cfg.input_device == "auto", None);
        let _ = input_submenu.append(&input_auto);
        let _ = input_submenu.append(&PredefinedMenuItem::separator());
        let mut input_device_items = vec![(input_auto, "auto".to_string())];
        if let Ok(devices) = crate::audio::list_devices() {
            for name in devices {
                let checked = cfg.input_device == name;
                let item = CheckMenuItem::new(&name, true, checked, None);
                let _ = input_submenu.append(&item);
                input_device_items.push((item, name));
            }
        }

        // Idle unload submenu
        let idle_submenu = Submenu::new("Idle Unload", true);
        let mut idle_items = Vec::new();
        for (label, minutes) in IDLE_PRESETS {
            let checked = *minutes == cfg.idle_unload_minutes;
            let item = CheckMenuItem::new(*label, true, checked, None);
            let _ = idle_submenu.append(&item);
            idle_items.push((item, *minutes));
        }

        // Backend submenu (Whisper local vs Voxtral API)
        let backend_submenu = Submenu::new("Transcription Engine", true);
        let backend_parakeet = CheckMenuItem::new(
            "Parakeet TDT v3 (600 MB)",
            true, cfg.backend == "parakeet", None,
        );
        let backend_voxtral_local = CheckMenuItem::new(
            "Voxtral Realtime 2602 (2.3 GB, streaming)",
            true, cfg.backend == "voxtral-local", None,
        );
        let backend_whisper = CheckMenuItem::new(
            "Whisper large-v3-turbo (550 MB)",
            true, cfg.backend == "whisper", None,
        );
        let _ = backend_submenu.append(&backend_parakeet);
        let _ = backend_submenu.append(&backend_voxtral_local);
        let _ = backend_submenu.append(&backend_whisper);

        // Toggles
        let notifications_item = CheckMenuItem::new("Notifications", true, cfg.notifications, None);
        let sound_item = CheckMenuItem::new("Sound Feedback", true, cfg.sound_feedback, None);
        let debug_item = CheckMenuItem::new("Debug Logging", true, cfg.debug, None);
        let quit_item = MenuItem::new("Quit Whisper Push", true, None);

        // Permissions
        let perms = crate::permissions::check_all();
        let mic_label = format!("{} Microphone \u{2014} {}", perms.microphone.symbol(), perms.microphone.label());
        let acc_label = format!("{} Accessibility \u{2014} {}", perms.accessibility.symbol(), perms.accessibility.label());
        let mic_perm_item = MenuItem::new(&mic_label, true, None);
        let acc_perm_item = MenuItem::new(&acc_label, true, None);
        let perms_submenu = Submenu::new(
            if perms.all_granted() { "Permissions \u{2713}" } else { "\u{26a0} Permissions" },
            true,
        );
        let _ = perms_submenu.append(&mic_perm_item);
        let _ = perms_submenu.append(&acc_perm_item);

        // Assemble — flat menu (submenus crash on macOS Tahoe)
        let menu = Menu::new();

        let _ = menu.append(&status_item);
        let warn_item = if !perms.all_granted() {
            let w = MenuItem::new(
                &format!("\u{26a0} {} permission(s) missing", perms.missing_count()),
                false, None,
            );
            let _ = menu.append(&w);
            Some(w)
        } else {
            None
        };
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&toggle_item);

        // Permissions (only show if something is missing)
        if !perms.all_granted() {
            let _ = menu.append(&PredefinedMenuItem::separator());
            let _ = menu.append(&mic_perm_item);
            let _ = menu.append(&acc_perm_item);
        }

        let _ = menu.append(&PredefinedMenuItem::separator());

        // Engine selector
        let _ = menu.append(&backend_parakeet);
        let _ = menu.append(&backend_voxtral_local);
        let _ = menu.append(&backend_whisper);

        let _ = menu.append(&PredefinedMenuItem::separator());

        let _ = menu.append(&notifications_item);
        let _ = menu.append(&sound_item);

        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&quit_item);

        // Collect IDs
        let hotkey_ids: Vec<_> = hotkey_items.iter().map(|(i, h, m)| (i.id().0.clone(), h.clone(), m.clone())).collect();
        let input_ids: Vec<_> = input_device_items.iter().map(|(i, n)| (i.id().0.clone(), n.clone())).collect();
        let idle_ids: Vec<_> = idle_items.iter().map(|(i, m)| (i.id().0.clone(), *m)).collect();

        self.menu_items = Some(MenuItems {
            toggle_id: toggle_item.id().0.clone(),
            quit_id: quit_item.id().0.clone(),
            notif_id: notifications_item.id().0.clone(),
            sound_id: sound_item.id().0.clone(),
            debug_id: debug_item.id().0.clone(),
            mic_perm_id: mic_perm_item.id().0.clone(),
            acc_perm_id: acc_perm_item.id().0.clone(),
            mic_perm_item, acc_perm_item, perms_submenu, warn_item,
            backend_items: vec![
                (backend_parakeet, "parakeet".into()),
                (backend_voxtral_local, "voxtral-local".into()),
                (backend_whisper, "whisper".into()),
            ],
            status_item, toggle_item,
            notifications_item, sound_item, debug_item,
            hotkey_ids, hotkey_items,
            input_ids, input_device_items, input_submenu,
            idle_ids, idle_items,
        });

        // Build tray
        let mut builder = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Whisper Push");
        if let Some(icon) = load_icon(ICON_IDLE) {
            builder = builder.with_icon(icon);
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

    fn process_event(&mut self, event: Event) {
        let mi = match &self.menu_items {
            Some(m) => m,
            None => return,
        };

        match event {
            Event::ModelReady => {
                self.state.set(State::Idle);
                mi.toggle_item.set_text("Start Recording");
                mi.toggle_item.set_enabled(true);
                let disp = format_hotkey_display(&self.state.config.hotkey, &self.state.config.hotkey_mode);
                mi.status_item.set_text(&format!("Whisper Push ({disp})"));
                set_tray_icon(&self.tray, State::Idle);
                if self.config.lock().unwrap().notifications {
                    crate::notify::send("Whisper Push", "Model loaded and ready!");
                }
                info!("Ready");
            }

            Event::MenuClicked(ref id) => {
                if id == &mi.quit_id { std::process::exit(0); }
                if id == &mi.toggle_id { self.process_event(Event::HotkeyToggle); return; }
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
                if id == &mi.notif_id { let mut c = self.config.lock().unwrap(); c.notifications = !c.notifications; let _ = c.save(); return; }
                if id == &mi.sound_id { let mut c = self.config.lock().unwrap(); c.sound_feedback = !c.sound_feedback; let _ = c.save(); return; }
                if id == &mi.debug_id { let mut c = self.config.lock().unwrap(); c.debug = !c.debug; let _ = c.save(); return; }
                for (item_id, hotkey, mode) in &mi.hotkey_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap();
                        c.hotkey = hotkey.clone(); c.hotkey_mode = mode.clone(); let _ = c.save();
                        for (item, hk, _) in &mi.hotkey_items { item.set_checked(hk == hotkey); }
                        mi.status_item.set_text(&format!("Whisper Push ({})", format_hotkey_display(hotkey, mode)));
                        crate::notify::send("Whisper Push", "Hotkey changed. Restart to apply.");
                        return;
                    }
                }
                for (item_id, name) in &mi.input_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap(); c.input_device = name.clone(); let _ = c.save();
                        for (item, n) in &mi.input_device_items { item.set_checked(n == name); }
                        mi.input_submenu.set_text(&format!("Input: {name}"));
                        return;
                    }
                }
                for (item_id, minutes) in &mi.idle_ids {
                    if id == item_id {
                        let mut c = self.config.lock().unwrap(); c.idle_unload_minutes = *minutes; let _ = c.save();
                        for (item, m) in &mi.idle_items { item.set_checked(*m == *minutes); }
                        return;
                    }
                }
                // Backend selection
                for (item, backend_value) in &mi.backend_items {
                    if id == &item.id().0 {
                        // Save config and update checkmarks
                        let mut c = self.config.lock().unwrap();
                        c.backend = backend_value.clone();
                        let _ = c.save();
                        drop(c);
                        for (bi, _) in &mi.backend_items {
                            bi.set_checked(bi.id() == item.id());
                        }

                        // Auto-download model if needed, then notify
                        let bv = backend_value.clone();
                        std::thread::spawn(move || {
                            info!("Switching to {bv}...");
                            match crate::model_manager::ensure_model(&bv) {
                                Ok(()) => {
                                    crate::notify::send("Whisper Push",
                                        &format!("Switched to {bv}. Restart to apply."));
                                }
                                Err(e) => {
                                    crate::notify::send("Whisper Push",
                                        &format!("Failed to set up {bv}: {e}"));
                                }
                            }
                        });
                        return;
                    }
                }
            }

            Event::HotkeyDown => {
                if self.state.current() != State::Idle { return; }
                let device = self.config.lock().unwrap().input_device.clone();
                match AudioCapture::start(&device) {
                    Ok(cap) => {
                        *self.capture.lock().unwrap() = Some(cap);
                        self.hold_pending.store(true, std::sync::atomic::Ordering::Relaxed);
                        let pending = self.hold_pending.clone();
                        let tx = self.state.tx.clone();
                        let delay_ms = (self.hold_delay * 1000.0) as u64;
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                            if pending.load(std::sync::atomic::Ordering::Relaxed) {
                                pending.store(false, std::sync::atomic::Ordering::Relaxed);
                                let _ = tx.send(Event::StateChanged(State::Recording));
                            }
                        });
                    }
                    Err(e) => warn!("Capture failed: {e}"),
                }
            }

            Event::HotkeyUp => {
                if self.hold_pending.load(std::sync::atomic::Ordering::Relaxed) {
                    self.hold_pending.store(false, std::sync::atomic::Ordering::Relaxed);
                    self.capture.lock().unwrap().take();
                    return;
                }
                if self.state.current() != State::Recording { return; }
                self.finish_recording();
            }

            Event::HotkeyToggle => {
                match self.state.current() {
                    State::Idle => {
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
                }
            }

            Event::StateChanged(State::Recording) => {
                self.state.set(State::Recording);
                mi.toggle_item.set_text("Recording...");
                set_tray_icon(&self.tray, State::Recording);
                if self.config.lock().unwrap().sound_feedback {
                    crate::audio::playback::play_sound("start");
                }
            }

            Event::Transcribed(text) => {
                if let Err(e) = crate::paste::paste_text(&text) {
                    tracing::error!("Paste failed: {e}");
                }
                if self.config.lock().unwrap().notifications {
                    let preview = if text.len() > 50 { format!("{}...", &text[..50]) } else { text };
                    crate::notify::send("Whisper Push", &format!("Typed: {preview}"));
                }
                self.state.set(State::Idle);
                mi.toggle_item.set_text("Start Recording");
                mi.toggle_item.set_enabled(true);
                set_tray_icon(&self.tray, State::Idle);
            }

            Event::PromptPermissions => {
                info!("Checking/prompting permissions...");
                let status = crate::permissions::check_all();
                if !status.all_granted() {
                    crate::permissions::prompt_missing(&status);
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
                    let mic_label = format!("{} Microphone \u{2014} {}", status.microphone.symbol(), status.microphone.label());
                    let acc_label = format!("{} Accessibility \u{2014} {}", status.accessibility.symbol(), status.accessibility.label());
                    mi.mic_perm_item.set_text(&mic_label);
                    mi.mic_perm_item.set_enabled(status.microphone != crate::permissions::PermState::Granted);
                    mi.acc_perm_item.set_text(&acc_label);
                    mi.acc_perm_item.set_enabled(status.accessibility != crate::permissions::PermState::Granted);
                    mi.perms_submenu.set_text(
                        if status.all_granted() { "Permissions \u{2713}" } else { "\u{26a0} Permissions" }
                    );
                    if let Some(ref w) = mi.warn_item {
                        if status.all_granted() {
                            w.set_text("\u{2713} All permissions granted");
                        } else {
                            w.set_text(&format!("\u{26a0} {} permission(s) missing", status.missing_count()));
                        }
                    }
                }
                info!("Permissions refreshed: mic={:?} acc={:?}", status.microphone, status.accessibility);
                // Re-check again in 5s if still not all granted
                if !status.all_granted() {
                    let tx = self.state.tx.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        let _ = tx.send(Event::RefreshPermissions);
                    });
                }
            }

            Event::StateChanged(s) => {
                self.state.set(s);
                set_tray_icon(&self.tray, s);
                if s == State::Idle {
                    if let Some(mi) = &self.menu_items {
                        mi.toggle_item.set_text("Start Recording");
                        mi.toggle_item.set_enabled(true);
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

        let audio = self.capture.lock().unwrap().take().map(|c| c.stop()).unwrap_or_default();
        if audio.len() < 4800 {
            self.state.set(State::Idle);
            mi.toggle_item.set_text("Start Recording");
            mi.toggle_item.set_enabled(true);
            set_tray_icon(&self.tray, State::Idle);
            return;
        }

        let tx = self.state.tx.clone();
        let cfg = self.config.lock().unwrap().clone();
        let backend = match cfg.backend.as_str() {
            "parakeet" => crate::transcribe::Backend::Parakeet,
            "voxtral-local" => crate::transcribe::Backend::VoxtralLocal,
            _ => crate::transcribe::Backend::WhisperLocal(cfg.model.clone()),
        };
        std::thread::spawn(move || {
            match crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend) {
                Ok(text) if !text.is_empty() => { let _ = tx.send(Event::Transcribed(text)); }
                Ok(_) => { let _ = tx.send(Event::StateChanged(State::Idle)); }
                Err(e) => { tracing::error!("Transcription: {e}"); let _ = tx.send(Event::StateChanged(State::Idle)); }
            }
        });
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {}

    fn window_event(
        &mut self, _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId, _event: winit::event::WindowEvent,
    ) {}

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        // Pure Wait — absolutely NO periodic wake-ups.
        // Any wake-up (even WaitUntil) causes macOS to close the tray menu.
        // All hotkey/transcription logic runs in background threads.
        // Menu events are handled in about_to_wait (called after menu closes).
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);

        if cause == winit::event::StartCause::Init {
            // Load model SYNCHRONOUSLY before creating the tray,
            // so the menu starts in "Ready" state and never needs updating.
            // This avoids modifying menu items after creation (which closes
            // the menu on macOS Tahoe).
            let model_name = self.state.config.model.clone();
            match crate::transcribe::load_model(&model_name) {
                Ok(()) => {
                    self.state.set(State::Idle);
                    info!("Model loaded");
                }
                Err(e) => {
                    tracing::error!("Model load failed: {e}");
                    crate::notify::send("Whisper Push", &format!("Model failed: {e}"));
                }
            }

            // Create tray icon AFTER model is loaded (menu is born in "Ready" state)
            self.create_tray();

            // Wake the macOS run loop so the icon appears immediately
            #[cfg(target_os = "macos")]
            unsafe {
                use objc2_core_foundation::{CFRunLoopGetMain, CFRunLoopWakeUp};
                let rl = CFRunLoopGetMain().unwrap();
                CFRunLoopWakeUp(&rl);
            }

            // Start hotkey listener + autonomous pipeline thread.
            // The pipeline runs entirely in background threads — it never
            // touches the winit event loop, so the tray menu stays open.
            let hotkey_cfg = self.state.config.hotkey.clone();
            let hotkey_mode = self.state.config.hotkey_mode.clone();
            let pipeline_cfg = self.config.clone();
            let (htx, hrx) = crossbeam_channel::unbounded();
            let _ = crate::hotkey::start_listener(&hotkey_cfg, &hotkey_mode, htx);

            // Pipeline thread: hotkey events → capture → transcribe → paste
            std::thread::spawn(move || {
                pipeline_loop(hrx, pipeline_cfg);
            });

            // No startup notification — NSUserNotificationCenter delivery
            // can disrupt the macOS run loop and close the tray menu.
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
fn pipeline_loop(rx: Receiver<Event>, config: Arc<Mutex<Config>>) {
    use std::sync::atomic::{AtomicBool, Ordering};

    let hold_pending = Arc::new(AtomicBool::new(false));
    let recording = Arc::new(AtomicBool::new(false));
    let capture: Arc<Mutex<Option<crate::audio::capture::AudioCapture>>> =
        Arc::new(Mutex::new(None));

    loop {
        match rx.recv() {
            Ok(Event::HotkeyDown) => {
                if recording.load(Ordering::Relaxed) { continue; }
                let cfg = config.lock().unwrap();
                let device = cfg.input_device.clone();
                let delay = cfg.hold_delay;
                drop(cfg);

                match crate::audio::capture::AudioCapture::start(&device) {
                    Ok(cap) => {
                        *capture.lock().unwrap() = Some(cap);
                        hold_pending.store(true, Ordering::Relaxed);

                        let pending = hold_pending.clone();
                        let rec = recording.clone();
                        let cfg2 = config.clone();
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_secs_f64(delay));
                            if pending.load(Ordering::Relaxed) {
                                pending.store(false, Ordering::Relaxed);
                                rec.store(true, Ordering::Relaxed);
                                if cfg2.lock().unwrap().sound_feedback {
                                    crate::audio::playback::play_sound("start");
                                }
                                info!("Recording...");
                            }
                        });
                    }
                    Err(e) => warn!("Capture failed: {e}"),
                }
            }

            Ok(Event::HotkeyUp) => {
                if hold_pending.load(Ordering::Relaxed) {
                    // Quick tap — discard
                    hold_pending.store(false, Ordering::Relaxed);
                    capture.lock().unwrap().take();
                    continue;
                }
                if !recording.load(Ordering::Relaxed) { continue; }
                recording.store(false, Ordering::Relaxed);

                let cfg = config.lock().unwrap().clone();
                if cfg.sound_feedback {
                    crate::audio::playback::play_sound("stop");
                }

                let audio = capture.lock().unwrap().take()
                    .map(|c| c.stop())
                    .unwrap_or_default();

                if audio.len() < 4800 {
                    info!("Too short, skipping");
                    continue;
                }

                info!("Processing {:.1}s of audio...", audio.len() as f32 / 16000.0);

                let backend = match cfg.backend.as_str() {
                    "parakeet" => crate::transcribe::Backend::Parakeet,
                    "voxtral-local" => crate::transcribe::Backend::VoxtralLocal,
                    _ => crate::transcribe::Backend::WhisperLocal(cfg.model.clone()),
                };

                match crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend) {
                    Ok(text) if !text.is_empty() => {
                        info!("Pasting: '{}'", if text.len() > 80 { &text[..80] } else { &text });
                        if let Err(e) = crate::paste::paste_text(&text) {
                            tracing::error!("Paste failed: {e}");
                        }
                        if cfg.notifications {
                            let preview = if text.len() > 50 {
                                format!("{}...", &text[..50])
                            } else { text };
                            crate::notify::send("Whisper Push", &format!("Typed: {preview}"));
                        }
                    }
                    Ok(_) => info!("No speech detected"),
                    Err(e) => tracing::error!("Transcription: {e}"),
                }
            }

            Ok(Event::HotkeyToggle) => {
                if !recording.load(Ordering::Relaxed) {
                    // Start recording
                    let device = config.lock().unwrap().input_device.clone();
                    match crate::audio::capture::AudioCapture::start(&device) {
                        Ok(cap) => {
                            *capture.lock().unwrap() = Some(cap);
                            recording.store(true, Ordering::Relaxed);
                            if config.lock().unwrap().sound_feedback {
                                crate::audio::playback::play_sound("start");
                            }
                            info!("Recording (toggle)...");
                        }
                        Err(e) => warn!("Capture failed: {e}"),
                    }
                } else {
                    // Stop and transcribe (same as HotkeyUp)
                    recording.store(false, Ordering::Relaxed);
                    let cfg = config.lock().unwrap().clone();
                    if cfg.sound_feedback {
                        crate::audio::playback::play_sound("stop");
                    }
                    let audio = capture.lock().unwrap().take()
                        .map(|c| c.stop())
                        .unwrap_or_default();
                    if audio.len() < 4800 { continue; }

                    let backend = match cfg.backend.as_str() {
                        "parakeet" => crate::transcribe::Backend::Parakeet,
                        "voxtral-local" => crate::transcribe::Backend::VoxtralLocal,
                        _ => crate::transcribe::Backend::WhisperLocal(cfg.model.clone()),
                    };
                    match crate::transcribe::transcribe_with_backend(&audio, &cfg.language, &backend) {
                        Ok(text) if !text.is_empty() => {
                            let _ = crate::paste::paste_text(&text);
                            if cfg.notifications {
                                crate::notify::send("Whisper Push", &format!("Typed: {}", &text));
                            }
                        }
                        Ok(_) => info!("No speech"),
                        Err(e) => tracing::error!("Transcription: {e}"),
                    }
                }
            }

            Ok(_) => {} // Ignore other events
            Err(_) => break,
        }
    }
}

fn set_tray_icon(tray: &Option<TrayIcon>, state: State) {
    let data = match state {
        State::Loading | State::Processing => ICON_PROCESSING,
        State::Idle => ICON_IDLE,
        State::Recording => ICON_RECORDING,
    };
    if let (Some(tray), Some(icon)) = (tray, load_icon(data)) {
        let _ = tray.set_icon(Some(icon));
    }
}

fn format_hotkey_display(hotkey: &str, mode: &str) -> String {
    let symbols: &[(&str, &str)] = &[
        ("cmd", "\u{2318}"), ("shift", "\u{21e7}"), ("alt", "\u{2325}"), ("ctrl", "\u{2303}"),
        ("rctrl", "\u{2303}R"), ("rcmd", "\u{2318}R"), ("ralt", "\u{2325}R"), ("space", "Space"),
    ];
    let mut r = if mode == "hold" { "Hold ".into() } else { String::new() };
    for (i, p) in hotkey.to_lowercase().split('+').enumerate() {
        let p = p.trim();
        if i > 0 { r.push('+'); }
        if let Some((_, s)) = symbols.iter().find(|(k, _)| *k == p) { r.push_str(s); }
        else { r.push_str(&p.to_uppercase()); }
    }
    r
}
