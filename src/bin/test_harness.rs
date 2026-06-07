//! E2E test harness for whisper-push.
//!
//! Provides CLI commands to simulate hotkeys (CGEvent), play audio to a device
//! (sox), and verify results by tailing the app log file.
//!
//! Usage:
//!   whisper-push-test hotkey-down ctrl
//!   whisper-push-test hotkey-up ctrl
//!   whisper-push-test hotkey-hold ctrl 3.0
//!   whisper-push-test play-to "BlackHole 2ch" /tmp/test.wav
//!   whisper-push-test wait-log "Pasting" 30
//!   whisper-push-test check-log "Pasting"

use clap::{Parser, Subcommand};
use std::process;

#[derive(Parser)]
#[command(
    name = "whisper-push-test",
    about = "E2E test harness for whisper-push"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Post a CGEvent key-down for the given key (e.g. "ctrl", "rctrl", "cmd+shift+a")
    HotkeyDown { key: String },
    /// Post a CGEvent key-up for the given key
    HotkeyUp { key: String },
    /// Hold a key for a duration: key-down, sleep, key-up
    HotkeyHold { key: String, secs: f64 },
    /// Play a WAV file to a CoreAudio device via sox
    PlayTo { device: String, wav: String },
    /// Tail today's log file until a pattern matches (exit 0) or timeout (exit 1)
    WaitLog { pattern: String, timeout: u64 },
    /// Check if a pattern exists in today's log file (exit 0 = found, exit 1 = not found)
    CheckLog { pattern: String },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::HotkeyDown { key } => post_hotkey(&key, true),
        Command::HotkeyUp { key } => post_hotkey(&key, false),
        Command::HotkeyHold { key, secs } => hotkey_hold(&key, secs),
        Command::PlayTo { device, wav } => play_to(&device, &wav),
        Command::WaitLog { pattern, timeout } => wait_log(&pattern, timeout),
        Command::CheckLog { pattern } => check_log(&pattern),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}

// ─── Keycodes (mirrors src/hotkey/macos.rs) ─────────────────────────────────

const KEYCODE_LCTRL: u16 = 59;
const KEYCODE_RCTRL: u16 = 62;
const KEYCODE_LSHIFT: u16 = 56;
const KEYCODE_RSHIFT: u16 = 60;
const KEYCODE_LALT: u16 = 58;
const KEYCODE_RALT: u16 = 61;
const KEYCODE_LCMD: u16 = 55;
const KEYCODE_RCMD: u16 = 54;

const NAMED_KEYS: &[(&str, u16)] = &[
    ("a", 0),
    ("b", 11),
    ("c", 8),
    ("d", 2),
    ("e", 14),
    ("f", 3),
    ("g", 5),
    ("h", 4),
    ("i", 34),
    ("j", 38),
    ("k", 40),
    ("l", 37),
    ("m", 46),
    ("n", 45),
    ("o", 31),
    ("p", 35),
    ("q", 12),
    ("r", 15),
    ("s", 1),
    ("t", 17),
    ("u", 32),
    ("v", 9),
    ("w", 13),
    ("x", 7),
    ("y", 16),
    ("z", 6),
    ("space", 49),
    ("return", 36),
    ("tab", 48),
    ("escape", 53),
];

struct ParsedKey {
    keycode: u16,
    #[cfg(target_os = "macos")]
    flags: core_graphics::event::CGEventFlags,
}

fn parse_key(spec: &str) -> Result<ParsedKey, String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = spec;
        Err("hotkey commands are macOS-only".into())
    }

    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::CGEventFlags;

        let parts: Vec<&str> = spec.split('+').collect();
        let mut flags = CGEventFlags::CGEventFlagNull;
        let mut keycode: Option<u16> = None;

        for part in &parts {
            let p = part.to_lowercase();
            match p.as_str() {
                "ctrl" | "lctrl" => {
                    flags |= CGEventFlags::CGEventFlagControl;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_LCTRL);
                    }
                }
                "rctrl" => {
                    flags |= CGEventFlags::CGEventFlagControl;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_RCTRL);
                    }
                }
                "shift" | "lshift" => {
                    flags |= CGEventFlags::CGEventFlagShift;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_LSHIFT);
                    }
                }
                "rshift" => {
                    flags |= CGEventFlags::CGEventFlagShift;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_RSHIFT);
                    }
                }
                "alt" | "option" | "lalt" => {
                    flags |= CGEventFlags::CGEventFlagAlternate;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_LALT);
                    }
                }
                "ralt" => {
                    flags |= CGEventFlags::CGEventFlagAlternate;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_RALT);
                    }
                }
                "cmd" | "lcmd" => {
                    flags |= CGEventFlags::CGEventFlagCommand;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_LCMD);
                    }
                }
                "rcmd" => {
                    flags |= CGEventFlags::CGEventFlagCommand;
                    if keycode.is_none() {
                        keycode = Some(KEYCODE_RCMD);
                    }
                }
                other => {
                    if let Some((_, code)) = NAMED_KEYS.iter().find(|(n, _)| *n == other) {
                        keycode = Some(*code);
                    } else {
                        return Err(format!("unknown key: {other}"));
                    }
                }
            }
        }

        let keycode = keycode.ok_or_else(|| "no key specified".to_string())?;
        Ok(ParsedKey { keycode, flags })
    }
}

// ─── CGEvent posting (macOS) ────────────────────────────────────────────────

fn post_hotkey(key: &str, down: bool) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (key, down);
        Err("hotkey commands are macOS-only".into())
    }

    #[cfg(target_os = "macos")]
    {
        use core_graphics::event::{CGEvent, CGEventTapLocation, CGEventType};
        use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

        let parsed = parse_key(key)?;
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .map_err(|_| "failed to create CGEventSource")?;

        // Modifier keys (ctrl, shift, alt, cmd) generate FlagsChanged events,
        // NOT KeyDown/KeyUp. The CGEventTap in the app listens for FlagsChanged,
        // so we must post the right event type.
        let is_modifier = [
            KEYCODE_LCTRL,
            KEYCODE_RCTRL,
            KEYCODE_LSHIFT,
            KEYCODE_RSHIFT,
            KEYCODE_LALT,
            KEYCODE_RALT,
            KEYCODE_LCMD,
            KEYCODE_RCMD,
        ]
        .contains(&parsed.keycode);

        if is_modifier {
            // For modifiers: create a FlagsChanged event.
            // When "down", set the modifier flag; when "up", clear it.
            let event = CGEvent::new_keyboard_event(source, parsed.keycode, down)
                .map_err(|_| "failed to create CGEvent")?;
            // Override the event type to FlagsChanged
            event.set_type(CGEventType::FlagsChanged);
            // Set flags: present when down, absent when up
            if down {
                event.set_flags(parsed.flags);
            } else {
                event.set_flags(core_graphics::event::CGEventFlags::CGEventFlagNull);
            }
            event.post(CGEventTapLocation::HID);
        } else {
            // For regular keys: standard KeyDown/KeyUp
            let event = CGEvent::new_keyboard_event(source, parsed.keycode, down)
                .map_err(|_| "failed to create CGEvent")?;
            event.set_flags(parsed.flags);
            event.post(CGEventTapLocation::HID);
        }

        let dir = if down { "down" } else { "up" };
        eprintln!(
            "posted: {key} {dir} (keycode={}, modifier={is_modifier})",
            parsed.keycode
        );
        Ok(())
    }
}

fn hotkey_hold(key: &str, secs: f64) -> Result<(), String> {
    post_hotkey(key, true)?;
    std::thread::sleep(std::time::Duration::from_secs_f64(secs));
    post_hotkey(key, false)
}

// ─── Audio playback via sox ─────────────────────────────────────────────────

fn play_to(device: &str, wav: &str) -> Result<(), String> {
    let status = process::Command::new("sox")
        .args([wav, "-t", "coreaudio", device])
        .status()
        .map_err(|e| format!("failed to run sox: {e} (install with: brew install sox)"))?;

    if status.success() {
        eprintln!("played {wav} to {device}");
        Ok(())
    } else {
        Err(format!("sox exited with {status}"))
    }
}

// ─── Log inspection ─────────────────────────────────────────────────────────

/// Find the most recently modified whisper-push log file.
/// Handles UTC date rollover by picking the newest file instead of computing today's date.
fn latest_log_path() -> Option<std::path::PathBuf> {
    let log_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("whisper-push")
        .join("logs");

    let mut best: Option<(std::path::PathBuf, std::time::SystemTime)> = None;

    if let Ok(entries) = std::fs::read_dir(&log_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("whisper-push.log.") {
                if let Ok(meta) = path.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if best.as_ref().is_none_or(|(_, t)| modified > *t) {
                            best = Some((path, modified));
                        }
                    }
                }
            }
        }
    }

    best.map(|(p, _)| p)
}

fn wait_log(pattern: &str, timeout: u64) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);

    // Wait for a log file to exist
    let path = loop {
        if let Some(p) = latest_log_path() {
            break p;
        }
        if std::time::Instant::now() > deadline {
            return Err("timeout: no log file found".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };

    eprintln!(
        "watching {} for '{pattern}' (timeout {timeout}s)",
        path.display()
    );

    let mut file =
        std::fs::File::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;

    // Seek to end — only watch new lines
    file.seek(SeekFrom::End(0))
        .map_err(|e| format!("seek failed: {e}"))?;

    let mut reader = BufReader::new(file);
    let mut line = String::new();

    loop {
        if std::time::Instant::now() > deadline {
            return Err(format!(
                "timeout after {timeout}s: pattern '{pattern}' not found"
            ));
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                // No new data — also check if a newer log file appeared (date rollover)
                if let Some(newer) = latest_log_path() {
                    if newer != path {
                        eprintln!("log rolled to {}", newer.display());
                        return wait_log(
                            pattern,
                            deadline.duration_since(std::time::Instant::now()).as_secs(),
                        );
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(_) => {
                if line.contains(pattern) {
                    eprintln!("matched: {}", line.trim());
                    return Ok(());
                }
            }
            Err(e) => return Err(format!("read error: {e}")),
        }
    }
}

fn check_log(pattern: &str) -> Result<(), String> {
    let path = latest_log_path().ok_or("no log file found")?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    if content.contains(pattern) {
        eprintln!("found '{pattern}' in {}", path.display());
        Ok(())
    } else {
        Err(format!(
            "pattern '{pattern}' not found in {}",
            path.display()
        ))
    }
}
