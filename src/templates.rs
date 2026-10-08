//! Voice templates: a trigger phrase → an expansion.
//!
//! When a dictation matches a trigger (case/space/punctuation-insensitive, whole
//! utterance), the expansion is pasted instead. Stored in `templates.toml`
//! beside `config.toml`; multi-line expansions use TOML triple-quoted strings,
//! so long snippets (signatures, boilerplate) can be edited in the file.

use crate::util::LockSafe;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// (trigger, content), insertion order preserved (first match wins).
static TEMPLATES: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
/// Set when the list changes, so the tray refreshes its Templates submenu.
static DIRTY: AtomicBool = AtomicBool::new(false);
static LOADED: AtomicBool = AtomicBool::new(false);

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct File {
    #[serde(default)]
    template: Vec<Entry>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct Entry {
    trigger: String,
    content: String,
}

/// `templates.toml`, beside `config.toml`.
pub fn file_path() -> PathBuf {
    crate::config::config_path()
        .parent()
        .map(|p| p.join("templates.toml"))
        .unwrap_or_else(|| PathBuf::from("templates.toml"))
}

/// Normalise for matching: lowercase, then strip leading/trailing whitespace and
/// punctuation (the model often appends a period). Internal spacing is kept so
/// multi-word triggers ("my address") still work.
fn norm(s: &str) -> String {
    s.to_lowercase()
        .trim()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace())
        .to_string()
}

fn ensure_loaded() {
    if LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Err(e) = load_from_disk() {
        tracing::warn!("templates.toml: {e}");
    }
}

/// Read `templates.toml` into memory; returns the template count. A missing
/// file is an empty list. One that can't be used — unreadable (not UTF-8) or
/// failing to parse (a hand edit with a typo) — is moved aside to
/// `templates.toml.bad`, like config.toml, and the list in memory is kept:
/// leaving it in place instead let the next Add Template overwrite every
/// template the user had.
fn load_from_disk() -> anyhow::Result<usize> {
    let path = file_path();
    let move_aside = |why: String| {
        let bad = path.with_extension("toml.bad");
        let _ = std::fs::rename(&path, &bad);
        anyhow::anyhow!("{why} (moved aside to {})", bad.display())
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(move_aside(e.to_string())),
    };
    let file = toml::from_str::<File>(&text).map_err(|e| move_aside(e.message().to_string()))?;
    let parsed: Vec<(String, String)> = file
        .template
        .into_iter()
        .filter(|e| !e.trigger.trim().is_empty())
        .map(|e| (e.trigger, e.content))
        .collect();
    let n = parsed.len();
    *TEMPLATES.lock_safe() = parsed;
    DIRTY.store(true, Ordering::Relaxed);
    Ok(n)
}

/// Re-read `templates.toml` from disk (after the user edits it); returns the
/// template count, or why the file couldn't be used.
pub fn reload() -> anyhow::Result<usize> {
    LOADED.store(true, Ordering::Relaxed);
    load_from_disk()
}

/// If `dictation` matches a trigger, the expansion to paste instead.
pub fn expand(dictation: &str) -> Option<String> {
    ensure_loaded();
    let key = norm(dictation);
    if key.is_empty() {
        return None;
    }
    TEMPLATES
        .lock_safe()
        .iter()
        .find(|(t, _)| norm(t) == key)
        .map(|(_, c)| c.clone())
}

/// Add (or replace, by normalised trigger) a template, then persist.
pub fn add(trigger: &str, content: &str) -> anyhow::Result<()> {
    let trigger = trigger.trim();
    if trigger.is_empty() {
        anyhow::bail!("a trigger word is required");
    }
    if content.is_empty() {
        anyhow::bail!("the template content is empty");
    }
    ensure_loaded();
    {
        let key = norm(trigger);
        let mut g = TEMPLATES.lock_safe();
        g.retain(|(t, _)| norm(t) != key);
        g.push((trigger.to_string(), content.to_string()));
    }
    save()
}

/// Remove a template by (normalised) trigger, then persist. Returns true if one
/// was removed.
pub fn remove(trigger: &str) -> anyhow::Result<bool> {
    ensure_loaded();
    let key = norm(trigger);
    let removed = {
        let mut g = TEMPLATES.lock_safe();
        let before = g.len();
        g.retain(|(t, _)| norm(t) != key);
        g.len() != before
    };
    if removed {
        save()?;
    }
    Ok(removed)
}

fn save() -> anyhow::Result<()> {
    let entries: Vec<Entry> = TEMPLATES
        .lock_safe()
        .iter()
        .map(|(t, c)| Entry {
            trigger: t.clone(),
            content: c.clone(),
        })
        .collect();
    let body = toml::to_string_pretty(&File { template: entries })?;
    let path = file_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Atomic write (tmp + rename) so a crash mid-write can't truncate the file.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(
        &tmp,
        format!(
            "# Whisper Push templates — say the trigger to paste the content.\n\
             # Multi-line content uses TOML triple-quotes, e.g.:\n\
             #   [[template]]\n#   trigger = \"signature\"\n#   content = \"\"\"\n#   Best,\n#   Marceau\n#   \"\"\"\n\n{body}"
        ),
    )?;
    std::fs::rename(&tmp, &path)?;
    DIRTY.store(true, Ordering::Relaxed);
    Ok(())
}

/// Ensure `templates.toml` exists (so "Open" works on first run); returns its path.
pub fn ensure_file() -> PathBuf {
    let path = file_path();
    if !path.exists() {
        let _ = save(); // writes the header + whatever's loaded (likely empty)
    }
    path
}

/// The trigger phrases, in order.
pub fn triggers() -> Vec<String> {
    ensure_loaded();
    TEMPLATES
        .lock_safe()
        .iter()
        .map(|(t, _)| t.clone())
        .collect()
}

/// True at most once per change — the tray polls this to refresh its submenu.
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_is_case_space_punct_insensitive() {
        assert_eq!(norm("  Signature. "), "signature");
        assert_eq!(norm("My Address!"), "my address");
        assert_eq!(norm("hello"), norm("Hello,"));
    }
}
