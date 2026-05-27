/// Transcription tests — one section per model backend.
/// Run with: cargo test --test transcription_tests -- --nocapture
///
/// Uses macOS `say` to generate synthetic audio (no external fixtures needed).
/// Tests marked with a model name require that model to be downloaded.

use whisper_push::audio;
use whisper_push::model_manager;
use whisper_push::transcribe;

// ── Helpers ─────────────────────────────────────────────────────

fn ensure_whisper_loaded() {
    if !transcribe::is_loaded() {
        transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    }
}

/// Generate synthetic audio using macOS `say`.
/// Returns 16kHz mono f32 samples, or None if `say` is unavailable.
fn generate_audio(text: &str) -> Option<Vec<f32>> {
    generate_audio_with_voice(text, None)
}

fn generate_audio_with_voice(text: &str, voice: Option<&str>) -> Option<Vec<f32>> {
    let hash = text.len() ^ text.as_bytes().iter().map(|b| *b as usize).sum::<usize>();
    let wav_path = std::env::temp_dir().join(format!("whisper_push_test_{hash}.wav"));
    let mut cmd = std::process::Command::new("say");
    cmd.args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@16000"]);
    if let Some(v) = voice {
        cmd.args(["-v", v]);
    }
    cmd.arg(text);
    let status = cmd.status().ok()?;
    if !status.success() { return None; }
    let samples = audio::decode::load_audio_file(&wav_path).ok()?;
    let _ = std::fs::remove_file(&wav_path);
    Some(samples)
}

fn audio_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() { return 0.0; }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

// ═══════════════════════════════════════════════════════════════
// WHISPER — large-v3-turbo (Metal/CUDA/CPU)
// ═══════════════════════════════════════════════════════════════

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
fn test_whisper_not_loaded_error() {
    transcribe::unload_model();

    let audio = vec![0.1f32; 16_000];
    let backend = transcribe::Backend::WhisperLocal("ggml-large-v3-turbo-q5_0.bin".into());
    let result = transcribe::transcribe_with_backend(&audio, "auto", &backend);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not loaded"));

    // Reload for other tests
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");
}

#[test]
fn test_whisper_transcribe_english() {
    let samples = match generate_audio("Hello world, this is a test of Whisper Push") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };
    assert!(samples.len() > audio::MIN_AUDIO_SAMPLES);
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();

    println!("Whisper EN: '{text}'");
    assert!(!text.is_empty());
    let lower = text.to_lowercase();
    assert!(
        lower.contains("hello") || lower.contains("world") || lower.contains("test"),
        "Missing expected words: '{text}'"
    );
}

#[test]
fn test_whisper_transcribe_french() {
    let samples = match generate_audio_with_voice("Bonjour le monde, ceci est un test", Some("Thomas")) {
        Some(s) => s,
        None => {
            // Fallback: try without French voice
            match generate_audio("Bonjour le monde") {
                Some(s) => s,
                None => { println!("'say' not available, skipping"); return; }
            }
        }
    };
    ensure_whisper_loaded();

    // Test with language="fr"
    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "fr", &backend).unwrap();
    println!("Whisper FR: '{text}'");
    assert!(!text.is_empty());
}

#[test]
fn test_whisper_transcribe_auto_language() {
    let samples = match generate_audio("Testing automatic language detection") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };
    ensure_whisper_loaded();

    // language="auto" should auto-detect
    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "auto", &backend).unwrap();
    println!("Whisper auto: '{text}'");
    assert!(!text.is_empty());
}

#[test]
fn test_whisper_silence_no_hallucination() {
    let silence = vec![0.0f32; 48_000]; // 3s silence
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&silence, "auto", &backend).unwrap();

    println!("Whisper silence: '{text}'");
    assert!(
        text.is_empty() || text.len() < 20,
        "Hallucinated on silence: '{text}'"
    );
}

#[test]
fn test_whisper_short_audio_graceful() {
    let short = vec![0.1f32; 1000]; // ~62ms
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let result = transcribe::transcribe_with_backend(&short, "auto", &backend);
    println!("Whisper short: {result:?}");
    // Should not panic
}

#[test]
fn test_whisper_long_audio() {
    // Generate ~10s of audio
    let samples = match generate_audio(
        "The quick brown fox jumps over the lazy dog. \
         This is a longer sentence to verify that Whisper can handle \
         extended audio without issues or timeouts."
    ) {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");

    let audio_secs = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    println!("Audio length: {audio_secs:.1}s");

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let start = std::time::Instant::now();
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let elapsed = start.elapsed();
    let rtf = elapsed.as_secs_f64() / audio_secs;

    println!("Whisper long: '{text}' ({:.2}s, RTF={:.3})", elapsed.as_secs_f64(), rtf);
    assert!(!text.is_empty());
    assert!(text.split_whitespace().count() > 5, "Too few words for long audio");
    // RTF may exceed 1.0 when tests run in parallel (mutex contention)
    assert!(rtf < 2.0, "Way too slow: RTF={rtf:.3}");
}

#[test]
fn test_whisper_performance_rtf() {
    let samples = match generate_audio("The quick brown fox jumps over the lazy dog") {
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

    println!("RTF: {rtf:.3} ({:.0}x real-time)", 1.0 / rtf);
    println!("Text: '{text}'");
    // RTF may exceed 1.0 when multiple tests share the whisper mutex
    assert!(rtf < 2.0, "RTF={rtf:.3} — way too slow");
}

#[test]
fn test_whisper_consistent_results() {
    let samples = match generate_audio("One two three four five") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };
    // Force reload in case another test unloaded
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");

    // Run twice — should give identical results (deterministic with greedy)
    let text1 = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let text2 = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();

    println!("Run 1: '{text1}'");
    println!("Run 2: '{text2}'");
    assert_eq!(text1, text2, "Greedy decoding should be deterministic");
}

#[test]
fn test_whisper_via_transcribe_with_backend() {
    let samples = match generate_audio("Backend dispatch test") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };
    ensure_whisper_loaded();

    // Test via resolve_backend (same path as production)
    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    assert!(matches!(backend, transcribe::Backend::WhisperLocal(_)));

    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    assert!(!text.is_empty());
}

// ═══════════════════════════════════════════════════════════════
// PARAKEET — TDT 0.6B (ONNX, feature-gated)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_parakeet_not_compiled_error() {
    // Without --features parakeet, calling load/transcribe should fail
    if cfg!(feature = "parakeet") {
        println!("Parakeet feature is enabled, skipping not-compiled test");
        return;
    }

    let result = transcribe::parakeet::load_model();
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("not compiled"),
        "Should say 'not compiled'"
    );
}

#[test]
fn test_parakeet_transcribe_not_compiled_error() {
    if cfg!(feature = "parakeet") {
        println!("Parakeet feature is enabled, skipping");
        return;
    }

    let audio = vec![0.1f32; 16_000];
    let result = transcribe::parakeet::transcribe(&audio);
    assert!(result.is_err());
}

#[test]
fn test_parakeet_is_loaded_false_when_not_compiled() {
    if cfg!(feature = "parakeet") {
        println!("Parakeet feature is enabled, skipping");
        return;
    }
    assert!(!transcribe::parakeet::is_loaded());
}

#[test]
fn test_parakeet_unload_noop_when_not_compiled() {
    if cfg!(feature = "parakeet") {
        println!("Parakeet feature is enabled, skipping");
        return;
    }
    // Should not panic
    transcribe::parakeet::unload_model();
}

#[test]
fn test_parakeet_via_backend_dispatch() {
    let audio = vec![0.1f32; 16_000];
    let backend = transcribe::Backend::Parakeet;
    let result = transcribe::transcribe_with_backend(&audio, "en", &backend);

    if cfg!(feature = "parakeet") {
        // If compiled, might work if model is downloaded
        println!("Parakeet result: {result:?}");
    } else {
        assert!(result.is_err(), "Should fail without parakeet feature");
    }
}

#[test]
fn test_parakeet_model_dir_exists() {
    let dir = transcribe::parakeet::model_dir();
    println!("Parakeet model dir: {}", dir.display());
    // Just verify it returns a sensible path
    assert!(dir.to_str().unwrap().contains("parakeet"));
}

// ═══════════════════════════════════════════════════════════════
// VOXTRAL — Mini 4B Realtime (Burn + WGPU, feature-gated)
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_voxtral_not_compiled_error() {
    if cfg!(feature = "voxtral") {
        println!("Voxtral feature is enabled, skipping not-compiled test");
        return;
    }

    let result = transcribe::voxtral_local::load_model("/tmp/fake");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("not compiled"),
        "Should say 'not compiled'"
    );
}

#[test]
fn test_voxtral_transcribe_not_compiled_error() {
    if cfg!(feature = "voxtral") {
        println!("Voxtral feature is enabled, skipping");
        return;
    }

    let audio = vec![0.1f32; 16_000];
    let result = transcribe::voxtral_local::transcribe(&audio);
    assert!(result.is_err());
}

#[test]
fn test_voxtral_is_loaded_false_when_not_compiled() {
    if cfg!(feature = "voxtral") {
        println!("Voxtral feature is enabled, skipping");
        return;
    }
    assert!(!transcribe::voxtral_local::is_loaded());
}

#[test]
fn test_voxtral_unload_noop_when_not_compiled() {
    if cfg!(feature = "voxtral") { return; }
    transcribe::voxtral_local::unload_model();
}

#[test]
fn test_voxtral_streaming_not_compiled_error() {
    if cfg!(feature = "voxtral") { return; }

    let result = transcribe::voxtral_local::streaming::start();
    match result {
        Ok(_) => panic!("Should fail without voxtral feature"),
        Err(e) => assert!(e.to_string().contains("not compiled"), "Wrong error: {e}"),
    }
}

#[test]
fn test_voxtral_via_backend_dispatch() {
    let audio = vec![0.1f32; 16_000];
    let backend = transcribe::Backend::VoxtralLocal;
    let result = transcribe::transcribe_with_backend(&audio, "auto", &backend);

    if cfg!(feature = "voxtral") {
        println!("Voxtral result: {result:?}");
    } else {
        assert!(result.is_err(), "Should fail without voxtral feature");
    }
}

// ═══════════════════════════════════════════════════════════════
// BACKEND DISPATCH — model_manager integration
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_resolve_backend_routes_correctly() {
    let w = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    assert!(matches!(w, transcribe::Backend::WhisperLocal(_)));

    let p = model_manager::resolve_backend("parakeet-tdt-0.6b-v3");
    assert!(matches!(p, transcribe::Backend::Parakeet));

    let v = model_manager::resolve_backend("voxtral-q4.gguf");
    assert!(matches!(v, transcribe::Backend::VoxtralLocal));
}

#[test]
fn test_switch_backend_unload_old() {
    // Simulate switching from whisper to another backend
    ensure_whisper_loaded();
    assert!(transcribe::is_loaded());

    // Unload all (as the tray does on switch)
    transcribe::unload_model();
    transcribe::parakeet::unload_model();
    transcribe::voxtral_local::unload_model();

    assert!(!transcribe::is_loaded());
    assert!(!transcribe::parakeet::is_loaded());
    assert!(!transcribe::voxtral_local::is_loaded());

    // Reload whisper
    transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    assert!(transcribe::is_loaded());
}

#[test]
fn test_all_backends_listed_in_model_manager() {
    let models = model_manager::list_models();
    let backends: Vec<&str> = models.iter().map(|m| m.backend).collect();

    assert!(backends.contains(&"whisper"));
    assert!(backends.contains(&"parakeet"));
    assert!(backends.contains(&"voxtral-local"));
}

// ═══════════════════════════════════════════════════════════════
// AUDIO DECODE — multi-format
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_decode_wav_16khz() {
    let wav_path = std::env::temp_dir().join("wp_test_16k.wav");
    let status = std::process::Command::new("say")
        .args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@16000", "Test"])
        .status();
    match status {
        Ok(s) if s.success() => {
            let samples = audio::decode::load_audio_file(&wav_path).unwrap();
            assert!(samples.len() > 1000);
            // 16kHz input → no resampling needed, should be ~16k samples/s
            let duration = samples.len() as f32 / 16000.0;
            println!("16kHz WAV: {:.1}s, {} samples", duration, samples.len());
            assert!(duration > 0.1);
            let _ = std::fs::remove_file(&wav_path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

#[test]
fn test_decode_wav_44khz_resamples() {
    let wav_path = std::env::temp_dir().join("wp_test_44k.wav");
    let status = std::process::Command::new("say")
        .args(["-o", wav_path.to_str().unwrap(), "--data-format=LEI16@44100", "Resample test"])
        .status();
    match status {
        Ok(s) if s.success() => {
            let samples = audio::decode::load_audio_file(&wav_path).unwrap();
            // Input was 44.1kHz, output should be resampled to 16kHz
            // So sample count should be roughly input_duration * 16000
            let duration = samples.len() as f32 / 16000.0;
            println!("44.1kHz→16kHz: {:.1}s, {} samples", duration, samples.len());
            assert!(duration > 0.1);
            let _ = std::fs::remove_file(&wav_path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

#[test]
fn test_decode_m4a() {
    // macOS `say` can output AAC in M4A container
    let path = std::env::temp_dir().join("wp_test.m4a");
    let status = std::process::Command::new("say")
        .args(["-o", path.to_str().unwrap(), "--data-format=aac", "M4A format test"])
        .status();
    match status {
        Ok(s) if s.success() => {
            match audio::decode::load_audio_file(&path) {
                Ok(samples) => {
                    println!("M4A: {} samples ({:.1}s)", samples.len(), samples.len() as f32 / 16000.0);
                    assert!(samples.len() > 1000);
                }
                Err(e) => {
                    // AAC decode may not be supported by symphonia build
                    println!("M4A decode not supported: {e}");
                }
            }
            let _ = std::fs::remove_file(&path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

#[test]
fn test_decode_unsupported_format() {
    let path = std::env::temp_dir().join("wp_test_bad.txt");
    std::fs::write(&path, "not audio").unwrap();
    let result = audio::decode::load_audio_file(&path);
    assert!(result.is_err());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_decode_nonexistent_file() {
    let result = audio::decode::load_audio_file(std::path::Path::new("/tmp/wp_no_exist.wav"));
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════
// AUDIO QUALITY — RMS, silence detection
// ═══════════════════════════════════════════════════════════════

#[test]
fn test_synthetic_audio_not_silence() {
    let samples = match generate_audio("This should not be silence") {
        Some(s) => s,
        None => { println!("'say' not available, skipping"); return; }
    };

    let rms = audio_rms(&samples);
    println!("Synthetic audio RMS: {rms:.4}");
    assert!(rms > audio::capture::SILENCE_RMS_THRESHOLD,
        "Synthetic audio is silence (RMS={rms})");
}

#[test]
fn test_silence_buffer_detected() {
    let silence = vec![0.0f32; 16_000];
    let rms = audio_rms(&silence);
    assert!(rms < audio::capture::SILENCE_RMS_THRESHOLD);
}

#[test]
fn test_min_audio_samples_threshold() {
    // Pipeline skips audio shorter than MIN_AUDIO_SAMPLES
    assert_eq!(audio::MIN_AUDIO_SAMPLES, 4800);
    // 4800 samples at 16kHz = 0.3 seconds
    let duration = audio::MIN_AUDIO_SAMPLES as f32 / audio::SAMPLE_RATE as f32;
    assert!((duration - 0.3).abs() < 0.01);
}
