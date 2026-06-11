use crate::util::LockSafe;
use anyhow::Result;
use cpal::traits::{DeviceTrait, StreamTrait};
use rubato::Resampler;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use super::{RESAMPLE_CHUNK_SIZE, SAMPLE_RATE};

pub const SILENCE_RMS_THRESHOLD: f32 = 0.001;

/// Audio recorder that captures to an in-memory f32 buffer at 16kHz mono.
pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
}

impl AudioCapture {
    /// Start capturing audio from the specified device (or default).
    pub fn start(device_name: &str) -> Result<Self> {
        let device = super::find_input_device(device_name)?;
        let config = device.default_input_config()?;
        let device_sr = config.sample_rate().0;
        let device_channels = config.channels() as usize;

        info!(
            "Recording from '{}' @ {}Hz {}ch",
            device.name().unwrap_or_default(),
            device_sr,
            device_channels
        );

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let buffer_clone = buffer.clone();
        let resampler = super::create_resampler(device_sr)?;
        let resample_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono = super::downmix_to_mono(data, device_channels);

                // Feed the live mic level to the "listening" pill (cheap RMS).
                if !mono.is_empty() {
                    let rms =
                        (mono.iter().map(|s| s * s).sum::<f32>() / mono.len() as f32).sqrt();
                    crate::overlay::feed_level(rms);
                }

                if let Some(ref resampler) = resampler {
                    let mut acc = resample_buf.lock_safe();
                    acc.extend_from_slice(&mono);

                    while acc.len() >= RESAMPLE_CHUNK_SIZE {
                        let chunk: Vec<f32> = acc.drain(..RESAMPLE_CHUNK_SIZE).collect();
                        if let Ok(mut r) = resampler.lock() {
                            if let Ok(resampled) = r.process(&[&chunk], None) {
                                if let Some(channel) = resampled.first() {
                                    buffer_clone.lock_safe().extend_from_slice(channel);
                                }
                            }
                        }
                    }
                } else {
                    buffer_clone.lock_safe().extend_from_slice(&mono);
                }
            },
            |err| warn!("Audio stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            stream: Some(stream),
            buffer,
        })
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
        let rms = if audio.is_empty() {
            0.0
        } else {
            (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt()
        };
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
