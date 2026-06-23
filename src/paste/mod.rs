use anyhow::Result;
use std::time::Duration;
use tracing::{info, warn};

// These delays sit on the critical path of *every* dictation (between text
// ready and the user seeing it pasted), so they're tuned as low as still-
// reliable. Trimmed from the original 50/150/100 (≈300 ms) → ≈180 ms.
/// After putting our text on the clipboard, before firing Cmd/Ctrl+V — lets the
/// pasteboard propagate. The OS clipboard is ready within a frame; 50 ms was
/// mostly dead latency.
const CLIPBOARD_SETTLE: Duration = Duration::from_millis(20);
/// After firing paste, before touching the clipboard again — lets the target app
/// consume the paste.
const PASTE_CONSUME: Duration = Duration::from_millis(120);
/// A final beat before writing the user's original clipboard back, so the restore
/// can't race the paste the app just read.
const RESTORE_SETTLE: Duration = Duration::from_millis(40);

/// Paste text at the cursor position.
/// Saves clipboard → sets text → simulates Cmd/Ctrl+V → restores clipboard.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    // Auto-capture (Wispr-style): before pasting the new dictation, learn from
    // any edit the user made to the previously-pasted field.
    crate::dictionary::capture_pending_correction();

    let mut clipboard = arboard::Clipboard::new()?;

    // Save current clipboard content
    let saved = clipboard.get_text().ok();

    // Set our text
    clipboard.set_text(text)?;

    // Small delay for clipboard to be ready
    std::thread::sleep(CLIPBOARD_SETTLE);

    // Simulate paste keystroke. Capture the result instead of `?`-ing it so we
    // ALWAYS restore the user's clipboard below — otherwise a failed keystroke
    // would leave their clipboard clobbered with the dictated text.
    let paste_result = simulate_paste();

    // Wait for the paste to be consumed
    std::thread::sleep(PASTE_CONSUME);

    // Restore previous clipboard
    if let Some(old) = saved {
        // Brief delay before restoring
        std::thread::sleep(RESTORE_SETTLE);
        if let Err(e) = clipboard.set_text(&old) {
            warn!("Could not restore clipboard: {e}");
        }
    }

    paste_result?;
    info!("Pasted {} chars", text.len());

    // Snapshot this field so a later edit can be auto-learned.
    crate::dictionary::arm_correction_capture();
    Ok(())
}

/// Type text progressively at the cursor — for streaming transcription.
/// Uses clipboard + Cmd/Ctrl+V for each word (more reliable than character-by-character).
/// Reserved: streaming dictation is disabled (see CLAUDE.md); batch paste is the live path.
#[allow(dead_code)]
pub fn type_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let mut clipboard = arboard::Clipboard::new()?;
    clipboard.set_text(text)?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    simulate_paste()?;
    std::thread::sleep(std::time::Duration::from_millis(30));

    Ok(())
}

/// Simulate Cmd+V (macOS) or Ctrl+V (Linux/Windows) keystroke.
fn simulate_paste() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        // Use CGEvent directly — enigo's TSM calls crash from background threads.
        use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;

        // Key code 9 = 'v'
        let key_down = CGEvent::new_keyboard_event(source.clone(), 9, true)
            .map_err(|_| anyhow::anyhow!("Failed to create key down event"))?;
        key_down.set_flags(CGEventFlags::CGEventFlagCommand);
        key_down.post(CGEventTapLocation::HID);

        std::thread::sleep(std::time::Duration::from_millis(30));

        let key_up = CGEvent::new_keyboard_event(source, 9, false)
            .map_err(|_| anyhow::anyhow!("Failed to create key up event"))?;
        key_up.set_flags(CGEventFlags::CGEventFlagCommand);
        key_up.post(CGEventTapLocation::HID);

        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    {
        use enigo::{Enigo, Key, Keyboard, Settings};
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow::anyhow!("Failed to create input simulator: {e}"))?;
        enigo
            .key(Key::Control, enigo::Direction::Press)
            .map_err(|e| anyhow::anyhow!("Key press failed: {e}"))?;
        enigo
            .key(Key::Unicode('v'), enigo::Direction::Click)
            .map_err(|e| anyhow::anyhow!("Key click failed: {e}"))?;
        enigo
            .key(Key::Control, enigo::Direction::Release)
            .map_err(|e| anyhow::anyhow!("Key release failed: {e}"))?;
        Ok(())
    }
}
