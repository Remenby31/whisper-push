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

        *VOXTRAL.lock().unwrap() = Some(VoxtralState {
            model, tokenizer, mel_extractor, pad_config, t_embed,
        });

        info!("Voxtral Q4 model loaded and ready");
        Ok(())
    }

    pub fn is_loaded() -> bool {
        VOXTRAL.lock().unwrap().is_some()
    }

    pub fn unload_model() {
        *VOXTRAL.lock().unwrap() = None;
        info!("Voxtral model unloaded");
    }

    pub fn transcribe(audio: &[f32]) -> Result<String> {
        let guard = VOXTRAL.lock().unwrap();
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
}

// Public API: delegates to inner when feature is enabled
#[cfg(feature = "voxtral")]
pub use inner::{load_model, unload_model, is_loaded, transcribe};

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
