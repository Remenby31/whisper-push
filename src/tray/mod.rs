use crate::audio::capture::AudioCapture;
use crate::config::Config;
use crate::state::{AppState, Event, State};
use anyhow::Result;
use crossbeam_channel::Receiver;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu, CheckMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use tracing::{info, warn};

/// Run the tray icon + event loop. Blocks on main thread.
pub fn run(state: AppState, rx: Receiver<Event>) -> Result<()> {
    info!("Starting system tray...");

    // Build menu
    let status_item = MenuItem::new("Whisper Push — Loading...", false, None);
    let toggle_item = MenuItem::new("Loading model...", false, None);
    let sep1 = PredefinedMenuItem::separator();
    let notifications_item = CheckMenuItem::new("Notifications", true, state.config.notifications, None);
    let sound_item = CheckMenuItem::new("Sound Feedback", true, state.config.sound_feedback, None);
    let debug_item = CheckMenuItem::new("Debug Logging", true, state.config.debug, None);
    let sep2 = PredefinedMenuItem::separator();
    let config_item = MenuItem::new("Open Config...", true, None);
    let quit_item = MenuItem::new("Quit Whisper Push", true, None);

    let menu = Menu::new();
    menu.append(&status_item)?;
    menu.append(&sep1)?;
    menu.append(&toggle_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&notifications_item)?;
    menu.append(&sound_item)?;
    menu.append(&debug_item)?;
    menu.append(&sep2)?;
    menu.append(&config_item)?;
    menu.append(&quit_item)?;

    // Create tray icon (use a simple text icon for now — can be replaced with PNG later)
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("Whisper Push")
        .with_title("◆") // macOS menu bar text
        .build()?;

    // Store tray handle for icon updates
    let tray = Arc::new(Mutex::new(tray));

    // Capture handle for hold-to-talk
    let capture: Arc<Mutex<Option<AudioCapture>>> = Arc::new(Mutex::new(None));

    // Hold mode state
    let hold_pending = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hold_delay = state.config.hold_delay;

    // Load model in background
    let model_name = state.config.model.clone();
    let tx_model = state.tx.clone();
    std::thread::spawn(move || {
        match crate::transcribe::load_model(&model_name) {
            Ok(()) => { let _ = tx_model.send(Event::ModelReady); }
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

    // IDs for menu event matching
    let toggle_id = toggle_item.id().clone();
    let config_id = config_item.id().clone();
    let quit_id = quit_item.id().clone();
    let notif_id = notifications_item.id().clone();
    let sound_id = sound_item.id().clone();
    let debug_id = debug_item.id().clone();

    // Config clone for mutations
    let config = Arc::new(Mutex::new(state.config.clone()));

    // Process menu events in a separate thread
    let tx_menu = state.tx.clone();
    let config_menu = config.clone();
    std::thread::spawn(move || {
        loop {
            if let Ok(event) = MenuEvent::receiver().recv() {
                if event.id() == &quit_id {
                    let _ = tx_menu.send(Event::Quit);
                } else if event.id() == &toggle_id {
                    let _ = tx_menu.send(Event::HotkeyToggle);
                } else if event.id() == &config_id {
                    let path = crate::config::config_path();
                    #[cfg(target_os = "macos")]
                    { let _ = std::process::Command::new("open").arg(&path).spawn(); }
                    #[cfg(target_os = "linux")]
                    { let _ = std::process::Command::new("xdg-open").arg(&path).spawn(); }
                    #[cfg(target_os = "windows")]
                    { let _ = std::process::Command::new("notepad").arg(&path).spawn(); }
                } else if event.id() == &notif_id {
                    let mut cfg = config_menu.lock().unwrap();
                    cfg.notifications = !cfg.notifications;
                    let _ = cfg.save();
                } else if event.id() == &sound_id {
                    let mut cfg = config_menu.lock().unwrap();
                    cfg.sound_feedback = !cfg.sound_feedback;
                    let _ = cfg.save();
                } else if event.id() == &debug_id {
                    let mut cfg = config_menu.lock().unwrap();
                    cfg.debug = !cfg.debug;
                    let _ = cfg.save();
                }
            }
        }
    });

    // Main event loop
    let cfg = config.clone();
    loop {
        match rx.recv() {
            Ok(Event::ModelReady) => {
                state.set(State::Idle);
                toggle_item.set_text("Start Recording");
                toggle_item.set_enabled(true);
                let hotkey_display = format_hotkey_display(&state.config.hotkey, &state.config.hotkey_mode);
                status_item.set_text(&format!("Whisper Push ({hotkey_display})"));
                update_tray_title(&tray, State::Idle);
                if cfg.lock().unwrap().notifications {
                    crate::notify::send("Whisper Push", "Model loaded and ready!");
                }
                info!("Ready — listening for hotkey");
            }

            Ok(Event::HotkeyDown) => {
                if state.current() != State::Idle { continue; }

                // Pre-roll: start capturing immediately
                let device = cfg.lock().unwrap().input_device.clone();
                match AudioCapture::start(&device) {
                    Ok(cap) => {
                        *capture.lock().unwrap() = Some(cap);
                        hold_pending.store(true, std::sync::atomic::Ordering::Relaxed);

                        // Hold delay timer: if key is still held after delay, commit
                        let pending = hold_pending.clone();
                        let tx_delay = state.tx.clone();
                        let delay_ms = (hold_delay * 1000.0) as u64;
                        std::thread::spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                            if pending.load(std::sync::atomic::Ordering::Relaxed) {
                                pending.store(false, std::sync::atomic::Ordering::Relaxed);
                                let _ = tx_delay.send(Event::StateChanged(State::Recording));
                            }
                        });
                    }
                    Err(e) => warn!("Failed to start capture: {e}"),
                }
            }

            Ok(Event::HotkeyUp) => {
                if hold_pending.load(std::sync::atomic::Ordering::Relaxed) {
                    // Released during hold delay — discard pre-roll
                    hold_pending.store(false, std::sync::atomic::Ordering::Relaxed);
                    if let Some(cap) = capture.lock().unwrap().take() {
                        drop(cap);
                    }
                    info!("Quick tap — discarded");
                    continue;
                }

                if state.current() != State::Recording { continue; }

                // Stop recording and transcribe
                state.set(State::Processing);
                toggle_item.set_text("Processing...");
                toggle_item.set_enabled(false);
                update_tray_title(&tray, State::Processing);

                if cfg.lock().unwrap().sound_feedback {
                    crate::audio::playback::play_sound("stop");
                }

                let audio = capture.lock().unwrap().take()
                    .map(|c| c.stop())
                    .unwrap_or_default();

                if audio.len() < 4800 {
                    // Less than 0.3s — skip
                    info!("Audio too short ({:.1}s), skipping", audio.len() as f32 / 16000.0);
                    state.set(State::Idle);
                    toggle_item.set_text("Start Recording");
                    toggle_item.set_enabled(true);
                    update_tray_title(&tray, State::Idle);
                    continue;
                }

                let tx_transcribe = state.tx.clone();
                let lang = cfg.lock().unwrap().language.clone();
                std::thread::spawn(move || {
                    match crate::transcribe::transcribe(&audio, &lang) {
                        Ok(text) if !text.is_empty() => {
                            let _ = tx_transcribe.send(Event::Transcribed(text));
                        }
                        Ok(_) => {
                            info!("No speech detected");
                            let _ = tx_transcribe.send(Event::StateChanged(State::Idle));
                        }
                        Err(e) => {
                            tracing::error!("Transcription error: {e}");
                            let _ = tx_transcribe.send(Event::StateChanged(State::Idle));
                        }
                    }
                });
            }

            Ok(Event::HotkeyToggle) => {
                match state.current() {
                    State::Idle => {
                        // Start recording
                        let device = cfg.lock().unwrap().input_device.clone();
                        match AudioCapture::start(&device) {
                            Ok(cap) => {
                                *capture.lock().unwrap() = Some(cap);
                                state.set(State::Recording);
                                toggle_item.set_text("Stop & Transcribe");
                                update_tray_title(&tray, State::Recording);
                                if cfg.lock().unwrap().sound_feedback {
                                    crate::audio::playback::play_sound("start");
                                }
                                info!("Recording started (toggle mode)");
                            }
                            Err(e) => warn!("Failed to start capture: {e}"),
                        }
                    }
                    State::Recording => {
                        // Stop and transcribe
                        state.set(State::Processing);
                        toggle_item.set_text("Processing...");
                        toggle_item.set_enabled(false);
                        update_tray_title(&tray, State::Processing);

                        if cfg.lock().unwrap().sound_feedback {
                            crate::audio::playback::play_sound("stop");
                        }

                        let audio = capture.lock().unwrap().take()
                            .map(|c| c.stop())
                            .unwrap_or_default();

                        let tx_t = state.tx.clone();
                        let lang = cfg.lock().unwrap().language.clone();
                        std::thread::spawn(move || {
                            match crate::transcribe::transcribe(&audio, &lang) {
                                Ok(text) if !text.is_empty() => {
                                    let _ = tx_t.send(Event::Transcribed(text));
                                }
                                Ok(_) => {
                                    let _ = tx_t.send(Event::StateChanged(State::Idle));
                                }
                                Err(e) => {
                                    tracing::error!("Transcription error: {e}");
                                    let _ = tx_t.send(Event::StateChanged(State::Idle));
                                }
                            }
                        });
                    }
                    _ => {}
                }
            }

            Ok(Event::Transcribed(text)) => {
                info!("Pasting: '{}'", if text.len() > 80 { &text[..80] } else { &text });
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
                update_tray_title(&tray, State::Idle);
            }

            Ok(Event::StateChanged(State::Recording)) => {
                // Hold confirmed (after delay) — commit the recording
                state.set(State::Recording);
                toggle_item.set_text("Recording... (release to stop)");
                update_tray_title(&tray, State::Recording);
                if cfg.lock().unwrap().sound_feedback {
                    crate::audio::playback::play_sound("start");
                }
                info!("Hold confirmed — recording");
            }

            Ok(Event::StateChanged(new_state)) => {
                state.set(new_state);
                update_tray_title(&tray, new_state);
                match new_state {
                    State::Idle => {
                        toggle_item.set_text("Start Recording");
                        toggle_item.set_enabled(true);
                    }
                    _ => {}
                }
            }

            Ok(Event::AudioCaptured(_)) => {
                // Handled inline above
            }

            Ok(Event::Quit) => {
                info!("Quitting...");
                crate::transcribe::unload_model();
                break;
            }

            Err(_) => break,
        }
    }

    Ok(())
}

fn update_tray_title(tray: &Arc<Mutex<TrayIcon>>, state: State) {
    let title = match state {
        State::Loading => "◐",
        State::Idle => "◆",
        State::Recording => "●",
        State::Processing => "◐",
    };
    if let Ok(t) = tray.lock() {
        t.set_title(Some(title));
    }
}

/// Format hotkey for display.
fn format_hotkey_display(hotkey: &str, mode: &str) -> String {
    let symbols: &[(&str, &str)] = &[
        ("cmd", "⌘"), ("command", "⌘"),
        ("shift", "⇧"),
        ("alt", "⌥"), ("option", "⌥"),
        ("ctrl", "⌃"), ("control", "⌃"),
        ("lctrl", "⌃L"), ("rctrl", "⌃R"),
        ("lshift", "⇧L"), ("rshift", "⇧R"),
        ("lalt", "⌥L"), ("ralt", "⌥R"),
        ("lcmd", "⌘L"), ("rcmd", "⌘R"),
        ("space", "Space"),
    ];

    let mut result = String::new();
    if mode == "hold" {
        result.push_str("Hold ");
    }

    for (i, part) in hotkey.to_lowercase().split('+').enumerate() {
        let part = part.trim();
        if i > 0 { result.push('+'); }
        if let Some((_, sym)) = symbols.iter().find(|(k, _)| *k == part) {
            result.push_str(sym);
        } else {
            result.push_str(&part.to_uppercase());
        }
    }
    result
}
