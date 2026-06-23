//! Recent-dictation history.
//!
//! The last runs are kept in memory and mirrored to a plain-text file
//! (`history.txt`, beside `config.toml`) so the user can find and re-copy past
//! transcriptions. One escaped line per run (newest last), capped at
//! [`MAX_ENTRIES`]. Loading + persistence are best-effort: history is a
//! convenience, never load-bearing, so any IO error is silently ignored.

use crate::util::LockSafe;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// How many runs we keep (in memory + on disk).
const MAX_ENTRIES: usize = 50;

static ENTRIES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// Set when the list changes, so the tray refreshes its History submenu.
static DIRTY: AtomicBool = AtomicBool::new(false);
static LOADED: AtomicBool = AtomicBool::new(false);

/// `history.txt`, beside `config.toml`.
pub fn file_path() -> PathBuf {
    crate::config::config_path()
        .parent()
        .map(|p| p.join("history.txt"))
        .unwrap_or_else(|| PathBuf::from("history.txt"))
}

/// Flatten a (possibly multi-line) entry to one round-trippable line.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\r', "").replace('\n', "\\n")
}

/// Inverse of [`escape`].
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Load the file into memory once (lazily, on first access).
fn ensure_loaded() {
    if LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Ok(content) = std::fs::read_to_string(file_path()) {
        let mut g = ENTRIES.lock_safe();
        for line in content.lines() {
            if !line.is_empty() {
                g.push_back(unescape(line));
            }
        }
        while g.len() > MAX_ENTRIES {
            g.pop_front();
        }
    }
}

/// Record a finished dictation (newest at the back). No-op for empty text.
pub fn record(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    ensure_loaded();
    let snapshot = {
        let mut g = ENTRIES.lock_safe();
        g.push_back(text.to_string());
        while g.len() > MAX_ENTRIES {
            g.pop_front();
        }
        g.iter().map(|e| escape(e)).collect::<Vec<_>>().join("\n")
    };
    let path = file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{snapshot}\n"));
    DIRTY.store(true, Ordering::Relaxed);
}

/// Recent runs, newest first (at most [`MAX_ENTRIES`]).
pub fn recent() -> Vec<String> {
    ensure_loaded();
    ENTRIES.lock_safe().iter().rev().cloned().collect()
}

/// Wipe the history (memory + file).
pub fn clear() {
    ENTRIES.lock_safe().clear();
    let _ = std::fs::remove_file(file_path());
    DIRTY.store(true, Ordering::Relaxed);
}

/// True at most once per change — the tray polls this to refresh its submenu.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}
