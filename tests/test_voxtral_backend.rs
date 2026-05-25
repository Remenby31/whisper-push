/// Test Voxtral via transcribe_with_backend (lazy loading in current thread).
/// This simulates exactly what the pipeline_loop and test button do.
#[test]
fn test_voxtral_via_backend_dispatch() {
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
    println!("Testing via transcribe_with_backend (VoxtralLocal)");
    println!("File: {}", path.display());

    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    println!("Audio: {:.1}s", samples.len() as f32 / 16000.0);

    let backend = whisper_push::transcribe::Backend::VoxtralLocal;

    let start = std::time::Instant::now();
    match whisper_push::transcribe::transcribe_with_backend(&samples, "auto", &backend) {
        Ok(text) => {
            println!("OK ({:.2}s): {text}", start.elapsed().as_secs_f64());
            assert!(!text.is_empty());
        }
        Err(e) => {
            println!("ERROR: {e}");
            panic!("transcribe_with_backend failed: {e}");
        }
    }

    println!("✓ Voxtral via backend dispatch works");
}
