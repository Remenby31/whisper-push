//! Kokoro-82M text-to-speech — the return channel for the MCP `speak` tool.
//!
//! Whisper Push is speech-to-text everywhere else; this is the one place it
//! speaks back. Everything stays local: the model is an 82M-parameter
//! Apache-2.0 ONNX graph that runs on CPU in a few hundred milliseconds.
//!
//! We drive `ort` directly rather than using `kokoro-en`'s `KokoroTts`, because
//! that type phonemizes internally with the English G2P and exposes no
//! phoneme-level entry point — which would make non-English voices impossible.
//! `kokoro-en` is still used, but as a G2P + tokenizer library (see [`g2p`]),
//! so all languages meet at the same tensor.

pub mod g2p;

use anyhow::{Context, Result, bail};
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing::info;

/// Kokoro always emits 24 kHz mono. The output device is rarely 24 kHz, so
/// playback must resample — see `audio::playback::play_pcm`.
pub const SAMPLE_RATE: u32 = 24_000;

const REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
pub const DEFAULT_VOICE: &str = "af_heart";

/// Style vectors are 256 floats per frame, one frame per possible token count.
const STYLE_DIM: usize = 256;

/// The loaded session, kept for the process lifetime once built.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn kokoro_dir() -> PathBuf {
    crate::config::data_dir().join("models").join("kokoro")
}

/// `true` once the model file is on disk (no download needed to speak).
pub fn is_downloaded(variant: &str) -> bool {
    kokoro_dir().join(format!("{variant}.onnx")).exists()
}

/// Fetch one file from the Kokoro repo into `models/kokoro/`, unless present.
///
/// Mirrors `transcribe::parakeet`'s downloader, including the timeout: hf-hub
/// has no request deadline of its own, so a dead socket would otherwise wedge
/// the calling thread forever.
fn ensure_file(remote: &str, local: &str) -> Result<PathBuf> {
    let dir = kokoro_dir();
    let dest = dir.join(local);
    if dest.exists() {
        return Ok(dest);
    }
    std::fs::create_dir_all(&dir)?;
    info!("Kokoro: downloading {remote}...");

    let remote_owned = remote.to_string();
    let cached = crate::util::run_with_timeout(
        crate::transcribe::DOWNLOAD_TIMEOUT,
        move || -> Result<PathBuf> {
            let api = hf_hub::api::sync::Api::new()?;
            api.model(REPO.to_string())
                .get(&remote_owned)
                .with_context(|| format!("Failed to download {remote_owned}"))
        },
    )
    .ok_or_else(|| anyhow::anyhow!("Download of {remote} timed out"))??;

    std::fs::copy(&cached, &dest).with_context(|| format!("Failed to copy {remote}"))?;
    info!("Kokoro: {local} ready");
    Ok(dest)
}

/// A voice's style vectors: one 256-float row per token count.
///
/// The `.bin` files are a bare little-endian f32 dump of `[frames, 256]` — no
/// header, so the frame count is just the file length.
fn load_voice(voice: &str) -> Result<Vec<Vec<f32>>> {
    if !voice
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        // The name goes into a URL and a path; keep it boring.
        bail!("Invalid voice name '{voice}'");
    }
    let path = ensure_file(&format!("voices/{voice}.bin"), &format!("{voice}.bin"))?;
    let bytes = std::fs::read(&path)?;

    let row = STYLE_DIM * 4;
    if bytes.is_empty() || bytes.len() % row != 0 {
        bail!(
            "Voice '{voice}' is malformed ({} bytes, not a multiple of {row})",
            bytes.len()
        );
    }
    Ok(bytes
        .chunks_exact(row)
        .map(|frame| {
            frame
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()
        })
        .collect())
}

/// Build the session once. CPU only, deliberately: the default `model_q8f16`
/// variant cannot execute on any CoreML configuration, so asking for it would
/// just cost a failed probe on every launch. An 82M graph synthesises a
/// sentence in well under a second on CPU.
fn with_session<T>(variant: &str, f: impl FnOnce(&mut Session) -> Result<T>) -> Result<T> {
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let model = ensure_file(&format!("onnx/{variant}.onnx"), &format!("{variant}.onnx"))?;
        let t = std::time::Instant::now();
        let session = Session::builder()?
            .commit_from_file(&model)
            .with_context(|| format!("Failed to load {}", model.display()))?;
        info!("Kokoro model loaded ({:.1}s)", t.elapsed().as_secs_f64());
        *guard = Some(session);
    }
    f(guard.as_mut().expect("just populated"))
}

/// A validated, phonemized utterance ready to render.
///
/// Splitting preparation from rendering is what lets the MCP tool return
/// immediately *without* swallowing the errors worth reporting: everything a
/// caller can actually fix — unknown voice, missing espeak-ng, empty text — is
/// caught in [`prepare`], which is milliseconds. Only model loading and
/// inference happen later, in the background.
#[derive(Debug, Clone)]
pub struct Speech {
    /// Kokoro's ALBERT encoder has a 512-token context. Each chunk contains at
    /// most 510 phoneme tokens plus the two boundary tokens added by the
    /// tokenizer.
    token_chunks: Vec<Vec<i64>>,
    voice: String,
    /// Kept for logging only.
    chars: usize,
}

/// Validate and phonemize. Fast, and the only step that can fail for a reason
/// the caller could act on.
pub fn prepare(text: &str, voice: &str) -> Result<Speech> {
    let text = text.trim();
    if text.is_empty() {
        bail!("Nothing to speak");
    }
    let phonemes = g2p::phonemize(text, voice)?;
    let token_chunks = tokenize_phonemes(&phonemes);
    if token_chunks.is_empty() {
        bail!("Text produced no usable phonemes");
    }
    // Surface a bad voice name now rather than from a background thread.
    let _ = load_voice(voice)?;
    Ok(Speech {
        token_chunks,
        voice: voice.to_string(),
        chars: text.len(),
    })
}

/// Split before tokenization so each ONNX call stays within Kokoro's
/// 512-position encoder limit. The two boundary tokens are added afterwards.
fn tokenize_phonemes(phonemes: &str) -> Vec<Vec<i64>> {
    kokoro_en::chunk_phonemes(phonemes, kokoro_en::MAX_PHONEME_CHARS)
        .into_iter()
        .map(|chunk| kokoro_en::get_token_ids(&chunk, false))
        // A two-token result contains only the start/end padding.
        .filter(|tokens| tokens.len() > 2)
        .collect()
}

/// Convenience for callers that want it all in one go (the `say` CLI).
pub fn synth(text: &str, voice: &str, variant: &str) -> Result<Vec<f32>> {
    render(&prepare(text, voice)?, variant)
}

/// Run the model. Slow: a cold session load plus inference.
pub fn render(speech: &Speech, variant: &str) -> Result<Vec<f32>> {
    let Speech {
        token_chunks,
        voice,
        chars,
    } = speech;
    let (token_chunks, voice) = (token_chunks.clone(), voice.as_str());

    let pack = load_voice(voice)?;
    let t = std::time::Instant::now();
    let audio = with_session(variant, |session| {
        let mut audio = Vec::new();
        for tokens in token_chunks {
            // The style row is chosen by token count: longer utterances get a
            // different prosody vector. Clamp for malformed/short voice packs.
            let idx = (tokens.len() - 1).min(pack.len().saturating_sub(1));
            let style = pack
                .get(idx)
                .ok_or_else(|| anyhow::anyhow!("Voice '{voice}' has no style vectors"))?
                .clone();

            let n = tokens.len();
            let outputs = session.run(ort::inputs![
                "input_ids" => Tensor::from_array(([1_usize, n], tokens))?,
                "style" => Tensor::from_array(([1_usize, style.len()], style))?,
                "speed" => Tensor::from_array(([1_usize], vec![1.0_f32]))?,
            ])?;
            let (_, wav) = outputs["waveform"].try_extract_tensor::<f32>()?;
            audio.extend_from_slice(wav);
        }
        Ok(audio)
    })?;

    info!(
        "Kokoro: {chars} chars → {:.1}s of audio in {:.2}s ({voice})",
        audio.len() as f32 / SAMPLE_RATE as f32,
        t.elapsed().as_secs_f64()
    );
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_voice_names_that_could_escape_the_path() {
        for bad in ["../../etc/passwd", "af heart", "af/heart", "af;rm"] {
            assert!(load_voice(bad).is_err(), "{bad} should be rejected");
        }
    }

    #[test]
    fn empty_text_is_an_error_not_a_click() {
        assert!(synth("", "af_heart", "model_q8f16").is_err());
        assert!(synth("   ", "af_heart", "model_q8f16").is_err());
    }

    #[test]
    fn unknown_voice_fails_before_any_download() {
        // phonemizer_for rejects it, so we never reach the network.
        assert!(synth("hello", "qq_nobody", "model_q8f16").is_err());
    }

    #[test]
    fn long_phoneme_sequences_are_chunked_below_the_model_limit() {
        let phonemes = std::iter::repeat_n("həlˈoʊ", 120)
            .collect::<Vec<_>>()
            .join(" ");
        let chunks = tokenize_phonemes(&phonemes);

        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|tokens| tokens.len() <= kokoro_en::MAX_PHONEME_CHARS + 2)
        );
    }
}
