//! Model manager — download, verify, and manage transcription models.

use std::path::PathBuf;

/// Available models with their sizes and download sources.
pub struct ModelInfo {
    /// Model file / canonical id (also the value stored in `config.model`).
    pub name: &'static str,
    /// Short human label for menus (mirrors the onboarding picker).
    pub label: &'static str,
    pub size_mb: u32,
    pub description: &'static str,
    pub is_downloaded: bool,
}

/// List all available models and their download status. This is the single
/// source of truth for the tray "Engine" dropdown and mirrors the onboarding
/// model picker (`macos/Onboarding/Sources/ModelPickerView.swift`) — keep the
/// two in sync (same names, labels, sizes).
pub fn list_models() -> Vec<ModelInfo> {
    vec![
        ModelInfo {
            name: "parakeet-tdt-0.6b-v3-int8",
            label: "Parakeet TDT v3 (int8)",
            size_mb: 670,
            description: "Parakeet TDT 0.6B int8 — fastest + lightest, 25 EU languages",
            is_downloaded: parakeet_variant_downloaded(true),
        },
        ModelInfo {
            name: "parakeet-tdt-0.6b-v3",
            label: "Parakeet TDT v3 (fp32)",
            size_mb: 2500,
            description: "Parakeet TDT 0.6B fp32 — highest accuracy, 25 EU languages",
            is_downloaded: parakeet_variant_downloaded(false),
        },
        ModelInfo {
            name: "ggml-small-q5_1.bin",
            label: "Whisper Small (q5)",
            size_mb: 181,
            description: "Whisper small Q5 — 99 languages, lightweight",
            is_downloaded: whisper_model_path("ggml-small-q5_1.bin").exists(),
        },
        ModelInfo {
            name: "ggml-large-v3-turbo-q5_0.bin",
            label: "Whisper Turbo (q5)",
            size_mb: 550,
            description: "Whisper large-v3-turbo Q5 — 99 languages, ~1.2s/10s audio",
            is_downloaded: whisper_model_path("ggml-large-v3-turbo-q5_0.bin").exists(),
        },
        ModelInfo {
            name: "voxtral-q4.gguf",
            label: "Voxtral Realtime",
            size_mb: 2300,
            description: "Voxtral Mini 4B Q4 — streaming, 13 languages, ~400ms/10s audio",
            is_downloaded: voxtral_model_dir().join("voxtral-q4.gguf").exists(),
        },
    ]
}

/// Is the requested Parakeet variant the one currently on disk? Both variants
/// share `models/parakeet/` (same filenames); a `.variant` marker file records
/// which is present (absent ⇒ legacy fp32 install). Mirrors the Swift check.
fn parakeet_variant_downloaded(want_int8: bool) -> bool {
    let dir = parakeet_model_dir();
    if !dir.join("vocab.txt").exists() {
        return false;
    }
    let variant = std::fs::read_to_string(dir.join(".variant"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "fp32".into());
    if want_int8 {
        variant == "int8"
    } else {
        variant == "fp32"
    }
}

/// Look up a model by its `name` (config value).
pub fn find_model(name: &str) -> Option<ModelInfo> {
    list_models().into_iter().find(|m| m.name == name)
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

fn whisper_model_path(filename: &str) -> PathBuf {
    crate::config::whisper_model_path(filename)
}

fn parakeet_model_dir() -> PathBuf {
    crate::config::parakeet_dir()
}

fn voxtral_model_dir() -> PathBuf {
    crate::config::voxtral_dir()
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
    fn test_list_models_mirrors_onboarding() {
        let models = list_models();
        // The five models the onboarding picker offers (keep in sync).
        assert_eq!(models.len(), 5);
        let names: Vec<&str> = models.iter().map(|m| m.name).collect();
        assert!(names.contains(&"parakeet-tdt-0.6b-v3-int8"));
        assert!(names.contains(&"parakeet-tdt-0.6b-v3"));
        assert!(names.contains(&"ggml-small-q5_1.bin"));
        assert!(names.contains(&"ggml-large-v3-turbo-q5_0.bin"));
        assert!(names.contains(&"voxtral-q4.gguf"));
        // find_model round-trips and the backend is derivable from each name.
        for m in &models {
            assert!(find_model(m.name).is_some());
            assert!(!backend_for_model(m.name).is_empty());
        }
    }

    #[test]
    fn test_list_models_fields_nonempty() {
        for m in list_models() {
            assert!(m.size_mb > 0, "Model {} has 0 size", m.name);
            assert!(!m.name.is_empty());
            assert!(!m.label.is_empty());
            assert!(!m.description.is_empty());
        }
    }
}
