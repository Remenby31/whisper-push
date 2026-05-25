/// Quick mic capture test — run with: cargo test --test test_mic -- --nocapture
#[test]
fn test_mic_capture() {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{Arc, Mutex};

    let host = cpal::default_host();
    let device = host.default_input_device().expect("no input device");
    let config = device.default_input_config().expect("no config");

    println!("Device: {}", device.name().unwrap());
    println!("Sample rate: {} Hz", config.sample_rate().0);
    println!("Channels: {}", config.channels());

    let buf: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let buf2 = buf.clone();

    let stream = device.build_input_stream(
        &config.into(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            buf2.lock().unwrap().extend_from_slice(data);
        },
        |e| eprintln!("Error: {e}"),
        None,
    ).expect("build stream failed");

    stream.play().expect("play failed");
    println!("Recording 3 seconds... (speak now!)");
    std::thread::sleep(std::time::Duration::from_secs(3));
    drop(stream);

    let samples = buf.lock().unwrap();
    let rms: f32 = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let max = samples.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
    println!("Samples: {}", samples.len());
    println!("RMS: {:.6}", rms);
    println!("Max: {:.6}", max);

    if rms < 0.001 {
        println!("⚠️  Audio is SILENCE — mic permission may not be granted");
    } else {
        println!("✓ Audio captured! RMS={:.4}", rms);
    }

    assert!(samples.len() > 0, "No samples captured");
}
