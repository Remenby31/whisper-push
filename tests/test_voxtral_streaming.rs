/// Test Voxtral streaming: decode MP3 → split into 500ms chunks → feed one by one.
#[test]
fn test_voxtral_streaming_from_file() {
    let test_files: Vec<_> = std::fs::read_dir(dirs::home_dir().unwrap().join("Downloads"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "mp3").unwrap_or(false))
        .collect();

    if test_files.is_empty() {
        println!("No MP3 files, skipping");
        return;
    }

    let path = test_files[0].path();
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
    println!("=== Voxtral Streaming Test ===");
    println!("File: {}", path.display());

    // Decode audio
    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    println!("Audio: {:.1}s ({} samples)", samples.len() as f32 / 16000.0, samples.len());

    // Ensure Voxtral is loaded
    let dir = dirs::data_dir().unwrap().join("whisper-push/models/voxtral");
    if !dir.join("voxtral-q4.gguf").exists() {
        println!("Voxtral model not downloaded, skipping");
        return;
    }

    println!("Loading Voxtral...");
    whisper_push::transcribe::voxtral_local::load_model(dir.to_str().unwrap()).unwrap();

    // Start streaming session
    println!("Starting streaming session...");
    let mut session = whisper_push::transcribe::voxtral_local::streaming::start().unwrap();

    // Split into 500ms chunks (8000 samples at 16kHz)
    let chunk_size = 8000;
    let chunks: Vec<&[f32]> = samples.chunks(chunk_size).collect();
    println!("Feeding {} chunks of {}ms...", chunks.len(), chunk_size * 1000 / 16000);

    let start = std::time::Instant::now();
    let mut all_words: Vec<String> = Vec::new();

    for (i, chunk) in chunks.iter().enumerate() {
        match whisper_push::transcribe::voxtral_local::streaming::feed_chunk(&mut session, chunk) {
            Ok(words) => {
                if !words.is_empty() {
                    println!("  Chunk {}/{}: +{} words: {:?}", i + 1, chunks.len(), words.len(), words);
                    all_words.extend(words);
                } else {
                    println!("  Chunk {}/{}: (no new words)", i + 1, chunks.len());
                }
            }
            Err(e) => {
                println!("  Chunk {}/{}: ERROR: {e}", i + 1, chunks.len());
                break;
            }
        }
    }

    // Finish
    let final_text = whisper_push::transcribe::voxtral_local::streaming::finish(session).unwrap();
    let elapsed = start.elapsed();

    println!("\n=== Results ===");
    println!("Streaming words: {:?}", all_words);
    println!("Final text: '{final_text}'");
    println!("Time: {:.2}s", elapsed.as_secs_f64());
    println!("Audio: {:.1}s", samples.len() as f32 / 16000.0);

    if all_words.is_empty() && final_text.is_empty() {
        println!("WARNING: No text produced!");
    } else {
        println!("✓ Streaming test passed");
    }
}
