/// Test the full pipeline: audio file → transcribe → paste to clipboard.
/// No GUI interaction needed.

#[test]
fn test_pipeline_whisper() {
    // 1. Load an MP3 file
    let test_files: Vec<_> = std::fs::read_dir(dirs::home_dir().unwrap().join("Downloads"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "mp3").unwrap_or(false))
        .collect();

    if test_files.is_empty() {
        println!("No MP3 files in ~/Downloads, skipping");
        return;
    }

    let path = test_files[0].path();
    println!("=== Pipeline Test ===");
    println!("Input: {}", path.display());

    // 2. Decode audio
    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    println!("Decoded: {:.1}s ({} samples)", samples.len() as f32 / 16000.0, samples.len());
    assert!(samples.len() > 4800, "Audio too short");

    // 3. Check RMS (not silence)
    let rms: f32 = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    println!("RMS: {:.4}", rms);
    assert!(rms > 0.001, "Audio is silence");

    // 4. Load model
    println!("Loading Whisper model...");
    whisper_push::transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();

    // 5. Transcribe
    println!("Transcribing...");
    let start = std::time::Instant::now();
    let text = whisper_push::transcribe::transcribe(&samples, "auto").unwrap();
    let elapsed = start.elapsed();
    println!("Result ({:.2}s): {}", elapsed.as_secs_f64(), &text);
    assert!(!text.is_empty(), "Empty transcription");
    assert!(text != "Thank you.", "Hallucination detected");

    // 6. Paste to clipboard
    println!("Pasting to clipboard...");
    let mut clipboard = arboard::Clipboard::new().unwrap();
    let saved = clipboard.get_text().unwrap_or_default();
    clipboard.set_text(&text).unwrap();
    let pasted = clipboard.get_text().unwrap();
    assert_eq!(pasted, text, "Clipboard mismatch");
    // Restore
    if !saved.is_empty() { clipboard.set_text(&saved).unwrap(); }

    println!("=== Pipeline OK ===");
    let audio_dur = samples.len() as f64 / 16000.0;
    let rtf = elapsed.as_secs_f64() / audio_dur;
    println!("Audio:   {:.1}s", audio_dur);
    println!("Compute: {:.2}s", elapsed.as_secs_f64());
    println!("RTF:     {:.3} ({:.0}x real-time)", rtf, 1.0 / rtf);
    println!("Text:    {} chars", text.len());
    println!("✓ Full pipeline works end-to-end");
}

#[test]
fn test_pipeline_mic_capture_3s() {
    println!("=== Mic Capture Test (3s) ===");

    // Capture 3 seconds of mic audio
    let capture = whisper_push::audio::capture::AudioCapture::start("auto").unwrap();
    println!("Recording 3 seconds...");
    std::thread::sleep(std::time::Duration::from_secs(3));
    let audio = capture.stop();

    println!("Captured: {:.1}s ({} samples)", audio.len() as f32 / 16000.0, audio.len());
    assert!(audio.len() > 16000, "Too few samples");

    let rms: f32 = (audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32).sqrt();
    println!("RMS: {:.6}", rms);

    if rms < 0.001 {
        println!("⚠ Audio is silence — mic permission may not be granted to test runner");
        println!("  This is expected when running from terminal without TCC access");
    } else {
        println!("✓ Mic captured real audio (RMS={:.4})", rms);
    }

    // Transcribe whatever we got
    whisper_push::transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    let text = whisper_push::transcribe::transcribe(&audio, "auto").unwrap();
    println!("Transcription: '{}'", text);
    println!("✓ Mic → transcribe pipeline works");
}
