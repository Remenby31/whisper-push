use crate::util::LockSafe;
use crate::config::Config;
use anyhow::Result;
use crossbeam_channel::Sender;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The dictionary changed (add/remove/correct/reload) — refresh its submenu
    DictChanged,
    /// The license state changed (activate/deactivate) — refresh its submenu
    LicenseChanged,
    /// Trailing tray-icon refresh: re-apply the current state's icon on the main
    /// thread after a coalesced burst (see `tray::set_tray_icon`).
    RefreshTrayIcon,
    /// Load model on the pipeline thread (needed for WGPU same-thread requirement)
    LoadModel(String),
    /// A new version is available (version, download_url)
    UpdateAvailable(String, String),
    /// Update download/install failed (error message)
    UpdateFailed(String),
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
        *self.state.lock_safe()
    }

    pub fn set(&self, new_state: State) {
        let mut s = self.state.lock_safe();
        if *s != new_state {
            *s = new_state;
            // Stamp when we entered Processing (0 otherwise) so the watchdog can
            // detect a wedge — a lost Processing→Idle transition that would
            // otherwise leave the app silently refusing dictations.
            PROCESSING_SINCE.store(
                if new_state == State::Processing {
                    now_secs()
                } else {
                    0
                },
                Ordering::Relaxed,
            );
            let _ = self.tx.send(Event::StateChanged(new_state));
        }
    }
}

/// Unix-secs since the app entered `Processing` (0 = not processing).
static PROCESSING_SINCE: AtomicU64 = AtomicU64::new(0);

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How long (secs) the app has been stuck in `Processing`, or `None` if it isn't
/// processing. A real transcription finishes in well under 10 s, so a large
/// value means the state machine wedged (see `tray`'s watchdog).
pub fn processing_stuck_secs() -> Option<u64> {
    match PROCESSING_SINCE.load(Ordering::Relaxed) {
        0 => None,
        since => Some(now_secs().saturating_sub(since)),
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
        let file = std::fs::OpenOptions::new().write(true).open(&lock_path);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_display() {
        assert_eq!(format!("{}", State::Loading), "Loading model...");
        assert_eq!(format!("{}", State::Idle), "Ready");
        assert_eq!(format!("{}", State::Recording), "Recording");
        assert_eq!(format!("{}", State::Processing), "Processing");
    }

    #[test]
    fn test_state_starts_loading() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let state = AppState::new(Config::default(), tx);
        assert_eq!(state.current(), State::Loading);
    }

    #[test]
    fn test_state_transitions() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let state = AppState::new(Config::default(), tx);

        state.set(State::Idle);
        assert_eq!(state.current(), State::Idle);
        // Should have emitted a StateChanged event
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, Event::StateChanged(State::Idle)));

        state.set(State::Recording);
        assert_eq!(state.current(), State::Recording);

        state.set(State::Processing);
        assert_eq!(state.current(), State::Processing);

        state.set(State::Idle);
        assert_eq!(state.current(), State::Idle);
    }

    #[test]
    fn test_state_no_duplicate_events() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let state = AppState::new(Config::default(), tx);

        state.set(State::Idle);
        state.set(State::Idle); // same state — should not emit

        // Only one event
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }
}
