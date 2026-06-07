//! Acoustic-dictionary glue.
//!
//! Corrects a spoken word by its **sound**, not the ASR's (varying) spelling.
//! On every dictation we keep the audio + per-word spans; when the user corrects
//! a word we fingerprint that word's audio and remember `fingerprint → term`; on
//! later dictations we fingerprint each word and DTW-match it. So "Kasar" is
//! recovered whether the model wrote *Khazar*, *Kaza* or *Caza*.
//!
//! Model-agnostic: uses the backend's word timings when available (Parakeet),
//! else falls back to energy-based segmentation (Whisper/Voxtral/anything).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock, RwLock};
use whisper_push_acoustic::{AcousticStore, fingerprint, segment_by_energy};

pub const SAMPLE_RATE: u32 = 16_000;
/// DTW distance under which two words are "the same sound". Tuned on real
/// speech with loudness-invariant MFCCs: same word ≈ 2.8–3.3, different name
/// (Kasar/César) ≈ 10 → 6.0 separates them with margin on both sides.
const MATCH_THRESHOLD: f32 = 6.0;
/// Shortest audio segment (samples) we'll fingerprint (~30 ms).
const MIN_SEGMENT: usize = 480;

/// A word and its audio span (seconds), as produced by the ASR or by energy
/// segmentation.
#[derive(Clone, Debug)]
pub struct WordTiming {
    pub text: String,
    pub start: f32,
    pub end: f32,
}

struct LastAudio {
    audio: Vec<f32>,
    words: Vec<WordTiming>,
}

/// Recent dictations' audio + word spans, so a correction (panel or in-place)
/// can fingerprint the right word even though it may have been pasted a couple
/// of dictations ago.
static HISTORY: Mutex<VecDeque<LastAudio>> = Mutex::new(VecDeque::new());
const HISTORY_CAP: usize = 3;
static STORE: OnceLock<RwLock<AcousticStore>> = OnceLock::new();
static STORE_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

/// Load the persisted acoustic store at startup.
pub fn init() {
    let path = crate::config::data_dir().join("acoustic.bin");
    let store = AcousticStore::load(&path);
    tracing::info!("acoustic dictionary: {} fingerprint(s)", store.len());
    let _ = STORE.set(RwLock::new(store));
    let _ = STORE_PATH.set(path);
}

/// In-memory store with NO persistence path — used by the e2e self-test so it
/// never touches the user's real `acoustic.bin` (`save()` no-ops without a path).
#[allow(dead_code)]
pub fn init_ephemeral() {
    let _ = STORE.set(RwLock::new(AcousticStore::default()));
}

fn store_read() -> Option<std::sync::RwLockReadGuard<'static, AcousticStore>> {
    STORE
        .get()
        .map(|l| l.read().unwrap_or_else(|e| e.into_inner()))
}

/// **Hot path.** Keep this dictation's audio + word spans (for a later
/// correction) and apply acoustic matching to recover known words. Returns the
/// possibly-corrected text. `words` may be empty (no backend timings) — we then
/// segment by energy.
pub fn process(audio: &[f32], raw: &str, words: Vec<WordTiming>, _lang: &str) -> String {
    let word_strs = word_tokens(raw);
    // Fill in spans if the backend gave none (model-agnostic fallback).
    let words = if words.is_empty() && !word_strs.is_empty() {
        let segs = segment_by_energy(audio, word_strs.len());
        word_strs
            .iter()
            .zip(segs)
            .map(|(w, (s, e))| WordTiming {
                text: w.clone(),
                start: s as f32 / SAMPLE_RATE as f32,
                end: e as f32 / SAMPLE_RATE as f32,
            })
            .collect()
    } else {
        words
    };

    let corrected = apply_match(audio, raw, &words, _lang);
    let mut hist = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    hist.push_front(LastAudio {
        audio: audio.to_vec(),
        words,
    });
    hist.truncate(HISTORY_CAP);
    corrected
}

/// Replace each word whose audio matches a stored fingerprint with that term.
fn apply_match(audio: &[f32], raw: &str, words: &[WordTiming], lang: &str) -> String {
    let Some(store) = store_read() else {
        return raw.to_string();
    };
    if store.is_empty() || words.is_empty() {
        return raw.to_string();
    }
    // For each timed word, the acoustically-matched term (if any). Guards:
    // never touch an everyday word (kills common-word collisions like
    // sales/Sails), and never replace a word that's already the term.
    let matches: Vec<Option<String>> = words
        .iter()
        .map(|w| {
            if whisper_push_dict::is_common_word(&w.text, lang) {
                return None;
            }
            segment_fp(audio, w).and_then(|fp| {
                store
                    .best_match(&fp, MATCH_THRESHOLD)
                    .filter(|t| !eq_ignore(t, &w.text))
                    .map(|t| t.to_string())
            })
        })
        .collect();
    drop(store);

    if matches.iter().all(Option::is_none) {
        return raw.to_string();
    }

    // Apply to the raw text: align the k-th word token to words[k] (both in
    // model order) and swap matched ones, preserving punctuation/spacing.
    let toks = tokenize(raw);
    let mut out = String::with_capacity(raw.len());
    let mut wi = 0;
    for t in toks {
        match t {
            Tok::Sep(s) => out.push_str(&s),
            Tok::Word(w) => {
                if let Some(Some(term)) = matches.get(wi) {
                    tracing::info!("acoustic: '{w}' → '{term}'");
                    out.push_str(term);
                } else {
                    out.push_str(&w);
                }
                wi += 1;
            }
        }
    }
    out
}

/// Learn that the spoken word `heard` (from the last dictation) should be
/// `term`. Fingerprints that word's audio segment and persists it.
pub fn learn_word(heard: &str, term: &str) -> bool {
    let fp = {
        let hist = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
        let hn = whisper_push_dict::normalize(heard);
        // Search recent dictations (most recent first) for that spoken word.
        let mut found = None;
        'search: for last in hist.iter() {
            for w in &last.words {
                if whisper_push_dict::normalize(&w.text) == hn {
                    if let Some(fp) = segment_fp(&last.audio, w) {
                        found = Some(fp);
                        break 'search;
                    }
                }
            }
        }
        match found {
            Some(fp) => fp,
            None => return false,
        }
    };
    let Some(lock) = STORE.get() else {
        return false;
    };
    {
        let mut store = lock.write().unwrap_or_else(|e| e.into_inner());
        store.learn(term, fp);
    }
    save();
    tracing::info!("acoustic: learned the sound of '{heard}' → '{term}'");
    true
}

/// Number of stored acoustic fingerprints (for the tray).
pub fn len() -> usize {
    store_read().map(|s| s.len()).unwrap_or(0)
}

/// First word of the most recent dictation, as the model *raw* heard it (before
/// any correction). Used by the e2e self-test to learn the right sound.
#[allow(dead_code)]
pub fn last_heard_word() -> Option<String> {
    let hist = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    hist.front()
        .and_then(|l| l.words.first())
        .map(|w| w.text.clone())
}

/// Forget all learned voiceprints (tray "Forget voiceprints").
pub fn clear() {
    if let Some(lock) = STORE.get() {
        *lock.write().unwrap_or_else(|e| e.into_inner()) = AcousticStore::default();
        save();
    }
}

fn save() {
    if let (Some(lock), Some(path)) = (STORE.get(), STORE_PATH.get()) {
        let store = lock.read().unwrap_or_else(|e| e.into_inner());
        let _ = store.save(path);
    }
}

fn segment_fp(audio: &[f32], w: &WordTiming) -> Option<whisper_push_acoustic::Fingerprint> {
    // Guard against NaN/Inf timings before the f32→usize cast.
    if !w.start.is_finite() || !w.end.is_finite() || w.end <= w.start {
        return None;
    }
    let s = ((w.start * SAMPLE_RATE as f32).max(0.0) as usize).min(audio.len());
    let e = ((w.end * SAMPLE_RATE as f32) as usize).min(audio.len());
    if e <= s || e - s < MIN_SEGMENT {
        return None;
    }
    Some(fingerprint(&audio[s..e], SAMPLE_RATE))
}

fn eq_ignore(a: &str, b: &str) -> bool {
    whisper_push_dict::normalize(a) == whisper_push_dict::normalize(b)
}

// ── Minimal tokenizer (same Word/Sep split as the dictionary) ──────────────
enum Tok {
    Word(String),
    Sep(String),
}

fn tokenize(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut is_word: Option<bool> = None;
    for c in text.chars() {
        let w = c.is_alphanumeric();
        match is_word {
            Some(b) if b == w => buf.push(c),
            Some(b) => {
                out.push(if b {
                    Tok::Word(std::mem::take(&mut buf))
                } else {
                    Tok::Sep(std::mem::take(&mut buf))
                });
                buf.push(c);
                is_word = Some(w);
            }
            None => {
                buf.push(c);
                is_word = Some(w);
            }
        }
    }
    if let Some(b) = is_word {
        out.push(if b { Tok::Word(buf) } else { Tok::Sep(buf) });
    }
    out
}

fn word_tokens(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter_map(|t| match t {
            Tok::Word(w) => Some(w),
            Tok::Sep(_) => None,
        })
        .collect()
}
