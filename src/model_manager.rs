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

// ── Downloading ──────────────────────────────────────────────────────────────
// ONE place that knows where a model comes from and how it lands on disk. The
// onboarding wizard (all platforms), the tray's "download this engine" click and
// the lazy first-use download in `transcribe` all go through here, so a changed
// repo or filename is a one-line edit instead of a hunt through three modules.

/// One remote file of a model: where to fetch it and where it lands.
#[derive(Debug, Clone)]
pub struct DownloadFile {
    pub url: String,
    pub dest: PathBuf,
}

/// Progress of a running download, reported per chunk.
#[derive(Debug, Clone)]
pub struct Progress {
    /// 0-based index of the file being fetched, within this model's plan.
    pub file_index: usize,
    /// File name being written (not the full path).
    pub file_name: String,
    pub downloaded: u64,
    /// 0 when the server sends no Content-Length.
    pub total: u64,
}

const HF: &str = "https://huggingface.co";
/// Parakeet ONNX export we ship. NB: `nvidia/…` is `.nemo` only and
/// `onnx-community/parakeet-ctc-0.6b-ONNX` is CTC English-only — never "restore"
/// either one (see src/transcribe/parakeet.rs).
const PARAKEET_REPO: &str = "istupakov/parakeet-tdt-0.6b-v3-onnx";

/// Every file `model` needs, in fetch order. Empty for an unknown name.
pub fn download_plan(model: &str) -> Vec<DownloadFile> {
    let file = |url: String, dest: PathBuf| DownloadFile { url, dest };
    match model {
        "ggml-large-v3-turbo-q5_0.bin" | "ggml-large-v3-turbo.bin" | "ggml-small-q5_1.bin" => {
            vec![file(
                format!("{HF}/ggerganov/whisper.cpp/resolve/main/{model}"),
                crate::config::whisper_model_path(model),
            )]
        }
        // int8 graphs are self-contained; fp32 ships a large `.onnx.data`
        // sidecar. Either way they are saved under the fixed names parakeet-rs
        // expects — ONNX Runtime executes the quantised ops transparently.
        "parakeet-tdt-0.6b-v3-int8" | "parakeet-tdt-0.6b-v3" => {
            let int8 = model.ends_with("-int8");
            let dir = crate::config::parakeet_dir();
            let base = format!("{HF}/{PARAKEET_REPO}/resolve/main");
            let names: &[(&str, &str)] = if int8 {
                &[
                    ("encoder-model.int8.onnx", "encoder-model.onnx"),
                    ("decoder_joint-model.int8.onnx", "decoder_joint-model.onnx"),
                    ("vocab.txt", "vocab.txt"),
                ]
            } else {
                &[
                    ("encoder-model.onnx", "encoder-model.onnx"),
                    ("encoder-model.onnx.data", "encoder-model.onnx.data"),
                    ("decoder_joint-model.onnx", "decoder_joint-model.onnx"),
                    ("vocab.txt", "vocab.txt"),
                ]
            };
            names
                .iter()
                .map(|(src, dst)| file(format!("{base}/{src}"), dir.join(dst)))
                .collect()
        }
        "voxtral-q4.gguf" => {
            let dir = crate::config::voxtral_dir();
            let base = format!("{HF}/TrevorJS/voxtral-mini-realtime-gguf/resolve/main");
            ["voxtral-q4.gguf", "tekken.json"]
                .iter()
                .map(|n| file(format!("{base}/{n}"), dir.join(n)))
                .collect()
        }
        // Unknown name: try it as a whisper.cpp ggml file, which is what the
        // lazy loader used to do.
        other => vec![file(
            format!("{HF}/ggerganov/whisper.cpp/resolve/main/{other}"),
            crate::config::whisper_model_path(other),
        )],
    }
}

/// The plan minus what is already on disk.
pub fn missing_files(model: &str) -> Vec<DownloadFile> {
    let plan = download_plan(model);
    // A Parakeet variant switch keeps the same filenames, so "the file exists"
    // is the wrong question — the whole set has to come down again.
    if stale_parakeet_variant(model) {
        return plan;
    }
    plan.into_iter().filter(|f| !f.dest.exists()).collect()
}

/// Is `models/parakeet/` populated with the *other* variant than `model` asks
/// for? Both variants share one directory and identical filenames; only the
/// `.variant` marker tells them apart.
fn stale_parakeet_variant(model: &str) -> bool {
    let Some(suffix) = model.strip_prefix("parakeet-tdt-0.6b-v3") else {
        return false;
    };
    let want = if suffix == "-int8" { "int8" } else { "fp32" };
    let dir = crate::config::parakeet_dir();
    if !dir.join("vocab.txt").exists() {
        return false; // nothing there yet — a normal first download
    }
    let on_disk = std::fs::read_to_string(dir.join(".variant"))
        .ok()
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "fp32".into());
    on_disk != want
}

/// Download every missing file of `model`, calling `on_progress` as bytes land.
/// Each file is written to `<dest>.part` and renamed on completion, so an
/// interrupted run never leaves a truncated model that looks installed.
pub fn download(model: &str, on_progress: &mut dyn FnMut(Progress)) -> anyhow::Result<()> {
    // Swapping Parakeet variants overwrites the same filenames — clear the old
    // set first so a failed pull can't leave a half-int8, half-fp32 directory
    // that loads and then produces garbage.
    if stale_parakeet_variant(model) {
        let dir = crate::config::parakeet_dir();
        tracing::info!("Parakeet variant switch — clearing {}", dir.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
    let files = missing_files(model);
    let count = files.len();
    if count == 0 {
        return Ok(());
    }
    for (index, f) in files.iter().enumerate() {
        let name = f
            .dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        tracing::info!("Downloading {} → {}", f.url, f.dest.display());
        fetch(f, &mut |downloaded, total| {
            on_progress(Progress {
                file_index: index,
                file_name: name.clone(),
                downloaded,
                total,
            })
        })?;
    }
    // Record which Parakeet variant is on disk — both share models/parakeet/
    // with identical filenames, so only this marker tells them apart.
    if let Some(variant) = model.strip_prefix("parakeet-tdt-0.6b-v3") {
        let variant = if variant == "-int8" { "int8" } else { "fp32" };
        let dir = crate::config::parakeet_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(".variant"), variant);
    }
    // Final tick so a watcher lands on 100% for this model.
    on_progress(Progress {
        file_index: count,
        file_name: String::new(),
        downloaded: 0,
        total: 0,
    });
    Ok(())
}

/// Stream one file to disk. `on_bytes(downloaded, total)` fires per chunk.
fn fetch(f: &DownloadFile, on_bytes: &mut dyn FnMut(u64, u64)) -> anyhow::Result<()> {
    use std::io::{Read, Write};

    if let Some(parent) = f.dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let resp = ureq::get(&f.url)
        .config()
        // Bounded, but generous: a 2.5 GB model on a slow line is legitimate.
        // Without a ceiling a half-open socket would hang the caller forever.
        .timeout_global(Some(std::time::Duration::from_secs(3600)))
        .build()
        .header(
            "User-Agent",
            &format!("whisper-push/{}", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|e| anyhow::anyhow!("{}: {e}", f.url))?;

    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let part = f.dest.with_extension("part");
    let mut out = std::fs::File::create(&part)?;
    let mut reader = resp.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        done += n as u64;
        on_bytes(done, total);
    }
    out.flush()?;
    drop(out);
    if total > 0 && done < total {
        let _ = std::fs::remove_file(&part);
        anyhow::bail!("{} ended early ({done} of {total} bytes)", f.dest.display());
    }
    std::fs::rename(&part, &f.dest)?;
    Ok(())
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

    /// Every model the picker offers must have somewhere to come from — a model
    /// with an empty plan silently "downloads" nothing and then fails to load.
    #[test]
    fn every_listed_model_has_a_download_plan() {
        for m in list_models() {
            let plan = download_plan(m.name);
            assert!(!plan.is_empty(), "{} has no download plan", m.name);
            for f in plan {
                assert!(f.url.starts_with("https://"), "{} : {}", m.name, f.url);
                assert!(
                    f.dest.starts_with(crate::config::models_dir()),
                    "{} writes outside models/: {}",
                    m.name,
                    f.dest.display()
                );
            }
        }
    }

    /// The int8 graphs are fetched under their `.int8.onnx` names but must land
    /// under the plain ones — parakeet-rs looks for those, and nothing else
    /// would catch the mismatch until a user's first dictation failed.
    #[test]
    fn parakeet_int8_lands_under_the_names_the_loader_expects() {
        let plan = download_plan("parakeet-tdt-0.6b-v3-int8");
        let names: Vec<String> = plan
            .iter()
            .map(|f| f.dest.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"encoder-model.onnx".to_string()));
        assert!(names.contains(&"decoder_joint-model.onnx".to_string()));
        assert!(names.contains(&"vocab.txt".to_string()));
        assert!(
            plan.iter()
                .any(|f| f.url.contains("encoder-model.int8.onnx"))
        );
        // The int8 graph is self-contained: no external .data sidecar.
        assert!(!names.iter().any(|n| n.ends_with(".data")));
    }

    /// fp32 is the variant WITH the sidecar — the two plans must not be the same.
    #[test]
    fn parakeet_fp32_carries_its_sidecar() {
        let plan = download_plan("parakeet-tdt-0.6b-v3");
        assert!(
            plan.iter()
                .any(|f| f.url.ends_with("encoder-model.onnx.data"))
        );
    }

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
