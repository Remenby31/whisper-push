use crate::config::Config;
use anyhow::Result;
use crossbeam_channel::Sender;
use std::sync::{Arc, Mutex};

/// Application states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Loading,
    Idle,
    Recording,
    Processing,
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            State::Loading => write!(f, "Loading model..."),
            State::Idle => write!(f, "Ready"),
            State::Recording => write!(f, "Recording"),
            State::Processing => write!(f, "Processing"),
        }
    }
}

/// Events sent between components.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum Event {
    /// Hotkey pressed (start recording / pre-roll)
    HotkeyDown,
    /// Hotkey released (stop recording in hold mode)
    HotkeyUp,
    /// Hotkey toggled (toggle mode)
    HotkeyToggle,
    /// A custom hotkey was captured (hotkey string, mode)
    HotkeyCaptured(String, String),
    /// Recording captured, ready to transcribe
    AudioCaptured(Vec<f32>),
    /// Transcription result
    Transcribed(String),
    /// Model loaded and ready
    ModelReady,
    /// State changed (for tray icon update)
    StateChanged(State),
    /// Menu item clicked (menu item id string)
    MenuClicked(String),
    /// Prompt for missing permissions (after event loop is running)
    PromptPermissions,
    /// Refresh permission status in the menu
    RefreshPermissions,
    /// Request quit
    Quit,
}

/// Shared application state.
pub struct AppState {
    pub config: Config,
    pub state: Arc<Mutex<State>>,
    pub tx: Sender<Event>,
}

impl AppState {
    pub fn new(config: Config, tx: Sender<Event>) -> Self {
        Self {
            config,
            state: Arc::new(Mutex::new(State::Loading)),
            tx,
        }
    }

    pub fn current(&self) -> State {
        *self.state.lock().unwrap()
    }

    pub fn set(&self, new_state: State) {
        let mut s = self.state.lock().unwrap();
        if *s != new_state {
            *s = new_state;
            let _ = self.tx.send(Event::StateChanged(new_state));
        }
    }
}

/// Acquire single-instance lock. Returns the file handle (must be kept alive).
pub fn acquire_lock() -> Result<std::fs::File> {
    let lock_path = crate::config::data_dir().join("whisper-push.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&lock_path)?;
    use fs4::fs_std::FileExt;
    file.try_lock_exclusive()
        .map_err(|_| anyhow::anyhow!("Another instance is already running"))?;
    Ok(file)
}

pub fn current_status() -> String {
    // Simple check: is the lock file held?
    let lock_path = crate::config::data_dir().join("whisper-push.lock");
    if lock_path.exists() {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path);
        match file {
            Ok(f) => {
                use fs4::fs_std::FileExt;
                match f.try_lock_exclusive() {
                    Ok(_) => "Idle (no instance running)".into(),
                    Err(_) => "Running".into(),
                }
            }
            Err(_) => "Unknown".into(),
        }
    } else {
        "Idle (no instance running)".into()
    }
}

pub fn force_stop() {
    // Send SIGTERM to running instance (Unix) or equivalent
    tracing::info!("Force stop requested");
    // For now, just print — will implement proper IPC later
    println!("Stop signal sent");
}
