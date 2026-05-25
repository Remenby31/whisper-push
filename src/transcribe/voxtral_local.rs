//! Voxtral Mini 4B Realtime — local GPU transcription.
//!
//! Requires the `voxtral` feature flag. Uses the voxtral-mini-realtime-rs
//! crate (Burn + WGPU) with Q4 GGUF quantized weights (~2.5GB).
//!
//! Note: This integration depends on voxtral-mini-realtime-rs which is
//! still in active development. Build with `--features voxtral` to enable.
//! Download model: `hf download TrevorJS/voxtral-mini-realtime-gguf --local-dir models/`

#[cfg(feature = "voxtral")]
compile_error!("Voxtral local integration is work-in-progress. The voxtral-mini-realtime-rs \
    crate has a burn/cubecl type mismatch in the current git version. \
    Track https://github.com/TrevorS/voxtral-mini-realtime-rs for updates. \
    Use 'whisper' or 'voxtral-api' backends for now.");

/// Load the Voxtral Q4 GGUF model.
pub fn load_model(_model_dir: &str) -> anyhow::Result<()> {
    anyhow::bail!("Voxtral local not yet available. Use 'whisper' (local) or 'voxtral-api' (cloud).")
}

/// Unload the model.
pub fn unload_model() {}

/// Transcribe 16kHz mono f32 audio to text.
pub fn transcribe(_audio: &[f32]) -> anyhow::Result<String> {
    anyhow::bail!("Voxtral local not yet available. Use 'whisper' (local) or 'voxtral-api' (cloud).")
}
