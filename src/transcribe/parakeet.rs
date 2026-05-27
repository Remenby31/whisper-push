//! Parakeet TDT — fastest local ASR engine via ONNX Runtime.
//! Supports WebGPU (Metal on macOS), CUDA, DirectML, and CPU.

#[cfg(feature = "parakeet")]
mod inner {
    use anyhow::{Context, Result};
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tracing::info;
    use parakeet_rs::{Parakeet, Transcriber};

    static PARAKEET: Mutex<Option<Parakeet>> = Mutex::new(None);

    pub fn model_dir() -> PathBuf {
        crate::config::data_dir().join("models").join("parakeet")
    }

    /// Load Parakeet TDT model. Downloads from HuggingFace if not present.
    pub fn load_model() -> Result<()> {
        let dir = model_dir();

        if !dir.join("tokenizer.json").exists() {
            info!("Parakeet model not found, downloading...");
            download_model(&dir)?;
        }

        info!("Loading Parakeet from {}...", dir.display());
        let parakeet = Parakeet::from_pretrained(&dir, None)
            .map_err(|e| anyhow::anyhow!("Failed to load Parakeet: {e}"))?;

        *PARAKEET.lock().unwrap() = Some(parakeet);
        info!("Parakeet model loaded and ready");
        Ok(())
    }

    pub fn unload_model() {
        *PARAKEET.lock().unwrap() = None;
        info!("Parakeet model unloaded");
    }

    pub fn is_loaded() -> bool {
        PARAKEET.lock().unwrap().is_some()
    }

    /// Transcribe 16kHz mono f32 audio to text.
    pub fn transcribe(audio: &[f32]) -> Result<String> {
        let mut guard = PARAKEET.lock().unwrap();
        let parakeet = guard.as_mut()
            .ok_or_else(|| anyhow::anyhow!("Parakeet model not loaded"))?;

        let start = std::time::Instant::now();
        let result = parakeet.transcribe_samples(
            audio.to_vec(),
            16000,
            1,
            None,
        ).map_err(|e| anyhow::anyhow!("Parakeet transcription failed: {e}"))?;

        let text = result.text.trim().to_string();
        let elapsed = start.elapsed();
        info!("Parakeet: '{}' ({:.2}s)", text, elapsed.as_secs_f64());
        Ok(text)
    }

    /// Download Parakeet TDT v3 model from HuggingFace.
    fn download_model(dest: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(dest)?;

        let api = hf_hub::api::sync::Api::new()?;
        let repo = api.model("nvidia/parakeet-tdt-0.6b-v3".to_string());

        // Download required files
        let files = ["model.onnx", "tokenizer.json", "model_config.yaml", "preprocessor_config.json"];
        for filename in &files {
            info!("Downloading {filename}...");
            let src = repo.get(filename)
                .with_context(|| format!("Failed to download {filename}"))?;
            std::fs::copy(&src, dest.join(filename))
                .with_context(|| format!("Failed to copy {filename}"))?;
        }

        info!("Parakeet model downloaded to {}", dest.display());
        Ok(())
    }
}

#[cfg(feature = "parakeet")]
pub use inner::{load_model, unload_model, is_loaded, model_dir, transcribe};

#[cfg(not(feature = "parakeet"))]
pub fn load_model() -> anyhow::Result<()> {
    anyhow::bail!("Parakeet not compiled. Build with --features parakeet")
}
#[cfg(not(feature = "parakeet"))]
pub fn unload_model() {}
#[cfg(not(feature = "parakeet"))]
pub fn is_loaded() -> bool { false }
#[cfg(not(feature = "parakeet"))]
pub fn transcribe(_audio: &[f32]) -> anyhow::Result<String> {
    anyhow::bail!("Parakeet not compiled. Build with --features parakeet")
}
#[cfg(not(feature = "parakeet"))]
pub fn model_dir() -> std::path::PathBuf {
    crate::config::data_dir().join("models").join("parakeet")
}
