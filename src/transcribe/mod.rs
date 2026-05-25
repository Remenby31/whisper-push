pub mod voxtral_api;
pub mod voxtral_local;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::{info, warn};

/// Available transcription backends.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// Local whisper.cpp (Metal/CUDA/CPU)
    WhisperLocal(String), // model filename
    /// Local Voxtral Mini 4B Realtime (Burn + WGPU, Q4 GGUF)
    VoxtralLocal,
    /// Mistral Voxtral API (cloud)
    VoxtralAPI,
}

impl Backend {
    pub fn label(&self) -> &str {
        match self {
            Backend::WhisperLocal(m) => {
                if m.contains("large-v3-turbo") { "Whisper large-v3-turbo (local)" }
                else if m.contains("small") { "Whisper small (local)" }
                else if m.contains("base") { "Whisper base (local)" }
                else { "Whisper (local)" }
            }
            Backend::VoxtralLocal => "Voxtral Mini 4B (local, Q4 GPU)",
            Backend::VoxtralAPI => "Voxtral API (Mistral cloud)",
        }
    }
}

static MODEL: Mutex<Option<whisper_rs::WhisperContext>> = Mutex::new(None);

/// Get the path where models are stored.
pub fn model_path(filename: &str) -> PathBuf {
    crate::config::data_dir().join("models").join(filename)
}

/// Load the whisper model into memory. Blocks until ready.
pub fn load_model(model_name: &str) -> Result<()> {
    let path = model_path(model_name);

    if !path.exists() {
        info!("Model not found at {}, downloading...", path.display());
        download_model(model_name, &path)?;
    }

    info!("Loading model from {}...", path.display());

    let ctx = whisper_rs::WhisperContext::new_with_params(
        path.to_str().unwrap(),
        whisper_rs::WhisperContextParameters::default(),
    )
    .map_err(|e| anyhow::anyhow!("Failed to load model: {:?}", e))?;

    *MODEL.lock().unwrap() = Some(ctx);
    info!("Model loaded and ready");
    Ok(())
}

/// Unload the model to free memory.
pub fn unload_model() {
    *MODEL.lock().unwrap() = None;
    info!("Model unloaded");
}

/// Check if the model is loaded.
pub fn is_loaded() -> bool {
    MODEL.lock().unwrap().is_some()
}

/// Transcribe audio using the active backend.
pub fn transcribe_with_backend(audio: &[f32], language: &str, backend: &Backend, api_key: Option<&str>) -> Result<String> {
    match backend {
        Backend::WhisperLocal(_) => transcribe_whisper(audio, language),
        Backend::VoxtralLocal => voxtral_local::transcribe(audio),
        Backend::VoxtralAPI => {
            let key = api_key.ok_or_else(|| anyhow::anyhow!("Mistral API key required for Voxtral"))?;
            voxtral_api::transcribe(audio, key, language)
        }
    }
}

/// Transcribe a 16kHz mono f32 audio buffer to text (whisper local).
pub fn transcribe(audio: &[f32], language: &str) -> Result<String> {
    transcribe_whisper(audio, language)
}

fn transcribe_whisper(audio: &[f32], language: &str) -> Result<String> {
    let guard = MODEL.lock().unwrap();
    let ctx = guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Model not loaded"))?;

    let mut params = whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });

    if language != "auto" {
        params.set_language(Some(language));
    } else {
        params.set_language(None);
    }

    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_single_segment(true);

    // Create state and run inference
    let mut state = ctx.create_state()
        .map_err(|e| anyhow::anyhow!("Failed to create state: {:?}", e))?;

    state.full(params, audio)
        .map_err(|e| anyhow::anyhow!("Transcription failed: {:?}", e))?;

    let num_segments = state.full_n_segments();

    let mut text = String::new();
    for i in 0..num_segments {
        if let Some(segment) = state.get_segment(i) {
            if let Ok(segment_text) = segment.to_str() {
                let trimmed = segment_text.trim();
                if !trimmed.is_empty() {
                    if !text.is_empty() {
                        text.push(' ');
                    }
                    text.push_str(trimmed);
                }
            }
        }
    }

    info!("Transcribed: '{text}'");
    Ok(text)
}

/// Download a GGUF model from HuggingFace.
fn download_model(model_name: &str, dest: &PathBuf) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Map model name to HuggingFace repo and file
    let (repo, filename) = match model_name {
        "ggml-large-v3-turbo-q5_0.bin" => (
            "ggerganov/whisper.cpp",
            "ggml-large-v3-turbo-q5_0.bin",
        ),
        "ggml-large-v3-turbo.bin" => (
            "ggerganov/whisper.cpp",
            "ggml-large-v3-turbo.bin",
        ),
        other => {
            // Try as a direct repo/file reference
            ("ggerganov/whisper.cpp", other)
        }
    };

    info!("Downloading {filename} from {repo}...");

    let api = hf_hub::api::sync::Api::new()?;
    let repo = api.model(repo.to_string());
    let path = repo.get(filename)?;

    // Copy to our model directory
    std::fs::copy(&path, dest)?;

    info!("Model downloaded to {}", dest.display());
    Ok(())
}
