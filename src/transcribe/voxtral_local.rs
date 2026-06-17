//! Voxtral Mini 4B Realtime — local GPU transcription via Burn + WGPU.
//! Uses Q4 GGUF quantized weights (~2.5GB).

#[cfg(feature = "voxtral")]
mod inner {
    use crate::util::LockSafe;
    use anyhow::{Context, Result, bail};
    use burn::backend::Wgpu;
    use burn::tensor::Tensor;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tracing::info;

    use voxtral_mini_realtime::audio::{
        AudioBuffer,
        mel::{MelConfig, MelSpectrogram},
        pad::{PadConfig, pad_audio},
    };
    use voxtral_mini_realtime::gguf::{loader::Q4ModelLoader, model::Q4VoxtralModel};
    use voxtral_mini_realtime::models::time_embedding::TimeEmbedding;
    use voxtral_mini_realtime::tokenizer::VoxtralTokenizer;

    type Backend = Wgpu;

    struct VoxtralState {
        model: Q4VoxtralModel,
        tokenizer: VoxtralTokenizer,
        mel_extractor: MelSpectrogram,
        pad_config: PadConfig,
        t_embed: Tensor<Backend, 3>,
    }

    static VOXTRAL: Mutex<Option<VoxtralState>> = Mutex::new(None);

    pub fn load_model(model_dir: &str) -> Result<()> {
        // cubecl (burn's GPU layer) stores autotune cache in CWD/target/.
        // When running from a .app bundle, CWD may not be writable.
        // Set CWD to the data dir so the cache lands in a known location.
        let data_dir = crate::config::data_dir();
        let _ = std::fs::create_dir_all(&data_dir);
        let _ = std::env::set_current_dir(&data_dir);

        let device = Default::default();
        let model_path = PathBuf::from(model_dir).join("voxtral-q4.gguf");
        let tokenizer_path = PathBuf::from(model_dir).join("tekken.json");

        if !model_path.exists() {
            bail!("Voxtral GGUF not found at {}", model_path.display());
        }
        if !tokenizer_path.exists() {
            bail!("Tokenizer not found at {}", tokenizer_path.display());
        }

        info!("Loading Voxtral Q4 GGUF from {}...", model_path.display());
        let mut loader = Q4ModelLoader::from_file(&model_path).context("Failed to open GGUF")?;
        let model = loader.load(&device).context("Failed to load Q4 model")?;

        info!("Loading tokenizer from {}...", tokenizer_path.display());
        let tokenizer =
            VoxtralTokenizer::from_file(&tokenizer_path).context("Failed to load tokenizer")?;

        let mel_extractor = MelSpectrogram::new(MelConfig::voxtral());
        let pad_config = PadConfig::voxtral();
        let time_embed = TimeEmbedding::new(3072);
        let t_embed = time_embed.embed::<Backend>(6.0, &device);

        *VOXTRAL.lock_safe() = Some(VoxtralState {
            model,
            tokenizer,
            mel_extractor,
            pad_config,
            t_embed,
        });

        // GPU shader compilation happens lazily on first transcription.
        // Warmup with transcribe_streaming hangs on M4 Pro Metal, so we
        // skip it. The first dictation will be slower (~30-60s) as shaders
        // compile, but subsequent ones are instant.
        info!("Voxtral Q4 model loaded (first dictation will compile GPU shaders)");
        Ok(())
    }

    pub fn is_loaded() -> bool {
        VOXTRAL.lock_safe().is_some()
    }

    #[allow(dead_code)]
    pub fn unload_model() {
        *VOXTRAL.lock_safe() = None;
    }

    pub fn transcribe(audio: &[f32]) -> Result<String> {
        let guard = VOXTRAL.lock_safe();
        let state = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Voxtral not loaded"))?;

        let audio_buf = AudioBuffer::new(audio.to_vec(), 16000);
        let padded = pad_audio(&audio_buf, &state.pad_config);
        let mel = state.mel_extractor.compute_log(&padded.samples);
        let (mel_tensor, _, _) = mel_to_tensor(&mel).ok_or_else(|| anyhow::anyhow!("Empty mel"))?;

        let generated = state
            .model
            .transcribe_streaming(mel_tensor, state.t_embed.clone());
        let text = decode_tokens(&generated, &state.tokenizer)?;
        info!("Voxtral: '{}'", text.trim());
        Ok(text.trim().to_string())
    }

    /// Helper: mel frames → Burn tensor [1, n_mels, n_frames]
    fn mel_to_tensor(mel: &[Vec<f32>]) -> Option<(Tensor<Backend, 3>, usize, usize)> {
        let nf = mel.len();
        if nf == 0 {
            return None;
        }
        let nm = mel[0].len();
        let mut mt = vec![vec![0.0f32; nf]; nm];
        for (fi, frame) in mel.iter().enumerate() {
            for (mi, &v) in frame.iter().enumerate() {
                mt[mi][fi] = v;
            }
        }
        let flat: Vec<f32> = mt.into_iter().flatten().collect();
        let dev: burn::backend::wgpu::WgpuDevice = Default::default();
        Some((
            Tensor::from_data(burn::tensor::TensorData::new(flat, [1, nm, nf]), &dev),
            nm,
            nf,
        ))
    }

    /// Helper: decode token IDs (>= 1000) to text
    fn decode_tokens(tokens: &[i32], tokenizer: &VoxtralTokenizer) -> Result<String> {
        let text_tokens: Vec<u32> = tokens
            .iter()
            .filter(|&&t| t >= 1000)
            .map(|&t| t as u32)
            .collect();
        if text_tokens.is_empty() {
            return Ok(String::new());
        }
        tokenizer.decode(&text_tokens).context("Decode failed")
    }

    /// Streaming transcription API.
    /// Accumulates audio, re-encodes encoder each chunk (~50ms),
    /// persists decoder KV cache to only decode NEW positions (~5ms/token).
    /// Reserved: disabled pending the M4 Metal shader-compile fix (see CLAUDE.md).
    #[allow(dead_code)]
    pub mod streaming {
        use crate::util::LockSafe;
        use anyhow::Result;
        use tracing::info;
        use voxtral_mini_realtime::audio::AudioBuffer;
        use voxtral_mini_realtime::audio::mel::{MelConfig, MelSpectrogram};
        use voxtral_mini_realtime::audio::pad::{PadConfig, pad_audio};

        use super::VOXTRAL;

        const PREFIX_LEN: usize = 38;

        pub struct StreamingSession {
            mel_extractor: MelSpectrogram,
            pad_config: PadConfig,
            all_audio: Vec<f32>,
            generated_tokens: Vec<i32>,
            decoded_positions: usize,
            prefill_done: bool,
        }

        pub fn start() -> Result<StreamingSession> {
            let mut prefix = vec![1i32]; // BOS
            prefix.extend(std::iter::repeat_n(32i32, PREFIX_LEN - 1));
            Ok(StreamingSession {
                mel_extractor: MelSpectrogram::new(MelConfig::voxtral()),
                pad_config: PadConfig::voxtral(),
                all_audio: Vec::new(),
                generated_tokens: prefix,
                decoded_positions: 0,
                prefill_done: false,
            })
        }

        pub fn feed_chunk(session: &mut StreamingSession, audio: &[f32]) -> Result<Vec<String>> {
            session.all_audio.extend_from_slice(audio);

            // Need ~1.5s before first attempt
            if session.all_audio.len() < 24000 && !session.prefill_done {
                return Ok(Vec::new());
            }

            let guard = VOXTRAL.lock_safe();
            let state = guard
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Not loaded"))?;

            // Encode ALL accumulated audio (~50ms on M4 Pro Metal)
            let audio_buf = AudioBuffer::new(session.all_audio.clone(), 16000);
            let padded = pad_audio(&audio_buf, &session.pad_config);
            let mel = session.mel_extractor.compute_log(&padded.samples);
            let (mel_tensor, _, _) =
                super::mel_to_tensor(&mel).ok_or_else(|| anyhow::anyhow!("Empty mel"))?;
            let audio_embeds = state.model.encode_audio(mel_tensor);
            let [_, seq_len, _] = audio_embeds.dims();

            if seq_len <= session.decoded_positions {
                return Ok(Vec::new());
            }

            if seq_len < PREFIX_LEN {
                return Ok(Vec::new());
            }

            // Full re-decode each time (encoder embeddings change when more audio is added).
            // Use transcribe_streaming which handles prefill + decode internally.
            let (mel2, _, _) =
                super::mel_to_tensor(&mel).ok_or_else(|| anyhow::anyhow!("Empty mel"))?;
            let all_tokens = state
                .model
                .transcribe_streaming(mel2, state.t_embed.clone());

            // Extract only NEW tokens (diff with previous run)
            let prev_count = session.decoded_positions;
            let new_tokens: Vec<i32> = all_tokens.iter().skip(prev_count).cloned().collect();
            session.generated_tokens = {
                let mut prefix = vec![1i32];
                prefix.extend(std::iter::repeat_n(32i32, PREFIX_LEN - 1));
                prefix.extend(all_tokens.iter());
                prefix
            };
            session.decoded_positions = all_tokens.len();
            session.prefill_done = true;

            let text = super::decode_tokens(&new_tokens, &state.tokenizer)?;
            if text.trim().is_empty() {
                return Ok(Vec::new());
            }
            let words: Vec<String> = text
                .trim()
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if !words.is_empty() {
                info!("Streaming: +{} words: {:?}", words.len(), words);
            }
            Ok(words)
        }

        pub fn finish(session: StreamingSession) -> Result<String> {
            let guard = VOXTRAL.lock_safe();
            let state = guard.as_ref().ok_or_else(|| anyhow::anyhow!("Unloaded"))?;
            let all: Vec<i32> = session
                .generated_tokens
                .into_iter()
                .skip(PREFIX_LEN)
                .collect();
            let text = super::decode_tokens(&all, &state.tokenizer)?;
            info!("Streaming finished: '{}'", text.trim());
            Ok(text.trim().to_string())
        }
    }
}

// Reserved: streaming dictation is disabled (blocks on M4 Metal shader compile —
// see CLAUDE.md); batch mode is the live path. Kept for when that's fixed.
#[cfg(feature = "voxtral")]
#[allow(unused_imports)]
pub use inner::streaming;
#[cfg(feature = "voxtral")]
pub use inner::{is_loaded, load_model, transcribe, unload_model};

#[cfg(not(feature = "voxtral"))]
#[allow(dead_code)] // reserved: streaming disabled (see above)
pub mod streaming {
    pub struct StreamingSession;
    pub fn start() -> anyhow::Result<StreamingSession> {
        anyhow::bail!("Voxtral not compiled")
    }
    pub fn feed_chunk(_s: &mut StreamingSession, _a: &[f32]) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("Voxtral not compiled")
    }
    pub fn finish(_s: StreamingSession) -> anyhow::Result<String> {
        anyhow::bail!("Voxtral not compiled")
    }
}

#[cfg(not(feature = "voxtral"))]
pub fn load_model(_: &str) -> anyhow::Result<()> {
    anyhow::bail!("Voxtral not compiled")
}
#[cfg(not(feature = "voxtral"))]
pub fn is_loaded() -> bool {
    false
}
#[cfg(not(feature = "voxtral"))]
#[allow(dead_code)]
pub fn unload_model() {}
#[cfg(not(feature = "voxtral"))]
pub fn transcribe(_: &[f32]) -> anyhow::Result<String> {
    anyhow::bail!("Voxtral not compiled")
}
