use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use tracing::warn;

/// Embedded sound files (compiled into the binary).
const START_SOUND: &[u8] = include_bytes!("../../sounds/start.wav");
const STOP_SOUND: &[u8] = include_bytes!("../../sounds/stop.wav");

/// Play a start/stop sound non-blocking.
pub fn play_sound(name: &str) {
    let wav_data = match name {
        "start" => START_SOUND,
        "stop" => STOP_SOUND,
        _ => return,
    };

    // Spawn a thread to avoid blocking the caller
    let data = wav_data.to_vec();
    std::thread::spawn(move || {
        if let Err(e) = play_wav_bytes(&data) {
            warn!("Sound playback error: {e}");
        }
    });
}

fn play_wav_bytes(wav_data: &[u8]) -> Result<()> {
    let cursor = std::io::Cursor::new(wav_data);
    let reader = hound::WavReader::new(cursor)?;
    let spec = reader.spec();

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => reader
            .into_samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| s as f32 / i16::MAX as f32)
            .collect(),
    };

    let host = cpal::default_host();
    let device = host.default_output_device()
        .ok_or_else(|| anyhow::anyhow!("No output device"))?;
    let config = device.default_output_config()?;

    let samples = Arc::new(samples);
    let pos = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_clone = done.clone();
    let samples_clone = samples.clone();
    let pos_clone = pos.clone();

    let stream = device.build_output_stream(
        &config.into(),
        move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for sample in output.iter_mut() {
                let idx = pos_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if idx < samples_clone.len() {
                    *sample = samples_clone[idx];
                } else {
                    *sample = 0.0;
                    done_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        },
        |err| warn!("Playback error: {err}"),
        None,
    )?;

    stream.play()?;

    // Wait for playback to complete
    while !done.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Small tail to ensure the last buffer is flushed
    std::thread::sleep(std::time::Duration::from_millis(50));

    Ok(())
}
