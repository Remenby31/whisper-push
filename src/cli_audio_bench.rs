//! `whisper-push audio-bench` — measures what voice processing buys.
//!
//! - `run MANIFEST`: transcribe every WAV of a TSV manifest
//!   (`path<TAB>condition<TAB>reference text`) through the production pipeline
//!   (VAD compaction → engine; the dictionary is left off so the numbers are
//!   acoustic) and print word error rate + latency per condition.
//!   `tools/audio_bench/make_corpus.py` builds such a manifest offline (TTS
//!   speech mixed with music / a talking song / noise); `tools/audio_bench/
//!   live.sh` builds one from real microphone takes.
//! - `record OUT.wav [--vp]`: record from the mic, raw or through macOS
//!   VoiceProcessingIO, with the app's own capture code. Needs the Microphone
//!   permission of whatever launched it (a Terminal), so only a human runs it.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn run(
    manifest: &Path,
    model: Option<&str>,
    language: &str,
    max_rows: Option<usize>,
) -> Result<()> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let base = manifest.parent().unwrap_or(Path::new("."));
    let rows: Vec<(String, String, String)> = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            Some((it.next()?.into(), it.next()?.into(), it.next()?.into()))
        })
        .take(max_rows.unwrap_or(usize::MAX))
        .collect();
    anyhow::ensure!(!rows.is_empty(), "no rows in {}", manifest.display());

    let model = match model {
        Some(m) => m.to_string(),
        None => crate::config::Config::load()?.model,
    };
    let backend = crate::model_manager::resolve_backend(&model);
    eprintln!("Loading {model}…");
    crate::transcribe::ensure_loaded(&backend, &model)?;
    // Warm-up: the first inference pays shader/graph setup, not the condition.
    if let Some((p, _, _)) = rows.first() {
        let a = crate::audio::decode::load_audio_file(&base.join(p))?;
        let _ = crate::transcribe::transcribe_with_backend(&a, language, &backend);
    }

    #[derive(Default)]
    struct Acc {
        errors: usize,
        words: usize,
        secs: f64,
        audio: f64,
        n: usize,
    }
    let mut by_cond: BTreeMap<String, Acc> = BTreeMap::new();
    for (path, cond, reference) in &rows {
        let audio = crate::audio::decode::load_audio_file(&base.join(path))
            .with_context(|| format!("decoding {path}"))?;
        let t = Instant::now();
        let hyp = crate::transcribe::transcribe_with_backend(&audio, language, &backend)?;
        let secs = t.elapsed().as_secs_f64();
        let (e, n) = word_errors(reference, &hyp);
        println!(
            "{cond}\t{path}\t{:.0}%\t{:.0} ms\t{hyp}",
            100.0 * e as f64 / n.max(1) as f64,
            secs * 1e3
        );
        let a = by_cond.entry(cond.clone()).or_default();
        a.errors += e;
        a.words += n;
        a.secs += secs;
        a.audio += audio.len() as f64 / 16_000.0;
        a.n += 1;
    }

    println!("\n## {model} (language: {language})\n");
    println!("| condition | clips | WER | latency / clip | RTF |");
    println!("|---|---:|---:|---:|---:|");
    for (cond, a) in &by_cond {
        println!(
            "| {cond} | {} | {:.1} % | {:.0} ms | {:.3} |",
            a.n,
            100.0 * a.errors as f64 / a.words.max(1) as f64,
            1e3 * a.secs / a.n as f64,
            a.secs / a.audio.max(1e-9)
        );
    }
    Ok(())
}

pub fn record(out: &Path, vp: bool, secs: f32, device: &str) -> Result<()> {
    let t = Instant::now();
    let cap = crate::audio::capture::AudioCapture::start_with(device, vp)?;
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    std::thread::sleep(Duration::from_secs_f32(secs));
    let name = cap.device_name().to_string();
    let audio = cap.stop();
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: crate::audio::SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(out, spec)?;
    for s in &audio {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()?;
    println!(
        "{}\tvp={vp}\topen={open_ms:.0}ms\tdevice={name}\trms={:.4}",
        out.display(),
        crate::util::rms(&audio)
    );
    Ok(())
}

/// Lowercase words, punctuation as separators (apostrophes too: "l'équipe" →
/// "l", "équipe", so "l'équipe" vs "l équipe" isn't an error).
fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// (word edits, reference word count) — Levenshtein over words.
pub fn word_errors(reference: &str, hypothesis: &str) -> (usize, usize) {
    let r = words(reference);
    let h = words(hypothesis);
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for i in 1..=r.len() {
        let mut cur = vec![i; h.len() + 1];
        for j in 1..=h.len() {
            let sub = prev[j - 1] + usize::from(r[i - 1] != h[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        prev = cur;
    }
    (prev[h.len()], r.len())
}

#[cfg(test)]
mod tests {
    use super::word_errors;

    #[test]
    fn wer_counts_word_edits() {
        assert_eq!(word_errors("Le chat dort.", "le chat dort"), (0, 3));
        assert_eq!(word_errors("le chat dort", "le chien dort bien"), (2, 3));
        assert_eq!(word_errors("l'équipe", "l équipe"), (0, 2));
        assert_eq!(word_errors("un deux", ""), (2, 2));
    }
}
