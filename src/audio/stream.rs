//! Streaming audio capture — feeds chunks to a consumer in real-time.
//! Used for streaming transcription (Parakeet Nemotron, Voxtral Realtime).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Receiver;
use rubato::{FftFixedIn, Resampler};
use std::sync::Arc;
use tracing::{info, warn};

const TARGET_SAMPLE_RATE: u32 = 16_000;

/// A chunk of audio ready for transcription.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// 16kHz mono f32 samples
    pub samples: Vec<f32>,
    /// Timestamp of this chunk relative to recording start
    pub offset_samples: usize,
}

/// Streaming audio capture that sends chunks via a channel.
pub struct StreamingCapture {
    stream: Option<cpal::Stream>,
    /// Receives audio chunks as they're captured
    pub chunk_rx: Receiver<AudioChunk>,
    /// Total samples captured so far
    total_samples: Arc<std::sync::atomic::AtomicUsize>,
}

impl StreamingCapture {
    /// Start streaming capture. Sends AudioChunks every `chunk_duration_ms` milliseconds.
    pub fn start(device_name: &str, chunk_duration_ms: u32) -> anyhow::Result<Self> {
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
            "Streaming from '{}' @ {}Hz {}ch, chunk={}ms",
            device.name().unwrap_or_default(),
            device_sr, device_channels, chunk_duration_ms
        );

        let (chunk_tx, chunk_rx) = crossbeam_channel::bounded(32);
        let total_samples = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let total_clone = total_samples.clone();

        // How many samples per chunk at target rate
        let chunk_size_target = (TARGET_SAMPLE_RATE * chunk_duration_ms / 1000) as usize;
        // How many samples per chunk at device rate
        let _chunk_size_device = (device_sr * chunk_duration_ms / 1000) as usize;

        // Resampler
        let needs_resample = device_sr != TARGET_SAMPLE_RATE;
        let resampler = if needs_resample {
            Some(Arc::new(std::sync::Mutex::new(
                FftFixedIn::<f32>::new(
                    device_sr as usize,
                    TARGET_SAMPLE_RATE as usize,
                    1024, 1, 1,
                )?,
            )))
        } else {
            None
        };

        // Accumulator buffer
        let acc_buf: Arc<std::sync::Mutex<Vec<f32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let resample_acc: Arc<std::sync::Mutex<Vec<f32>>> = Arc::new(std::sync::Mutex::new(Vec::new()));

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

                // Resample if needed
                let samples_16k = if let Some(ref resampler) = resampler {
                    let mut racc = resample_acc.lock().unwrap();
                    racc.extend_from_slice(&mono);

                    let mut output = Vec::new();
                    let chunk = 1024;
                    while racc.len() >= chunk {
                        let c: Vec<f32> = racc.drain(..chunk).collect();
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

                // Accumulate and emit chunks
                let mut acc = acc_buf.lock().unwrap();
                acc.extend_from_slice(&samples_16k);

                while acc.len() >= chunk_size_target {
                    let chunk_data: Vec<f32> = acc.drain(..chunk_size_target).collect();
                    let offset = total_clone.fetch_add(chunk_size_target, std::sync::atomic::Ordering::Relaxed);
                    let _ = chunk_tx.try_send(AudioChunk {
                        samples: chunk_data,
                        offset_samples: offset,
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
            total_samples,
        })
    }

}

impl Drop for StreamingCapture {
    fn drop(&mut self) {
        self.stream.take();
    }
}
