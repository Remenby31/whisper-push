//! Screen-vocabulary capture (issue #18): a self-contained, macOS-only
//! pipeline that captures every connected display, runs Vision OCR (French +
//! English) on each, extracts candidate words via
//! `whisper_push_dict::extract_words`, and appends one timestamped record to
//! a local JSONL log — a sibling of `dictionary.toml`, but never merged into
//! it (that's deferred; see the issue). No screenshot is ever written to
//! disk: each captured image lives only long enough for OCR to run on it
//! (see `macos::capture_and_ocr_all_displays`).
//!
//! [`capture_and_log`] is also invokable on demand via the
//! `whisper-push screen-vocab-capture` CLI command, independently of the
//! dictation lifecycle, which is how #18 demoed and verified it on its own.
//!
//! Entirely best-effort end to end: a missing Screen Recording permission, no
//! connected displays, or an OCR failure never panics and never surfaces a
//! user-facing error — see [`Outcome`].
//!
//! Wired into the dictation lifecycle (#19) behind `Config::screen_vocab_enabled`
//! (off by default): [`CaptureGate`] is the pure press/release seam that
//! decides whether to kick off [`capture_and_log`] concurrently with audio
//! recording, and whether release must wait for it — see the tray pipeline's
//! `handle_pipeline_event` (`Event::HotkeyDown`/`HotkeyUp`/`HotkeyToggle`) for
//! the actual wiring.

#[cfg(target_os = "macos")]
mod macos;

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// One JSONL record: a timestamped capture's extracted candidate words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VocabRecord {
    /// Milliseconds since the Unix epoch when the capture was taken.
    pub ts_ms: u64,
    /// Deduplicated, normalized candidate words (see
    /// `whisper_push_dict::extract_words`). No relevance filtering — that's
    /// deliberate for this ticket, see the module docs.
    pub words: Vec<String>,
}

/// What one [`capture_and_log`] call did. The CLI dev-trigger prints this to
/// make the pipeline independently verifiable; a future daemon caller (#19)
/// can just ignore it — nothing here is ever an error the user has to see.
#[derive(Debug)]
pub enum Outcome {
    /// Not macOS — this feature is macOS-only for now. Only ever constructed
    /// on non-macOS targets — dead code on the macOS build that owns the CI
    /// matrix, which is why it's allow-listed here rather than left to warn.
    #[allow(dead_code)]
    Unsupported,
    /// Screen Recording permission isn't granted — no capture was attempted.
    PermissionDenied,
    /// No connected displays were found.
    NoDisplays,
    /// Capture + OCR ran; one record was appended to `path`.
    Logged {
        path: PathBuf,
        displays: usize,
        words: usize,
    },
    /// Capture + OCR ran, but the record could not be appended to `path`
    /// (disk full, permission denied, …) — the extracted words are lost.
    /// Distinct from [`Outcome::Logged`] so a caller (the CLI trigger today,
    /// a future daemon caller tomorrow) can actually notice a persistently
    /// failing write instead of it looking like silent success.
    WriteFailed { path: PathBuf, error: String },
}

/// `screen-vocab.jsonl` lives next to `dictionary.toml` / `config.toml` — a
/// separate store, never merged into the correction dictionary (see #18).
pub fn log_path() -> PathBuf {
    whisper_push_dict::default_path_beside(&crate::config::config_path(), "screen-vocab.jsonl")
}

/// Run the full capture → OCR → extract → log pipeline once. Safe to call
/// from anywhere — never blocks on user interaction, never panics, and the
/// caller doesn't need to check [`Outcome`] to stay safe (it's purely
/// informational, for the CLI trigger and future callers that want to log).
pub fn capture_and_log() -> Outcome {
    #[cfg(target_os = "macos")]
    {
        let raw_texts = match macos::capture_and_ocr_all_displays() {
            macos::Capture::PermissionDenied => return Outcome::PermissionDenied,
            macos::Capture::NoDisplays => return Outcome::NoDisplays,
            macos::Capture::Ok(texts) => texts,
        };
        let words = whisper_push_dict::extract_words(&raw_texts);
        let record = VocabRecord {
            ts_ms: now_ms(),
            words,
        };
        let path = log_path();
        if let Err(e) = append_record(&path, &record) {
            tracing::warn!("screen_vocab: failed to append {}: {e}", path.display());
            return Outcome::WriteFailed {
                path,
                error: e.to_string(),
            };
        }
        Outcome::Logged {
            path,
            displays: raw_texts.len(),
            words: record.words.len(),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Outcome::Unsupported
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Append `record` as one JSON line to `path` (created, with parent dirs, if
/// missing). Never truncates or rewrites existing lines — this log only ever
/// grows; no size/age cap, manual clearing only (deliberate, see #18/#20).
///
/// Written as a single `write_all` (content + trailing newline in one owned
/// buffer) rather than `writeln!`, which would issue two separate `write(2)`
/// calls on an O_APPEND file — POSIX only guarantees atomicity per syscall,
/// so two overlapping writers could otherwise interleave a line.
fn append_record(path: &Path, record: &VocabRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(record).expect("VocabRecord always serializes");
    line.push('\n');
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())
}

/// The pure press/release seam for wiring this pipeline into the dictation
/// lifecycle (#19). Generic over the "in-flight capture" handle type `H` so
/// the on/off gating and the "wait for the tail of an in-flight capture"
/// logic are unit-testable without spawning a real OS thread or touching
/// Vision/CoreGraphics — the tray pipeline instantiates it as
/// `CaptureGate<Receiver<Outcome>>` (a one-shot channel, not a `JoinHandle`,
/// because release must wait with a *bounded* timeout — see
/// `finish_screen_vocab_capture` in `tray/mod.rs`), spawning
/// [`capture_and_log`] on a detached thread that sends its `Outcome` back.
pub struct CaptureGate<H> {
    pending: Option<H>,
}

impl<H> Default for CaptureGate<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H> CaptureGate<H> {
    pub fn new() -> Self {
        Self { pending: None }
    }

    /// Call when a dictation press is confirmed (hold committed past
    /// `hold_delay`, or a toggle-press). No-ops — and, crucially, never calls
    /// `spawn` — when `enabled` is false, which is what makes "toggle off ⇒
    /// zero capture, ever" true regardless of anything else in this struct.
    ///
    /// If a capture is somehow still tracked here (shouldn't happen: `finish`
    /// always drains `pending` before the next press can start a new
    /// recording), it is replaced rather than joined — the old handle is
    /// dropped, which detaches its thread rather than cancelling it, so that
    /// capture still finishes and appends its record; this call just stops
    /// tracking it so a new dictation is never blocked on stale work.
    pub fn start(&mut self, enabled: bool, spawn: impl FnOnce() -> H) {
        if !enabled {
            return;
        }
        self.pending = Some(spawn());
    }

    /// Call at release, before transcription starts. Blocks on (joins) any
    /// capture still in flight, then stops tracking it. No-ops if nothing was
    /// started — the toggle was off, or `start` was never reached (e.g. a
    /// quick tap discarded before the hold committed).
    pub fn finish(&mut self, join: impl FnOnce(H)) {
        if let Some(h) = self.pending.take() {
            join(h);
        }
    }
}

#[cfg(test)]
mod capture_gate_tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[test]
    fn disabled_start_never_spawns() {
        let mut gate: CaptureGate<u32> = CaptureGate::new();
        let spawned = Cell::new(false);
        gate.start(false, || {
            spawned.set(true);
            1
        });
        assert!(!spawned.get());
        assert!(gate.pending.is_none());
    }

    #[test]
    fn enabled_start_spawns_and_tracks() {
        let mut gate: CaptureGate<u32> = CaptureGate::new();
        let calls = Cell::new(0);
        gate.start(true, || {
            calls.set(calls.get() + 1);
            42
        });
        assert_eq!(calls.get(), 1);
        assert_eq!(gate.pending, Some(42));
    }

    #[test]
    fn finish_joins_pending_and_clears() {
        let mut gate = CaptureGate::new();
        gate.start(true, || 7);
        let joined = Cell::new(None);
        gate.finish(|h| joined.set(Some(h)));
        assert_eq!(joined.get(), Some(7));
        assert!(gate.pending.is_none());
    }

    #[test]
    fn finish_is_a_noop_when_nothing_is_pending() {
        let mut gate: CaptureGate<u32> = CaptureGate::new();
        let called = Cell::new(false);
        gate.finish(|_| called.set(true));
        assert!(!called.get());
    }

    #[test]
    fn start_when_already_pending_replaces_without_joining() {
        // Defensive path: should never happen in production (finish() always
        // drains `pending` before a new press is possible), but must not
        // block — the old handle is just dropped (detached), not joined.
        let mut gate = CaptureGate::new();
        gate.start(true, || 1);
        let joined: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        gate.start(true, || {
            // `spawn` for the second press must run even though a value is
            // still pending — proving `start` doesn't block on the stale one.
            joined.borrow_mut().push(1); // sentinel: reached without joining
            2
        });
        assert_eq!(gate.pending, Some(2));
        assert_eq!(*joined.borrow(), vec![1]);
    }

    #[test]
    fn full_press_release_cycle_when_disabled_is_fully_inert() {
        let mut gate: CaptureGate<u32> = CaptureGate::new();
        gate.start(false, || panic!("must not spawn when disabled"));
        gate.finish(|_| panic!("must not join when nothing was started"));
        assert!(gate.pending.is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("wp_screen_vocab_test_{name}.jsonl"));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn log_path_is_a_sibling_of_the_config_file() {
        let path = log_path();
        assert_eq!(path.file_name().unwrap(), "screen-vocab.jsonl");
        assert_eq!(path.parent(), crate::config::config_path().parent());
    }

    #[test]
    fn append_record_creates_file_and_writes_one_line() {
        let path = temp_log("create");
        let record = VocabRecord {
            ts_ms: 1_700_000_000_000,
            words: vec!["kasar".into(), "onnx".into()],
        };
        append_record(&path, &record).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1);
        let got: VocabRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(got, record);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_record_preserves_existing_lines() {
        let path = temp_log("append");
        let first = VocabRecord {
            ts_ms: 1,
            words: vec!["first".into()],
        };
        let second = VocabRecord {
            ts_ms: 2,
            words: vec!["second".into()],
        };
        append_record(&path, &first).unwrap();
        append_record(&path, &second).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            serde_json::from_str::<VocabRecord>(lines[0]).unwrap(),
            first
        );
        assert_eq!(
            serde_json::from_str::<VocabRecord>(lines[1]).unwrap(),
            second
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn append_record_creates_missing_parent_dirs() {
        let path = std::env::temp_dir()
            .join("wp_screen_vocab_test_nested")
            .join("nested")
            .join("screen-vocab.jsonl");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
        let record = VocabRecord {
            ts_ms: 42,
            words: vec![],
        };
        append_record(&path, &record).unwrap();
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}
