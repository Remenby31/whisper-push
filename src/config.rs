use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global hotkey (e.g. "ctrl", "rctrl", "cmd+shift+space")
    pub hotkey: String,
    /// "hold" (push-to-talk) or "toggle" (press to start/stop)
    pub hotkey_mode: String,
    /// Seconds to wait before committing a hold (filters quick taps)
    pub hold_delay: f64,
    /// Language: "auto" or ISO code ("fr", "en", "de", ...)
    pub language: String,
    /// Whisper model name (used for HuggingFace download)
    pub model: String,
    /// Transcription backend: "parakeet", "whisper", or "voxtral-local"
    pub backend: String,
    /// Show OS notifications
    pub notifications: bool,
    /// Play start/stop sounds
    pub sound_feedback: bool,
    /// Input audio device: "auto" or exact name
    pub input_device: String,
    /// Output audio device (for sounds): "auto" or exact name
    pub output_device: String,
    /// Verbose logging
    pub debug: bool,
    /// Auto-start on login
    pub auto_start: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "ctrl".into(),
            hotkey_mode: "hold".into(),
            hold_delay: 0.15,
            language: "auto".into(),
            model: "ggml-large-v3-turbo-q5_0.bin".into(),
            backend: "whisper".into(),
            notifications: true,
            sound_feedback: true,
            input_device: "auto".into(),
            output_device: "auto".into(),
            debug: false,
            auto_start: false,
        }
    }
}

impl Config {
    /// Load config from the platform-default path.
    pub fn load() -> Result<Self> {
        let path = config_path();
        if path.exists() {
            Self::load_from(&path)
        } else {
            let cfg = Self::default();
            cfg.save()?;
            Ok(cfg)
        }
    }

    /// Load config from a specific file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&content)?;
        Ok(cfg)
    }

    /// Save config to the platform-default path.
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }
}

/// Platform-specific config file path.
pub fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-push");
    dir.join("config.toml")
}

/// Platform-specific data directory (models, logs).
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-push")
}

/// Platform-specific cache directory (temporary audio).
#[allow(dead_code)]
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("whisper-push")
}
