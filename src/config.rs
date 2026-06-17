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
    /// Check for updates on startup
    pub check_updates: bool,
    /// Adaptive dictation: post-correct transcriptions from the learned
    /// dictionary and learn from user corrections. Persistent + cross-model.
    pub dictionary_enabled: bool,
    /// Opt-in: when learning a proper noun, check its canonical spelling online
    /// (Wikipedia). Cold-path only, never per-dictation. Default off (the app is
    /// otherwise 100% local).
    pub online_enrichment: bool,
    /// Show the floating "listening" pill (live mic waveform) while recording.
    pub overlay_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "ctrl".into(),
            hotkey_mode: "hold".into(),
            hold_delay: 0.15,
            language: "auto".into(),
            model: "ggml-large-v3-turbo-q5_0.bin".into(),
            notifications: true,
            sound_feedback: true,
            input_device: "auto".into(),
            output_device: "auto".into(),
            debug: false,
            auto_start: false,
            check_updates: true,
            dictionary_enabled: true,
            online_enrichment: false,
            overlay_enabled: true,
        }
    }
}

impl Config {
    /// Load config from the platform-default path.
    ///
    /// A corrupt file (e.g. a crash during a non-atomic write by an older build)
    /// must never brick startup: back it up to `.bad` and fall back to defaults,
    /// rather than propagating the parse error and refusing to boot.
    pub fn load() -> Result<Self> {
        let path = config_path();
        if !path.exists() {
            let cfg = Self::default();
            cfg.save()?;
            return Ok(cfg);
        }
        match Self::load_from(&path) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                tracing::error!("config.toml unreadable ({e}); backing up to .bad, using defaults");
                let _ = std::fs::rename(&path, path.with_extension("toml.bad"));
                let cfg = Self::default();
                let _ = cfg.save();
                Ok(cfg)
            }
        }
    }

    /// Load config from a specific file.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&content)?;
        Ok(cfg)
    }

    /// Save config to the platform-default path. Written atomically (tmp file +
    /// rename) so a crash/power-loss mid-write can't truncate config.toml — this
    /// is the most-written user file (every tray toggle).
    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
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

// ─── Model paths (single source of truth — was re-derived in ~8 places) ──────

/// Directory holding downloaded models.
pub fn models_dir() -> PathBuf {
    data_dir().join("models")
}

/// Whisper `.bin` model path for the given filename.
pub fn whisper_model_path(filename: &str) -> PathBuf {
    models_dir().join(filename)
}

/// Parakeet ONNX model directory.
pub fn parakeet_dir() -> PathBuf {
    models_dir().join("parakeet")
}

/// Voxtral model directory.
pub fn voxtral_dir() -> PathBuf {
    models_dir().join("voxtral")
}

/// Log directory inside the data dir.
pub fn log_dir() -> PathBuf {
    data_dir().join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = Config::default();
        assert_eq!(cfg.hotkey, "ctrl");
        assert_eq!(cfg.hotkey_mode, "hold");
        assert_eq!(cfg.hold_delay, 0.15);
        assert_eq!(cfg.language, "auto");
        assert_eq!(cfg.model, "ggml-large-v3-turbo-q5_0.bin");
        assert!(cfg.notifications);
        assert!(cfg.sound_feedback);
        assert_eq!(cfg.input_device, "auto");
        assert_eq!(cfg.output_device, "auto");
        assert!(!cfg.debug);
        assert!(!cfg.auto_start);
    }

    #[test]
    fn test_config_roundtrip() {
        let cfg = Config::default();
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(cfg.hotkey, deserialized.hotkey);
        assert_eq!(cfg.model, deserialized.model);
        assert_eq!(cfg.language, deserialized.language);
    }

    #[test]
    fn test_config_load_missing_fields() {
        let partial = r#"
            hotkey = "rctrl"
            language = "fr"
        "#;
        let cfg: Config = toml::from_str(partial).unwrap();
        assert_eq!(cfg.hotkey, "rctrl");
        assert_eq!(cfg.language, "fr");
        // Defaults for missing fields
        assert_eq!(cfg.hotkey_mode, "hold");
        assert_eq!(cfg.model, "ggml-large-v3-turbo-q5_0.bin");
        assert!(cfg.notifications);
    }

    #[test]
    fn test_config_ignores_unknown_fields() {
        let with_unknown = r#"
            hotkey = "ctrl"
            backend = "whisper"
            some_future_field = true
        "#;
        // Should not panic — serde(default) + deny_unknown_fields not set
        let cfg: Config = toml::from_str(with_unknown).unwrap();
        assert_eq!(cfg.hotkey, "ctrl");
    }

    #[test]
    fn test_config_load_save_roundtrip() {
        let dir = std::env::temp_dir().join("whisper_push_test_config");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_config.toml");

        let mut cfg = Config::default();
        cfg.language = "fr".into();
        cfg.hotkey = "rctrl".into();
        let content = toml::to_string_pretty(&cfg).unwrap();
        std::fs::write(&path, &content).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.language, "fr");
        assert_eq!(loaded.hotkey, "rctrl");
        assert_eq!(loaded.model, "ggml-large-v3-turbo-q5_0.bin");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
