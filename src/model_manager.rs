//! Model manager — download, verify, and manage transcription models.

use anyhow::Result;
use std::path::PathBuf;
use tracing::info;

/// Available models with their sizes and download sources.
pub struct ModelInfo {
    pub name: &'static str,
    pub backend: &'static str,
    pub size_mb: u32,
    pub description: &'static str,
    pub is_downloaded: bool,
    pub path: PathBuf,
}

/// List all available models and their download status.
pub fn list_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            name: "ggml-large-v3-turbo-q5_0.bin",
            backend: "whisper",
            size_mb: 547,
            description: "Whisper large-v3-turbo Q5 — 99 languages, ~1.2s/10s audio",
            is_downloaded: whisper_model_path().exists(),
            path: whisper_model_path(),
        },
        ModelInfo {
            name: "parakeet-tdt-0.6b-v3",
            backend: "parakeet",
            size_mb: 600,
            description: "Parakeet TDT 0.6B — fastest, 25 EU languages, ~27ms/10s audio",
            is_downloaded: parakeet_model_dir().join("tokenizer.json").exists(),
            path: parakeet_model_dir(),
        },
        ModelInfo {
            name: "voxtral-q4.gguf",
            backend: "voxtral-local",
            size_mb: 2300,
            description: "Voxtral Mini 4B Q4 — streaming, 13 languages, ~400ms/10s audio",
            is_downloaded: voxtral_model_dir().join("voxtral-q4.gguf").exists(),
            path: voxtral_model_dir(),
        },
    ]
}

/// Check which models are downloaded.
pub fn print_status() {
    println!("Models:");
    for model in list_models() {
        let status = if model.is_downloaded { "✓" } else { "✗" };
        println!(
            "  {status} {:<35} {:>5}MB  {}",
            model.name, model.size_mb, model.description
        );
    }
}

fn whisper_model_path() -> PathBuf {
    crate::config::data_dir()
        .join("models")
        .join("ggml-large-v3-turbo-q5_0.bin")
}

fn parakeet_model_dir() -> PathBuf {
    crate::config::data_dir().join("models").join("parakeet")
}

fn voxtral_model_dir() -> PathBuf {
    crate::config::data_dir().join("models").join("voxtral")
}

/// Ensure the model for a given backend is downloaded.
pub fn ensure_model(backend: &str) -> Result<()> {
    match backend {
        "whisper" => {
            if !whisper_model_path().exists() {
                info!("Downloading Whisper model...");
                crate::notify::send("Whisper Push", "Downloading Whisper model (~547MB)...");
                crate::transcribe::load_model("ggml-large-v3-turbo-q5_0.bin")?;
                crate::notify::send("Whisper Push", "Whisper model ready!");
            }
        }
        "parakeet" => {
            if !parakeet_model_dir().join("tokenizer.json").exists() {
                info!("Downloading Parakeet model...");
                crate::notify::send("Whisper Push", "Downloading Parakeet model (~600MB)...");
                crate::transcribe::parakeet::load_model()?;
                crate::notify::send("Whisper Push", "Parakeet model ready!");
            }
        }
        "voxtral-local" => {
            if !voxtral_model_dir().join("voxtral-q4.gguf").exists() {
                info!("Voxtral Q4 model not found");
                crate::notify::send(
                    "Whisper Push",
                    "Download Voxtral Q4: hf download TrevorJS/voxtral-mini-realtime-gguf --local-dir models/",
                );
                anyhow::bail!("Voxtral Q4 model not downloaded");
            }
        }
        _ => {}
    }
    Ok(())
}
