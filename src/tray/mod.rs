use crate::audio::capture::AudioCapture;
use crate::config::Config;
use crate::state::{AppState, Event, State};
use anyhow::Result;
use crossbeam_channel::Receiver;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{
    CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use tracing::{info, warn};


const ICON_IDLE: &[u8] = include_bytes!("../../resources/icons/icon-idle.png");
const ICON_RECORDING: &[u8] = include_bytes!("../../resources/icons/icon-recording.png");
const ICON_PROCESSING: &[u8] = include_bytes!("../../resources/icons/icon-processing.png");

fn load_icon(data: &[u8]) -> Option<Icon> {
    let img = image::load_from_memory(data).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).ok()
}

/// Hotkey presets: (label, hotkey config value, mode)
const HOTKEY_PRESETS: &[(&str, &str, &str)] = &[
    ("Hold — Control", "ctrl", "hold"),
    ("Hold — Right Control", "rctrl", "hold"),
    ("Hold — Right Command", "rcmd", "hold"),
    ("Hold — Right Option", "ralt", "hold"),
    ("Toggle — ⌘⇧Space", "cmd+shift+space", "toggle"),
    ("Toggle — ⌃⇧Space", "ctrl+shift+space", "toggle"),
];

/// Idle unload presets: (label, minutes)
const IDLE_PRESETS: &[(&str, u32)] = &[
    ("Never", 0),
    ("After 5 min", 5),
    ("After 15 min", 15),
    ("After 30 min", 30),
];

pub fn run(state: AppState, rx: Receiver<Event>) -> Result<()> {
    info!("Starting system tray...");

    // Load model in background
    let model_name = state.config.model.clone();
    let tx_model = state.tx.clone();
    std::thread::spawn(move || {
        match crate::transcribe::load_model(&model_name) {
            Ok(()) => {
                let _ = tx_model.send(Event::ModelReady);
            }
            Err(e) => {
                tracing::error!("Failed to load model: {e}");
                crate::notify::send("Whisper Push", &format!("Model load failed: {e}"));
            }
        }
    });

    // Start hotkey listener
    crate::hotkey::start_listener(
        &state.config.hotkey,
        &state.config.hotkey_mode,
        state.tx.clone(),
    )?;

    // Forward menu events to our channel
    let tx_menu = state.tx.clone();
    let menu_rx = MenuEvent::receiver().clone();
    std::thread::spawn(move || {
        loop {
            if let Ok(event) = menu_rx.recv() {
                let _ = tx_menu.send(Event::MenuClicked(event.id().0.to_string()));
            }
        }
    });

    // ── Build menu ──────────────────────────────────────────────

    let status_item = MenuItem::new("Whisper Push — Loading...", false, None);
    let toggle_item = MenuItem::new("Loading model...", false, None);

    // Hotkey submenu
    let hotkey_submenu = Submenu::new("Hotkey", true);
    let mut hotkey_items: Vec<(CheckMenuItem, String, String)> = Vec::new();
    for (label, hotkey, mode) in HOTKEY_PRESETS {
        let checked = *hotkey == state.config.hotkey && *mode == state.config.hotkey_mode;
        let item = CheckMenuItem::new(*label, true, checked, None);
        hotkey_submenu.append(&item)?;
        hotkey_items.push((item, hotkey.to_string(), mode.to_string()));
    }

    // Input device submenu
    let input_submenu = Submenu::new(
        &format!("Input: {}", &state.config.input_device),
        true,
    );
    let input_auto = CheckMenuItem::new("Auto", true, state.config.input_device == "auto", None);
    input_submenu.append(&input_auto)?;
    input_submenu.append(&PredefinedMenuItem::separator())?;
    let mut input_device_items: Vec<(CheckMenuItem, String)> = vec![
        (input_auto, "auto".to_string()),
    ];
    if let Ok(devices) = crate::audio::list_devices() {
        for name in devices {
            let checked = state.config.input_device == name;
            let item = CheckMenuItem::new(&name, true, checked, None);
            input_submenu.append(&item)?;
            input_device_items.push((item, name));
        }
    }

    // Idle unload submenu
    let idle_submenu = Submenu::new("Idle Unload", true);
    let mut idle_items: Vec<(CheckMenuItem, u32)> = Vec::new();
    for (label, minutes) in IDLE_PRESETS {
        let checked = *minutes == state.config.idle_unload_minutes;
        let item = CheckMenuItem::new(*label, true, checked, None);
        idle_submenu.append(&item)?;
        idle_items.push((item, *minutes));
    }

    // Boolean toggles
    let notifications_item =
        CheckMenuItem::new("Notifications", true, state.config.notifications, None);
    let sound_item =
        CheckMenuItem::new("Sound Feedback", true, state.config.sound_feedback, None);
    let debug_item =
        CheckMenuItem::new("Debug Logging", true, state.config.debug, None);

    let quit_item = MenuItem::new("Quit Whisper Push", true, None);

    // Assemble menu
    let menu = Menu::new();
    menu.append(&status_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&toggle_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&hotkey_submenu)?;
    menu.append(&input_submenu)?;
    menu.append(&idle_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&notifications_item)?;
    menu.append(&sound_item)?;
    menu.append(&debug_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    // Collect IDs for event matching
    let toggle_id = toggle_item.id().clone();
    let quit_id = quit_item.id().clone();
    let notif_id = notifications_item.id().clone();
    let sound_id = sound_item.id().clone();
    let debug_id = debug_item.id().clone();

    // Collect hotkey item IDs
    let hotkey_ids: Vec<(String, String, String)> = hotkey_items
        .iter()
        .map(|(item, hk, mode)| (item.id().0.clone(), hk.clone(), mode.clone()))
        .collect();

    // Collect input device item IDs
    let input_ids: Vec<(String, String)> = input_device_items
        .iter()
        .map(|(item, name)| (item.id().0.clone(), name.clone()))
        .collect();

    // Collect idle item IDs
    let idle_ids: Vec<(String, u32)> = idle_items
        .iter()
        .map(|(item, min)| (item.id().0.clone(), *min))
        .collect();

    // Create tray icon
    let icon = load_icon(ICON_IDLE);
    let mut builder = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Whisper Push — Loading...");
    if let Some(ref ico) = icon {
        builder = builder.with_icon(ico.clone());
    }
    let tray = builder.build()?;

    // Shared state
    let capture: Arc<Mutex<Option<AudioCapture>>> = Arc::new(Mutex::new(None));
    let hold_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hold_delay = state.config.hold_delay;
    let config = Arc::new(Mutex::new(state.config.clone()));

    // ── Main thread event loop ────────────────────────────────
    // Leak all the state into a 'static context for the CFRunLoopTimer callback.
    // This is safe because the app runs until exit (the state is never freed).
    let ctx = Box::leak(Box::new(EventLoopCtx {
        state, config, tray, capture, hold_pending, hold_delay,
        toggle_item, status_item,
        toggle_id, quit_id, notif_id, sound_id, debug_id,
        notifications_item, sound_item, debug_item,
        hotkey_ids, hotkey_items, input_ids, input_device_items,
        input_submenu, idle_ids, idle_items, rx,
    }));

    #[cfg(target_os = "macos")]
    {
        use core_foundation::runloop::{CFRunLoop, CFRunLoopTimer, kCFRunLoopCommonModes};
        use core_foundation::date::CFAbsoluteTimeGetCurrent;
        use std::ffi::c_void;

        // C callback for CFRunLoopTimer — polls our event channel
        extern "C" fn timer_callback(_timer: core_foundation::runloop::CFRunLoopTimerRef, info: *mut c_void) {
            let ctx = unsafe { &*(info as *const EventLoopCtx) };
            while let Ok(event) = ctx.rx.try_recv() {
                handle_event_ctx(ctx, event);
            }
        }

        // Create a CFRunLoopTimerContext pointing to our leaked ctx
        let mut timer_ctx = core_foundation_sys::runloop::CFRunLoopTimerContext {
            version: 0,
            info: ctx as *mut EventLoopCtx as *mut c_void,
            retain: None,
            release: None,
            copyDescription: None,
        };

        let timer = unsafe {
            core_foundation::base::TCFType::wrap_under_create_rule(
                core_foundation_sys::runloop::CFRunLoopTimerCreate(
                    core_foundation::base::kCFAllocatorDefault,
                    CFAbsoluteTimeGetCurrent(),
                    0.016,  // 16ms interval
                    0,
                    0,
                    timer_callback,
                    &mut timer_ctx,
                )
            )
        };

        let run_loop = CFRunLoop::get_current();
        unsafe { run_loop.add_timer(&timer, kCFRunLoopCommonModes) };

        // Set activation policy (menu bar app, no dock icon)
        use objc2_app_kit::NSApplication;
        use objc2_foundation::MainThreadMarker;
        let mtm = MainThreadMarker::new().expect("must be on main thread");
        let app = NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);

        // NSApp.run() — blocks forever, handles tray menu popups, NSEvent monitors, etc.
        unsafe { app.run() };
    }

    #[cfg(not(target_os = "macos"))]
    {
        loop {
            match ctx.rx.recv_timeout(std::time::Duration::from_millis(50)) {
                Ok(event) => handle_event_ctx(ctx, event),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
        }
    }

    Ok(())
}

/// All state needed by the event loop, packed into a single struct
/// so we can leak it into a 'static CFRunLoopTimer callback.
struct EventLoopCtx {
    state: AppState,
    config: Arc<Mutex<Config>>,
    tray: TrayIcon,
    capture: Arc<Mutex<Option<AudioCapture>>>,
    hold_pending: Arc<std::sync::atomic::AtomicBool>,
    hold_delay: f64,
    toggle_item: MenuItem,
    status_item: MenuItem,
    toggle_id: tray_icon::menu::MenuId,
    quit_id: tray_icon::menu::MenuId,
    notif_id: tray_icon::menu::MenuId,
    sound_id: tray_icon::menu::MenuId,
    debug_id: tray_icon::menu::MenuId,
    notifications_item: CheckMenuItem,
    sound_item: CheckMenuItem,
    debug_item: CheckMenuItem,
    hotkey_ids: Vec<(String, String, String)>,
    hotkey_items: Vec<(CheckMenuItem, String, String)>,
    input_ids: Vec<(String, String)>,
    input_device_items: Vec<(CheckMenuItem, String)>,
    input_submenu: Submenu,
    idle_ids: Vec<(String, u32)>,
    idle_items: Vec<(CheckMenuItem, u32)>,
    rx: Receiver<Event>,
}

fn handle_event_ctx(ctx: &EventLoopCtx, event: Event) {
    handle_event(
        event, &ctx.state, &ctx.config, &ctx.tray, &ctx.capture,
        &ctx.hold_pending, ctx.hold_delay,
        &ctx.toggle_item, &ctx.status_item,
        &ctx.toggle_id, &ctx.quit_id, &ctx.notif_id, &ctx.sound_id, &ctx.debug_id,
        &ctx.notifications_item, &ctx.sound_item, &ctx.debug_item,
        &ctx.hotkey_ids, &ctx.hotkey_items,
        &ctx.input_ids, &ctx.input_device_items, &ctx.input_submenu,
        &ctx.idle_ids, &ctx.idle_items,
    );
}

#[allow(clippy::too_many_arguments)]
fn handle_event(
    event: Event,
    state: &AppState,
    cfg: &Arc<Mutex<Config>>,
    tray: &TrayIcon,
    capture: &Arc<Mutex<Option<AudioCapture>>>,
    hold_pending: &Arc<std::sync::atomic::AtomicBool>,
    hold_delay: f64,
    toggle_item: &MenuItem,
    status_item: &MenuItem,
    toggle_id: &tray_icon::menu::MenuId,
    quit_id: &tray_icon::menu::MenuId,
    notif_id: &tray_icon::menu::MenuId,
    sound_id: &tray_icon::menu::MenuId,
    debug_id: &tray_icon::menu::MenuId,
    notifications_item: &CheckMenuItem,
    sound_item: &CheckMenuItem,
    debug_item: &CheckMenuItem,
    hotkey_ids: &[(String, String, String)],
    hotkey_items: &[(CheckMenuItem, String, String)],
    input_ids: &[(String, String)],
    input_device_items: &[(CheckMenuItem, String)],
    input_submenu: &Submenu,
    idle_ids: &[(String, u32)],
    idle_items: &[(CheckMenuItem, u32)],
) {
    match event {
        Event::ModelReady => {
            state.set(State::Idle);
            toggle_item.set_text("Start Recording");
            toggle_item.set_enabled(true);
            let disp =
                format_hotkey_display(&state.config.hotkey, &state.config.hotkey_mode);
            status_item.set_text(&format!("Whisper Push ({disp})"));
            set_tray_icon(tray, State::Idle);
            if cfg.lock().unwrap().notifications {
                crate::notify::send("Whisper Push", "Model loaded and ready!");
            }
            info!("Ready — listening for hotkey");
        }

        Event::MenuClicked(ref id) => {
            // Quit
            if id == &quit_id.0 {
                info!("Quit");
                crate::transcribe::unload_model();
                std::process::exit(0);
            }
            // Toggle recording from menu
            if id == &toggle_id.0 {
                let _ = state.tx.send(Event::HotkeyToggle);
                return;
            }
            // Notifications toggle
            if id == &notif_id.0 {
                let mut c = cfg.lock().unwrap();
                c.notifications = !c.notifications;
                let _ = c.save();
                return;
            }
            // Sound toggle
            if id == &sound_id.0 {
                let mut c = cfg.lock().unwrap();
                c.sound_feedback = !c.sound_feedback;
                let _ = c.save();
                return;
            }
            // Debug toggle
            if id == &debug_id.0 {
                let mut c = cfg.lock().unwrap();
                c.debug = !c.debug;
                let _ = c.save();
                return;
            }

            // Hotkey preset selected
            for (item_id, hotkey, mode) in hotkey_ids {
                if id == item_id {
                    info!("Hotkey changed to: {hotkey} ({mode})");
                    let mut c = cfg.lock().unwrap();
                    c.hotkey = hotkey.clone();
                    c.hotkey_mode = mode.clone();
                    let _ = c.save();
                    // Update checkmarks
                    for (item, hk, _) in hotkey_items {
                        item.set_checked(hk == hotkey);
                    }
                    // Update status text
                    let disp = format_hotkey_display(hotkey, mode);
                    status_item.set_text(&format!("Whisper Push ({disp})"));
                    // Note: hotkey listener restart requires app restart for now
                    crate::notify::send(
                        "Whisper Push",
                        "Hotkey changed. Restart the app to apply.",
                    );
                    return;
                }
            }

            // Input device selected
            for (item_id, name) in input_ids {
                if id == item_id {
                    info!("Input device changed to: {name}");
                    let mut c = cfg.lock().unwrap();
                    c.input_device = name.clone();
                    let _ = c.save();
                    // Update checkmarks
                    for (item, n) in input_device_items {
                        item.set_checked(n == name);
                    }
                    // Update submenu title
                    input_submenu.set_text(&format!("Input: {name}"));
                    return;
                }
            }

            // Idle unload selected
            for (item_id, minutes) in idle_ids {
                if id == item_id {
                    info!("Idle unload changed to: {minutes} min");
                    let mut c = cfg.lock().unwrap();
                    c.idle_unload_minutes = *minutes;
                    let _ = c.save();
                    for (item, m) in idle_items {
                        item.set_checked(*m == *minutes);
                    }
                    return;
                }
            }
        }

        Event::HotkeyDown => {
            if state.current() != State::Idle {
                return;
            }
            let device = cfg.lock().unwrap().input_device.clone();
            match AudioCapture::start(&device) {
                Ok(cap) => {
                    *capture.lock().unwrap() = Some(cap);
                    hold_pending.store(true, std::sync::atomic::Ordering::Relaxed);
                    let pending = hold_pending.clone();
                    let tx = state.tx.clone();
                    let delay_ms = (hold_delay * 1000.0) as u64;
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
            if hold_pending.load(std::sync::atomic::Ordering::Relaxed) {
                hold_pending.store(false, std::sync::atomic::Ordering::Relaxed);
                capture.lock().unwrap().take();
                info!("Quick tap — discarded");
                return;
            }
            if state.current() != State::Recording {
                return;
            }
            do_finish(state, cfg, tray, capture, toggle_item);
        }

        Event::HotkeyToggle => match state.current() {
            State::Idle => {
                let device = cfg.lock().unwrap().input_device.clone();
                match AudioCapture::start(&device) {
                    Ok(cap) => {
                        *capture.lock().unwrap() = Some(cap);
                        state.set(State::Recording);
                        toggle_item.set_text("Stop & Transcribe");
                        set_tray_icon(tray, State::Recording);
                        if cfg.lock().unwrap().sound_feedback {
                            crate::audio::playback::play_sound("start");
                        }
                    }
                    Err(e) => warn!("Capture failed: {e}"),
                }
            }
            State::Recording => do_finish(state, cfg, tray, capture, toggle_item),
            _ => {}
        },

        Event::StateChanged(State::Recording) => {
            state.set(State::Recording);
            toggle_item.set_text("Recording...");
            set_tray_icon(tray, State::Recording);
            if cfg.lock().unwrap().sound_feedback {
                crate::audio::playback::play_sound("start");
            }
            info!("Hold confirmed — recording");
        }

        Event::Transcribed(text) => {
            if let Err(e) = crate::paste::paste_text(&text) {
                tracing::error!("Paste failed: {e}");
            }
            if cfg.lock().unwrap().notifications {
                let preview = if text.len() > 50 {
                    format!("{}...", &text[..50])
                } else {
                    text
                };
                crate::notify::send("Whisper Push", &format!("Typed: {preview}"));
            }
            state.set(State::Idle);
            toggle_item.set_text("Start Recording");
            toggle_item.set_enabled(true);
            set_tray_icon(tray, State::Idle);
        }

        Event::StateChanged(s) => {
            state.set(s);
            set_tray_icon(tray, s);
            if s == State::Idle {
                toggle_item.set_text("Start Recording");
                toggle_item.set_enabled(true);
            }
        }

        _ => {}
    }
}

fn do_finish(
    state: &AppState,
    cfg: &Arc<Mutex<Config>>,
    tray: &TrayIcon,
    capture: &Arc<Mutex<Option<AudioCapture>>>,
    toggle_item: &MenuItem,
) {
    state.set(State::Processing);
    toggle_item.set_text("Processing...");
    toggle_item.set_enabled(false);
    set_tray_icon(tray, State::Processing);

    if cfg.lock().unwrap().sound_feedback {
        crate::audio::playback::play_sound("stop");
    }

    let audio = capture
        .lock()
        .unwrap()
        .take()
        .map(|c| c.stop())
        .unwrap_or_default();
    if audio.len() < 4800 {
        info!("Too short, skipping");
        state.set(State::Idle);
        toggle_item.set_text("Start Recording");
        toggle_item.set_enabled(true);
        set_tray_icon(tray, State::Idle);
        return;
    }

    let tx = state.tx.clone();
    let lang = cfg.lock().unwrap().language.clone();
    std::thread::spawn(move || {
        match crate::transcribe::transcribe(&audio, &lang) {
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

fn set_tray_icon(tray: &TrayIcon, state: State) {
    let data = match state {
        State::Loading | State::Processing => ICON_PROCESSING,
        State::Idle => ICON_IDLE,
        State::Recording => ICON_RECORDING,
    };
    if let Some(icon) = load_icon(data) {
        let _ = tray.set_icon(Some(icon));
    }
}

fn format_hotkey_display(hotkey: &str, mode: &str) -> String {
    let symbols: &[(&str, &str)] = &[
        ("cmd", "⌘"),
        ("shift", "⇧"),
        ("alt", "⌥"),
        ("ctrl", "⌃"),
        ("lctrl", "⌃L"),
        ("rctrl", "⌃R"),
        ("rcmd", "⌘R"),
        ("ralt", "⌥R"),
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
