//! # whisper-push-dict
//!
//! The adaptive-dictation "brain": a persistent, cross-model vocabulary that
//! corrects transcriptions on the way out (no prompts fed to any ASR model) and
//! learns from the user's corrections — without a second local model.
//!
//! Two paths:
//!   * **Hot** ([`finalize_and_record`]) — runs after every transcription, in
//!     well under a millisecond. Deterministic exact rewrites + a heavily
//!     guarded fuzzy layer. See [`finalize`].
//!   * **Cold** ([`correct_last`]) — runs when the user fixes a dictation.
//!     Diffs, classifies (punctual fix vs free-form rewrite), and promotes or
//!     demotes entries. See [`learn`].
//!
//! The crate is pure (no ASR/GPU deps) so its tests compile in ~1–2s.

mod compiled;
mod finalize;
mod learn;
mod model;
mod normalize;
mod phonetic;

pub use compiled::{CommonWords, Compiled};
pub use finalize::{Applied, finalize, finalize_traced};
pub use learn::{EditClass, EditKind, LastDictation, LearnReport, Pair, classify, learn};
pub use model::{Dictionary, Entry, LoadError, Source};
pub use normalize::{key_of, normalize};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::SystemTime;

/// Everything the running daemon shares behind one lock.
struct Shared {
    path: PathBuf,
    /// Last-seen modification time of `path`, for hot-reload detection.
    mtime: Option<SystemTime>,
    dict: Dictionary,
    compiled: Arc<Compiled>,
    common: Arc<CommonWords>,
}

/// Modification time of `path`, or `None` if it doesn't exist / can't stat.
fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

static STATE: OnceLock<RwLock<Shared>> = OnceLock::new();
static LAST: Mutex<Option<LastDictation>> = Mutex::new(None);
/// Transient session-context terms (proper nouns visible on screen / clipboard),
/// matched with a relaxed threshold for the current dictation. See
/// [`set_session_context_from_texts`].
static SESSION: RwLock<Vec<crate::compiled::FuzzyTerm>> = RwLock::new(Vec::new());
/// Runtime on/off for correction (the tray "Adaptive Correction" toggle), so it
/// can be flipped without restarting the daemon.
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turn correction on/off at runtime. When off, [`finalize_and_record`] is a
/// pass-through (it still records the dictation so a manual correction works).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Whether correction is currently enabled.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Poison-tolerant lock helpers (project invariant I5).
fn read_state() -> Option<std::sync::RwLockReadGuard<'static, Shared>> {
    STATE
        .get()
        .map(|l| l.read().unwrap_or_else(|e| e.into_inner()))
}
fn write_state() -> Option<std::sync::RwLockWriteGuard<'static, Shared>> {
    STATE
        .get()
        .map(|l| l.write().unwrap_or_else(|e| e.into_inner()))
}
fn last_slot() -> std::sync::MutexGuard<'static, Option<LastDictation>> {
    LAST.lock().unwrap_or_else(|e| e.into_inner())
}

/// Load `dictionary.toml` from `path` and arm the engine. Safe to call once at
/// startup; subsequent calls reload from disk. A missing file is fine.
pub fn init(path: PathBuf) -> Result<(), LoadError> {
    let dict = Dictionary::load(&path)?;
    let common = Arc::new(CommonWords::builtin());
    let compiled = Arc::new(Compiled::build(&dict, common.clone()));
    let mtime = file_mtime(&path);
    let shared = Shared {
        path,
        mtime,
        dict,
        compiled,
        common,
    };
    match STATE.get() {
        Some(lock) => {
            *lock.write().unwrap_or_else(|e| e.into_inner()) = shared;
        }
        None => {
            let _ = STATE.set(RwLock::new(shared));
        }
    }
    Ok(())
}

/// Whether [`init`] has run.
pub fn is_initialized() -> bool {
    STATE.get().is_some()
}

/// Is `word` an everyday word in `lang`? Used by the acoustic layer to avoid
/// replacing common words by sound. Returns false if not initialized.
pub fn is_common_word(word: &str, lang: &str) -> bool {
    read_state()
        .map(|s| s.common.contains(&normalize(word), lang))
        .unwrap_or(false)
}

/// **Hot-reload.** If `dictionary.toml` changed on disk since we last loaded it
/// (e.g. the `dict` CLI added a word, or the user hand-edited the file), rebuild
/// the compiled tables. Returns whether a reload happened. A single `stat` when
/// nothing changed — cheap enough to call before every dictation.
pub fn reload_if_changed() -> bool {
    let Some(lock) = STATE.get() else {
        return false;
    };
    let (path, stored) = {
        let g = lock.read().unwrap_or_else(|e| e.into_inner());
        (g.path.clone(), g.mtime)
    };
    let disk = file_mtime(&path);
    if disk == stored {
        return false;
    }
    // Changed (or appeared/disappeared) — reload. A parse error leaves the
    // current in-memory dictionary intact (don't break correction on a typo).
    match Dictionary::load(&path) {
        Ok(dict) => {
            let mut g = lock.write().unwrap_or_else(|e| e.into_inner());
            let common = g.common.clone();
            g.compiled = Arc::new(Compiled::build(&dict, common));
            g.dict = dict;
            g.mtime = disk;
            true
        }
        Err(_) => false,
    }
}

/// **Hot path.** Rewrite `raw` with the dictionary and stash the trace so a
/// later [`correct_last`] can learn from it. Returns `raw` unchanged if the
/// engine isn't initialized. Picks up on-disk dictionary edits automatically.
pub fn finalize_and_record(raw: &str, lang: &str) -> String {
    if !is_enabled() {
        // Correction off — pass through, but still record so a *manual*
        // correction can teach the dictionary even with auto-correct disabled.
        *last_slot() = Some(LastDictation {
            raw: raw.to_string(),
            finalized: raw.to_string(),
            applied: Vec::new(),
            lang: lang.to_string(),
        });
        return raw.to_string();
    }
    // Dictation is human-paced, so a freshness check here is free and means the
    // daemon never serves a stale dictionary after a `dict add` / hand-edit.
    reload_if_changed();
    let Some(state) = read_state() else {
        return raw.to_string();
    };
    let compiled = state.compiled.clone();
    drop(state); // release the read lock before touching the LAST mutex
    let session = SESSION.read().unwrap_or_else(|e| e.into_inner());
    let (finalized, applied) = finalize::finalize_traced(raw, lang, &compiled, &session);
    drop(session);
    *last_slot() = Some(LastDictation {
        raw: raw.to_string(),
        finalized: finalized.clone(),
        applied,
        lang: lang.to_string(),
    });
    finalized
}

/// The most recent dictation (raw + finalized), if any — used to pre-fill a
/// correction UI.
pub fn last_dictation() -> Option<LastDictation> {
    last_slot().clone()
}

/// Arm the **session context** for the next dictation from raw text the user is
/// looking at (focused field, selection, clipboard, app name). Proper-noun-like
/// words (capitalized, ≥4 chars, not everyday words) become high-priority,
/// relaxed-threshold correction targets — so a name on screen gets recognized
/// even though it was never explicitly taught. Transient, in-RAM, never saved.
pub fn set_session_context_from_texts(texts: &[&str], lang: &str) {
    const MAX_TERMS: usize = 80;
    let Some(state) = read_state() else {
        return; // need the common-word guard to filter candidates
    };
    let common = state.common.clone();
    drop(state);

    let mut out: Vec<crate::compiled::FuzzyTerm> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    'outer: for text in texts {
        for tok in crate::normalize::tokenize(text) {
            let crate::normalize::Tok::Word(w) = tok else {
                continue;
            };
            // Proper-noun candidate: starts uppercase, long enough, not everyday.
            if !w.chars().next().is_some_and(|c| c.is_uppercase()) {
                continue;
            }
            let norm = normalize(&w);
            if norm.chars().count() < 4 || common.contains(&norm, lang) {
                continue;
            }
            if !seen.insert(norm.clone()) {
                continue;
            }
            out.push(crate::compiled::FuzzyTerm {
                term: Arc::from(w.as_str()),
                norm,
                starred: true,
                count: 1000,
                boost: true,
                context: Vec::new(),
                lang: (lang != "auto").then(|| lang.to_string()),
            });
            if out.len() >= MAX_TERMS {
                break 'outer;
            }
        }
    }
    *SESSION.write().unwrap_or_else(|e| e.into_inner()) = out;
}

/// Number of session-context terms currently armed (diagnostics).
pub fn session_context_len() -> usize {
    SESSION.read().unwrap_or_else(|e| e.into_inner()).len()
}

/// Clear the session context (e.g. correction disabled).
pub fn clear_session_context() {
    SESSION.write().unwrap_or_else(|e| e.into_inner()).clear();
}

/// Outcome of a [`correct_last`] call.
#[derive(Debug)]
pub enum Correction {
    /// Engine not initialized.
    NotReady,
    /// Nothing to correct (no dictation recorded yet).
    NoLast,
    /// A correction was processed (it may or may not have changed the dict).
    Done(LearnReport),
    /// Persisting the dictionary failed.
    SaveError(String),
}

/// **Cold path.** Learn from the user's corrected version of the last dictation.
pub fn correct_last(corrected: &str) -> Correction {
    let Some(last) = last_dictation() else {
        return if is_initialized() {
            Correction::NoLast
        } else {
            Correction::NotReady
        };
    };
    let Some(mut guard) = write_state() else {
        return Correction::NotReady;
    };
    let common = guard.common.clone();
    let report = learn::learn(&mut guard.dict, &last, corrected, &common);
    if report.changed {
        guard.compiled = Arc::new(Compiled::build(&guard.dict, common));
        if let Err(e) = guard.dict.save(&guard.path) {
            return Correction::SaveError(e.to_string());
        }
        guard.mtime = file_mtime(&guard.path); // our own write isn't a reload
    }
    Correction::Done(report)
}

/// Learn from an explicit `(finalized, corrected)` pair without relying on the
/// recorded last dictation. Drives the `dict learn` CLI and autonomous tests.
pub fn correct(finalized: &str, corrected: &str, lang: &str) -> Correction {
    let Some(mut guard) = write_state() else {
        return Correction::NotReady;
    };
    let common = guard.common.clone();
    let last = LastDictation {
        raw: finalized.to_string(),
        finalized: finalized.to_string(),
        applied: Vec::new(),
        lang: lang.to_string(),
    };
    let report = learn::learn(&mut guard.dict, &last, corrected, &common);
    if report.changed {
        guard.compiled = Arc::new(Compiled::build(&guard.dict, common));
        if let Err(e) = guard.dict.save(&guard.path) {
            return Correction::SaveError(e.to_string());
        }
        guard.mtime = file_mtime(&guard.path); // our own write isn't a reload
    }
    Correction::Done(report)
}

/// Snapshot of all entries (for a management UI / CLI listing).
pub fn list_entries() -> Vec<Entry> {
    read_state()
        .map(|s| s.dict.entries.clone())
        .unwrap_or_default()
}

/// Number of entries currently loaded.
pub fn entry_count() -> usize {
    read_state().map(|s| s.dict.entries.len()).unwrap_or(0)
}

/// Add or update a manual entry, then persist + recompile.
pub fn add_entry(
    term: &str,
    variants: &[String],
    starred: bool,
    lang: Option<&str>,
) -> Result<(), String> {
    let mut guard = write_state().ok_or("dictionary not initialized")?;
    {
        if let Some(e) = guard.dict.find_mut(term) {
            for v in variants {
                if !v.trim().is_empty() && !e.variants.iter().any(|x| key_of(x) == key_of(v)) {
                    e.variants.push(v.clone());
                }
            }
            e.starred = starred || e.starred;
            if let Some(l) = lang {
                e.lang = Some(l.to_string());
            }
        } else {
            let mut e = Entry::new(term);
            e.variants = variants
                .iter()
                .filter(|v| !v.trim().is_empty())
                .cloned()
                .collect();
            e.starred = starred;
            e.source = Source::Manual;
            e.lang = lang.map(|s| s.to_string());
            guard.dict.entries.push(e);
        }
    }
    rebuild_and_save(&mut guard)
}

/// Remove an entry by canonical term. Returns whether it existed.
pub fn remove_entry(term: &str) -> Result<bool, String> {
    let mut guard = write_state().ok_or("dictionary not initialized")?;
    let before = guard.dict.entries.len();
    guard.dict.entries.retain(|e| e.term != term);
    let removed = guard.dict.entries.len() != before;
    if removed {
        rebuild_and_save(&mut guard)?;
    }
    Ok(removed)
}

/// Reload the dictionary from disk (e.g. after the user hand-edits the TOML).
pub fn reload() -> Result<(), String> {
    let path = read_state().map(|s| s.path.clone());
    match path {
        Some(p) => init(p).map_err(|e| e.to_string()),
        None => Err("dictionary not initialized".into()),
    }
}

fn rebuild_and_save(guard: &mut Shared) -> Result<(), String> {
    let common = guard.common.clone();
    guard.compiled = Arc::new(Compiled::build(&guard.dict, common));
    guard.dict.save(&guard.path).map_err(|e| e.to_string())?;
    guard.mtime = file_mtime(&guard.path); // our own write isn't a reload
    Ok(())
}

/// The conventional dictionary path next to a given config file.
pub fn default_path_beside(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("dictionary.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_without_init_is_identity() {
        // STATE is process-global; this test only asserts the pre-init contract.
        if !is_initialized() {
            assert_eq!(finalize_and_record("hello world", "en"), "hello world");
        }
    }

    #[test]
    fn hot_reload_picks_up_external_writes() {
        let dir = std::env::temp_dir().join("wpdict_reload_test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("dictionary.toml");
        let _ = std::fs::remove_file(&path); // start absent → mtime None

        init(path.clone()).unwrap();
        assert_eq!(entry_count(), 0);

        // An external process (the `dict` CLI / a hand-edit) adds an entry.
        std::fs::write(
            &path,
            "version = 1\n[[entry]]\nterm = \"Kasar\"\nvariants = [\"cazar\"]\n",
        )
        .unwrap();

        assert!(reload_if_changed(), "should detect the new file");
        assert_eq!(entry_count(), 1);
        // ...and a subsequent dictation is corrected without an explicit reload.
        assert_eq!(finalize_and_record("cazar", "fr"), "Kasar");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
