//! Glue between the daemon and the pure `whisper-push-dict` brain.
//!
//! The heavy crate keeps zero dictionary logic of its own: it computes the
//! on-disk path from the existing config layout, arms the engine at startup,
//! and re-exports the management surface the tray/CLI need. The hot path
//! (`whisper_push_dict::finalize_and_record`) is called straight from
//! `transcribe::transcribe_with_backend`.

use std::path::PathBuf;

// Re-export the management API so callers can use `dictionary::…`.
pub use whisper_push_dict::{
    Correction, EditKind, Source, add_entry, correct, correct_last, entry_count, last_dictation,
    list_entries, reload, remove_entry,
};

/// `dictionary.toml` lives next to `config.toml`.
pub fn dictionary_path() -> PathBuf {
    whisper_push_dict::default_path_beside(&crate::config::config_path())
}

/// Load the dictionary and arm correction. A failure (or `enabled == false`)
/// simply leaves the engine inert — `finalize_and_record` then returns the raw
/// text unchanged, so transcription never depends on this succeeding.
pub fn init(enabled: bool) {
    whisper_push_dict::set_enabled(enabled);
    if !enabled {
        tracing::info!("dictionary: correction disabled");
        return;
    }
    let path = dictionary_path();
    match whisper_push_dict::init(path.clone()) {
        Ok(()) => tracing::info!("dictionary ready: {} entries", entry_count()),
        Err(e) => tracing::warn!(
            "dictionary disabled (load failed at {}): {e}",
            path.display()
        ),
    }
}

/// Ensure `dictionary.toml` exists on disk (so "Open" works on first run) and
/// return its path.
pub fn ensure_file() -> PathBuf {
    let path = dictionary_path();
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            &path,
            "# Whisper Push dictionary — learned & manual word corrections.\n# Edited live (changes are picked up on the next dictation).\nversion = 1\n",
        );
    }
    path
}

// ─── Automatic correction capture (Wispr-style) ────────────────────────────
//
// After we paste a dictation, we snapshot the focused text field via the macOS
// Accessibility API. When the *next* dictation is pasted (or the field is read
// again), we diff the snapshot against the field's current contents: if the
// user fixed a word, the same tested classifier that powers manual corrections
// auto-learns it (and ignores rewrites / everyday-word edits). Entirely
// best-effort — any Accessibility failure just means no auto-capture, never a
// broken dictation.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Set when the dictionary changed off the UI thread (auto-capture), so the
/// tray can refresh its word list on its next tick.
static MENU_DIRTY: AtomicBool = AtomicBool::new(false);

/// Baseline of the focused field right after the last paste, plus the dictation
/// language — `(text, lang)`. Only `String`s cross threads (never an AX element
/// reference), so there's no staleness or thread-safety hazard.
static PENDING: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Backstop on field size we'll even look at. We no longer diff the *whole*
/// field — `trim_to_edit_region` strips the common head/tail first (linear), so
/// document size is irrelevant to the cost. This cap only excludes a
/// terminal/scroll-back (300k+ chars) from being armed at all, where dynamic
/// output would be noise. 100k chars (~16k words) covers any real editable
/// document while staying well below a typical terminal buffer.
const MAX_FIELD: usize = 100_000;

/// Max words the *changed region* may span before we treat the edit as a rewrite
/// or a big paste rather than a punctual correction. Caps the classifier's
/// O(n·m) word-diff to a tiny window regardless of how large the document is.
const MAX_EDIT_WORDS: usize = 60;

/// Context words kept on each side of the changed region. The classifier's
/// document-level gate needs some unchanged "anchor" words to recognise a fix
/// (a bare one-word change has zero similarity → looks like a rewrite), and the
/// context-cue capture wants the neighbouring words. This makes a fix buried in
/// a 40k-char document look to the classifier exactly like the same fix made in
/// a short sentence — so its corpus-calibrated thresholds apply unchanged.
const EDIT_MARGIN_WORDS: usize = 6;

/// How often the armed poller re-reads the focused field, and for how many ticks
/// (so the edit is caught *while the user is still in the field*, not 12 s later
/// when focus has moved on). 1 s × 20 ≈ 20 s window; a stable edit is learned
/// ~2 s after it's made.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);
const POLL_TICKS: u32 = 20;

/// Last field content the poller saw, so we only learn an edit once it's been
/// *stable* for a tick — i.e. the user stopped typing. Without this, an
/// erase-then-retype is grabbed mid-way (word deleted) and the real fix is lost.
/// Reset on each arm.
static LAST_POLLED: Mutex<Option<String>> = Mutex::new(None);

/// Take-and-clear the "menu needs refreshing" flag (polled by the tray).
pub fn take_menu_dirty() -> bool {
    MENU_DIRTY.swap(false, Ordering::Relaxed)
}

/// Snapshot the focused field as the baseline for detecting later edits.
/// Called at the END of `paste_text`, by which point the paste has settled, so
/// the read is synchronous and fresh — no stored element, no background thread.
pub fn arm_correction_capture() {
    arm_correction_capture_inner(true);
}

/// As [`arm_correction_capture`], but `spawn_timer == false` skips the 12 s
/// fallback thread.
pub fn arm_correction_capture_inner(spawn_timer: bool) {
    if !whisper_push_dict::is_enabled() {
        return;
    }
    #[cfg(target_os = "macos")]
    {
        let lang = last_dictation()
            .map(|l| l.lang)
            .unwrap_or_else(|| "auto".into());
        match ax::focused_text() {
            Some(t) => arm_with_baseline(t, lang, spawn_timer),
            None => {
                tracing::info!(
                    "auto-capture: focused field exposes no text via Accessibility \
                     (this app may not support it — use 'Correct Last Dictation' instead)"
                );
                *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = spawn_timer;
}

/// **Core (reader-agnostic).** Store `baseline` as the snapshot to diff a later
/// edit against. Production calls this with a direct-AX read; the autonomous test
/// calls it with a System-Events read — same text, so the learning logic is
/// exercised identically. `spawn_timer` arms the 12 s fallback capture.
pub fn arm_with_baseline(baseline: String, lang: String, spawn_timer: bool) {
    if baseline.len() > MAX_FIELD {
        // Loud, not silent: this is a terminal/scroll-back, not an edit target.
        // Real documents (we now trim to the changed region, so size is fine) are
        // well under this; only a terminal buffer trips it.
        tracing::info!(
            "auto-capture: focused field is {} chars (> {MAX_FIELD}) — skipping \
             (looks like a terminal scroll-back, not an editable dictation)",
            baseline.len()
        );
        *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
        return;
    }
    tracing::info!("auto-capture: armed (field has {} chars)", baseline.len());
    *LAST_POLLED.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((baseline, lang));
    if spawn_timer {
        // Poll the focused field a few times instead of one fixed 12 s read, so
        // an in-place edit is caught while the user is still in the field —
        // before they switch apps (e.g. to a terminal). Each tick re-reads the
        // system-wide focused element fresh (no stored ref → thread-safe). The
        // pending is consumed once; whoever captures first (a poll tick or the
        // next paste) wins.
        std::thread::spawn(|| {
            for _ in 0..POLL_TICKS {
                std::thread::sleep(POLL_INTERVAL);
                if PENDING.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
                    return; // already captured (by a paste or an earlier tick)
                }
                #[cfg(target_os = "macos")]
                poll_for_edit();
            }
        });
    }
}

/// One poll tick: read the focused field and learn iff it's a *clear edit of the
/// armed dictation* — not unchanged, not a different/huge field, still mostly the
/// same words (so it's the edited dictation, not some other field the user
/// focused). Captures on the first such sighting, so an edit made just before the
/// user moves on (e.g. to a terminal) is still caught. A transient focus change
/// onto an unrelated/huge field is ignored, leaving the pending armed.
#[cfg(target_os = "macos")]
fn poll_for_edit() {
    let Some(current) = ax::focused_text() else {
        return; // can't read right now — keep waiting
    };
    let baseline = {
        let g = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            Some((b, _)) => b.clone(),
            None => return,
        }
    };
    if current == baseline || current.len() > MAX_FIELD || !looks_like_edit(&baseline, &current) {
        return; // unchanged / huge / unrelated field — keep waiting
    }
    // Debounce: learn only once the edit has settled (same as the previous tick),
    // so an erase-then-retype isn't captured at the half-deleted intermediate.
    let settled = {
        let mut last = LAST_POLLED.lock().unwrap_or_else(|e| e.into_inner());
        let same = last.as_deref() == Some(current.as_str());
        *last = Some(current.clone());
        same
    };
    if settled {
        capture_with_current(&current);
    }
}

/// Cheap "is `current` a light edit of `baseline`?" guard: at least half of the
/// baseline's words still appear, so we don't diff against an unrelated field the
/// user happened to focus mid-window.
#[cfg(target_os = "macos")]
fn looks_like_edit(baseline: &str, current: &str) -> bool {
    let bw: Vec<&str> = baseline.split_whitespace().collect();
    if bw.is_empty() {
        return false;
    }
    let cw: std::collections::HashSet<&str> = current.split_whitespace().collect();
    let shared = bw.iter().filter(|w| cw.contains(*w)).count();
    shared as f32 / bw.len() as f32 >= 0.5
}

/// If the field is still focused and was edited since the last paste, auto-learn
/// the correction. Reads the *currently* focused field synchronously (no stored
/// element), so it's safe; guarded by the same classifier as manual edits.
pub fn capture_pending_correction() {
    #[cfg(target_os = "macos")]
    {
        let Some(current) = ax::focused_text() else {
            tracing::info!("auto-capture: can't read the field now — skipped");
            // Drop the pending snapshot: we can't diff against it.
            *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return;
        };
        capture_with_current(&current);
    }
}

/// Reduce a `(baseline, current)` pair to just the changed region plus a small
/// word margin, by stripping the common leading and trailing words. **Linear in
/// the field size**, so auto-capture works on documents of any length: the
/// expensive O(n·m) word-LCS classifier then only ever sees the handful of words
/// that actually changed (± [`EDIT_MARGIN_WORDS`] of context).
///
/// Returns `None` when nothing changed, or when the changed region is larger than
/// [`MAX_EDIT_WORDS`] (a paragraph rewrite / big paste, not a punctual fix).
///
/// Trimming is also strictly *safer* against false positives: the classifier's
/// document-level rewrite gate now judges the local change on its own merits
/// instead of seeing it diluted by thousands of identical surrounding words (a
/// 2-word rewrite inside a 40k doc would otherwise score ~100% unchanged and slip
/// past the gate). The per-span phonetic gates are unaffected — the spans the LCS
/// finds are identical whether or not the common margins are present.
fn trim_to_edit_region(baseline: &str, current: &str) -> Option<(String, String)> {
    let bw: Vec<&str> = baseline.split_whitespace().collect();
    let cw: Vec<&str> = current.split_whitespace().collect();

    // Common leading words.
    let mut head = 0;
    while head < bw.len() && head < cw.len() && bw[head] == cw[head] {
        head += 1;
    }
    // Common trailing words (never crossing into the head).
    let max_tail = (bw.len() - head).min(cw.len() - head);
    let mut tail = 0;
    while tail < max_tail && bw[bw.len() - 1 - tail] == cw[cw.len() - 1 - tail] {
        tail += 1;
    }

    let b_hi = bw.len() - tail; // exclusive end of the baseline change
    let c_hi = cw.len() - tail; // exclusive end of the current change
    if head == b_hi && head == c_hi {
        return None; // identical word sequences
    }
    if b_hi - head > MAX_EDIT_WORDS || c_hi - head > MAX_EDIT_WORDS {
        return None; // change too large to be a punctual correction
    }

    // `lo` is shared: the prefix [0..head] is common to both sequences.
    let lo = head.saturating_sub(EDIT_MARGIN_WORDS);
    let b_end = (b_hi + EDIT_MARGIN_WORDS).min(bw.len());
    let c_end = (c_hi + EDIT_MARGIN_WORDS).min(cw.len());
    Some((bw[lo..b_end].join(" "), cw[lo..c_end].join(" ")))
}

/// **Core (reader-agnostic).** Diff the armed baseline against `current` and
/// auto-learn any punctual fix. Production feeds a direct-AX read; the autonomous
/// test feeds a System-Events read — identical from here down.
pub fn capture_with_current(current: &str) {
    let Some((baseline, lang)) = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return; // nothing armed, or already captured
    };
    if current == baseline {
        tracing::debug!("auto-capture: field unchanged since paste");
        return;
    }
    // Strip the unchanged head/tail so the classifier only sees the edit — this
    // is what lets auto-capture work in a real (large) document, not just a tiny
    // text field.
    let Some((region_old, region_new)) = trim_to_edit_region(&baseline, current) else {
        tracing::info!(
            "auto-capture: change region empty or larger than {MAX_EDIT_WORDS} words \
             — skipping (rewrite or big paste, not a punctual fix)"
        );
        return;
    };
    tracing::info!(
        "auto-capture: field edited (full {} → {} chars; change {:?} → {:?}), analyzing…",
        baseline.len(),
        current.len(),
        region_old,
        region_new
    );
    match correct(&region_old, &region_new, &lang) {
        Correction::Done(report) if !report.learned.is_empty() => {
            for (heard, term) in &report.learned {
                tracing::info!("auto-capture: learned {heard:?} → {term:?}");
                // Learn the sound too, from the recent dictation history.
                crate::acoustic::learn_word(heard, term);
            }
            MENU_DIRTY.store(true, Ordering::Relaxed);
            let n = report.learned.len();
            crate::notify::send(
                "Whisper Push",
                &format!(
                    "Learned {n} word{} from your correction",
                    if n == 1 { "" } else { "s" }
                ),
            );
        }
        Correction::Done(report) => {
            tracing::info!(
                "auto-capture: edit not learnable ({:?}) — rewrite or everyday word",
                report.kind
            );
        }
        other => tracing::info!("auto-capture: {other:?}"),
    }
}

/// Harvest proper-noun candidates from the user's current context — focused
/// field + selection (macOS Accessibility) and the clipboard (all platforms) —
/// and arm them as relaxed-threshold correction targets for the next dictation.
/// This is the "it knows the name because it's on screen" semantic layer.
/// Transient, never written to disk. Call just before transcribing.
pub fn update_session_context(lang: &str) {
    if !whisper_push_dict::is_enabled() {
        whisper_push_dict::clear_session_context();
        return;
    }
    let mut texts: Vec<String> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        if let Some(t) = ax::focused_text() {
            texts.push(t);
        }
        if let Some(t) = ax::selected_text() {
            texts.push(t);
        }
    }
    if let Ok(mut cb) = arboard::Clipboard::new() {
        if let Ok(t) = cb.get_text() {
            texts.push(t);
        }
    }
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    whisper_push_dict::set_session_context_from_texts(&refs, lang);
    tracing::debug!(
        "session context: {} term(s)",
        whisper_push_dict::session_context_len()
    );
}

/// Minimal Accessibility reader for the focused text field. Self-contained:
/// every CF reference it creates is released within the call, and no element
/// reference is ever stored or shared across threads.
#[cfg(target_os = "macos")]
mod ax {
    use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;

    type AXUIElementRef = *const c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> i32;
    }

    /// Read a string attribute (`kAXValue`, `kAXSelectedText`, …) of the element
    /// currently holding keyboard focus. Returns `None` on any failure (no focus,
    /// not a text field, permission missing, non-string value) — caller skips.
    /// Self-contained: every CF ref is released within the call; nothing stored.
    fn read_focused_attr(attribute: &str) -> Option<String> {
        unsafe {
            // System-wide element is "Create" (+1) → must release.
            let sys = AXUIElementCreateSystemWide();
            if sys.is_null() {
                return None;
            }
            let attr_focus = CFString::new("AXFocusedUIElement");
            let mut elem: CFTypeRef = std::ptr::null();
            let err =
                AXUIElementCopyAttributeValue(sys, attr_focus.as_concrete_TypeRef(), &mut elem);
            CFRelease(sys as CFTypeRef);
            if err != 0 || elem.is_null() {
                return None;
            }
            // `elem` is "Copy" (+1). Read the requested attribute, then release it.
            let attr = CFString::new(attribute);
            let mut val: CFTypeRef = std::ptr::null();
            let err = AXUIElementCopyAttributeValue(
                elem as AXUIElementRef,
                attr.as_concrete_TypeRef(),
                &mut val,
            );
            CFRelease(elem);
            if err != 0 || val.is_null() {
                return None;
            }
            // `val` is "Copy" (+1). Convert if a string, else release + bail.
            if CFGetTypeID(val) == <CFString as TCFType>::type_id() {
                Some(CFString::wrap_under_create_rule(val as CFStringRef).to_string())
            } else {
                CFRelease(val);
                None
            }
        }
    }

    /// Text of the focused field (`kAXValue`).
    pub fn focused_text() -> Option<String> {
        read_focused_attr("AXValue")
    }

    /// Currently selected text (`kAXSelectedText`).
    pub fn selected_text() -> Option<String> {
        read_focused_attr("AXSelectedText")
    }
}
