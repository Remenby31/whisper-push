use anyhow::Result;
use tracing::{info, warn};

/// Paste text at the cursor position.
/// Saves clipboard → sets text → simulates Cmd/Ctrl+V → restores clipboard.
pub fn paste_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let mut clipboard = arboard::Clipboard::new()?;

    // Save current clipboard content
    let saved = clipboard.get_text().ok();

    // Set our text
    clipboard.set_text(text)?;

    // Small delay for clipboard to be ready
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Simulate paste keystroke
    simulate_paste()?;

    // Wait for the paste to be consumed
    std::thread::sleep(std::time::Duration::from_millis(150));

    // Restore previous clipboard
    if let Some(old) = saved {
        // Brief delay before restoring
        std::thread::sleep(std::time::Duration::from_millis(100));
        if let Err(e) = clipboard.set_text(&old) {
            warn!("Could not restore clipboard: {e}");
        }
    }

    info!("Pasted {} chars", text.len());
    Ok(())
}

/// Simulate Cmd+V (macOS) or Ctrl+V (Linux/Windows) keystroke.
fn simulate_paste() -> Result<()> {
    use enigo::{Enigo, Key, Keyboard, Settings};

    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow::anyhow!("Failed to create input simulator: {e}"))?;

    #[cfg(target_os = "macos")]
    {
        enigo.key(Key::Meta, enigo::Direction::Press)
            .map_err(|e| anyhow::anyhow!("Key press failed: {e}"))?;
        enigo.key(Key::Unicode('v'), enigo::Direction::Click)
            .map_err(|e| anyhow::anyhow!("Key click failed: {e}"))?;
        enigo.key(Key::Meta, enigo::Direction::Release)
            .map_err(|e| anyhow::anyhow!("Key release failed: {e}"))?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        enigo.key(Key::Control, enigo::Direction::Press)
            .map_err(|e| anyhow::anyhow!("Key press failed: {e}"))?;
        enigo.key(Key::Unicode('v'), enigo::Direction::Click)
            .map_err(|e| anyhow::anyhow!("Key click failed: {e}"))?;
        enigo.key(Key::Control, enigo::Direction::Release)
            .map_err(|e| anyhow::anyhow!("Key release failed: {e}"))?;
    }

    Ok(())
}
