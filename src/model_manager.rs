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
        },
        ModelInfo {
            name: "parakeet-tdt-0.6b-v3",
            backend: "parakeet",
            size_mb: 600,
            description: "Parakeet TDT 0.6B — fastest, 25 EU languages, ~27ms/10s audio",
            is_downloaded: parakeet_model_dir().join("vocab.txt").exists(),
        },
        ModelInfo {
            name: "voxtral-q4.gguf",
            backend: "voxtral-local",
            size_mb: 2300,
            description: "Voxtral Mini 4B Q4 — streaming, 13 languages, ~400ms/10s audio",
            is_downloaded: voxtral_model_dir().join("voxtral-q4.gguf").exists(),
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

/// Derive the backend from a model name.
pub fn backend_for_model(model: &str) -> &'static str {
    if model.contains("parakeet") {
        "parakeet"
    } else if model.contains("voxtral") {
        "voxtral-local"
    } else {
        "whisper"
    }
}

/// Get the default model name for a backend (used by onboarding).
pub fn model_for_backend(backend: &str) -> &'static str {
    match backend {
        "parakeet" => "parakeet-tdt-0.6b-v3-int8",
        "voxtral-local" => "voxtral-q4.gguf",
        _ => "ggml-large-v3-turbo-q5_0.bin",
    }
}

/// Resolve a model name to a transcribe::Backend enum.
pub fn resolve_backend(model: &str) -> crate::transcribe::Backend {
    match backend_for_model(model) {
        "parakeet" => crate::transcribe::Backend::Parakeet,
        "voxtral-local" => crate::transcribe::Backend::VoxtralLocal,
        _ => crate::transcribe::Backend::WhisperLocal(model.to_string()),
    }
}

/// Check if the model for a backend is already downloaded.
pub fn is_model_downloaded(backend: &str) -> bool {
    match backend {
        "whisper" => whisper_model_path().exists(),
        "parakeet" => parakeet_model_dir().join("vocab.txt").exists(),
        "voxtral-local" => voxtral_model_dir().join("voxtral-q4.gguf").exists(),
        _ => false,
    }
}

/// Get the approximate download size in MB for a backend.
pub fn model_size_mb(backend: &str) -> u32 {
    match backend {
        "whisper" => 547,
        "parakeet" => 600,
        "voxtral-local" => 2300,
        _ => 0,
    }
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
            if !parakeet_model_dir().join("vocab.txt").exists() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_for_model_whisper() {
        assert_eq!(backend_for_model("ggml-large-v3-turbo-q5_0.bin"), "whisper");
    }

    #[test]
    fn test_backend_for_model_parakeet() {
        assert_eq!(backend_for_model("parakeet-tdt-0.6b-v3"), "parakeet");
    }

    #[test]
    fn test_backend_for_model_voxtral() {
        assert_eq!(backend_for_model("voxtral-q4.gguf"), "voxtral-local");
    }

    #[test]
    fn test_backend_for_model_unknown_defaults_to_whisper() {
        assert_eq!(backend_for_model("some-unknown-model.bin"), "whisper");
    }

    #[test]
    fn test_model_for_backend_roundtrip() {
        for backend in &["whisper", "parakeet", "voxtral-local"] {
            let model = model_for_backend(backend);
            assert_eq!(backend_for_model(model), *backend);
        }
    }

    #[test]
    fn test_model_for_backend_unknown_defaults_to_whisper() {
        assert_eq!(model_for_backend("unknown"), "ggml-large-v3-turbo-q5_0.bin");
    }

    #[test]
    fn test_resolve_backend_whisper() {
        let b = resolve_backend("ggml-large-v3-turbo-q5_0.bin");
        assert!(matches!(b, crate::transcribe::Backend::WhisperLocal(_)));
    }

    #[test]
    fn test_resolve_backend_parakeet() {
        let b = resolve_backend("parakeet-tdt-0.6b-v3");
        assert!(matches!(b, crate::transcribe::Backend::Parakeet));
    }

    #[test]
    fn test_resolve_backend_voxtral() {
        let b = resolve_backend("voxtral-q4.gguf");
        assert!(matches!(b, crate::transcribe::Backend::VoxtralLocal));
    }

    #[test]
    fn test_list_models_has_three_entries() {
        let models = list_models();
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].backend, "whisper");
        assert_eq!(models[1].backend, "parakeet");
        assert_eq!(models[2].backend, "voxtral-local");
    }

    #[test]
    fn test_list_models_sizes_positive() {
        for m in list_models() {
            assert!(m.size_mb > 0, "Model {} has 0 size", m.name);
            assert!(!m.name.is_empty());
            assert!(!m.description.is_empty());
        }
    }
}
