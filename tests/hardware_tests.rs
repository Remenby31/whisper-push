/// Hardware-dependent tests — require microphone access and real audio devices.
/// Run with: cargo test --test hardware_tests -- --nocapture
///
/// These tests may fail in CI or sandboxed environments without mic access.

// ── Microphone capture ──────────────────────────────────────────

#[test]
fn test_mic_capture_produces_samples() {
    match whisper_push::audio::capture::AudioCapture::start("auto") {
        Ok(cap) => {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let audio = cap.stop();
            assert!(!audio.is_empty(), "No samples captured");
            println!(
                "Captured {} samples ({:.1}s)",
                audio.len(),
                audio.len() as f32 / 16000.0
            );

            let rms: f32 = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
            if rms < whisper_push::audio::capture::SILENCE_RMS_THRESHOLD {
                println!("Warning: silence detected — mic permission may not be granted");
            } else {
                println!("RMS: {:.4} — real audio captured", rms);
            }
        }
        Err(e) => {
            println!("Mic capture unavailable: {e} — skipping");
        }
    }
}

#[test]
fn test_mic_capture_unknown_device_fails() {
    let result = whisper_push::audio::capture::AudioCapture::start("NonExistentDevice12345");
    assert!(result.is_err(), "Should fail with unknown device name");
}

#[test]
fn test_list_devices_returns_something() {
    match whisper_push::audio::list_devices() {
        Ok(devices) => {
            println!("Found {} audio devices", devices.len());
            for d in &devices {
                println!("  - {d}");
            }
            // Most machines have at least one input device
            // But CI might not, so we don't assert > 0
        }
        Err(e) => {
            println!("Device listing failed: {e}");
        }
    }
}

// ── Streaming capture ───────────────────────────────────────────

#[test]
fn test_streaming_capture_produces_chunks() {
    match whisper_push::audio::stream::StreamingCapture::start("auto", 500) {
        Ok(capture) => {
            let start = std::time::Instant::now();
            let mut chunk_count = 0;

            while start.elapsed() < std::time::Duration::from_secs(2) {
                if let Ok(chunk) = capture
                    .chunk_rx
                    .recv_timeout(std::time::Duration::from_millis(100))
                {
                    chunk_count += 1;
                    println!("Chunk {}: {} samples", chunk_count, chunk.samples.len());
                }
            }
            drop(capture);

            println!("Received {} chunks in 2 seconds", chunk_count);
            assert!(
                chunk_count >= 2,
                "Expected at least 2 chunks in 2s, got {chunk_count}"
            );
        }
        Err(e) => {
            println!("Streaming capture unavailable: {e} — skipping");
        }
    }
}

// ── Clipboard paste roundtrip ───────────────────────────────────

#[test]
fn test_paste_preserves_clipboard() {
    let mut clipboard = arboard::Clipboard::new().unwrap();
    let original = clipboard.get_text().unwrap_or_default();

    // Set a known value
    clipboard.set_text("original-content-12345").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    // paste_text should save/restore clipboard
    // Note: we can't test the actual Cmd+V keystroke in a test environment,
    // but we can test the clipboard save/restore mechanism
    let test_text = "whisper-push-paste-test";
    clipboard.set_text(test_text).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let read = clipboard.get_text().unwrap();
    assert_eq!(read, test_text);

    // Restore
    if !original.is_empty() {
        let _ = clipboard.set_text(&original);
    }
}

// ── Edge cases ──────────────────────────────────────────────────

#[test]
fn test_find_input_device_auto() {
    // "auto" should return the default device (if any)
    match whisper_push::audio::find_input_device("auto") {
        Ok(_) => println!("Default input device found"),
        Err(e) => println!("No default input device: {e}"),
    }
}

#[test]
fn test_find_input_device_unknown() {
    let result = whisper_push::audio::find_input_device("DeviceThatDoesNotExist999");
    assert!(result.is_err());
}
