// Some objc2-driven macOS deps (dispatch2/objc2_foundation) hit the default
// rustc macro recursion limit on stable. Bump it generously here.
#![recursion_limit = "1024"]

mod audio;
mod autostart;
mod config;
mod hardware;
mod hotkey;
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

    // Subcommands that don't need config: log to stderr only
    if cli.doctor {
        init_logging(false);
        info!("whisper-push v{}", env!("CARGO_PKG_VERSION"));
        return doctor::run();
    }

    if cli.models {
        init_logging(false);
        model_manager::print_status();
        return Ok(());
    }

    if let Some(ref audio_path) = cli.transcribe {
        init_logging(false);
        info!("whisper-push v{}", env!("CARGO_PKG_VERSION"));
        return cli_transcribe::run(audio_path, cli.language.as_deref());
    }

    if cli.status {
        println!("{}", state::current_status());
        return Ok(());
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

    // Init logging with file output (after config is loaded so we know debug flag)
    init_logging(cfg.debug);
    cleanup_old_logs();

    info!("whisper-push v{}", env!("CARGO_PKG_VERSION"));

    // Run the app (tray mode on macOS/Windows, or daemon on Linux)
    app::run(cfg)
}

/// Set up tracing: stderr + daily rolling log file.
///
/// When `debug` is true the level is `debug`, otherwise `info`.
/// A daily rolling file is written to `<data_dir>/logs/whisper-push.YYYY-MM-DD.log`.
fn init_logging(debug: bool) {
    use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

    let default_level = if debug { "debug" } else { "info" };
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let log_dir = config::log_dir();
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::daily(&log_dir, "whisper-push.log");

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false).with_writer(std::io::stderr))
        .with(
            fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(file_appender),
        )
        .init();
}

/// Remove log files older than 7 days.
fn cleanup_old_logs() {
    let log_dir = config::log_dir();
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 24 * 3600);

    let entries = match std::fs::read_dir(&log_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("log") {
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
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
        if cfg!(feature = "metal") {
            features.push("metal");
        }
        if cfg!(feature = "cuda") {
            features.push("cuda");
        }
        if cfg!(feature = "vulkan") {
            features.push("vulkan");
        }
        if cfg!(feature = "parakeet") {
            features.push("parakeet");
        }
        if cfg!(feature = "voxtral") {
            features.push("voxtral");
        }
        println!(
            "Features:  {}",
            if features.is_empty() {
                "none".into()
            } else {
                features.join(", ")
            }
        );

        // Audio devices (with timeout — CoreAudio can hang without NSApp)
        println!("\nInput devices:");
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

        println!("\nOutput devices:");
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = crate::audio::list_output_devices();
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
            let perms = crate::permissions::check_all();
            println!("\nMicrophone:       {}", perms.microphone.label());
            println!("Accessibility:    {}", perms.accessibility.label());
            println!(
                "Input Monitoring: {}  (required for the global hotkey)",
                perms.input_monitoring.label()
            );
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
        println!(
            "Audio: {:.1}s ({} samples @ 16kHz)",
            samples.len() as f32 / 16000.0,
            samples.len()
        );

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
    use crate::config::Config;
    use anyhow::Result;

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
