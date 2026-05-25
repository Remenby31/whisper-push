mod audio;
mod config;
mod hotkey;
mod notify;
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

    /// Force stop recording
    #[arg(long)]
    stop: bool,

    /// Run dependency/environment checks
    #[arg(long)]
    doctor: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Init logging
    let _guard = init_logging();

    info!("whisper-push v{}", env!("CARGO_PKG_VERSION"));

    if cli.doctor {
        return doctor::run();
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

    if cli.stop {
        state::force_stop();
        return Ok(());
    }

    // Run the app (tray mode on macOS/Windows, or daemon on Linux)
    app::run(cfg)
}

struct LogGuard;
impl Drop for LogGuard {
    fn drop(&mut self) {}
}

fn init_logging() -> LogGuard {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    LogGuard
}

mod doctor {
    use anyhow::Result;

    pub fn run() -> Result<()> {
        println!("whisper-push doctor");
        println!("Platform:  {} {}", std::env::consts::OS, std::env::consts::ARCH);

        // GPU backend
        #[cfg(target_os = "macos")]
        println!("GPU:       Metal (Apple Silicon)");
        #[cfg(all(not(target_os = "macos"), feature = "cuda"))]
        println!("GPU:       CUDA");
        #[cfg(all(not(target_os = "macos"), feature = "vulkan"))]
        println!("GPU:       Vulkan");
        #[cfg(all(not(target_os = "macos"), not(feature = "cuda"), not(feature = "vulkan")))]
        println!("GPU:       CPU only");

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

mod app {
    use anyhow::Result;
    use crate::config::Config;

    pub fn run(cfg: Config) -> Result<()> {
        // Ensure single instance
        let _lock = crate::state::acquire_lock()?;

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
