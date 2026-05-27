mod audio;
mod config;
mod hotkey;
mod autostart;
mod hardware;
mod model_manager;
mod notify;
mod onboarding;
mod paste;
mod permissions;
mod state;
mod transcribe;
mod tray;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "whisper-push", version, about)]
struct Cli {
    /// Override config file path
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Override language (auto, fr, en, de, ...)
    #[arg(short, long)]
    language: Option<String>,

    /// Show current status
    #[arg(short, long)]
    status: bool,

    /// Run dependency/environment checks
    #[arg(long)]
    doctor: bool,

    /// List available models and download status
    #[arg(long)]
    models: bool,

    /// Transcribe an audio file (MP3/WAV/OGG/FLAC) and print the result
    #[arg(long)]
    transcribe: Option<PathBuf>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init logging
    init_logging();

    info!("whisper-push v{}", env!("CARGO_PKG_VERSION"));

    if cli.doctor {
        return doctor::run();
    }

    if cli.models {
        model_manager::print_status();
        return Ok(());
    }

    if let Some(ref audio_path) = cli.transcribe {
        return cli_transcribe::run(audio_path, cli.language.as_deref());
    }

    // Load config
    let mut cfg = if let Some(path) = &cli.config {
        config::Config::load_from(path)?
    } else {
        config::Config::load()?
    };

    if let Some(lang) = &cli.language {
        cfg.language = lang.clone();
    }

    if cli.status {
        println!("{}", state::current_status());
        return Ok(());
    }

    // Run the app (tray mode on macOS/Windows, or daemon on Linux)
    app::run(cfg)
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

mod doctor {
    use anyhow::Result;

    pub fn run() -> Result<()> {
        println!("whisper-push doctor");

        // Hardware detection
        let hw = crate::hardware::detect();
        println!("Platform:  {} {}", hw.os, hw.arch);
        println!("GPU:       {}", hw.gpu.label());
        println!("Recommend: {}", crate::hardware::recommend_backend(&hw));

        // Compiled features
        let mut features = Vec::new();
        if cfg!(feature = "metal") { features.push("metal"); }
        if cfg!(feature = "cuda") { features.push("cuda"); }
        if cfg!(feature = "vulkan") { features.push("vulkan"); }
        if cfg!(feature = "parakeet") { features.push("parakeet"); }
        if cfg!(feature = "voxtral") { features.push("voxtral"); }
        println!("Features:  {}", if features.is_empty() { "none".into() } else { features.join(", ") });

        // Audio devices (with timeout — CoreAudio can hang without NSApp)
        println!("\nAudio devices:");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = crate::audio::list_devices();
            let _ = tx.send(result);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(Ok(devices)) => {
                for (i, name) in devices.iter().enumerate() {
                    println!("  [{i}] {name}");
                }
            }
            Ok(Err(e)) => println!("  Error: {e}"),
            Err(_) => println!("  (timeout — run as app for full device list)"),
        }

        // Model
        let model_path = crate::transcribe::model_path("ggml-large-v3-turbo-q5_0.bin");
        if model_path.exists() {
            println!("\nModel:     ggml-large-v3-turbo-q5_0 (ready)");
        } else {
            println!("\nModel:     not downloaded (will download on first use)");
        }

        // Permissions (macOS)
        #[cfg(target_os = "macos")]
        {
            let trusted = crate::permissions::is_accessibility_trusted();
            println!("\nAccessibility: {}", if trusted { "granted" } else { "NOT granted" });
        }

        println!("\nAll checks complete.");
        Ok(())
    }
}

mod cli_transcribe {
    use anyhow::Result;
    use std::path::Path;
    use std::time::Instant;

    pub fn run(path: &Path, language: Option<&str>) -> Result<()> {
        let lang = language.unwrap_or("auto");

        // Load audio
        println!("Loading {}...", path.display());
        let samples = crate::audio::decode::load_audio_file(path)?;
        println!("Audio: {:.1}s ({} samples @ 16kHz)", samples.len() as f32 / 16000.0, samples.len());

        // Load model
        let cfg = crate::config::Config::load()?;
        println!("Loading model: {}...", cfg.model);
        crate::transcribe::load_model(&cfg.model)?;

        // Transcribe
        println!("Transcribing...");
        let start = Instant::now();
        let backend = crate::model_manager::resolve_backend(&cfg.model);
        let text = crate::transcribe::transcribe_with_backend(&samples, lang, &backend)?;
        let elapsed = start.elapsed();

        println!("\n--- Result ({:.2}s) ---", elapsed.as_secs_f64());
        println!("{text}");

        let audio_duration = samples.len() as f64 / 16000.0;
        let rtf = elapsed.as_secs_f64() / audio_duration;
        println!("\n--- Stats ---");
        println!("Audio:    {:.1}s", audio_duration);
        println!("Compute:  {:.2}s", elapsed.as_secs_f64());
        println!("RTF:      {:.3} ({:.0}x real-time)", rtf, 1.0 / rtf);

        Ok(())
    }
}

mod app {
    use anyhow::Result;
    use crate::config::Config;

    pub fn run(mut cfg: Config) -> Result<()> {
        // Ensure single instance
        let _lock = crate::state::acquire_lock()?;

        // First-launch onboarding
        if crate::onboarding::check_first_launch() {
            let model = crate::onboarding::run();
            cfg.model = model;
        }

        tracing::info!("Starting whisper-push daemon");

        // Permissions are now checked in the tray menu setup

        // Init state machine
        let (tx, rx) = crossbeam_channel::unbounded();
        let state = crate::state::AppState::new(cfg.clone(), tx.clone());

        // Start tray (blocks on main thread)
        crate::tray::run(state, rx)?;

        Ok(())
    }
}
