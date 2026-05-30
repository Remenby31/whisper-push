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

    /// Print current macOS permission status as JSON and exit. Used by the
    /// onboarding wizard to poll TCC state of the daemon binary.
    #[arg(long, hide = true)]
    permissions_json: bool,

    /// Fire all missing OS permission prompts and exit. Currently unused
    /// by the wizard (we prefer per-row Grant), kept for diagnostics.
    #[arg(long, hide = true)]
    permissions_prime: bool,

    /// Fire the OS prompt for a SINGLE permission ("mic", "accessibility",
    /// or "input_monitoring") and exit. For "mic" the subprocess parks
    /// until the popup is resolved so macOS doesn't dismiss it.
    #[arg(long, hide = true, value_name = "KIND")]
    permissions_request: Option<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Permission CLI hooks — handled BEFORE init_logging so stdout stays
    // clean (the wizard parses our stdout as JSON).
    if cli.permissions_json {
        let s = permissions::check_all();
        println!(
            "{{\"microphone\":\"{}\",\"accessibility\":\"{}\",\"input_monitoring\":\"{}\",\"all_granted\":{}}}",
            perm_state_str(s.microphone),
            perm_state_str(s.accessibility),
            perm_state_str(s.input_monitoring),
            s.all_granted()
        );
        return Ok(());
    }
    if cli.permissions_prime {
        let status = permissions::check_all();
        permissions::prompt_missing(&status);
        return Ok(());
    }
    if let Some(ref kind) = cli.permissions_request {
        permissions::request_one(kind);
        // Mic uses AVCaptureDevice.requestAccess which shows a popup OWNED
        // by the calling process. If we exit too fast macOS dismisses it.
        // Park here, polling the mic state, and exit as soon as it
        // stabilizes (Granted/Denied) or 30s elapse.
        if kind == "mic" || kind == "microphone" {
            for _ in 0..60 {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let s = permissions::check_all();
                if s.microphone == permissions::PermState::Granted
                    || s.microphone == permissions::PermState::Denied
                {
                    break;
                }
            }
        }
        return Ok(());
    }

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

fn perm_state_str(s: permissions::PermState) -> &'static str {
    match s {
        permissions::PermState::Granted => "granted",
        permissions::PermState::Denied => "denied",
        permissions::PermState::NotRequested => "not_requested",
        permissions::PermState::Unknown => "unknown",
    }
}

fn init_logging() {
    use tracing_subscriber::{EnvFilter, fmt};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt().with_env_filter(filter).with_target(false).init();
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

        // First-launch onboarding. If the wizard exited without finishing
        // (e.g. user closed the window), `run()` returns None. Exit clean
        // without marking complete so the next launch retries.
        if crate::onboarding::check_first_launch() {
            match crate::onboarding::run() {
                Some(model) => cfg.model = model,
                None => {
                    tracing::info!("Onboarding interrupted — exiting");
                    return Ok(());
                }
            }
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
