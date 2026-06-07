//! # whisper-push-acoustic
//!
//! A **speaker-specific acoustic dictionary**: it corrects a spoken word by its
//! *sound*, not by the ASR's (varying) spelling. When the user corrects a word,
//! we fingerprint the audio of that word (MFCC) and remember `fingerprint →
//! term`. On later dictations we fingerprint each word and DTW-match it against
//! the store — so "Kasar" is recovered whether the model wrote *Khazar*, *Kaza*
//! or *Caza*, because the audio is the same.
//!
//! 100% local, classical DSP (FFT + mel + DCT + DTW), **no neural model**.

use serde::{Deserialize, Serialize};

// ── MFCC parameters (16 kHz speech) ────────────────────────────────────────
const FRAME_LEN: usize = 400; // 25 ms @ 16 kHz
const HOP: usize = 160; //       10 ms
const FFT_SIZE: usize = 512;
const N_MEL: usize = 26;
const N_MFCC: usize = 13;
const PREEMPH: f32 = 0.97;

/// A word's acoustic fingerprint: a sequence of MFCC frames (cepstral-mean
/// normalized so it's robust across recordings of the same speaker).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Fingerprint {
    /// `frames[t][c]` = MFCC coefficient `c` at frame `t`.
    frames: Vec<[f32; N_MFCC]>,
}

impl Fingerprint {
    pub fn len(&self) -> usize {
        self.frames.len()
    }
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// Compute the MFCC fingerprint of a mono audio segment.
pub fn fingerprint(samples: &[f32], sample_rate: u32) -> Fingerprint {
    if samples.len() < FRAME_LEN {
        return Fingerprint::default();
    }
    // Pre-emphasis (boost high frequencies, as in standard ASR front-ends).
    let mut emph = vec![0.0f32; samples.len()];
    emph[0] = samples[0];
    for i in 1..samples.len() {
        emph[i] = samples[i] - PREEMPH * samples[i - 1];
    }

    let window = hamming(FRAME_LEN);
    let filterbank = mel_filterbank(sample_rate);
    let fft = fft_512();
    let n_bins = FFT_SIZE / 2 + 1;

    let mut frames: Vec<[f32; N_MFCC]> = Vec::new();
    let mut start = 0;
    while start + FRAME_LEN <= emph.len() {
        // Windowed, zero-padded frame → FFT.
        let mut buf = vec![rustfft::num_complex::Complex::<f32>::new(0.0, 0.0); FFT_SIZE];
        for n in 0..FRAME_LEN {
            buf[n].re = emph[start + n] * window[n];
        }
        fft.process(&mut buf);

        // Power spectrum.
        let mut power = vec![0.0f32; n_bins];
        for (k, p) in power.iter_mut().enumerate() {
            *p = buf[k].norm_sqr();
        }

        // Mel energies → log.
        let mut logmel = [0.0f32; N_MEL];
        for (m, filt) in filterbank.iter().enumerate() {
            let mut e = 0.0;
            for (k, &w) in filt.iter().enumerate() {
                e += w * power[k];
            }
            logmel[m] = (e + 1e-10).ln();
        }

        // DCT-II → first N_MFCC cepstral coefficients.
        let mut mfcc = [0.0f32; N_MFCC];
        for (k, c) in mfcc.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (m, &lm) in logmel.iter().enumerate() {
                sum += lm
                    * (std::f32::consts::PI * k as f32 * (m as f32 + 0.5) / N_MEL as f32).cos();
            }
            *c = sum;
        }
        frames.push(mfcc);
        start += HOP;
    }

    cepstral_mean_normalize(&mut frames);
    Fingerprint { frames }
}

/// Normalized DTW distance between two fingerprints in `[0, ∞)`; small means
/// "same word". Returns `INFINITY` if either is empty.
pub fn distance(a: &Fingerprint, b: &Fingerprint) -> f32 {
    let (n, m) = (a.frames.len(), b.frames.len());
    if n == 0 || m == 0 {
        return f32::INFINITY;
    }
    // Sakoe-Chiba band would speed this up; for word-length sequences the full
    // DP is cheap enough.
    let mut prev = vec![f32::INFINITY; m + 1];
    let mut cur = vec![f32::INFINITY; m + 1];
    prev[0] = 0.0;
    // dp[0][0]=0, but first row otherwise INF
    for i in 1..=n {
        cur[0] = f32::INFINITY;
        for j in 1..=m {
            let cost = frame_dist(&a.frames[i - 1], &b.frames[j - 1]);
            let best = prev[j].min(cur[j - 1]).min(prev[j - 1]);
            cur[j] = cost + best;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[m] / (n + m) as f32
}

fn frame_dist(a: &[f32; N_MFCC], b: &[f32; N_MFCC]) -> f32 {
    // Skip coefficient 0 (the log-energy term) so the distance is invariant to
    // loudness / mic gain — only the spectral shape (what the word sounds like)
    // matters.
    let mut s = 0.0;
    for k in 1..N_MFCC {
        let d = a[k] - b[k];
        s += d * d;
    }
    s.sqrt()
}

/// Cached forward FFT (planning is expensive; the plan is reused every frame).
fn fft_512() -> std::sync::Arc<dyn rustfft::Fft<f32>> {
    use std::sync::OnceLock;
    static FFT: OnceLock<std::sync::Arc<dyn rustfft::Fft<f32>>> = OnceLock::new();
    FFT.get_or_init(|| rustfft::FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE))
        .clone()
}

fn cepstral_mean_normalize(frames: &mut [[f32; N_MFCC]]) {
    if frames.is_empty() {
        return;
    }
    let mut mean = [0.0f32; N_MFCC];
    for f in frames.iter() {
        for k in 0..N_MFCC {
            mean[k] += f[k];
        }
    }
    let n = frames.len() as f32;
    for m in mean.iter_mut() {
        *m /= n;
    }
    for f in frames.iter_mut() {
        for k in 0..N_MFCC {
            f[k] -= mean[k];
        }
    }
}

fn hamming(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / (n as f32 - 1.0)).cos())
        .collect()
}

fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Triangular mel filterbank: `N_MEL` filters over the FFT power bins.
fn mel_filterbank(sample_rate: u32) -> Vec<Vec<f32>> {
    let n_bins = FFT_SIZE / 2 + 1;
    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(sample_rate as f32 / 2.0);
    let points: Vec<usize> = (0..N_MEL + 2)
        .map(|i| {
            let mel = mel_min + (mel_max - mel_min) * i as f32 / (N_MEL + 1) as f32;
            let hz = mel_to_hz(mel);
            (((FFT_SIZE as f32 + 1.0) * hz / sample_rate as f32).floor() as usize).min(n_bins - 1)
        })
        .collect();

    let mut filters = vec![vec![0.0f32; n_bins]; N_MEL];
    for m in 1..=N_MEL {
        let (l, c, r) = (points[m - 1], points[m], points[m + 1]);
        for k in l..c {
            if c > l {
                filters[m - 1][k] = (k - l) as f32 / (c - l) as f32;
            }
        }
        for k in c..r {
            if r > c {
                filters[m - 1][k] = (r - k) as f32 / (r - c) as f32;
            }
        }
    }
    filters
}

// ── Acoustic store ─────────────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
struct AcousticEntry {
    term: String,
    fp: Fingerprint,
    count: u32,
}

/// Persistent set of `fingerprint → term` mappings.
#[derive(Default, Serialize, Deserialize)]
pub struct AcousticStore {
    entries: Vec<AcousticEntry>,
}

impl AcousticStore {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remember that this audio fingerprint should be transcribed as `term`.
    pub fn learn(&mut self, term: &str, fp: Fingerprint) {
        if fp.is_empty() {
            return;
        }
        // Merge into an existing same-term + very-close fingerprint to avoid
        // unbounded growth; otherwise add a new exemplar.
        for e in &mut self.entries {
            if e.term == term && distance(&e.fp, &fp) < MERGE_DISTANCE {
                e.count = e.count.saturating_add(1);
                return;
            }
        }
        self.entries.push(AcousticEntry {
            term: term.to_string(),
            fp,
            count: 1,
        });
    }

    /// The best-matching term for `fp` if any stored exemplar is within
    /// `max_distance`, else `None`.
    pub fn best_match(&self, fp: &Fingerprint, max_distance: f32) -> Option<&str> {
        self.nearest(fp)
            .filter(|(_, d)| *d <= max_distance)
            .map(|(t, _)| t)
    }

    /// The nearest stored term and its DTW distance (diagnostics / tuning).
    pub fn nearest(&self, fp: &Fingerprint) -> Option<(&str, f32)> {
        if fp.is_empty() {
            return None;
        }
        let mut best: Option<(&str, f32)> = None;
        for e in &self.entries {
            let d = distance(&e.fp, fp);
            if best.map_or(true, |(_, bd)| d < bd) {
                best = Some((e.term.as_str(), d));
            }
        }
        best
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = bincode::serialize(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("bin.tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    pub fn load(path: &std::path::Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => bincode::deserialize(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

/// Two exemplars of the same term closer than this are merged on `learn`
/// (much stricter than the match threshold, so distinct recordings are kept as
/// separate exemplars for robustness rather than collapsed).
const MERGE_DISTANCE: f32 = 3.0;

// ── Model-agnostic word segmentation (energy/VAD fallback) ─────────────────
//
// When a backend gives no word timings (Voxtral, or any future engine), we
// split the audio into `n_words` spans ourselves by silence gaps. Approximate,
// but consistent enough for DTW matching.

fn equal_split(len: usize, n: usize) -> Vec<(usize, usize)> {
    (0..n).map(|i| (len * i / n, len * (i + 1) / n)).collect()
}

/// Split `audio` into `n_words` `(start, end)` sample ranges by energy.
pub fn segment_by_energy(audio: &[f32], n_words: usize) -> Vec<(usize, usize)> {
    if n_words == 0 || audio.is_empty() {
        return Vec::new();
    }
    if n_words == 1 {
        return vec![(0, audio.len())];
    }
    const FRAME: usize = 160; // 10 ms @ 16 kHz
    let energies: Vec<f32> = audio
        .chunks(FRAME)
        .map(|c| (c.iter().map(|x| x * x).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let nf = energies.len();
    if nf <= n_words {
        return equal_split(audio.len(), n_words);
    }
    let maxe = energies.iter().cloned().fold(0.0f32, f32::max);
    let thr = maxe * 0.15;

    // Contiguous above-threshold runs = candidate words (frame ranges).
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < nf {
        if energies[i] > thr {
            let s = i;
            while i < nf && energies[i] > thr {
                i += 1;
            }
            runs.push((s, i));
        } else {
            i += 1;
        }
    }
    if runs.is_empty() {
        return equal_split(audio.len(), n_words);
    }
    // Merge across the smallest gaps until we have at most n_words.
    while runs.len() > n_words {
        let mut best = 0;
        let mut best_gap = usize::MAX;
        for k in 0..runs.len() - 1 {
            let gap = runs[k + 1].0.saturating_sub(runs[k].1);
            if gap < best_gap {
                best_gap = gap;
                best = k;
            }
        }
        runs[best].1 = runs[best + 1].1;
        runs.remove(best + 1);
    }
    // Split the longest run until we have n_words.
    while runs.len() < n_words {
        let mut best = 0;
        let mut best_len = 0;
        for (k, &(s, e)) in runs.iter().enumerate() {
            if e - s > best_len {
                best_len = e - s;
                best = k;
            }
        }
        let (s, e) = runs[best];
        if e - s < 2 {
            break; // can't split further
        }
        let mid = (s + e) / 2;
        runs[best] = (s, mid);
        runs.insert(best + 1, (mid, e));
    }
    runs.iter()
        .map(|&(s, e)| (s * FRAME, (e * FRAME).min(audio.len())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthesize a `dur`-second frequency sweep (chirp) — non-stationary like
    /// real speech, so MFCC frames vary over time. Optional deterministic noise.
    fn chirp(f0: f32, f1: f32, dur: f32, noise: f32) -> Vec<f32> {
        let sr = 16000.0;
        let n = (sr * dur) as usize;
        let pi = std::f32::consts::PI;
        (0..n)
            .map(|i| {
                let t = i as f32 / sr;
                // Instantaneous phase of a linear chirp.
                let phase = 2.0 * pi * (f0 * t + (f1 - f0) * t * t / (2.0 * dur));
                let base = phase.sin();
                let jitter = ((i.wrapping_mul(2654435761)) % 1000) as f32 / 1000.0 - 0.5;
                base + noise * jitter
            })
            .collect()
    }

    #[test]
    fn same_signal_is_near_zero() {
        let fp = fingerprint(&chirp(200.0, 1200.0, 0.4, 0.0), 16000);
        assert!(!fp.is_empty());
        assert!(distance(&fp, &fp) < 1e-3);
    }

    #[test]
    fn discriminates_by_sound() {
        let a = fingerprint(&chirp(200.0, 1200.0, 0.4, 0.0), 16000);
        let a_noisy = fingerprint(&chirp(200.0, 1200.0, 0.4, 0.05), 16000);
        let far = fingerprint(&chirp(1400.0, 300.0, 0.4, 0.0), 16000); // opposite sweep
        let d_same = distance(&a, &a_noisy);
        let d_diff = distance(&a, &far);
        assert!(d_same < d_diff, "same={d_same} should be < diff={d_diff}");
        assert!(d_diff > d_same * 2.0, "expected clear separation");
    }

    #[test]
    fn store_matches_learned_term() {
        let mut store = AcousticStore::default();
        store.learn("Kasar", fingerprint(&chirp(250.0, 1000.0, 0.4, 0.0), 16000));
        let probe = fingerprint(&chirp(250.0, 1000.0, 0.4, 0.04), 16000);
        let far = fingerprint(&chirp(1500.0, 400.0, 0.4, 0.0), 16000);
        // Threshold midway between same-word and different-word distances.
        let d_far = distance(
            &fingerprint(&chirp(250.0, 1000.0, 0.4, 0.0), 16000),
            &far,
        );
        let thr = d_far * 0.5;
        assert_eq!(store.best_match(&probe, thr), Some("Kasar"));
        assert_eq!(store.best_match(&far, thr), None);
    }
}
