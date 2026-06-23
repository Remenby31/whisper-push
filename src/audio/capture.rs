use crate::util::LockSafe;
use anyhow::Result;
use cpal::traits::{DeviceTrait, StreamTrait};
use rubato::Resampler;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use super::{RESAMPLE_CHUNK_SIZE, SAMPLE_RATE};

pub const SILENCE_RMS_THRESHOLD: f32 = 0.001;

/// Pre-reserve the recording buffer (~30 s @ 16 kHz) so the audio callback's
/// `extend_from_slice` doesn't reallocate mid-stream (a realloc on the real-time
/// render thread can cause audible glitches / dropped samples).
const PREALLOC_SAMPLES: usize = SAMPLE_RATE as usize * 30;

/// Audio recorder that captures to an in-memory f32 buffer at 16kHz mono.
pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    /// Set if the input stream errored mid-capture (device unplugged, etc.).
    device_lost: Arc<AtomicBool>,
    /// The device actually opened (after resolving "auto" / a missing pin), so
    /// the pipeline can mark it dead and fall back if it captured no signal.
    device_name: String,
}

impl AudioCapture {
    /// Start capturing audio from the specified device (or default).
    pub fn start(device_name: &str) -> Result<Self> {
        let device = super::find_input_device(device_name)?;
        let config = device.default_input_config()?;
        let device_sr = config.sample_rate().0;
        let device_channels = config.channels() as usize;
        let resolved_name = device.name().unwrap_or_else(|_| device_name.to_string());

        info!("Recording from '{resolved_name}' @ {device_sr}Hz {device_channels}ch");

        let buffer: Arc<Mutex<Vec<f32>>> =
            Arc::new(Mutex::new(Vec::with_capacity(PREALLOC_SAMPLES)));
        let buffer_clone = buffer.clone();
        let resampler = super::create_resampler(device_sr)?;
        let resample_buf: Arc<Mutex<Vec<f32>>> =
            Arc::new(Mutex::new(Vec::with_capacity(RESAMPLE_CHUNK_SIZE * 4)));
        let device_lost = Arc::new(AtomicBool::new(false));
        let device_lost_cb = device_lost.clone();

        // Reused scratch so the real-time audio callback never allocates:
        //  - `mono`:  downmix output
        //  - `inbuf`: one resampler input chunk (drained out of `acc`)
        //  - `out`:   the resampler's output buffer (pre-sized by rubato)
        let mut mono = Vec::<f32>::with_capacity(RESAMPLE_CHUNK_SIZE * 2);
        let mut inbuf = Vec::<f32>::with_capacity(RESAMPLE_CHUNK_SIZE);
        let mut out: Vec<Vec<f32>> = match resampler {
            Some(ref r) => r.lock_safe().output_buffer_allocate(true),
            None => Vec::new(),
        };

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // A panic here would unwind across the CoreAudio C callback — UB.
                // Contain it: lose this block, keep the stream alive.
                let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    super::downmix_into(data, device_channels, &mut mono);

                    // Feed the live mic level to the "listening" pill (cheap RMS).
                    if !mono.is_empty() {
                        crate::overlay::feed_level(crate::util::rms(&mono));
                    }

                    if let Some(ref resampler) = resampler {
                        let mut acc = resample_buf.lock_safe();
                        acc.extend_from_slice(&mono);
                        // `lock_safe` (not bare `lock()`): a poisoned resampler
                        // must not silently stop resampling → empty transcription.
                        let mut r = resampler.lock_safe();
                        while acc.len() >= RESAMPLE_CHUNK_SIZE {
                            // Drain the chunk BEFORE resampling, into a reused input
                            // buffer: a panic inside `process` then can't make us
                            // re-feed the same samples forever. `process_into_buffer`
                            // writes into the pre-allocated `out`, so there is NO
                            // heap allocation on the real-time thread (the plain
                            // `process` convenience method allocates its output Vec
                            // every call — the dropout hazard this avoids).
                            inbuf.clear();
                            inbuf.extend(acc.drain(..RESAMPLE_CHUNK_SIZE));
                            if let Ok((_, n_out)) = r.process_into_buffer(&[&inbuf], &mut out, None)
                            {
                                if let Some(channel) = out.first() {
                                    buffer_clone
                                        .lock_safe()
                                        .extend_from_slice(&channel[..n_out]);
                                }
                            }
                        }
                    } else {
                        buffer_clone.lock_safe().extend_from_slice(&mono);
                    }
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
            stream: Some(stream),
            buffer,
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

    /// Stop capture and return the recorded audio as 16kHz mono f32.
    pub fn stop(mut self) -> Vec<f32> {
        // Explicitly pause before drop — on macOS, dropping a cpal Stream
        // alone doesn't always tear down the AudioUnit immediately, leaving
        // the system "mic in use" indicator lit. pause() forces it down.
        if let Some(stream) = self.stream.as_ref() {
            let _ = stream.pause();
        }
        self.stream.take();
        let audio = std::mem::take(&mut *self.buffer.lock_safe());
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
        if let Some(stream) = self.stream.as_ref() {
            let _ = stream.pause();
        }
        self.stream.take();
    }
}
