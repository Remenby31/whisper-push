//! Floating overlay window — shows live transcription text.
//! Appears at the bottom-right of the screen during recording.

use std::sync::{Arc, Mutex};
use tracing::info;

/// Shared overlay state — updated from the transcription thread,
/// rendered by the overlay window.
#[derive(Clone)]
pub struct OverlayState {
    inner: Arc<Mutex<OverlayInner>>,
}

struct OverlayInner {
    text: String,
    visible: bool,
}

impl OverlayState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(OverlayInner {
                text: String::new(),
                visible: false,
            })),
        }
    }

    /// Show the overlay with initial text.
    pub fn show(&self, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.text = text.to_string();
        inner.visible = true;
        show_native_overlay(&inner.text);
    }

    /// Update the text (appends new words).
    pub fn update_text(&self, text: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.text = text.to_string();
        if inner.visible {
            update_native_overlay(&inner.text);
        }
    }

    /// Append text to existing.
    pub fn append_text(&self, new_text: &str) {
        let mut inner = self.inner.lock().unwrap();
        if !inner.text.is_empty() {
            inner.text.push(' ');
        }
        inner.text.push_str(new_text);
        if inner.visible {
            update_native_overlay(&inner.text);
        }
    }

    /// Hide the overlay.
    pub fn hide(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.visible = false;
        inner.text.clear();
        hide_native_overlay();
    }

    /// Get current text.
    pub fn text(&self) -> String {
        self.inner.lock().unwrap().text.clone()
    }

    /// Check if visible.
    pub fn is_visible(&self) -> bool {
        self.inner.lock().unwrap().visible
    }
}

// ── macOS Native Overlay (NSPanel) ──────────────────────────────

#[cfg(target_os = "macos")]
fn show_native_overlay(text: &str) {
    // Use osascript to show a HUD-style notification overlay
    // This is a simple approach; a proper NSPanel would be better but requires
    // more AppKit integration. The osascript approach works without additional code.
    let script = format!(
        r#"
        tell application "System Events"
            display notification "{}" with title "🎙 Recording..."
        end tell
        "#,
        text.replace('"', r#"\""#),
    );
    let _ = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    info!("Overlay shown");
}

#[cfg(target_os = "macos")]
fn update_native_overlay(text: &str) {
    // macOS notifications can't be updated in-place easily.
    // For a real streaming overlay, we'd need NSPanel or a HUD window.
    // For now, we just log — the tray icon state indicates recording.
    info!("Overlay text: {text}");
}

#[cfg(target_os = "macos")]
fn hide_native_overlay() {
    info!("Overlay hidden");
}

// ── Linux / Windows stubs ───────────────────────────────────────

#[cfg(not(target_os = "macos"))]
fn show_native_overlay(text: &str) {
    info!("Overlay shown: {text}");
}

#[cfg(not(target_os = "macos"))]
fn update_native_overlay(text: &str) {
    info!("Overlay text: {text}");
}

#[cfg(not(target_os = "macos"))]
fn hide_native_overlay() {
    info!("Overlay hidden");
}
