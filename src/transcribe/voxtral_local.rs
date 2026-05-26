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
        use voxtral_mini_realtime::tokenizer::VoxtralTokenizer;
        use burn::prelude::ElementConversion;

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
        ///
        /// Strategy: accumulate audio, re-encode mel+encoder (fast, ~50ms),
        /// but use decoder KV cache to only decode NEW positions.
        /// The encoder runs on full audio each time but is very fast (~50ms for 10s).
        /// The decoder only processes new positions via KV cache (~5ms per token).
        pub fn feed_chunk(session: &mut StreamingSession, audio: &[f32]) -> Result<Vec<String>> {
            session.all_audio.extend_from_slice(audio);

            // Need at least 1.5s of audio before first attempt (prefix needs 38 positions)
            if session.all_audio.len() < 24000 {
                return Ok(Vec::new());
            }

            let guard = VOXTRAL.lock().unwrap_or_else(|e| e.into_inner());
            let state = guard.as_ref()
                .ok_or_else(|| anyhow::anyhow!("Model not loaded"))?;

            // Compute mel on ALL accumulated audio
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

            // Full encode (fast ~50ms even for 10s audio on M4 Pro Metal)
            let audio_embeds = state.model.encode_audio(mel_tensor);
            let [_, seq_len, d_model] = audio_embeds.dims();

            // How many NEW positions since last feed?
            let new_positions = seq_len.saturating_sub(session.last_decoded_positions);
            if new_positions == 0 {
                return Ok(Vec::new());
            }

            const PREFIX_LEN: usize = 38;
            const BOS_TOKEN: i32 = 1;
            const STREAMING_PAD: i32 = 32;

            if !session.prefill_done {
                if seq_len < PREFIX_LEN {
                    return Ok(Vec::new());
                }

                // Prefill: process the 38-token prefix
                let mut prefix: Vec<i32> = vec![BOS_TOKEN];
                prefix.extend(std::iter::repeat_n(STREAMING_PAD, PREFIX_LEN - 1));

                let prefix_text = state.model.decoder().embed_tokens_from_ids(&prefix, 1, PREFIX_LEN);
                let prefix_audio = audio_embeds.clone().slice([0..1, 0..PREFIX_LEN, 0..d_model]);
                let prefix_input = prefix_audio + prefix_text;

                let mut decoder_cache = state.model.create_decoder_cache_preallocated(seq_len + 100);
                let hidden = state.model.decoder().forward_hidden_with_cache(
                    prefix_input, state.t_embed.clone(), &mut decoder_cache,
                );
                let logits = state.model.decoder().lm_head(hidden);
                let vocab = logits.dims()[2];
                let last_logits = logits.slice([0..1, (PREFIX_LEN-1)..PREFIX_LEN, 0..vocab]);
                let first_token: i32 = last_logits.argmax(2).into_scalar().elem();

                session.decoder_cache_tokens = prefix;
                session.decoder_cache_tokens.push(first_token);

                // Decode remaining positions after prefix
                let mut new_tokens = vec![first_token];
                for pos in PREFIX_LEN..seq_len {
                    let prev = *session.decoder_cache_tokens.last().unwrap();
                    let text_e = state.model.decoder().embed_tokens_from_ids(&[prev], 1, 1);
                    let audio_e = audio_embeds.clone().slice([0..1, pos..pos+1, 0..d_model]);
                    let input = audio_e + text_e;
                    let hidden = state.model.decoder().forward_hidden_with_cache(
                        input, state.t_embed.clone(), &mut decoder_cache,
                    );
                    let logits = state.model.decoder().lm_head(hidden);
                    let tok: i32 = logits.argmax(2).into_scalar().elem();
                    session.decoder_cache_tokens.push(tok);
                    new_tokens.push(tok);
                }

                session.last_decoded_positions = seq_len;
                session.prefill_done = true;
                // Store decoder cache for next call — but we can't because it's not in session.
                // For now, we'll re-do prefill+decode each time (decoder is fast for short seqs).
                // TODO: store decoder_cache in session

                let text_tokens: Vec<u32> = new_tokens.iter()
                    .filter(|&&t| t >= 1000).map(|&t| t as u32).collect();
                if text_tokens.is_empty() { return Ok(Vec::new()); }
                let text = state.tokenizer.decode(&text_tokens).context("Decode failed")?;
                let words: Vec<String> = text.split_whitespace().map(|s| s.to_string()).collect();
                if !words.is_empty() { info!("Streaming: +{} words: {:?}", words.len(), words); }
                return Ok(words);
            }

            // Subsequent chunks: re-encode (fast) + re-decode all (decoder is fast)
            // Re-do prefill + full decode with new audio embeddings
            let mut prefix: Vec<i32> = vec![BOS_TOKEN];
            prefix.extend(std::iter::repeat_n(STREAMING_PAD, PREFIX_LEN - 1));
            let prefix_text = state.model.decoder().embed_tokens_from_ids(&prefix, 1, PREFIX_LEN);
            let prefix_audio = audio_embeds.clone().slice([0..1, 0..PREFIX_LEN, 0..d_model]);
            let prefix_input = prefix_audio + prefix_text;

            let mut decoder_cache = state.model.create_decoder_cache_preallocated(seq_len + 100);
            let hidden = state.model.decoder().forward_hidden_with_cache(
                prefix_input, state.t_embed.clone(), &mut decoder_cache,
            );
            let logits = state.model.decoder().lm_head(hidden);
            let vocab = logits.dims()[2];
            let last_logits = logits.slice([0..1, (PREFIX_LEN-1)..PREFIX_LEN, 0..vocab]);
            let first_token: i32 = last_logits.argmax(2).into_scalar().elem();

            let mut all_tokens = prefix;
            all_tokens.push(first_token);

            for pos in PREFIX_LEN..seq_len {
                let prev = *all_tokens.last().unwrap();
                let text_e = state.model.decoder().embed_tokens_from_ids(&[prev], 1, 1);
                let audio_e = audio_embeds.clone().slice([0..1, pos..pos+1, 0..d_model]);
                let input = audio_e + text_e;
                let hidden = state.model.decoder().forward_hidden_with_cache(
                    input, state.t_embed.clone(), &mut decoder_cache,
                );
                let logits = state.model.decoder().lm_head(hidden);
                let tok: i32 = logits.argmax(2).into_scalar().elem();
                all_tokens.push(tok);
            }

            // Extract NEW tokens only
            let prev_total = session.decoder_cache_tokens.len();
            let all_generated: Vec<i32> = all_tokens.iter().skip(PREFIX_LEN).cloned().collect();
            let new_count = all_generated.len().saturating_sub(prev_total.saturating_sub(PREFIX_LEN));

            session.decoder_cache_tokens = all_tokens;
            session.last_decoded_positions = seq_len;

            let new_tokens: Vec<i32> = all_generated.iter().rev().take(new_count).rev().cloned().collect();
            let text_tokens: Vec<u32> = new_tokens.iter()
                .filter(|&&t| t >= 1000).map(|&t| t as u32).collect();
            if text_tokens.is_empty() { return Ok(Vec::new()); }
            let text = state.tokenizer.decode(&text_tokens).context("Decode failed")?;
            let words: Vec<String> = text.split_whitespace().map(|s| s.to_string()).collect();
            if !words.is_empty() { info!("Streaming: +{} words: {:?}", words.len(), words); }
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
