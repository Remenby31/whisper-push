/// Test streaming audio capture — captures 3 seconds in 500ms chunks.
#[test]
fn test_streaming_capture() {
    let capture = whisper_push::audio::stream::StreamingCapture::start("auto", 500)
        .expect("Failed to start streaming capture");

    println!("Streaming for 3 seconds (500ms chunks)...");
    let start = std::time::Instant::now();
    let mut chunk_count = 0;

    while start.elapsed() < std::time::Duration::from_secs(3) {
        if let Ok(chunk) = capture.chunk_rx.recv_timeout(std::time::Duration::from_millis(100)) {
            chunk_count += 1;
            let rms: f32 = (chunk.samples.iter().map(|s| s * s).sum::<f32>() / chunk.samples.len() as f32).sqrt();
            println!(
                "  Chunk {}: {} samples ({:.1}ms), RMS={:.6}, offset={}",
                chunk_count,
                chunk.samples.len(),
                chunk.samples.len() as f32 / 16.0,
                rms,
                chunk.offset_samples,
            );
        }
    }

    let duration = capture.duration_secs();
    drop(capture);

    println!("Total: {} chunks, {:.1}s captured", chunk_count, duration);
    assert!(chunk_count >= 4, "Expected at least 4 chunks in 3 seconds, got {chunk_count}");
    assert!(duration > 2.0, "Expected >2s of audio, got {duration:.1}s");
    println!("✓ Streaming capture works");
}
