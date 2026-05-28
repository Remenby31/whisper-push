use crate::state::Event;
use crossbeam_channel::Sender;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
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

/// Apply a new hotkey to the running listener immediately (no restart).
#[allow(unused_variables)]
pub fn rebind(hotkey: &str, mode: &str) {
    #[cfg(target_os = "macos")]
    macos::rebind(hotkey, mode);
}

/// Arm capture of the next key combo; the result arrives as
/// `Event::HotkeyCaptured` on `tx`. (macOS only for now.)
#[allow(unused_variables)]
pub fn start_capture(tx: Sender<Event>) {
    #[cfg(target_os = "macos")]
    macos::start_capture(tx);
}
