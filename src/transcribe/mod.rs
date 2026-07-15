pub mod parakeet;
pub mod voxtral_local;

use crate::util::LockSafe;
use anyhow::Result;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
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

impl Backend {
    /// Stable, human-facing name for logs and toasts — one label per backend, so
    /// the same backend never appears two different ways (was `{:?}` Debug in some
    /// places, a lowercase id string in others).
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Parakeet => "Parakeet",
            Backend::WhisperLocal(_) => "Whisper",
            Backend::VoxtralLocal => "Voxtral",
        }
    }
}

static MODEL: Mutex<Option<whisper_rs::WhisperContext>> = Mutex::new(None);

// ─── Keep-warm ───────────────────────────────────────────────────────────────
// A large model (Parakeet ships 2.3 GB of FP32 weights) is mmapped by the
// runtime, so macOS compresses/swaps those pages out during idle periods. The
// first dictation after a pause then pays a multi-second page-in + decompress
// *before* any inference runs (measured: ~2 GB re-faulted, 11–18 s), which reads
// as "the model takes 20 s to load" — though warm dictations stay at ~0.5–1.5 s.
//
// mlock can't pin the weights on macOS: the OS refuses to wire shared file-backed
// pages (EPERM), the MAP_PRIVATE variant only makes a useless dirty copy, and
// ONNX Runtime owns its own mappings anyway. So we keep them resident the proven
// way — a tiny silent inference every interval touches every weight, so the OS
// never reclaims them. It runs whenever a model is loaded (gated by
// `config.keep_model_resident`); system sleep freezes the thread, so an asleep
// Mac burns nothing. This is the single thing that makes the first dictation of
// the day instant instead of cold.

/// Heartbeat between keep-warm ticks. Reclaim is pressure-driven, not a fixed
/// timer (logs show full eviction after ~3 min under load, but survival past
/// ~10 min when idle): a 90 s re-touch keeps the pages "recently used" so under
/// normal pressure they stay hot indefinitely while awake. Under an acute
/// pressure spike eviction can still beat a single 90 s gap → at most one
/// bounded, self-healing cold dictation (the next inference re-touches every
/// weight). 90 s is a conservative, latency-favouring choice; raise it to trade
/// a little robustness for battery.
const KEEP_WARM_INTERVAL: Duration = Duration::from_secs(90);
/// Silence length for a warm tick: 1 s @ 16 kHz, enough to run the full encoder
/// forward pass (touching every weight) on any backend.
pub(crate) const WARM_SAMPLES: usize = 16_000;

/// Upper bound on a model download. Generous — a ~1.5 GB Whisper / 2.3 GB
/// Parakeet pull over a slow link must not be aborted — but finite, so a
/// dead socket eventually surfaces as an error instead of a permanent wedge.
pub(crate) const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// Cumulative (major, minor) page faults for this process. Major faults are the
/// ones that hit disk (the model's mmapped weights being read back in after the
/// OS evicted them); minor faults are served from RAM (e.g. decompression). The
/// delta around an inference tells us *why* a cold dictation is slow — disk
/// page-in vs decompression vs neither (pure compute). One cheap syscall.
#[cfg(unix)]
pub(crate) fn page_faults() -> (i64, i64) {
    let mut u = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage fills a valid rusage for RUSAGE_SELF; we read it only on success.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, u.as_mut_ptr()) } != 0 {
        return (0, 0);
    }
    let u = unsafe { u.assume_init() };
    (u.ru_majflt as i64, u.ru_minflt as i64)
}
#[cfg(not(unix))]
pub(crate) fn page_faults() -> (i64, i64) {
    (0, 0)
}

/// macOS Apple-Silicon page size (also the unit `vm_stat`/`footprint` report).
const PAGE_BYTES: i64 = 16_384;

/// Whisper keep-warm: a tiny inference on silence to keep the weights resident.
/// Non-blocking — if a real transcription holds the model lock, skip this tick.
fn warm_whisper() {
    let Some(guard) = MODEL.try_lock_safe() else {
        return;
    };
    let Some(ctx) = guard.as_ref() else {
        return; // Whisper isn't the loaded backend
    };
    let Ok(mut state) = ctx.create_state() else {
        return;
    };
    let mut params =
        whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(true);
    let t = std::time::Instant::now();
    let _ = state.full(params, &vec![0.0f32; WARM_SAMPLES]);
    tracing::debug!("whisper kept warm ({:.2}s)", t.elapsed().as_secs_f64());
}

/// Spawn the keep-warm heartbeat. Every interval, while a model is loaded, it
/// runs a tiny silent inference that touches every weight, so macOS never
/// reclaims the pages and the first dictation after any idle gap — including the
/// first one of the day — stays instant instead of paying a multi-second
/// page-in. Each backend's `warm()` is a cheap no-op when it isn't the loaded
/// one. Voxtral is intentionally excluded: WGPU forbids using the model from a
/// thread other than the one that loaded it.
///
/// `enabled` mirrors `config.keep_model_resident`. When off, the thread isn't
/// spawned and the first dictation after an idle gap pays the cold-start page-in.
pub fn spawn_keep_warm(enabled: bool) {
    if !enabled {
        info!("keep-warm disabled (config.keep_model_resident = false)");
        return;
    }
    info!(
        "keep-warm armed (every {}s while a model is loaded; paused while the Mac sleeps)",
        KEEP_WARM_INTERVAL.as_secs()
    );
    std::thread::Builder::new()
        .name("keep-warm".into())
        .spawn(|| {
            loop {
                std::thread::sleep(KEEP_WARM_INTERVAL);
                // Engines (parakeet-rs / whisper.cpp) can panic; the real
                // transcription path guards against this with catch_unwind at the
                // choke point, and every other long-lived engine loop in the app
                // does the same. Without this, one panicked warm tick would kill
                // the heartbeat for the rest of the session — silently bringing
                // the cold-start back while real dictations still work. The locks
                // are poison-tolerant (`try_lock_safe`), so the next tick recovers.
                let _ = std::panic::catch_unwind(|| {
                    parakeet::warm();
                    warm_whisper();
                });
            }
        })
        .ok();
}

/// Path to a Whisper model file in the user data dir (downloaded on first run).
pub fn model_path(filename: &str) -> PathBuf {
    crate::config::whisper_model_path(filename)
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
    let path_str = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Model path is not valid UTF-8: {}", path.display()))?;
    let ctx = whisper_rs::WhisperContext::new_with_params(path_str, ctx_params)
        .map_err(|e| anyhow::anyhow!("Failed to load model: {:?}", e))?;

    *MODEL.lock_safe() = Some(ctx);
    info!("Model loaded and ready");
    Ok(())
}

/// Unload the model to free memory.
pub fn unload_model() {
    *MODEL.lock_safe() = None;
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
        Backend::Parakeet => parakeet::load_model(model_name),
        Backend::WhisperLocal(_) => load_model(model_name),
        Backend::VoxtralLocal => Ok(()),
    }
}

/// Check if the model is loaded.
#[allow(dead_code)] // Used by integration tests
pub fn is_loaded() -> bool {
    MODEL.lock_safe().is_some()
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
    if !crate::license::gate() {
        return Ok(String::new());
    }
    // Catch any panic from an engine (parakeet-rs / whisper.cpp / wgpu) or the
    // correction layers HERE, at the single choke point, and turn it into an
    // Err. A bad input then can't kill the caller's (spawned) thread or wedge the
    // UI at "Processing", and — because the engine model locks are poison-tolerant
    // — the next dictation recovers cleanly instead of panicking forever.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transcribe_inner(audio, language, backend)
    }))
    .unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_else(|| "unknown panic".into());
        Err(anyhow::anyhow!("Transcription panicked: {msg}"))
    })
}

/// The actual transcription pipeline (engine → acoustic → dictionary). Split out
/// so `transcribe_with_backend` can wrap it in `catch_unwind` at the one place
/// all backends pass through.
fn transcribe_inner(audio: &[f32], language: &str, backend: &Backend) -> Result<String> {
    // Diagnose cold starts: count the page faults the inference triggers. A cold
    // dictation (model evicted during idle) shows thousands of *major* faults
    // (disk page-in of the weights); a warm one shows ~0. This is what makes the
    // first dictation after a pause slow — measured, not assumed.
    let (maj0, min0) = page_faults();
    let infer_start = std::time::Instant::now();
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
                    let dir = crate::config::voxtral_dir();
                    voxtral_local::load_model(dir.to_str().unwrap_or(""))?;
                }
            }
            (voxtral_local::transcribe(audio)?, Vec::new())
        }
    };

    let (maj1, min1) = page_faults();
    let (dmaj, dmin) = (maj1 - maj0, min1 - min0);
    let secs = infer_start.elapsed().as_secs_f64();
    if dmaj > 0 {
        // Cold path: the weights had to be read back from disk before inference.
        info!(
            "cold inference: {secs:.2}s, paged in ~{} MB from disk ({dmaj} major + {dmin} minor faults)",
            dmaj * PAGE_BYTES / 1_048_576
        );
    } else {
        tracing::debug!("warm inference: {secs:.2}s, {dmin} minor faults, 0 major");
    }

    // 1. Acoustic layer (model-agnostic): correct words by their SOUND, and
    //    retain audio+timings so a later correction can learn a fingerprint.
    let acoustic = crate::acoustic::process(audio, &raw, words, language);
    // 2. Text layer: learned-dictionary post-correction + record for learning.
    Ok(whisper_push_dict::finalize_and_record(&acoustic, language))
}

fn transcribe_whisper(audio: &[f32], language: &str) -> Result<String> {
    let guard = MODEL.lock_safe();
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

    // hf-hub's blocking client has no request deadline of its own, so a
    // dead/half-open TCP socket would block here forever — and this runs on the
    // single pipeline thread, wedging every future hotkey. Bound it: on timeout
    // we return Err so the caller (LoadModel) restores Idle + the hotkeys. The
    // orphaned download thread is harmless — the temp→dest copy only runs on a
    // value we actually receive in time.
    let repo = repo.to_string();
    let filename = filename.to_string();
    let path = crate::util::run_with_timeout(DOWNLOAD_TIMEOUT, move || -> Result<PathBuf> {
        let api = hf_hub::api::sync::Api::new()?;
        Ok(api.model(repo).get(&filename)?)
    })
    .ok_or_else(|| {
        anyhow::anyhow!(
            "Model download timed out after {}s",
            DOWNLOAD_TIMEOUT.as_secs()
        )
    })??;

    // Copy to our model directory
    std::fs::copy(&path, dest)?;

    info!("Model downloaded to {}", dest.display());
    Ok(())
}
