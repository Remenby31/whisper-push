//! Silence compaction — shorten long pauses before the audio reaches an engine,
//! and recognise a recording that holds no speech at all.
//!
//! A push-to-talk dictation often holds seconds of room tone: before the first
//! word, while the user thinks, after the last word. Measured on French clips
//! with a 5–12 s pause of low room noise in the middle, Parakeet v3 lost or
//! mangled everything after the pause (44 % WER vs 7 % without it), and Whisper
//! hallucinates on silence ("Thank you.", "Sous-titrage Société Radio-Canada").
//! Those pauses carry no words, so each one is cut down to a short natural gap;
//! the engine sees continuous speech and does less work. A clip with no speech
//! at all never reaches the engine.
//!
//! What is speech is decided by Silero VAD (whisper.cpp's built-in port), not by
//! energy: an energy floor cannot tell soft speech from room tone, and cut a
//! passage spoken 30 dB softer than the rest. Silero is a speech model, so a
//! quiet room, a fan and a click are "no speech" while a whisper is speech. The
//! VAD only ever *decides*; when it is unavailable or fails, the audio goes to
//! the engine untouched — never lost.

use crate::util::LockSafe;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use whisper_rs::{WhisperVadContext, WhisperVadContextParams, WhisperVadParams};

const RATE: usize = 16_000;
/// A pause shorter than this is speech rhythm — left alone.
const MAX_GAP: usize = RATE * 6 / 5; // 1.2 s
/// What is kept on each side of a cut pause (also the lead-in / tail kept).
/// A 1 s gap, not shorter: Parakeet dropped a whole soft passage when the
/// pause before it was squeezed to 0.48 s, and kept it at 1 s. On the A/B
/// sets 1 s scored better overall, within noise (3 words in 668) of 0.48 s on
/// the long-pause clips.
const KEEP: usize = RATE / 2; // 0.5 s → a 1 s gap mid-sentence

/// Silero VAD v6.2.0 in ggml form, from huggingface.co/ggml-org/whisper-vad
/// (MIT, like Silero itself; 885 KB). Embedded so the first dictation works
/// offline, with no download to fail. v6.2.0 rather than v5.1.2 (same size,
/// same repo): on speech 30 dB softer than the rest of the clip v5 dropped
/// whole seconds that v6 found.
const SILERO: &[u8] = include_bytes!("../../resources/models/ggml-silero-v6.2.0.bin");
const SILERO_FILE: &str = "ggml-silero-v6.2.0.bin";
/// Frame-probability threshold. Lower than Silero's 0.5 default: a missed
/// syllable is a cut word, a false "speech" only keeps a pause.
const THRESHOLD: f32 = 0.3;
/// Shortest speech run kept — a lone "oui" is ~0.25 s.
const MIN_SPEECH_MS: i32 = 100;
/// The VAD hears a gain-normalised copy: a whisper 25–40 dB below a normal
/// voice is speech, and Silero misses it at that level. Room tone boosted the
/// same way still reads as no speech; digital silence stays silence.
const VAD_TARGET_RMS: f32 = 0.1;
const VAD_MAX_GAIN: f32 = 31.6; // +30 dB

/// `None` when the VAD heard no speech at all. Otherwise `audio` with every
/// pause longer than 1.2 s shortened to 1 s and a lead-in / tail longer than
/// that trimmed to 0.5 s — borrowed (zero-copy) when nothing is cut, and
/// unchanged whenever the VAD can't run.
pub fn compact(audio: &[f32]) -> Option<Cow<'_, [f32]>> {
    let speech = match speech_segments(audio) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("VAD unavailable ({e}) — audio passed through uncut");
            return Some(Cow::Borrowed(audio));
        }
    };
    if speech.is_empty() {
        // whisper.cpp reports a failed VAD compute as success with no segments,
        // so "no speech" is only trusted when the signal agrees: flat room tone
        // or silence. Anything that clearly stands out of its own background
        // still goes to the engine — a lost dictation is worse than the odd
        // hallucinated "Thank you." on a cough.
        if stands_out(audio) {
            tracing::debug!("VAD heard no speech in a dynamic signal — passed through");
            return Some(Cow::Borrowed(audio));
        }
        return None;
    }
    let cuts = cuts(&speech, audio.len());
    if cuts.is_empty() {
        return Some(Cow::Borrowed(audio));
    }
    let mut out = Vec::with_capacity(audio.len());
    let mut pos = 0;
    for (from, to) in cuts {
        out.extend_from_slice(&audio[pos..from]);
        pos = to;
    }
    out.extend_from_slice(&audio[pos..]);
    tracing::debug!(
        "Silence compaction: {:.1}s → {:.1}s",
        audio.len() as f32 / RATE as f32,
        out.len() as f32 / RATE as f32
    );
    Some(Cow::Owned(out))
}

/// Does some of `audio` rise well above its own floor (≥ 20 dB between the
/// 10th and 95th percentile of 20 ms frame levels, and above near-silence)?
/// Room tone and digital silence don't; speech, and loud non-speech, do.
fn stands_out(audio: &[f32]) -> bool {
    let mut frames: Vec<f32> = audio.chunks(RATE / 50).map(crate::util::rms).collect();
    if frames.is_empty() {
        return false;
    }
    frames.sort_by(f32::total_cmp);
    let floor = frames[frames.len() / 10];
    let peak = frames[frames.len() * 95 / 100];
    peak > 0.003 && peak > floor * 10.0
}

/// Sample ranges to drop, given the sorted speech `segments` of a clip `len`
/// samples long: the inner part of every non-speech stretch over `MAX_GAP`,
/// `KEEP` left next to speech (a lead-in / tail has speech on one side only).
/// Never touches a sample inside a segment.
fn cuts(segments: &[(usize, usize)], len: usize) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    let mut prev_end = None;
    for &(start, end) in segments.iter().chain([(len, len)].iter()) {
        let gap_from = prev_end.unwrap_or(0);
        if start > gap_from && start - gap_from > MAX_GAP {
            let from = prev_end.map_or(0, |e| e + KEEP);
            let to = if start == len { len } else { start - KEEP };
            cuts.push((from, to));
        }
        prev_end = Some(end.max(gap_from));
    }
    cuts
}

/// Speech segments of `audio`, as sample ranges, by Silero VAD on the CPU.
fn speech_segments(audio: &[f32]) -> Result<Vec<(usize, usize)>, String> {
    if audio.is_empty() {
        return Ok(Vec::new());
    }
    let mut ctx = vad()?.lock_safe();
    let t = std::time::Instant::now();
    let mut params = WhisperVadParams::new();
    params.set_threshold(THRESHOLD);
    params.set_min_speech_duration(MIN_SPEECH_MS);
    params.set_speech_pad(0); // `KEEP` is the padding
    let segments: Vec<(usize, usize)> = ctx
        .segments_from_samples(params, &vad_input(audio))
        .map_err(|e| format!("{e:?}"))?
        // Timestamps are centiseconds; the last window is zero-padded past the end.
        .map(|s| {
            let at = |cs: f32| ((cs * (RATE / 100) as f32) as usize).min(audio.len());
            (at(s.start), at(s.end))
        })
        .collect();
    tracing::debug!(
        "VAD: {} speech segment(s) in {:.1}s of audio ({:.1} ms)",
        segments.len(),
        audio.len() as f32 / RATE as f32,
        t.elapsed().as_secs_f64() * 1e3
    );
    Ok(segments)
}

/// `audio` boosted so its loud frames (95th percentile of 20 ms RMS) reach
/// `VAD_TARGET_RMS`, never cut and never above `VAD_MAX_GAIN`.
fn vad_input(audio: &[f32]) -> Cow<'_, [f32]> {
    let mut rms: Vec<f32> = audio.chunks(RATE / 50).map(crate::util::rms).collect();
    rms.sort_by(f32::total_cmp);
    let loud = rms[(rms.len() * 95 / 100).min(rms.len() - 1)];
    let gain = if loud > 0.0 {
        (VAD_TARGET_RMS / loud).clamp(1.0, VAD_MAX_GAIN)
    } else {
        1.0
    };
    if gain == 1.0 {
        return Cow::Borrowed(audio);
    }
    Cow::Owned(audio.iter().map(|s| (s * gain).clamp(-1.0, 1.0)).collect())
}

/// The VAD, loaded on first use and kept for the process (~2 ms to load, ~1 MB).
/// A load failure is remembered: compaction is then off for the session rather
/// than retried on every dictation.
fn vad() -> Result<&'static Mutex<WhisperVadContext>, String> {
    static VAD: OnceLock<Result<Mutex<WhisperVadContext>, String>> = OnceLock::new();
    VAD.get_or_init(|| {
        let path = model_file().map_err(|e| format!("writing the Silero model: {e}"))?;
        let mut params = WhisperVadContextParams::new();
        // CPU, one thread: the engine owns the GPU and the other cores, and
        // Silero is tiny — threads cost more than they save on 32 ms windows.
        params.set_use_gpu(false);
        params.set_n_threads(1);
        let ctx = WhisperVadContext::new(&path.to_string_lossy(), params)
            .map_err(|e| format!("loading {}: {e:?}", path.display()))?;
        Ok(Mutex::new(ctx))
    })
    .as_ref()
    .map_err(Clone::clone)
}

/// whisper.cpp only loads a VAD from a file: write the embedded model once to
/// the models dir (rewritten if it differs), through a per-process temp name
/// so a daemon and a CLI run racing here can't load each other's half file.
fn model_file() -> std::io::Result<PathBuf> {
    #[cfg(not(test))]
    let dir = crate::config::models_dir();
    #[cfg(test)]
    let dir = std::env::temp_dir();
    let path = dir.join(SILERO_FILE);
    if std::fs::read(&path).is_ok_and(|b| b == SILERO) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let part = dir.join(format!("{SILERO_FILE}.{}.part", std::process::id()));
    std::fs::write(&part, SILERO)?;
    std::fs::rename(&part, &path)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: usize = RATE;

    /// `secs` of deterministic low-level noise (room tone).
    fn hiss(secs: f32, amp: f32) -> Vec<f32> {
        let mut x = 12345u32;
        (0..(secs * RATE as f32) as usize)
            .map(|_| {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amp * ((x >> 8) as f32 / (1u32 << 24) as f32 - 0.5)
            })
            .collect()
    }
    fn secs(v: &[f32]) -> f32 {
        v.len() as f32 / RATE as f32
    }
    fn kept(cuts: &[(usize, usize)], len: usize) -> usize {
        len - cuts.iter().map(|(a, b)| b - a).sum::<usize>()
    }
    /// Real speech from macOS `say` at 16 kHz, or `None` where it doesn't
    /// exist (Linux CI): those tests then skip, the cut math above still runs.
    fn say(voice: Option<&str>, text: &str) -> Option<Vec<f32>> {
        let wav = std::env::temp_dir().join(format!(
            "wp_silence_{}_{}.wav",
            std::process::id(),
            text.len()
        ));
        let mut cmd = std::process::Command::new("say");
        cmd.args(["-o", wav.to_str()?, "--data-format=LEI16@16000"]);
        if let Some(v) = voice {
            cmd.args(["-v", v]);
        }
        if !cmd.arg(text).status().ok()?.success() {
            return None;
        }
        let audio = crate::audio::decode::load_audio_file(&wav).ok();
        let _ = std::fs::remove_file(&wav);
        audio
    }
    /// The VAD hears speech and at most the clip's own lead-in / tail goes.
    fn assert_speech_kept(audio: &[f32]) {
        let cut = secs(audio) - secs(&compact(audio).expect("speech"));
        assert!(cut <= 0.5, "{cut:.2}s cut");
    }
    fn gain(v: &[f32], db: f32) -> Vec<f32> {
        let g = 10f32.powf(db / 20.0);
        v.iter().map(|s| s * g).collect()
    }

    // ── Cut math (no model) ──────────────────────────────────────────────────

    #[test]
    fn long_pause_between_speech_is_shortened() {
        let c = cuts(&[(0, 2 * S), (8 * S, 10 * S)], 10 * S);
        assert_eq!(c, vec![(2 * S + KEEP, 8 * S - KEEP)]);
        assert_eq!(kept(&c, 10 * S), 4 * S + 2 * KEEP);
    }

    #[test]
    fn lead_in_and_tail_are_trimmed() {
        let c = cuts(&[(3 * S, 5 * S)], 8 * S);
        assert_eq!(c, vec![(0, 3 * S - KEEP), (5 * S + KEEP, 8 * S)]);
    }

    #[test]
    fn speech_rhythm_is_untouched() {
        // Pauses of 0.5 s are part of speaking, not dead air; nor is a short
        // lead-in or tail.
        let c = cuts(&[(S / 2, S), (3 * S / 2, 3 * S)], 7 * S / 2);
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn speech_samples_are_never_cut() {
        let segs = [(S, 2 * S), (2 * S + 100, 5 * S), (9 * S, 9 * S + 50)];
        for (a, b) in cuts(&segs, 12 * S) {
            assert!(segs.iter().all(|&(s, e)| b <= s || a >= e), "({a},{b})");
        }
    }

    // ── Silero (embedded model, no download) ─────────────────────────────────

    #[test]
    fn all_silence_is_no_speech() {
        assert!(compact(&vec![0.0; 5 * S]).is_none(), "digital silence");
        assert!(compact(&hiss(5.0, 0.01)).is_none(), "room tone");
        assert!(compact(&[]).is_none());
    }

    #[test]
    fn a_signal_that_stands_out_is_never_dropped() {
        // A pure tone isn't speech to Silero, but it rises 30 dB above the
        // room: if the VAD silently failed on real speech, this is what it
        // would look like — the engine must still get it.
        let tone: Vec<f32> = (0..S)
            .map(|i| 0.2 * (i as f32 * 440.0 * std::f32::consts::TAU / S as f32).sin())
            .collect();
        let audio = [hiss(2.0, 0.003), tone, hiss(2.0, 0.003)].concat();
        assert!(compact(&audio).is_some());
    }

    #[test]
    fn long_pause_is_shortened() {
        let Some(a) = say(None, "Please send the report to Marie.") else {
            return;
        };
        let audio = [a.clone(), hiss(6.0, 0.003), a.clone()].concat();
        let out = compact(&audio).expect("speech");
        let gap = secs(&out) - 2.0 * secs(&a);
        assert!((0.8..1.6).contains(&gap), "pause left: {gap:.2}s");
    }

    #[test]
    fn soft_trailing_speech_is_kept() {
        // Normal speech straight into a passage 30 dB softer, no lead-in: the
        // case an energy floor cut as "pause".
        let (Some(a), Some(b)) = (
            say(None, "The meeting moved to Thursday afternoon."),
            say(None, "and bring the signed contract with you please."),
        ) else {
            return;
        };
        let audio = [a, gain(&b, -30.0)].concat();
        assert_speech_kept(&audio);
    }

    #[test]
    fn whisper_and_click_are_kept() {
        // A whisper 25 dB down, after a 20 ms click (the hotkey / start chime
        // caught by pre-roll): speech, not a click in a silent room.
        let Some(w) = say(Some("Whisper"), "call me back tomorrow morning")
            .or_else(|| say(None, "call me back tomorrow morning"))
        else {
            return;
        };
        let click: Vec<f32> = (0..S / 50).map(|i| 0.3 * (i as f32 * 0.4).sin()).collect();
        let audio = [click, gain(&w, -25.0)].concat();
        assert_speech_kept(&audio);
    }
}
