//! Parakeet TDT — fastest local ASR engine via ONNX Runtime.
//! Supports WebGPU (Metal on macOS), CUDA, DirectML, and CPU.

#[cfg(feature = "parakeet")]
mod inner {
    use crate::util::LockSafe;
    use anyhow::Result;
    use parakeet_rs::{ParakeetTDT, Transcriber};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tracing::info;

    static PARAKEET: Mutex<Option<ParakeetTDT>> = Mutex::new(None);

    pub fn model_dir(model_name: &str) -> PathBuf {
        crate::config::parakeet_dir(model_name)
    }

    /// Load a Parakeet TDT model: `parakeet-ultra-int8` (default), or v3 as
    /// `…-int8` (~670 MB, self-contained graphs) / fp32 (a 42 MB graph + a
    /// 2.3 GB `.onnx.data` sidecar). int8 is ~3.8x smaller — far less for the OS
    /// to compress/decompress under memory pressure — and faster on CPU.
    ///
    /// The v3 variants share `models/parakeet/` under the same filenames
    /// (parakeet-rs wants fixed names); `model_manager` owns that swap and its
    /// `.variant` marker, so "what is missing" is asked there, once.
    pub fn load_model(model_name: &str) -> Result<()> {
        let dir = model_dir(model_name);
        if !crate::model_manager::missing_files(model_name).is_empty() {
            info!("{model_name}: downloading...");
            // ONE downloader for every model and every caller (wizard, tray,
            // here). No progress sink on this lazy path.
            crate::model_manager::download(model_name, &mut |_| {})?;
        }

        info!("Loading {model_name} from {}...", dir.display());
        let parakeet = ParakeetTDT::from_pretrained(&dir, None)
            .map_err(|e| anyhow::anyhow!("Failed to load Parakeet: {e}"))?;

        *PARAKEET.lock_safe() = Some(parakeet);
        info!("Parakeet model loaded and ready");
        Ok(())
    }

    pub fn unload_model() {
        *PARAKEET.lock_safe() = None;
        info!("Parakeet model unloaded");
    }

    #[allow(dead_code)]
    pub fn is_loaded() -> bool {
        PARAKEET.lock_safe().is_some()
    }

    /// Keep the model's pages resident by running a tiny inference on silence.
    /// Non-blocking — if a real transcription holds the lock, skip this tick.
    pub fn warm() {
        let Some(mut guard) = PARAKEET.try_lock_safe() else {
            return;
        };
        let Some(parakeet) = guard.as_mut() else {
            return; // Parakeet isn't the loaded backend
        };
        let silence = vec![0.0f32; crate::transcribe::WARM_SAMPLES];
        let t = std::time::Instant::now();
        match parakeet.transcribe_samples(silence, 16000, 1, None) {
            Ok(_) => tracing::debug!("parakeet kept warm ({:.2}s)", t.elapsed().as_secs_f64()),
            Err(e) => tracing::debug!("parakeet warm failed: {e}"),
        }
    }

    /// Transcribe 16kHz mono f32 audio to text. (Kept for tests; the daemon
    /// uses `transcribe_timed`.)
    #[allow(dead_code)]
    pub fn transcribe(audio: &[f32]) -> Result<String> {
        let mut guard = PARAKEET.lock_safe();
        let parakeet = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Parakeet model not loaded"))?;

        let start = std::time::Instant::now();
        let result = parakeet
            .transcribe_samples(audio.to_vec(), 16000, 1, None)
            .map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {e}"))?;

        let text = result.text.trim().to_string();
        let elapsed = start.elapsed();
        info!("Parakeet: '{}' ({:.2}s)", text, elapsed.as_secs_f64());
        Ok(text)
    }

    /// Transcribe and also return per-word timings (for the acoustic dictionary).
    pub fn transcribe_timed(audio: &[f32]) -> Result<(String, Vec<crate::acoustic::WordTiming>)> {
        let mut guard = PARAKEET.lock_safe();
        let parakeet = guard
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Parakeet model not loaded"))?;

        let start = std::time::Instant::now();
        let result = parakeet
            .transcribe_samples(audio.to_vec(), 16000, 1, None)
            .map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {e}"))?;
        let text = result.text.trim().to_string();

        // Merge SentencePiece tokens (▁ = word start) into words with spans.
        let mut words: Vec<crate::acoustic::WordTiming> = Vec::new();
        let mut cur: Option<crate::acoustic::WordTiming> = None;
        for t in &result.tokens {
            let starts = t.text.starts_with(' ') || t.text.starts_with('\u{2581}');
            let clean = t.text.trim_start_matches('\u{2581}').trim_start();
            if clean.is_empty() {
                if let Some(c) = cur.as_mut() {
                    c.end = t.end;
                }
                continue;
            }
            if starts || cur.is_none() {
                if let Some(c) = cur.take() {
                    words.push(c);
                }
                cur = Some(crate::acoustic::WordTiming {
                    text: clean.to_string(),
                    start: t.start,
                    end: t.end,
                });
            } else if let Some(c) = cur.as_mut() {
                c.text.push_str(clean);
                c.end = t.end;
            }
        }
        if let Some(c) = cur {
            words.push(c);
        }

        info!(
            "Parakeet: '{}' ({:.2}s, {} words timed)",
            text,
            start.elapsed().as_secs_f64(),
            words.len()
        );
        Ok((text, words))
    }
}

#[cfg(feature = "parakeet")]
#[allow(unused_imports)] // Used by integration tests
pub use inner::{
    is_loaded, load_model, model_dir, transcribe, transcribe_timed, unload_model, warm,
};

#[cfg(not(feature = "parakeet"))]
pub fn transcribe_timed(
    _audio: &[f32],
) -> anyhow::Result<(String, Vec<crate::acoustic::WordTiming>)> {
    anyhow::bail!("Parakeet not compiled. Build with --features parakeet")
}

#[cfg(not(feature = "parakeet"))]
pub fn load_model(_model_name: &str) -> anyhow::Result<()> {
    anyhow::bail!("Parakeet not compiled. Build with --features parakeet")
}
#[cfg(not(feature = "parakeet"))]
pub fn unload_model() {}
#[cfg(not(feature = "parakeet"))]
pub fn warm() {}
#[cfg(not(feature = "parakeet"))]
pub fn is_loaded() -> bool {
    false
}
#[cfg(not(feature = "parakeet"))]
pub fn transcribe(_audio: &[f32]) -> anyhow::Result<String> {
    anyhow::bail!("Parakeet not compiled. Build with --features parakeet")
}
#[cfg(not(feature = "parakeet"))]
pub fn model_dir(model_name: &str) -> std::path::PathBuf {
    crate::config::parakeet_dir(model_name)
}
