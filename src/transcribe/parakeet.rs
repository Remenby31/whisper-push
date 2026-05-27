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

    /// Download the Parakeet ONNX model from HuggingFace.
    ///
    /// `parakeet_rs::Parakeet` (the CTC model) expects `model.onnx` +
    /// `tokenizer.json` in the directory (the previous `nvidia/...` repo only
    /// ships `.nemo` files, hence the download 404'd). The onnx-community
    /// export keeps the weights in `onnx/`.
    fn download_model(dest: &PathBuf) -> Result<()> {
        std::fs::create_dir_all(dest)?;

        let api = hf_hub::api::sync::Api::new()?;
        let repo = api.model("onnx-community/parakeet-ctc-0.6b-ONNX".to_string());

        // (path in repo, local filename). model.onnx + tokenizer.json are
        // required; the external-weights .onnx_data is only present for large
        // exports, so it is best-effort.
        let required = [("onnx/model.onnx", "model.onnx"), ("tokenizer.json", "tokenizer.json")];
        for (src_name, local) in &required {
            info!("Downloading {src_name}...");
            let src = repo.get(src_name)
                .with_context(|| format!("Failed to download {src_name}"))?;
            std::fs::copy(&src, dest.join(local))
                .with_context(|| format!("Failed to copy {local}"))?;
        }
        if let Ok(src) = repo.get("onnx/model.onnx_data") {
            let _ = std::fs::copy(&src, dest.join("model.onnx_data"));
        }

        info!("Parakeet model downloaded to {}", dest.display());
        Ok(())
    }
}

#[cfg(feature = "parakeet")]
pub use inner::{is_loaded, load_model, transcribe};

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
