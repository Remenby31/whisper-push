//! Screen-vocabulary capture (issue #18): a self-contained, macOS-only
//! pipeline that captures every connected display, runs Vision OCR (French +
//! English) on each, extracts candidate words via
//! `whisper_push_dict::extract_words`, and appends one timestamped record to
//! a local JSONL log — a sibling of `dictionary.toml`, but never merged into
//! it (that's deferred; see the issue). No screenshot is ever written to
//! disk: each captured image lives only long enough for OCR to run on it
//! (see `macos::capture_and_ocr_all_displays`).
//!
//! Not wired into the dictation lifecycle yet (that's #19, blocked on this
//! ticket) — [`capture_and_log`] is invoked on demand today, via the
//! `whisper-push screen-vocab-capture` CLI command, so the pipeline is
//! demoable and verifiable on its own.
//!
//! Entirely best-effort end to end: a missing Screen Recording permission, no
//! connected displays, or an OCR failure never panics and never surfaces a
//! user-facing error — see [`Outcome`].

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
