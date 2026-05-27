pub mod capture;
pub mod decode;
pub mod playback;
pub mod stream;

use anyhow::Result;

/// Whisper expects 16kHz mono audio.
pub const SAMPLE_RATE: u32 = 16_000;
/// Minimum audio length to attempt transcription (0.3s at 16kHz).
pub const MIN_AUDIO_SAMPLES: usize = 4800;

/// List available input audio devices.
pub fn list_devices() -> Result<Vec<String>> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .input_devices()?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok(devices)
}
