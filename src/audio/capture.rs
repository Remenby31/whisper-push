use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use rubato::{FftFixedIn, Resampler};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

const TARGET_SAMPLE_RATE: u32 = 16_000;
const TARGET_CHANNELS: u16 = 1;

/// Audio recorder that captures to an in-memory f32 buffer at 16kHz mono.
pub struct AudioCapture {
    stream: Option<cpal::Stream>,
    buffer: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
}

impl AudioCapture {
    /// Start capturing audio from the specified device (or default).
    pub fn start(device_name: &str) -> Result<Self> {
        let host = cpal::default_host();

        let device = if device_name == "auto" {
            host.default_input_device()
                .ok_or_else(|| anyhow::anyhow!("No input device found"))?
        } else {
            host.input_devices()?
                .find(|d| d.name().map(|n| n == device_name).unwrap_or(false))
                .ok_or_else(|| anyhow::anyhow!("Device '{}' not found", device_name))?
        };

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

        // Set up resampler if needed
        let needs_resample = device_sr != TARGET_SAMPLE_RATE;
        let resampler = if needs_resample {
            let chunk_size = 1024;
            Some(Arc::new(Mutex::new(
                FftFixedIn::<f32>::new(
                    device_sr as usize,
                    TARGET_SAMPLE_RATE as usize,
                    chunk_size,
                    1,    // sub_chunks
                    1,    // channels (we downmix first)
                )?,
            )))
        } else {
            None
        };

        // Accumulator for resampler (needs full chunks)
        let resample_buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Downmix to mono
                let mono: Vec<f32> = if device_channels > 1 {
                    data.chunks(device_channels)
                        .map(|frame| frame.iter().sum::<f32>() / device_channels as f32)
                        .collect()
                } else {
                    data.to_vec()
                };

                if let Some(ref resampler) = resampler {
                    // Accumulate samples and process in chunks
                    let mut acc = resample_buf.lock().unwrap();
                    acc.extend_from_slice(&mono);

                    let chunk_size = 1024;
                    while acc.len() >= chunk_size {
                        let chunk: Vec<f32> = acc.drain(..chunk_size).collect();
                        if let Ok(mut r) = resampler.lock() {
                            if let Ok(resampled) = r.process(&[&chunk], None) {
                                if let Some(channel) = resampled.first() {
                                    buffer_clone.lock().unwrap().extend_from_slice(channel);
                                }
                            }
                        }
                    }
                } else {
                    buffer_clone.lock().unwrap().extend_from_slice(&mono);
                }
            },
            |err| warn!("Audio stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            stream: Some(stream),
            buffer,
            device_sample_rate: device_sr,
        })
    }

    /// Stop capture and return the recorded audio as 16kHz mono f32.
    pub fn stop(mut self) -> Vec<f32> {
        // Drop the stream to stop recording
        self.stream.take();
        let audio = std::mem::take(&mut *self.buffer.lock().unwrap());
        let duration = audio.len() as f32 / TARGET_SAMPLE_RATE as f32;
        let rms = if audio.is_empty() {
            0.0
        } else {
            (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt()
        };
        let max = audio.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
        info!("Captured {:.1}s of audio ({} samples, RMS={:.6}, max={:.6})", duration, audio.len(), rms, max);
        if rms < 0.001 {
            warn!("Audio is silence — microphone may not be captured. Check permission in System Settings → Privacy → Microphone");
        }
        audio
    }

    /// Get current RMS level (for UI metering).
    pub fn current_rms(&self) -> f32 {
        let buf = self.buffer.lock().unwrap();
        if buf.is_empty() {
            return 0.0;
        }
        // RMS of last 1600 samples (~100ms at 16kHz)
        let window = &buf[buf.len().saturating_sub(1600)..];
        let sum: f32 = window.iter().map(|s| s * s).sum();
        (sum / window.len() as f32).sqrt()
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stream.take(); // ensure stream is stopped
    }
}
