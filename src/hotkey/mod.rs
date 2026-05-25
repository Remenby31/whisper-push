use crossbeam_channel::Sender;
use crate::state::Event;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

/// Start listening for global hotkey events.
/// Sends HotkeyDown/HotkeyUp (hold mode) or HotkeyToggle (toggle mode) events.
pub fn start_listener(hotkey: &str, mode: &str, tx: Sender<Event>) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    macos::start(hotkey, mode, tx)?;

    #[cfg(target_os = "linux")]
    linux::start(hotkey, mode, tx)?;

    #[cfg(target_os = "windows")]
    windows::start(hotkey, mode, tx)?;

    Ok(())
}
