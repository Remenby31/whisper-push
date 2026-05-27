pub mod capture;
pub mod decode;
pub mod playback;
pub mod stream;

use anyhow::Result;

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

/// List available output audio devices (used for sound-feedback playback).
pub fn list_output_devices() -> Result<Vec<String>> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let devices: Vec<String> = host
        .output_devices()?
        .filter_map(|d| d.name().ok())
        .collect();
    Ok(devices)
}
