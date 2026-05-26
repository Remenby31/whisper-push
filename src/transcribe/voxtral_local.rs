//! Voxtral Mini 4B Realtime — local GPU transcription via Burn + WGPU.
//! Uses Q4 GGUF quantized weights (~2.5GB).

#[cfg(feature = "voxtral")]
mod inner {
    use anyhow::{bail, Context, Result};
    use burn::backend::Wgpu;
    use burn::tensor::Tensor;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use tracing::info;

    use voxtral_mini_realtime::audio::{
        pad::{pad_audio, PadConfig},
        mel::{MelConfig, MelSpectrogram},
        AudioBuffer,
    };
    use voxtral_mini_realtime::models::time_embedding::TimeEmbedding;
    use voxtral_mini_realtime::tokenizer::VoxtralTokenizer;
    use voxtral_mini_realtime::gguf::{loader::Q4ModelLoader, model::Q4VoxtralModel};

    type Backend = Wgpu;

    struct VoxtralState {
        model: Q4VoxtralModel,
        tokenizer: VoxtralTokenizer,
        mel_extractor: MelSpectrogram,
        pad_config: PadConfig,
        t_embed: Tensor<Backend, 3>,
        __loaded_thread: std::thread::ThreadId,
    }

    static VOXTRAL: Mutex<Option<VoxtralState>> = Mutex::new(None);

    pub fn load_model(model_dir: &str) -> Result<()> {
        let device = Default::default();
        let model_path = PathBuf::from(model_dir).join("voxtral-q4.gguf");
        let tokenizer_path = PathBuf::from(model_dir).join("tekken.json");

        if !model_path.exists() {
            bail!(
                "Voxtral GGUF not found at {}.\n\
                 Download: hf download TrevorJS/voxtral-mini-realtime-gguf --local-dir {}",
                model_path.display(), model_dir
            );
        }
        if !tokenizer_path.exists() {
            bail!(
                "Tokenizer not found at {}.\n\
                 Download: hf download TrevorJS/voxtral-mini-realtime-gguf --local-dir {}",
                tokenizer_path.display(), model_dir
            );
        }

        info!("Loading Voxtral Q4 GGUF from {}...", model_path.display());
        let mut loader = Q4ModelLoader::from_file(&model_path).context("Failed to open GGUF")?;
        let model = loader.load(&device).context("Failed to load Q4 model")?;

        info!("Loading tokenizer from {}...", tokenizer_path.display());
        let tokenizer = VoxtralTokenizer::from_file(&tokenizer_path).context("Failed to load tokenizer")?;

        let mel_extractor = MelSpectrogram::new(MelConfig::voxtral());
        let pad_config = PadConfig::voxtral();
        let time_embed = TimeEmbedding::new(3072);
        let t_embed = time_embed.embed::<Backend>(6.0, &device);

        *VOXTRAL.lock().unwrap_or_else(|e| e.into_inner()) = Some(VoxtralState {
            model, tokenizer, mel_extractor, pad_config, t_embed,
            __loaded_thread: std::thread::current().id(),
        });

        info!("Voxtral Q4 model loaded — warming up GPU shaders...");

        // Warmup: run a tiny inference to compile all Metal/WGPU shaders now,
        // not on the first real transcription. Uses 1 second of silence.
        {
            let guard = VOXTRAL.lock().unwrap_or_else(|e| e.into_inner());
            let state = guard.as_ref().unwrap();
            let silence = vec![0.0f32; 16000]; // 1 second of silence
            let audio_buf = AudioBuffer::new(silence, 16000);
            let padded = pad_audio(&audio_buf, &state.pad_config);
            let mel = state.mel_extractor.compute_log(&padded.samples);
            let n_frames = mel.len();
            let n_mels = if n_frames > 0 { mel[0].len() } else { 0 };
            if n_frames > 0 {
                let mut mel_t = vec![vec![0.0f32; n_frames]; n_mels];
                for (fi, frame) in mel.iter().enumerate() {
                    for (mi, &val) in frame.iter().enumerate() {
                        mel_t[mi][fi] = val;
                    }
                }
                let flat: Vec<f32> = mel_t.into_iter().flatten().collect();
                let mel_tensor = burn::tensor::Tensor::from_data(
                    burn::tensor::TensorData::new(flat, [1, n_mels, n_frames]),
                    &device,
                );
                let _ = state.model.transcribe_streaming(mel_tensor, state.t_embed.clone());
            }
        }

        info!("Voxtral Q4 model ready (GPU shaders compiled)");
        Ok(())
    }

    pub fn is_loaded() -> bool {
        VOXTRAL.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn unload_model() {
        *VOXTRAL.lock().unwrap_or_else(|e| e.into_inner()) = None;
        info!("Voxtral model unloaded");
    }

    pub fn transcribe(audio: &[f32]) -> Result<String> {
        let guard = VOXTRAL.lock().unwrap_or_else(|e| e.into_inner());
        let state = guard.as_ref().ok_or_else(|| anyhow::anyhow!("Voxtral model not loaded"))?;

        let device = Default::default();
        let audio_buf = AudioBuffer::new(audio.to_vec(), 16000);

        let padded = pad_audio(&audio_buf, &state.pad_config);
        let mel = state.mel_extractor.compute_log(&padded.samples);
        let n_frames = mel.len();
        let n_mels = if n_frames > 0 { mel[0].len() } else { 0 };
        if n_frames == 0 {
            return Ok(String::new());
        }

        let mut mel_transposed = vec![vec![0.0f32; n_frames]; n_mels];
        for (frame_idx, frame) in mel.iter().enumerate() {
            for (mel_idx, &val) in frame.iter().enumerate() {
                mel_transposed[mel_idx][frame_idx] = val;
            }
        }
        let mel_flat: Vec<f32> = mel_transposed.into_iter().flatten().collect();
        let mel_tensor = Tensor::from_data(
            burn::tensor::TensorData::new(mel_flat, [1, n_mels, n_frames]),
            &device,
        );

        let generated = state.model.transcribe_streaming(mel_tensor, state.t_embed.clone());

        let text_tokens: Vec<u32> = generated.iter()
            .filter(|&&t| t >= 1000)
            .map(|&t| t as u32)
            .collect();

        let text = state.tokenizer.decode(&text_tokens).context("Failed to decode tokens")?;
        info!("Voxtral: '{}'", text.trim());
        Ok(text.trim().to_string())
    }

    /// Streaming transcription API.
    pub mod streaming {
        use anyhow::{Context, Result};
        use tracing::info;
        use voxtral_mini_realtime::audio::mel::{MelConfig, MelSpectrogram};
        use voxtral_mini_realtime::audio::pad::{pad_audio, PadConfig};
        use voxtral_mini_realtime::audio::AudioBuffer;
        use voxtral_mini_realtime::gguf::model::Q4StreamingSession;
        use voxtral_mini_realtime::tokenizer::VoxtralTokenizer;

        use super::VOXTRAL;

        /// Streaming session wrapper.
        /// Accumulates all audio and re-encodes from scratch each chunk
        /// (encoder is fast), using decoder KV cache for incremental decode.
        pub struct StreamingSession {
            mel_extractor: MelSpectrogram,
            pad_config: PadConfig,
            all_audio: Vec<f32>,           // accumulated audio samples
            last_decoded_positions: usize, // how many decoder positions we've already decoded
            decoder_cache_tokens: Vec<i32>, // all tokens generated so far
            prefill_done: bool,
        }

        /// Start a new streaming session.
        pub fn start() -> Result<StreamingSession> {
            Ok(StreamingSession {
                mel_extractor: MelSpectrogram::new(MelConfig::voxtral()),
                pad_config: PadConfig::voxtral(),
                all_audio: Vec::new(),
                last_decoded_positions: 0,
                decoder_cache_tokens: Vec::new(),
                prefill_done: false,
            })
        }

        /// Feed an audio chunk (16kHz mono f32) and return newly transcribed words.
        /// Uses "accumulate + full re-encode" approach: audio is accumulated,
        /// mel + encoder run on the full audio each time, but we only decode
        /// the NEW positions (the ones we haven't decoded yet).
        pub fn feed_chunk(session: &mut StreamingSession, audio: &[f32]) -> Result<Vec<String>> {
            session.all_audio.extend_from_slice(audio);

            // Need at least 1 second of audio before attempting transcription
            if session.all_audio.len() < 16000 {
                return Ok(Vec::new());
            }

            // Run full batch transcription on all accumulated audio
            let guard = VOXTRAL.lock().unwrap_or_else(|e| e.into_inner());
            let state = guard.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Model not loaded"))?;

            // Compute mel on all audio
            let audio_buf = AudioBuffer::new(session.all_audio.clone(), 16000);
            let padded = pad_audio(&audio_buf, &session.pad_config);
            let mel = session.mel_extractor.compute_log(&padded.samples);
            let n_frames = mel.len();
            let n_mels = if n_frames > 0 { mel[0].len() } else { 0 };
            if n_frames == 0 { return Ok(Vec::new()); }

            let mut mel_t = vec![vec![0.0f32; n_frames]; n_mels];
            for (fi, frame) in mel.iter().enumerate() {
                for (mi, &val) in frame.iter().enumerate() {
                    mel_t[mi][fi] = val;
                }
            }
            let mel_flat: Vec<f32> = mel_t.into_iter().flatten().collect();
            let device: burn::backend::wgpu::WgpuDevice = Default::default();
            let mel_tensor = burn::tensor::Tensor::from_data(
                burn::tensor::TensorData::new(mel_flat, [1, n_mels, n_frames]),
                &device,
            );

            // Full encode (fast, ~50ms for 10s audio on M4 Pro Metal)
            let audio_embeds = state.model.encode_audio(mel_tensor);
            let [_, seq_len, _] = audio_embeds.dims();

            // How many NEW positions to decode?
            let new_positions = seq_len.saturating_sub(session.last_decoded_positions);
            if new_positions == 0 {
                return Ok(Vec::new());
            }

            // Run transcribe_streaming on the full audio (re-does prefill+decode)
            // but we only extract the NEW tokens at the end
            let all_tokens = state.model.transcribe_streaming(
                // Re-create mel tensor for transcribe_streaming
                {
                    let mel2 = session.mel_extractor.compute_log(&padded.samples);
                    let mut mt = vec![vec![0.0f32; n_frames]; n_mels];
                    for (fi, frame) in mel2.iter().enumerate() {
                        for (mi, &val) in frame.iter().enumerate() {
                            mt[mi][fi] = val;
                        }
                    }
                    let flat: Vec<f32> = mt.into_iter().flatten().collect();
                    burn::tensor::Tensor::from_data(
                        burn::tensor::TensorData::new(flat, [1, n_mels, n_frames]),
                        &device,
                    )
                },
                state.t_embed.clone(),
            );

            // Extract only the NEW tokens (skip the ones we already reported)
            let prev_count = session.decoder_cache_tokens.len();
            session.decoder_cache_tokens = all_tokens.clone();
            session.last_decoded_positions = seq_len;

            let new_tokens: Vec<i32> = all_tokens.into_iter().skip(prev_count).collect();

            // Decode text tokens (>= 1000)
            let text_tokens: Vec<u32> = new_tokens.iter()
                .filter(|&&t| t >= 1000)
                .map(|&t| t as u32)
                .collect();

            if text_tokens.is_empty() {
                return Ok(Vec::new());
            }

            let text = state.tokenizer.decode(&text_tokens)
                .context("Failed to decode tokens")?;
            let words: Vec<String> = text.split_whitespace()
                .map(|w| w.to_string())
                .collect();

            if !words.is_empty() {
                info!("Streaming: +{} words: {:?}", words.len(), words);
            }

            Ok(words)
        }

        /// Finish streaming and return the complete transcription.
        pub fn finish(session: StreamingSession) -> Result<String> {
            let text_tokens: Vec<u32> = session.decoder_cache_tokens.iter()
                .filter(|&&t| t >= 1000)
                .map(|&t| t as u32)
                .collect();

            let guard = VOXTRAL.lock().unwrap_or_else(|e| e.into_inner());
            let state = guard.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Model unloaded"))?;

            let text = state.tokenizer.decode(&text_tokens)
                .context("Failed to decode final tokens")?;

            info!("Streaming finished: '{}'", text.trim());
            Ok(text.trim().to_string())
        }
    }
}

// Public API: delegates to inner when feature is enabled
#[cfg(feature = "voxtral")]
pub use inner::{load_model, is_loaded, transcribe};
#[cfg(feature = "voxtral")]
pub use inner::streaming;

#[cfg(not(feature = "voxtral"))]
pub mod streaming {
    pub struct StreamingSession;
    pub fn start() -> anyhow::Result<StreamingSession> {
        anyhow::bail!("Voxtral not compiled")
    }
    pub fn feed_chunk(_session: &mut StreamingSession, _audio: &[f32]) -> anyhow::Result<Vec<String>> {
        anyhow::bail!("Voxtral not compiled")
    }
    pub fn finish(_session: StreamingSession) -> anyhow::Result<String> {
        anyhow::bail!("Voxtral not compiled")
    }
}

#[cfg(not(feature = "voxtral"))]
pub fn load_model(_model_dir: &str) -> anyhow::Result<()> {
    anyhow::bail!("Voxtral not compiled. Build with --features voxtral")
}
#[cfg(not(feature = "voxtral"))]
pub fn is_loaded() -> bool { false }
#[cfg(not(feature = "voxtral"))]
pub fn unload_model() {}
#[cfg(not(feature = "voxtral"))]
pub fn transcribe(_audio: &[f32]) -> anyhow::Result<String> {
    anyhow::bail!("Voxtral not compiled. Build with --features voxtral")
}
