#[test]
fn test_voxtral_transcribe() {
    // Find a test MP3
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
    println!("Testing Voxtral with: {}", path.display());

    // Decode
    let samples = whisper_push::audio::decode::load_audio_file(&path).unwrap();
    println!("Audio: {:.1}s ({} samples)", samples.len() as f32 / 16000.0, samples.len());

    // Load Voxtral
    let dir = dirs::data_dir().unwrap().join("whisper-push/models/voxtral");
    println!("Model dir: {}", dir.display());
    println!("GGUF exists: {}", dir.join("voxtral-q4.gguf").exists());
    println!("Tokenizer exists: {}", dir.join("tekken.json").exists());

    println!("Loading Voxtral Q4...");
    match whisper_push::transcribe::voxtral_local::load_model(dir.to_str().unwrap()) {
        Ok(()) => println!("Model loaded!"),
        Err(e) => {
            println!("LOAD ERROR: {e}");
            return;
        }
    }

    // Transcribe
    println!("Transcribing...");
    let start = std::time::Instant::now();
    match whisper_push::transcribe::voxtral_local::transcribe(&samples) {
        Ok(text) => {
            println!("Result ({:.2}s): {text}", start.elapsed().as_secs_f64());
            assert!(!text.is_empty(), "Empty result");
        }
        Err(e) => {
            println!("TRANSCRIBE ERROR: {e}");
            panic!("Voxtral transcription failed: {e}");
        }
    }
}
