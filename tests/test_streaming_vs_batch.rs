/// Compare streaming vs batch Voxtral transcription on the same audio.
/// The final text should be identical or very close.
#[test]
fn test_streaming_vs_batch_same_result() {
    let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();

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
    println!("=== Streaming vs Batch Test ===");
    println!("File: {}", path.display());

    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    println!("Audio: {:.1}s", samples.len() as f32 / 16000.0);

    // Ensure model is loaded
    let dir = dirs::data_dir().unwrap().join("whisper-push/models/voxtral");
    if !dir.join("voxtral-q4.gguf").exists() {
        println!("Voxtral not downloaded, skipping");
        return;
    }
    whisper_push::transcribe::voxtral_local::load_model(dir.to_str().unwrap()).unwrap();

    // Batch transcription
    println!("\n--- Batch ---");
    let batch_start = std::time::Instant::now();
    let batch_text = whisper_push::transcribe::voxtral_local::transcribe(&samples).unwrap();
    let batch_time = batch_start.elapsed();
    println!("Batch ({:.2}s): {}", batch_time.as_secs_f64(), &batch_text);

    // Streaming transcription
    println!("\n--- Streaming ---");
    let stream_start = std::time::Instant::now();
    let mut session = whisper_push::transcribe::voxtral_local::streaming::start().unwrap();
    let chunk_size = 8000; // 500ms
    let mut all_words: Vec<String> = Vec::new();
    for chunk in samples.chunks(chunk_size) {
        match whisper_push::transcribe::voxtral_local::streaming::feed_chunk(&mut session, chunk) {
            Ok(words) if !words.is_empty() => {
                all_words.extend(words);
            }
            Ok(_) => {}
            Err(e) => { println!("Error: {e}"); break; }
        }
    }
    let stream_final = whisper_push::transcribe::voxtral_local::streaming::finish(session).unwrap();
    let stream_time = stream_start.elapsed();
    let stream_text = if stream_final.is_empty() { all_words.join(" ") } else { stream_final };
    println!("Stream ({:.2}s): {}", stream_time.as_secs_f64(), &stream_text);

    // Compare
    println!("\n--- Comparison ---");
    println!("Batch:  {} chars", batch_text.len());
    println!("Stream: {} chars", stream_text.len());

    // They should be very similar (may differ slightly due to re-encoding)
    let batch_words: Vec<&str> = batch_text.split_whitespace().collect();
    let stream_words: Vec<&str> = stream_text.split_whitespace().collect();
    let common = batch_words.iter().zip(stream_words.iter())
        .take_while(|(a, b)| a == b).count();
    let similarity = common as f32 / batch_words.len().max(1) as f32;

    println!("Common prefix: {}/{} words ({:.0}% match)", common, batch_words.len(), similarity * 100.0);

    assert!(similarity > 0.5, "Streaming and batch differ too much: {:.0}%", similarity * 100.0);
    println!("✓ Streaming vs batch: {:.0}% word match", similarity * 100.0);
}
