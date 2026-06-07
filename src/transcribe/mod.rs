pub mod parakeet;
pub mod voxtral_local;

use anyhow::Result;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::info;

/// Available transcription backends.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// Parakeet TDT — fastest (ONNX Runtime, WebGPU/CUDA/CPU)
    Parakeet,
    /// Local whisper.cpp (Metal/CUDA/CPU)
    WhisperLocal(String), // model filename
    /// Local Voxtral Mini 4B Realtime (Burn + WGPU, Q4 GGUF)
    VoxtralLocal,
}

static MODEL: Mutex<Option<whisper_rs::WhisperContext>> = Mutex::new(None);

/// Get the path where the model file lives.
///
/// Priority: a model bundled inside the .app `Contents/Resources/models/`
/// Path to a model file in the user data dir (downloaded on first run).
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

    let mut ctx_params = whisper_rs::WhisperContextParameters::default();
    ctx_params.use_gpu(true);
    ctx_params.flash_attn(true);
    let ctx = whisper_rs::WhisperContext::new_with_params(path.to_str().unwrap(), ctx_params)
        .map_err(|e| anyhow::anyhow!("Failed to load model: {:?}", e))?;

    *MODEL.lock().unwrap_or_else(|e| e.into_inner()) = Some(ctx);
    info!("Model loaded and ready");
    Ok(())
}

/// Unload the model to free memory.
pub fn unload_model() {
    *MODEL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    info!("Model unloaded");
}

/// Load the engine for `backend`, backend-aware. Parakeet and Voxtral have
/// their own loaders; only Whisper uses the generic ggml download path. This is
/// the single entry point CLI commands (`--transcribe`, `self-test`) should use
/// so a non-Whisper model is never accidentally routed through the whisper
/// downloader (which 404s on a Parakeet model name). Voxtral is intentionally
/// lazy — it must load on the same thread that transcribes, which
/// `transcribe_with_backend` handles.
pub fn ensure_loaded(backend: &Backend, model_name: &str) -> Result<()> {
    match backend {
        Backend::Parakeet => parakeet::load_model(),
        Backend::WhisperLocal(_) => load_model(model_name),
        Backend::VoxtralLocal => Ok(()),
    }
}

/// Check if the model is loaded.
#[allow(dead_code)] // Used by integration tests
pub fn is_loaded() -> bool {
    MODEL.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Transcribe audio using the active backend.
///
/// Every backend funnels through this one choke point, so the adaptive
/// dictionary is applied uniformly and model-agnostically: the raw model output
/// is post-corrected by `finalize_and_record` (deterministic learned fixes +
/// guarded fuzzy) and the trace is stashed so a later correction can learn from
/// it. When the dictionary is empty/disabled this is a ~0-cost pass-through.
pub fn transcribe_with_backend(audio: &[f32], language: &str, backend: &Backend) -> Result<String> {
    // Licensing gate — the single choke point all dictation passes through, so
    // it can't be bypassed (hold, toggle, tray "Test", CLI --transcribe). Empty
    // output is already handled gracefully by every caller (= "no speech").
    if !crate::license::is_entitled() {
        crate::license::on_blocked();
        return Ok(String::new());
    }
    // Raw text + per-word timings (empty when the backend gives none → the
    // acoustic layer segments by energy, keeping it model-agnostic).
    let (raw, words): (String, Vec<crate::acoustic::WordTiming>) = match backend {
        Backend::Parakeet => {
            // Parakeet may have failed to download/load at startup (in which
            // case Whisper was loaded as the fallback). Don't hard-fail the
            // transcription — fall back to Whisper transparently.
            match parakeet::transcribe_timed(audio) {
                Ok(tw) => tw,
                Err(e) => {
                    tracing::warn!("Parakeet unavailable ({e}); using Whisper instead");
                    (transcribe_whisper(audio, language)?, Vec::new())
                }
            }
        }
        Backend::WhisperLocal(_) => (transcribe_whisper(audio, language)?, Vec::new()),
        Backend::VoxtralLocal => {
            // Voxtral must be loaded on the SAME thread that transcribes
            // (WGPU/Metal doesn't support cross-thread model usage)
            #[cfg(feature = "voxtral")]
            {
                if !voxtral_local::is_loaded() {
                    info!("Loading Voxtral Q4 on transcription thread...");
                    let dir = crate::config::data_dir().join("models").join("voxtral");
                    voxtral_local::load_model(dir.to_str().unwrap_or(""))?;
                }
            }
            (voxtral_local::transcribe(audio)?, Vec::new())
        }
    };

    // 1. Acoustic layer (model-agnostic): correct words by their SOUND, and
    //    retain audio+timings so a later correction can learn a fingerprint.
    let acoustic = crate::acoustic::process(audio, &raw, words, language);
    // 2. Text layer: learned-dictionary post-correction + record for learning.
    Ok(whisper_push_dict::finalize_and_record(&acoustic, language))
}

fn transcribe_whisper(audio: &[f32], language: &str) -> Result<String> {
    let guard = MODEL.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Model not loaded"))?;

    let mut params =
        whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });

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
    let mut state = ctx
        .create_state()
        .map_err(|e| anyhow::anyhow!("Failed to create state: {:?}", e))?;

    state
        .full(params, audio)
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
        "ggml-large-v3-turbo-q5_0.bin" => ("ggerganov/whisper.cpp", "ggml-large-v3-turbo-q5_0.bin"),
        "ggml-large-v3-turbo.bin" => ("ggerganov/whisper.cpp", "ggml-large-v3-turbo.bin"),
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
