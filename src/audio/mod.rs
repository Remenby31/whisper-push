pub mod capture;
pub mod playback;

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
