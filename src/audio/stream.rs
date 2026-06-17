//! Streaming audio capture — feeds chunks to a consumer in real-time.
//! Used for streaming transcription (Voxtral Realtime).
//!
//! Reserved: streaming dictation is disabled (blocks on the M4 Metal shader
//! compile — see CLAUDE.md); the live path is batch capture in `capture.rs`.
//! Kept for when streaming is re-enabled.
#![allow(dead_code)]

use crate::util::LockSafe;
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::Receiver;
use rubato::Resampler;
use std::sync::Arc;
use tracing::{info, warn};

use super::{RESAMPLE_CHUNK_SIZE, SAMPLE_RATE};

/// A chunk of audio ready for transcription.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// 16kHz mono f32 samples
    pub samples: Vec<f32>,
}

/// Streaming audio capture that sends chunks via a channel.
pub struct StreamingCapture {
    stream: Option<cpal::Stream>,
    /// Receives audio chunks as they're captured
    pub chunk_rx: Receiver<AudioChunk>,
}

impl StreamingCapture {
    /// Start streaming capture. Sends AudioChunks every `chunk_duration_ms` milliseconds.
    pub fn start(device_name: &str, chunk_duration_ms: u32) -> anyhow::Result<Self> {
        let device = super::find_input_device(device_name)?;
        let config = device.default_input_config()?;
        let device_sr = config.sample_rate().0;
        let device_channels = config.channels() as usize;

        info!(
            "Streaming from '{}' @ {}Hz {}ch, chunk={}ms",
            device.name().unwrap_or_default(),
            device_sr,
            device_channels,
            chunk_duration_ms
        );

        let (chunk_tx, chunk_rx) = crossbeam_channel::bounded(32);

        let chunk_size_target = (SAMPLE_RATE * chunk_duration_ms / 1000) as usize;
        let resampler = super::create_resampler(device_sr)?;
        let acc_buf: Arc<std::sync::Mutex<Vec<f32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let resample_acc: Arc<std::sync::Mutex<Vec<f32>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mono = super::downmix_to_mono(data, device_channels);

                let samples_16k = if let Some(ref resampler) = resampler {
                    let mut racc = resample_acc.lock_safe();
                    racc.extend_from_slice(&mono);

                    let mut output = Vec::new();
                    while racc.len() >= RESAMPLE_CHUNK_SIZE {
                        let c: Vec<f32> = racc.drain(..RESAMPLE_CHUNK_SIZE).collect();
                        if let Ok(mut r) = resampler.lock() {
                            if let Ok(resampled) = r.process(&[&c], None) {
                                if let Some(ch) = resampled.first() {
                                    output.extend_from_slice(ch);
                                }
                            }
                        }
                    }
                    output
                } else {
                    mono
                };

                let mut acc = acc_buf.lock_safe();
                acc.extend_from_slice(&samples_16k);

                while acc.len() >= chunk_size_target {
                    let chunk_data: Vec<f32> = acc.drain(..chunk_size_target).collect();
                    let _ = chunk_tx.try_send(AudioChunk {
                        samples: chunk_data,
                    });
                }
            },
            |err| warn!("Audio stream error: {err}"),
            None,
        )?;

        stream.play()?;

        Ok(Self {
            stream: Some(stream),
            chunk_rx,
        })
    }
}

impl Drop for StreamingCapture {
    fn drop(&mut self) {
        // Pause before drop so macOS releases the AudioUnit and the system
        // "mic in use" indicator turns off (cpal Stream::drop alone is not
        // enough on macOS).
        if let Some(stream) = self.stream.as_ref() {
            let _ = stream.pause();
        }
        self.stream.take();
    }
}
