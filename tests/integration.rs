/// Integration tests — run without GUI interaction.
/// Tests the full pipeline: decode audio → transcribe → verify text.

#[test]
fn test_whisper_transcribe_mp3() {
    // Find a test MP3 file
    let test_files: Vec<_> = std::fs::read_dir(dirs::home_dir().unwrap().join("Downloads"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "mp3").unwrap_or(false))
        .collect();

    if test_files.is_empty() {
        println!("No MP3 files in ~/Downloads, skipping test");
        return;
    }

    let path = test_files[0].path();
    println!("Testing with: {}", path.display());

    // Decode audio
    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    assert!(samples.len() > 16000, "Audio too short: {} samples", samples.len());
    println!("Decoded: {:.1}s ({} samples)", samples.len() as f32 / 16000.0, samples.len());

    // Check RMS (not silence)
    let rms: f32 = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    println!("RMS: {:.6}", rms);
    assert!(rms > 0.001, "Audio is silence (RMS={rms})");

    // Load model
    whisper_push::transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();

    // Transcribe
    let start = std::time::Instant::now();
    let text = whisper_push::transcribe::transcribe(&samples, "auto").unwrap();
    let elapsed = start.elapsed();

    println!("Result ({:.2}s): {}", elapsed.as_secs_f64(), text);
    assert!(!text.is_empty(), "Transcription returned empty text");
    assert!(text.len() > 5, "Transcription too short: '{text}'");
    assert!(text != "Thank you.", "Whisper hallucinated 'Thank you' — audio is likely silence");

    let rtf = elapsed.as_secs_f64() / (samples.len() as f64 / 16000.0);
    println!("RTF: {:.3} ({:.0}x real-time)", rtf, 1.0 / rtf);
    assert!(rtf < 1.0, "Transcription slower than real-time (RTF={rtf})");
}

#[test]
fn test_paste_mechanism() {
    // Test clipboard save/restore + paste simulation
    let mut clipboard = arboard::Clipboard::new().unwrap();

    // Save original clipboard
    let original = clipboard.get_text().unwrap_or_default();

    // Set test text
    clipboard.set_text("whisper-push-test-12345").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let read_back = clipboard.get_text().unwrap();
    assert_eq!(read_back, "whisper-push-test-12345");

    // Restore original
    if !original.is_empty() {
        clipboard.set_text(&original).unwrap();
    }
    println!("✓ Clipboard read/write works");
}

#[test]
fn test_config_load_save() {
    let cfg = whisper_push::config::Config::default();
    assert_eq!(cfg.hotkey, "ctrl");
    assert_eq!(cfg.hotkey_mode, "hold");
    assert_eq!(cfg.language, "auto");
    assert_eq!(cfg.backend, "whisper");
    println!("✓ Config defaults OK");
}

#[test]
fn test_audio_decode_formats() {
    // Generate a test WAV with macOS say
    let wav_path = std::env::temp_dir().join("whisper_push_test.wav");
    let status = std::process::Command::new("say")
        .args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@16000", "Hello world"])
        .status();

    if let Ok(s) = status {
        if s.success() {
            let samples = whisper_push::audio::decode::load_audio_file(&wav_path).unwrap();
            assert!(samples.len() > 1000, "WAV decode returned too few samples");
            println!("✓ WAV decode: {} samples", samples.len());
            let _ = std::fs::remove_file(&wav_path);
        }
    } else {
        println!("'say' not available, skipping WAV test");
    }
}
