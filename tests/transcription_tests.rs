/// Transcription integration tests — require the Whisper model to be downloaded.
/// Run with: cargo test --test transcription_tests -- --nocapture
///
/// These tests use macOS `say` to generate synthetic audio, so they work
/// without any external test fixtures.

use whisper_push::audio;
use whisper_push::model_manager;
use whisper_push::transcribe;

fn ensure_whisper_loaded() {
    if !transcribe::is_loaded() {
        transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    }
}

/// Generate synthetic audio using macOS `say` command.
/// Returns 16kHz mono f32 samples, or None if `say` is not available.
fn generate_audio(text: &str) -> Option<Vec<f32>> {
    let wav_path = std::env::temp_dir().join(format!(
        "whisper_push_test_{}.wav",
        text.len()
    ));
    let status = std::process::Command::new("say")
        .args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@16000", text])
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let samples = audio::decode::load_audio_file(&wav_path).ok()?;
    let _ = std::fs::remove_file(&wav_path);
    Some(samples)
}

#[test]
fn test_whisper_transcribe_synthetic() {
    let samples = match generate_audio("Hello world, this is a test of Whisper Push") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };

    assert!(samples.len() > audio::MIN_AUDIO_SAMPLES);

    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();

    println!("Transcribed: '{text}'");
    assert!(!text.is_empty(), "Transcription returned empty text");
    // Should contain at least one of the key words
    let lower = text.to_lowercase();
    assert!(
        lower.contains("hello") || lower.contains("world") || lower.contains("test") || lower.contains("whisper"),
        "Transcription doesn't contain expected words: '{text}'"
    );
}

#[test]
fn test_whisper_transcribe_silence() {
    // 3 seconds of silence
    let silence = vec![0.0f32; 48_000];

    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&silence, "auto", &backend).unwrap();

    println!("Silence transcription: '{text}'");
    // Should be empty or very short — definitely not a hallucination
    assert!(
        text.is_empty() || text.len() < 20,
        "Whisper hallucinated on silence: '{text}'"
    );
}

#[test]
fn test_whisper_transcribe_short_audio() {
    // Less than MIN_AUDIO_SAMPLES — the pipeline skips this, but the raw transcriber
    // should handle it gracefully
    let short = vec![0.1f32; 1000]; // ~62ms

    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    // Should not panic, may return empty or error
    let result = transcribe::transcribe_with_backend(&short, "auto", &backend);
    println!("Short audio result: {result:?}");
    // We just check it doesn't panic
}

#[test]
fn test_whisper_load_unload_reload() {
    transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    assert!(transcribe::is_loaded());

    transcribe::unload_model();
    assert!(!transcribe::is_loaded());

    transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    assert!(transcribe::is_loaded());
}

#[test]
fn test_whisper_performance_rtf() {
    let samples = match generate_audio("The quick brown fox jumps over the lazy dog. This is a longer sentence to test real-time factor.") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };

    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let start = std::time::Instant::now();
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let elapsed = start.elapsed();

    let audio_duration = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    let rtf = elapsed.as_secs_f64() / audio_duration;

    println!("Audio: {:.1}s, Compute: {:.2}s, RTF: {:.3}", audio_duration, elapsed.as_secs_f64(), rtf);
    println!("Text: '{text}'");

    assert!(rtf < 1.0, "Transcription slower than real-time: RTF={rtf:.3}");
}

#[test]
fn test_transcribe_without_model_loaded() {
    // Unload first to ensure clean state
    transcribe::unload_model();

    let audio = vec![0.1f32; 16_000];
    let backend = transcribe::Backend::WhisperLocal("ggml-large-v3-turbo-q5_0.bin".into());
    let result = transcribe::transcribe_with_backend(&audio, "auto", &backend);

    assert!(result.is_err(), "Should fail when model is not loaded");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("not loaded"), "Error should mention 'not loaded': {err}");

    // Reload for other tests
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");
}

// ── Audio decode formats ────────────────────────────────────────

#[test]
fn test_decode_wav_synthetic() {
    let wav_path = std::env::temp_dir().join("whisper_push_test_decode.wav");
    let status = std::process::Command::new("say")
        .args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@16000", "Test"])
        .status();

    match status {
        Ok(s) if s.success() => {
            let samples = audio::decode::load_audio_file(&wav_path).unwrap();
            assert!(samples.len() > 1000, "WAV decode too few samples: {}", samples.len());
            let _ = std::fs::remove_file(&wav_path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

#[test]
fn test_decode_unsupported_format() {
    let path = std::env::temp_dir().join("whisper_push_test_bad.txt");
    std::fs::write(&path, "this is not audio").unwrap();

    let result = audio::decode::load_audio_file(&path);
    assert!(result.is_err(), "Should fail on text file");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_decode_nonexistent_file() {
    let result = audio::decode::load_audio_file(std::path::Path::new("/tmp/does_not_exist_xyz.wav"));
    assert!(result.is_err());
}
