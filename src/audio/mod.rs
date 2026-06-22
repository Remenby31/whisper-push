pub mod capture;
pub mod decode;
pub mod playback;
pub mod stream;

use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait};
use rubato::FftFixedIn;
use std::sync::{Arc, Mutex};

/// Whisper expects 16kHz mono audio.
pub const SAMPLE_RATE: u32 = 16_000;
/// Minimum audio length to attempt transcription (0.3s at 16kHz).
pub const MIN_AUDIO_SAMPLES: usize = 4800;
/// Resampler chunk size.
pub const RESAMPLE_CHUNK_SIZE: usize = 1024;

/// List available input audio devices.
pub fn list_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .input_devices()?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok(devices)
}

/// List available output audio devices (used for sound-feedback playback).
pub fn list_output_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok(devices)
}

/// Find an input device by name ("auto" = default).
pub fn find_input_device(name: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if name == "auto" {
        host.default_input_device()
            .ok_or_else(|| anyhow::anyhow!("No input device found"))
    } else {
        host.input_devices()?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .or_else(|| {
                // The pinned device is gone (unplugged headset, disconnected
                // dock). Rather than refuse to record, fall back to the system
                // default — dictation keeps working; the user can re-pick later.
                tracing::warn!(
                    "Input device '{name}' not found — falling back to the default device"
                );
                host.default_input_device()
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Device '{}' not found and no default input device", name)
            })
    }
}

/// Create a resampler from device sample rate to 16kHz, if needed.
pub fn create_resampler(device_sr: u32) -> Result<Option<Arc<Mutex<FftFixedIn<f32>>>>> {
    if device_sr == SAMPLE_RATE {
        return Ok(None);
    }
    let resampler = FftFixedIn::<f32>::new(
        device_sr as usize,
        SAMPLE_RATE as usize,
        RESAMPLE_CHUNK_SIZE,
        1,
        1,
    )?;
    Ok(Some(Arc::new(Mutex::new(resampler))))
}

/// Downmix interleaved multi-channel audio to mono.
pub fn downmix_to_mono(data: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        data.to_vec()
    } else {
        data.chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    }
}

/// Downmix into a reused buffer — no per-call heap allocation, for the real-time
/// audio callback (allocating on the render thread risks dropouts).
pub fn downmix_into(data: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 1 {
        out.extend_from_slice(data);
    } else {
        out.extend(
            data.chunks(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_downmix_mono_passthrough() {
        let data = vec![0.1, 0.2, 0.3];
        let result = downmix_to_mono(&data, 1);
        assert_eq!(result, data);
    }

    #[test]
    fn test_downmix_stereo() {
        // L=1.0 R=0.0 → mono=0.5, L=0.0 R=1.0 → mono=0.5
        let data = vec![1.0, 0.0, 0.0, 1.0];
        let result = downmix_to_mono(&data, 2);
        assert_eq!(result, vec![0.5, 0.5]);
    }

    #[test]
    fn test_downmix_surround_51() {
        // 6 channels, all 0.6 → mono = 0.6
        let data = vec![0.6; 6];
        let result = downmix_to_mono(&data, 6);
        assert_eq!(result.len(), 1);
        assert!((result[0] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn test_downmix_empty() {
        let result = downmix_to_mono(&[], 2);
        assert!(result.is_empty());
    }

    #[test]
    fn test_constants() {
        assert_eq!(SAMPLE_RATE, 16_000);
        assert_eq!(MIN_AUDIO_SAMPLES, 4800); // 0.3s at 16kHz
        assert_eq!(RESAMPLE_CHUNK_SIZE, 1024);
    }

    #[test]
    fn test_create_resampler_same_rate_returns_none() {
        let r = create_resampler(16_000).unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn test_create_resampler_different_rate_returns_some() {
        let r = create_resampler(44_100).unwrap();
        assert!(r.is_some());
    }
}
