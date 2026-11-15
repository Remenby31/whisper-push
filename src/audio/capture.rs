use crate::util::LockSafe;
use anyhow::Result;
use cpal::traits::{DeviceTrait, StreamTrait};
use rubato::{FftFixedIn, Resampler};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use super::{RESAMPLE_CHUNK_SIZE, SAMPLE_RATE};

pub const SILENCE_RMS_THRESHOLD: f32 = 0.001;

/// Pre-reserve the recording buffer (~60 s @ 16 kHz, ~3.8 MB) so the audio
/// callback's `extend_from_slice` doesn't reallocate mid-stream (a realloc on
/// the real-time render thread can cause audible glitches / dropped samples).
/// 30 s was too short: 40 s dictations show up in the logs.
const PREALLOC_SAMPLES: usize = SAMPLE_RATE as usize * 60;

/// Everything between a device callback and the 16 kHz mono recording: downmix,
/// resample, append. Owned behind ONE mutex that only the callback takes while
/// the stream runs (live readers go through `Meter`), so the real-time thread
/// never waits on another thread. Shared by the cpal and VPIO backends.
pub struct Sink {
    channels: usize,
    in_rate: u32,
    resampler: Option<FftFixedIn<f32>>,
    mono: Vec<f32>,
    acc: Vec<f32>,
    inbuf: Vec<f32>,
    out: Vec<Vec<f32>>,
    /// Device frames received — what the 16 kHz length must add up to.
    in_frames: u64,
    peak: f32,
    buffer: Vec<f32>,
}

impl Sink {
    pub fn new(in_rate: u32, channels: usize) -> Result<Self> {
        let resampler = super::new_resampler(in_rate)?;
        let out = match resampler {
            Some(ref r) => r.output_buffer_allocate(true),
            None => Vec::new(),
        };
        Ok(Self {
            channels,
            in_rate,
            resampler,
            // Reused scratch so the real-time audio callback never allocates.
            mono: Vec::with_capacity(RESAMPLE_CHUNK_SIZE * 2),
            acc: Vec::with_capacity(RESAMPLE_CHUNK_SIZE * 4),
            inbuf: Vec::with_capacity(RESAMPLE_CHUNK_SIZE),
            out,
            in_frames: 0,
            peak: 0.0,
            buffer: Vec::with_capacity(PREALLOC_SAMPLES),
        })
    }

    /// One device callback's worth of interleaved samples.
    pub fn push(&mut self, data: &[f32]) {
        super::downmix_into(data, self.channels, &mut self.mono);
        if self.mono.is_empty() {
            return;
        }
        self.in_frames += self.mono.len() as u64;
        // Feed the live mic level to the "listening" pill (cheap RMS).
        crate::overlay::feed_level(crate::util::rms(&self.mono));

        let Some(r) = self.resampler.as_mut() else {
            let peak = self.mono.iter().fold(self.peak, |m, s| m.max(s.abs()));
            self.peak = peak;
            self.buffer.extend_from_slice(&self.mono);
            return;
        };
        self.acc.extend_from_slice(&self.mono);
        while self.acc.len() >= RESAMPLE_CHUNK_SIZE {
            // Drain the chunk BEFORE resampling, into a reused input buffer: a
            // panic inside `process` then can't make us re-feed the same
            // samples forever. `process_into_buffer` writes into the
            // pre-allocated `out` — no heap allocation on the real-time thread.
            self.inbuf.clear();
            self.inbuf.extend(self.acc.drain(..RESAMPLE_CHUNK_SIZE));
            if let Ok((_, n)) = r.process_into_buffer(&[&self.inbuf], &mut self.out, None) {
                let chunk = &self.out[0][..n];
                self.peak = chunk.iter().fold(self.peak, |m, s| m.max(s.abs()));
                self.buffer.extend_from_slice(chunk);
            }
        }
    }

    /// Samples (16 kHz) so far and their peak — for `Meter`.
    fn level(&self) -> (usize, f32) {
        (self.buffer.len(), self.peak)
    }

    /// The finished recording. Runs once the stream is paused (never on the
    /// audio thread), so it may allocate:
    /// - flushes the last partial resampler chunk and the filter's delay line —
    ///   previously the final ~20–30 ms (one chunk at 48 kHz + delay) never
    ///   came out, which could clip the end of a word on a quick release;
    /// - drops the filter's leading delay so the result lines up with the input;
    /// - clamps to [-1, 1]: a knock on the chassis delivered samples up to 5.1
    ///   from the built-in mic, values no engine was trained on.
    fn finish(&mut self) -> Vec<f32> {
        let mut audio = std::mem::take(&mut self.buffer);
        let expected = self.expected_len();
        if let Some(r) = self.resampler.as_mut() {
            let delay = r.output_delay();
            let rest = std::mem::take(&mut self.acc);
            let mut out = r.output_buffer_allocate(true);
            if let Ok((_, n)) = r.process_partial_into_buffer(Some(&[&rest]), &mut out, None) {
                audio.extend_from_slice(&out[0][..n]);
            }
            while audio.len() < expected + delay {
                match r.process_partial_into_buffer(None::<&[&[f32]]>, &mut out, None) {
                    Ok((_, n)) if n > 0 => audio.extend_from_slice(&out[0][..n]),
                    _ => break,
                }
            }
            audio.drain(..delay.min(audio.len()));
            audio.truncate(expected);
        }
        let mut clipped = 0usize;
        for s in audio.iter_mut() {
            if s.abs() > 1.0 {
                *s = s.clamp(-1.0, 1.0);
                clipped += 1;
            }
        }
        if clipped > 0 {
            warn!("{clipped} sample(s) beyond full scale clamped to ±1.0");
        }
        audio
    }

    fn expected_len(&self) -> usize {
        (self.in_frames * SAMPLE_RATE as u64 / self.in_rate as u64) as usize
    }
}

/// What the pipeline reads WHILE recording (mid-recording dead-mic check, the
/// fallback probe), published by the callback with plain atomics so a reader
/// never holds the lock the real-time thread needs. Before, `live_peak` scanned
/// the whole buffer under that lock.
#[derive(Default)]
pub struct Meter {
    len: AtomicUsize,
    peak_bits: AtomicU32,
}

impl Meter {
    pub(crate) fn update(&self, sink: &Sink) {
        let (len, peak) = sink.level();
        self.len.store(len, Ordering::Relaxed);
        self.peak_bits.store(peak.to_bits(), Ordering::Relaxed);
    }
}

enum Backend {
    Cpal(cpal::Stream),
    #[cfg(target_os = "macos")]
    Vpio(super::vpio::VpioStream),
}

impl Backend {
    fn pause(&mut self) {
        match self {
            Backend::Cpal(s) => {
                let _ = s.pause();
            }
            #[cfg(target_os = "macos")]
            Backend::Vpio(v) => v.pause(),
        }
    }
}

/// Audio recorder that captures to an in-memory f32 buffer at 16kHz mono.
pub struct AudioCapture {
    stream: Option<Backend>,
    sink: Arc<Mutex<Sink>>,
    meter: Arc<Meter>,
    /// Set if the input stream errored mid-capture (device unplugged, etc.).
    device_lost: Arc<AtomicBool>,
    /// The device actually opened (after resolving "auto" / a missing pin), so
    /// the pipeline can mark it dead and fall back if it captured no signal.
    device_name: String,
}

impl AudioCapture {
    /// Start capturing audio from the specified device (or default), raw.
    pub fn start(device_name: &str) -> Result<Self> {
        Self::start_with(device_name, false)
    }

    /// Start capturing; `voice_processing` asks for the OS echo canceller /
    /// noise suppressor / AGC (macOS VoiceProcessingIO). It is best-effort: if
    /// the unit can't be opened, or off macOS, this records raw instead.
    pub fn start_with(device_name: &str, voice_processing: bool) -> Result<Self> {
        #[cfg(target_os = "macos")]
        if voice_processing {
            let meter = Arc::new(Meter::default());
            match super::vpio::start(device_name, meter.clone()) {
                Ok((stream, name, sink)) => {
                    return Ok(Self {
                        stream: Some(Backend::Vpio(stream)),
                        sink,
                        meter,
                        device_lost: Arc::new(AtomicBool::new(false)),
                        device_name: name,
                    });
                }
                Err(e) => warn!("Voice processing unavailable ({e:#}) — recording raw"),
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = voice_processing;
        Self::start_cpal(device_name)
    }

    fn start_cpal(device_name: &str) -> Result<Self> {
        let device = super::find_input_device(device_name)?;
        let config = device.default_input_config()?;
        let device_sr = config.sample_rate().0;
        let device_channels = config.channels() as usize;
        let resolved_name = device.name().unwrap_or_else(|_| device_name.to_string());

        info!("Recording from '{resolved_name}' @ {device_sr}Hz {device_channels}ch");

        let sink = Arc::new(Mutex::new(Sink::new(device_sr, device_channels)?));
        let sink_cb = sink.clone();
        let meter = Arc::new(Meter::default());
        let meter_cb = meter.clone();
        let device_lost = Arc::new(AtomicBool::new(false));
        let device_lost_cb = device_lost.clone();

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // A panic here would unwind across the CoreAudio C callback — UB.
                // Contain it: lose this block, keep the stream alive.
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    // `lock_safe` (not bare `lock()`): a poisoned sink must not
                    // silently stop recording → empty transcription. Nobody else
                    // takes this lock while the stream runs.
                    let mut s = sink_cb.lock_safe();
                    s.push(data);
                    meter_cb.update(&s);
                }));
            },
            move |err| {
                warn!("Audio stream error: {err}");
                device_lost_cb.store(true, Ordering::Relaxed);
            },
            None,
        )?;

        stream.play()?;

        Ok(Self {
            stream: Some(Backend::Cpal(stream)),
            sink,
            meter,
            device_lost,
            device_name: resolved_name,
        })
    }

    /// True if the input stream reported an error mid-capture (device unplugged).
    pub fn device_lost(&self) -> bool {
        self.device_lost.load(Ordering::Relaxed)
    }

    /// The name of the device actually opened (after resolving "auto"/fallback).
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Peak amplitude captured so far, read live without stopping the stream —
    /// powers the mid-recording dead-mic health check and the fallback probe.
    pub fn live_peak(&self) -> f32 {
        f32::from_bits(self.meter.peak_bits.load(Ordering::Relaxed))
    }

    /// Samples (16 kHz mono) captured so far, read live.
    pub fn live_len(&self) -> usize {
        self.meter.len.load(Ordering::Relaxed)
    }

    /// Stop capture and return the recorded audio as 16kHz mono f32.
    pub fn stop(mut self) -> Vec<f32> {
        // Explicitly pause before drop — on macOS, dropping a cpal Stream
        // alone doesn't always tear down the AudioUnit immediately, leaving
        // the system "mic in use" indicator lit. pause() forces it down.
        if let Some(stream) = self.stream.as_mut() {
            stream.pause();
        }
        self.stream.take();
        let audio = self.sink.lock_safe().finish();
        let duration = audio.len() as f32 / SAMPLE_RATE as f32;
        let rms = crate::util::rms(&audio);
        let max = audio.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        info!(
            "Captured {:.1}s of audio ({} samples, RMS={:.6}, max={:.6})",
            duration,
            audio.len(),
            rms,
            max
        );
        if rms < SILENCE_RMS_THRESHOLD {
            warn!(
                "Audio is silence — microphone may not be captured. Check permission in System Settings → Privacy → Microphone"
            );
        }
        audio
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.as_mut() {
            stream.pause();
        }
        self.stream.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, hz: f32, n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    /// Feed `input` in device-sized callbacks, like CoreAudio does.
    fn record(rate: u32, input: &[f32]) -> Vec<f32> {
        let mut sink = Sink::new(rate, 1).unwrap();
        for block in input.chunks(512) {
            sink.push(block);
        }
        sink.finish()
    }

    #[test]
    fn resampled_length_matches_input_duration() {
        // 1.01 s at 48 kHz: not a multiple of the 1024-frame chunk, so the
        // tail used to be dropped (up to ~21 ms + the filter delay).
        let n = 48_480;
        let out = record(48_000, &sine(48_000, 440.0, n, 0.5));
        assert_eq!(out.len(), n / 3);
    }

    #[test]
    fn tail_reaches_the_end() {
        // Silence then a burst in the last 15 ms: it must survive the flush.
        let mut input = vec![0.0; 48_000];
        let burst = sine(48_000, 300.0, 720, 0.8);
        input.extend_from_slice(&burst);
        let out = record(48_000, &input);
        let tail = &out[out.len() - 160..];
        assert!(
            crate::util::rms(tail) > 0.2,
            "tail rms {}",
            crate::util::rms(tail)
        );
    }

    #[test]
    fn output_is_aligned_with_input() {
        // Same sine at 16 kHz vs resampled from 48 kHz: no leading delay left.
        let out = record(48_000, &sine(48_000, 200.0, 48_000, 0.5));
        let reference = sine(16_000, 200.0, 16_000, 0.5);
        let err: f32 = out[2000..14000]
            .iter()
            .zip(&reference[2000..14000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(err < 0.02, "max deviation {err}");
    }

    #[test]
    fn passthrough_at_16k_and_clamped() {
        let mut input = sine(16_000, 300.0, 16_000, 0.3);
        input[100] = 5.1;
        input[200] = -3.0;
        let out = record(16_000, &input);
        assert_eq!(out.len(), 16_000);
        assert_eq!(out[100], 1.0);
        assert_eq!(out[200], -1.0);
        assert_eq!(out[300], input[300]);
    }

    #[test]
    fn meter_tracks_len_and_peak_without_the_lock() {
        let mut sink = Sink::new(16_000, 2).unwrap();
        sink.push(&[0.1, 0.3, -0.5, -0.7]); // two stereo frames → two mono
        let meter = Meter::default();
        meter.update(&sink);
        assert_eq!(meter.len.load(Ordering::Relaxed), 2);
        assert!((f32::from_bits(meter.peak_bits.load(Ordering::Relaxed)) - 0.6).abs() < 1e-6);
    }
}
