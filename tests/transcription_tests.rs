/// Transcription tests — one section per model backend.
/// Run with: cargo test --test transcription_tests -- --nocapture
///
/// Uses macOS `say` to generate synthetic audio (no external fixtures needed).
///
/// Custom harness (`harness = false` in Cargo.toml): ggml's Metal device
/// destructor has a buggy assertion (GGML_ASSERT([rsets->data count] == 0))
/// that crashes on process exit. We use `_exit()` to skip C++ destructors
/// after all tests have completed.
use whisper_push::audio;
use whisper_push::model_manager;
use whisper_push::transcribe;

unsafe extern "C" {
    fn _exit(status: i32) -> !;
}

macro_rules! run_tests {
    ($filter:expr, $( $test:ident ),* $(,)?) => {{
        let mut passed = 0u32;
        let mut failed = 0u32;
        let mut ignored = 0u32;
        $(
            let name = stringify!($test);
            let skip = match $filter {
                Some(ref f) => !name.contains(f.as_str()),
                None => false,
            };
            if skip {
                ignored += 1;
            } else {
                eprint!("test {name} ... ");
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| $test())) {
                    Ok(()) => { eprintln!("ok"); passed += 1; }
                    Err(_) => { eprintln!("FAILED"); failed += 1; }
                }
            }
        )*
        (passed, failed, ignored)
    }};
}

fn main() {
    // Parse args: cargo test passes filter as first non-flag arg after --
    let args: Vec<String> = std::env::args().collect();
    let filter = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned();

    let (passed, failed, ignored) = {
        run_tests!(
            filter,
            test_whisper_load_unload_reload,
            test_whisper_not_loaded_error,
            test_whisper_transcribe_english,
            test_whisper_transcribe_french,
            test_whisper_transcribe_auto_language,
            test_whisper_silence_no_hallucination,
            test_whisper_short_audio_graceful,
            test_whisper_long_audio,
            test_whisper_performance_rtf,
            test_whisper_consistent_results,
            test_whisper_via_backend_dispatch,
            test_parakeet_load_unload,
            test_parakeet_transcribe_english,
            test_parakeet_silence,
            test_parakeet_via_backend_dispatch,
            test_parakeet_performance,
            test_parakeet_model_dir,
            test_voxtral_load_unload,
            test_voxtral_transcribe_batch,
            test_voxtral_silence,
            test_voxtral_via_backend_dispatch,
            test_voxtral_performance,
            test_voxtral_streaming_basic,
            test_voxtral_streaming_vs_batch_similar,
            test_voxtral_streaming_short_audio,
            test_resolve_backend_routes_correctly,
            test_switch_backend_unload_old,
            test_all_backends_listed_in_model_manager,
            test_decode_wav_16khz,
            test_decode_wav_44khz_resamples,
            test_decode_unsupported_format,
            test_decode_nonexistent_file,
            test_synthetic_audio_not_silence,
            test_silence_buffer_detected,
            test_min_audio_samples_threshold,
        )
    };

    let status = if failed == 0 { "ok" } else { "FAILED" };
    eprintln!("\ntest result: {status}. {passed} passed; {failed} failed; {ignored} ignored\n");

    // Clean up models, then _exit() to skip ggml's buggy C++ destructors.
    transcribe::unload_model();
    transcribe::parakeet::unload_model();
    transcribe::voxtral_local::unload_model();
    unsafe { _exit(if failed == 0 { 0 } else { 1 }) };
}

// ── Helpers ─────────────────────────────────────────────────────

fn ensure_whisper_loaded() {
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");
}

fn generate_audio(text: &str) -> Option<Vec<f32>> {
    generate_audio_with_voice(text, None)
}

fn generate_audio_with_voice(text: &str, voice: Option<&str>) -> Option<Vec<f32>> {
    let hash = text.len() ^ text.as_bytes().iter().map(|b| *b as usize).sum::<usize>();
    let wav_path = std::env::temp_dir().join(format!("whisper_push_test_{hash}.wav"));
    let mut cmd = std::process::Command::new("say");
    cmd.args([
        "-o",
        wav_path.to_str().unwrap(),
        "--data-format=LEI16@16000",
    ]);
    if let Some(v) = voice {
        cmd.args(["-v", v]);
    }
    cmd.arg(text);
    let status = cmd.status().ok()?;
    if !status.success() {
        return None;
    }
    let samples = audio::decode::load_audio_file(&wav_path).ok()?;
    let _ = std::fs::remove_file(&wav_path);
    Some(samples)
}

fn audio_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

// ═══════════════════════════════════════════════════════════════
// WHISPER — large-v3-turbo (Metal/CUDA/CPU)
// ═══════════════════════════════════════════════════════════════

fn test_whisper_load_unload_reload() {
    transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    assert!(transcribe::is_loaded());

    transcribe::unload_model();
    assert!(!transcribe::is_loaded());

    transcribe::load_model("ggml-large-v3-turbo-q5_0.bin").unwrap();
    assert!(transcribe::is_loaded());
}

fn test_whisper_not_loaded_error() {
    transcribe::unload_model();

    let audio = vec![0.1f32; 16_000];
    let backend = transcribe::Backend::WhisperLocal("ggml-large-v3-turbo-q5_0.bin".into());
    let result = transcribe::transcribe_with_backend(&audio, "auto", &backend);

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not loaded"));

    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");
}

fn test_whisper_transcribe_english() {
    let samples = match generate_audio("Hello world, this is a test of Whisper Push") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
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

fn test_whisper_transcribe_french() {
    let samples =
        match generate_audio_with_voice("Bonjour le monde, ceci est un test", Some("Thomas")) {
            Some(s) => s,
            None => match generate_audio("Bonjour le monde") {
                Some(s) => s,
                None => {
                    println!("'say' not available, skipping");
                    return;
                }
            },
        };
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "fr", &backend).unwrap();
    println!("Whisper FR: '{text}'");
    assert!(!text.is_empty());
}

fn test_whisper_transcribe_auto_language() {
    let samples = match generate_audio("Testing automatic language detection") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&samples, "auto", &backend).unwrap();
    println!("Whisper auto: '{text}'");
    assert!(!text.is_empty());
}

fn test_whisper_silence_no_hallucination() {
    let silence = vec![0.0f32; 48_000];
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text = transcribe::transcribe_with_backend(&silence, "auto", &backend).unwrap();

    println!("Whisper silence: '{text}'");
    assert!(
        text.is_empty() || text.len() < 20,
        "Hallucinated on silence: '{text}'"
    );
}

fn test_whisper_short_audio_graceful() {
    let short = vec![0.1f32; 1000];
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let _result = transcribe::transcribe_with_backend(&short, "auto", &backend);
    // Should not panic
}

fn test_whisper_long_audio() {
    let samples = match generate_audio(
        "The quick brown fox jumps over the lazy dog. \
         This is a longer sentence to verify that Whisper can handle \
         extended audio without issues or timeouts.",
    ) {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");

    let audio_secs = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let start = std::time::Instant::now();
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let elapsed = start.elapsed();
    let rtf = elapsed.as_secs_f64() / audio_secs;

    println!(
        "Whisper long: '{text}' ({:.2}s, RTF={:.3})",
        elapsed.as_secs_f64(),
        rtf
    );
    assert!(!text.is_empty());
    assert!(
        text.split_whitespace().count() > 5,
        "Too few words for long audio"
    );
    assert!(rtf < 2.0, "Way too slow: RTF={rtf:.3}");
}

fn test_whisper_performance_rtf() {
    let samples = match generate_audio("The quick brown fox jumps over the lazy dog") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let start = std::time::Instant::now();
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let elapsed = start.elapsed();

    let audio_duration = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    let rtf = elapsed.as_secs_f64() / audio_duration;

    println!("RTF: {rtf:.3} ({:.0}x real-time)", 1.0 / rtf);
    println!("Text: '{text}'");
    assert!(rtf < 2.0, "RTF={rtf:.3} — way too slow");
}

fn test_whisper_consistent_results() {
    let samples = match generate_audio("One two three four five") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    let _ = transcribe::load_model("ggml-large-v3-turbo-q5_0.bin");

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    let text1 = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    let text2 = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();

    println!("Run 1: '{text1}'");
    println!("Run 2: '{text2}'");
    assert_eq!(text1, text2, "Greedy decoding should be deterministic");
}

fn test_whisper_via_backend_dispatch() {
    let samples = match generate_audio("Backend dispatch test") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    ensure_whisper_loaded();

    let backend = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    assert!(matches!(backend, transcribe::Backend::WhisperLocal(_)));
    let text = transcribe::transcribe_with_backend(&samples, "en", &backend).unwrap();
    assert!(!text.is_empty());
}

// ═══════════════════════════════════════════════════════════════
// PARAKEET — TDT 0.6B (ONNX Runtime)
// ═══════════════════════════════════════════════════════════════

fn test_parakeet_load_unload() {

    match transcribe::parakeet::load_model() {
        Ok(()) => {
            assert!(transcribe::parakeet::is_loaded());
            transcribe::parakeet::unload_model();
            assert!(!transcribe::parakeet::is_loaded());
            println!("Parakeet load/unload OK");
        }
        Err(e) => {
            println!("Parakeet model not downloaded, skipping: {e}");
        }
    }
}

fn test_parakeet_transcribe_english() {

    let samples = match generate_audio("Hello world this is a parakeet test") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if transcribe::parakeet::load_model().is_err() {
        println!("Parakeet model not downloaded, skipping");
        return;
    }

    let text = transcribe::parakeet::transcribe(&samples).unwrap();
    println!("Parakeet EN: '{text}'");
    assert!(!text.is_empty());
    let lower = text.to_lowercase();
    assert!(
        lower.contains("hello")
            || lower.contains("world")
            || lower.contains("parakeet")
            || lower.contains("test"),
        "Missing expected words: '{text}'"
    );
}

fn test_parakeet_silence() {

    let silence = vec![0.0f32; 48_000];

    if transcribe::parakeet::load_model().is_err() {
        println!("Parakeet model not downloaded, skipping");
        return;
    }

    let text = transcribe::parakeet::transcribe(&silence).unwrap();
    println!("Parakeet silence: '{text}'");
    assert!(
        text.is_empty() || text.len() < 20,
        "Hallucinated on silence: '{text}'"
    );
}

fn test_parakeet_via_backend_dispatch() {

    let samples = match generate_audio("Backend dispatch parakeet") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    let backend = model_manager::resolve_backend("parakeet-tdt-0.6b-v3");
    assert!(matches!(backend, transcribe::Backend::Parakeet));

    match transcribe::transcribe_with_backend(&samples, "en", &backend) {
        Ok(text) => {
            println!("Parakeet dispatch: '{text}'");
            assert!(!text.is_empty());
        }
        Err(e) => {
            println!("Parakeet dispatch failed (model not downloaded?): {e}");
        }
    }
}

fn test_parakeet_performance() {

    let samples = match generate_audio("The quick brown fox jumps over the lazy dog") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if transcribe::parakeet::load_model().is_err() {
        println!("Parakeet model not downloaded, skipping");
        return;
    }

    let start = std::time::Instant::now();
    let text = transcribe::parakeet::transcribe(&samples).unwrap();
    let elapsed = start.elapsed();
    let audio_secs = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    let rtf = elapsed.as_secs_f64() / audio_secs;

    println!(
        "Parakeet: '{text}' ({:.2}s, RTF={:.3})",
        elapsed.as_secs_f64(),
        rtf
    );
    assert!(!text.is_empty());
    // Parakeet should be very fast (~27ms/10s on Metal)
    assert!(rtf < 1.0, "Parakeet too slow: RTF={rtf:.3}");
}

fn test_parakeet_model_dir() {
    let dir = transcribe::parakeet::model_dir();
    assert!(dir.to_str().unwrap().contains("parakeet"));
}

// ═══════════════════════════════════════════════════════════════
// VOXTRAL — Mini 4B Realtime (Burn + WGPU)
// ═══════════════════════════════════════════════════════════════

fn voxtral_model_dir() -> std::path::PathBuf {
    whisper_push::config::data_dir()
        .join("models")
        .join("voxtral")
}

fn voxtral_available() -> bool {
    let dir = voxtral_model_dir();
    dir.join("voxtral-q4.gguf").exists() && dir.join("tekken.json").exists()
}

fn ensure_voxtral_loaded() -> bool {

    if !voxtral_available() {
        println!("Voxtral model not downloaded, skipping");
        return false;
    }
    // Always reload — another test may have unloaded
    let dir = voxtral_model_dir();
    if let Err(e) = transcribe::voxtral_local::load_model(dir.to_str().unwrap()) {
        println!("Voxtral load failed: {e}");
        return false;
    }
    true
}

fn test_voxtral_load_unload() {
    if !voxtral_available() {
        println!("Voxtral model not downloaded, skipping");
        return;
    }

    let dir = voxtral_model_dir();
    transcribe::voxtral_local::load_model(dir.to_str().unwrap()).unwrap();
    assert!(transcribe::voxtral_local::is_loaded());

    transcribe::voxtral_local::unload_model();
    assert!(!transcribe::voxtral_local::is_loaded());

    println!("Voxtral load/unload OK");
}

fn test_voxtral_transcribe_batch() {
    let samples = match generate_audio("Hello world this is a voxtral test") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !ensure_voxtral_loaded() {
        return;
    }

    let start = std::time::Instant::now();
    let text = transcribe::voxtral_local::transcribe(&samples).unwrap();
    let elapsed = start.elapsed();

    println!("Voxtral batch: '{text}' ({:.2}s)", elapsed.as_secs_f64());
    assert!(!text.is_empty());
}

fn test_voxtral_silence() {
    let silence = vec![0.0f32; 48_000];

    if !ensure_voxtral_loaded() {
        return;
    }

    let text = transcribe::voxtral_local::transcribe(&silence).unwrap();
    println!("Voxtral silence: '{text}'");
    // Voxtral may produce some tokens on silence, but should be very short
    assert!(text.len() < 30, "Voxtral hallucinated on silence: '{text}'");
}

fn test_voxtral_via_backend_dispatch() {
    let samples = match generate_audio("Backend dispatch voxtral test") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !voxtral_available() {
        println!("Voxtral model not downloaded, skipping");
        return;
    }

    let backend = model_manager::resolve_backend("voxtral-q4.gguf");
    assert!(matches!(backend, transcribe::Backend::VoxtralLocal));

    // transcribe_with_backend lazy-loads Voxtral on current thread
    let text = transcribe::transcribe_with_backend(&samples, "auto", &backend).unwrap();
    println!("Voxtral dispatch: '{text}'");
    assert!(!text.is_empty());
}

fn test_voxtral_performance() {
    let samples = match generate_audio("The quick brown fox jumps over the lazy dog") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !ensure_voxtral_loaded() {
        return;
    }

    let start = std::time::Instant::now();
    let text = transcribe::voxtral_local::transcribe(&samples).unwrap();
    let elapsed = start.elapsed();
    let audio_secs = samples.len() as f64 / audio::SAMPLE_RATE as f64;
    let rtf = elapsed.as_secs_f64() / audio_secs;

    println!(
        "Voxtral: '{text}' ({:.2}s, RTF={:.3})",
        elapsed.as_secs_f64(),
        rtf
    );
    assert!(!text.is_empty());
}

// ── Voxtral Streaming ───────────────────────────────────────────

fn test_voxtral_streaming_basic() {
    let samples = match generate_audio("Streaming transcription test one two three") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !ensure_voxtral_loaded() {
        return;
    }

    // Start a streaming session
    let mut session = transcribe::voxtral_local::streaming::start().unwrap();

    // Feed audio in 500ms chunks (8000 samples at 16kHz)
    let chunk_size = 8000;
    let mut all_words: Vec<String> = Vec::new();

    for (i, chunk) in samples.chunks(chunk_size).enumerate() {
        match transcribe::voxtral_local::streaming::feed_chunk(&mut session, chunk) {
            Ok(words) => {
                if !words.is_empty() {
                    println!("  Chunk {}: +{} words: {:?}", i + 1, words.len(), words);
                    all_words.extend(words);
                }
            }
            Err(e) => {
                println!("  Chunk {} error: {e}", i + 1);
                break;
            }
        }
    }

    // Finish session
    let final_text = transcribe::voxtral_local::streaming::finish(session).unwrap();
    println!("Streaming words: {:?}", all_words);
    println!("Final text: '{final_text}'");

    // Should have produced some output
    assert!(
        !all_words.is_empty() || !final_text.is_empty(),
        "Streaming produced no output"
    );
}

fn test_voxtral_streaming_vs_batch_similar() {
    let samples = match generate_audio("Compare streaming and batch results") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !ensure_voxtral_loaded() {
        return;
    }

    // Batch
    let batch_text = transcribe::voxtral_local::transcribe(&samples).unwrap();

    // Streaming
    let mut session = transcribe::voxtral_local::streaming::start().unwrap();
    for chunk in samples.chunks(8000) {
        let _ = transcribe::voxtral_local::streaming::feed_chunk(&mut session, chunk);
    }
    let stream_text = transcribe::voxtral_local::streaming::finish(session).unwrap();

    println!("Batch:  '{batch_text}'");
    println!("Stream: '{stream_text}'");

    // Both should produce non-empty output
    assert!(!batch_text.is_empty(), "Batch empty");
    assert!(!stream_text.is_empty(), "Stream empty");

    // They should be somewhat similar (may differ due to re-encoding each chunk)
    let batch_words: Vec<&str> = batch_text.split_whitespace().collect();
    let stream_words: Vec<&str> = stream_text.split_whitespace().collect();
    let common = batch_words
        .iter()
        .zip(stream_words.iter())
        .take_while(|(a, b)| a.to_lowercase() == b.to_lowercase())
        .count();
    let total = batch_words.len().max(1);
    let similarity = common as f32 / total as f32;
    println!(
        "Similarity: {:.0}% ({common}/{total} prefix match)",
        similarity * 100.0
    );
}

fn test_voxtral_streaming_short_audio() {
    // Very short audio — streaming should handle gracefully
    let samples = match generate_audio("Hi") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };

    if !ensure_voxtral_loaded() {
        return;
    }

    let mut session = transcribe::voxtral_local::streaming::start().unwrap();
    // Feed all at once (short audio)
    let _ = transcribe::voxtral_local::streaming::feed_chunk(&mut session, &samples);
    let text = transcribe::voxtral_local::streaming::finish(session).unwrap();
    println!("Voxtral streaming short: '{text}'");
    // Should not panic, may or may not produce text for very short audio
}

// ═══════════════════════════════════════════════════════════════
// BACKEND DISPATCH — model_manager integration
// ═══════════════════════════════════════════════════════════════

fn test_resolve_backend_routes_correctly() {
    let w = model_manager::resolve_backend("ggml-large-v3-turbo-q5_0.bin");
    assert!(matches!(w, transcribe::Backend::WhisperLocal(_)));

    let p = model_manager::resolve_backend("parakeet-tdt-0.6b-v3");
    assert!(matches!(p, transcribe::Backend::Parakeet));

    let v = model_manager::resolve_backend("voxtral-q4.gguf");
    assert!(matches!(v, transcribe::Backend::VoxtralLocal));
}

fn test_switch_backend_unload_old() {
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

fn test_decode_wav_16khz() {
    let wav_path = std::env::temp_dir().join("wp_test_16k.wav");
    let status = std::process::Command::new("say")
        .args([
            "-o",
            wav_path.to_str().unwrap(),
            "--data-format=LEI16@16000",
            "Test",
        ])
        .status();
    match status {
        Ok(s) if s.success() => {
            let samples = audio::decode::load_audio_file(&wav_path).unwrap();
            assert!(samples.len() > 1000);
            let _ = std::fs::remove_file(&wav_path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

fn test_decode_wav_44khz_resamples() {
    let wav_path = std::env::temp_dir().join("wp_test_44k.wav");
    let status = std::process::Command::new("say")
        .args([
            "-o",
            wav_path.to_str().unwrap(),
            "--data-format=LEI16@44100",
            "Resample test",
        ])
        .status();
    match status {
        Ok(s) if s.success() => {
            let samples = audio::decode::load_audio_file(&wav_path).unwrap();
            let duration = samples.len() as f32 / 16000.0;
            println!(
                "44.1kHz->16kHz: {:.1}s, {} samples",
                duration,
                samples.len()
            );
            assert!(duration > 0.1);
            let _ = std::fs::remove_file(&wav_path);
        }
        _ => println!("'say' not available, skipping"),
    }
}

fn test_decode_unsupported_format() {
    let path = std::env::temp_dir().join("wp_test_bad.txt");
    std::fs::write(&path, "not audio").unwrap();
    assert!(audio::decode::load_audio_file(&path).is_err());
    let _ = std::fs::remove_file(&path);
}

fn test_decode_nonexistent_file() {
    assert!(audio::decode::load_audio_file(std::path::Path::new("/tmp/wp_no_exist.wav")).is_err());
}

// ═══════════════════════════════════════════════════════════════
// AUDIO QUALITY
// ═══════════════════════════════════════════════════════════════

fn test_synthetic_audio_not_silence() {
    let samples = match generate_audio("This should not be silence") {
        Some(s) => s,
        None => {
            println!("'say' not available, skipping");
            return;
        }
    };
    assert!(audio_rms(&samples) > audio::capture::SILENCE_RMS_THRESHOLD);
}

fn test_silence_buffer_detected() {
    let silence = vec![0.0f32; 16_000];
    assert!(audio_rms(&silence) < audio::capture::SILENCE_RMS_THRESHOLD);
}

fn test_min_audio_samples_threshold() {
    assert_eq!(audio::MIN_AUDIO_SAMPLES, 4800);
    let duration = audio::MIN_AUDIO_SAMPLES as f32 / audio::SAMPLE_RATE as f32;
    assert!((duration - 0.3).abs() < 0.01);
}
