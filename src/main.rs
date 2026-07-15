// Some objc2-driven macOS deps (dispatch2/objc2_foundation) hit the default
// rustc macro recursion limit on stable. Bump it generously here.
#![recursion_limit = "1024"]

mod acoustic;
mod audio;
mod autostart;
mod config;
mod dictionary;
mod enrich;
mod hardware;
mod history;
mod hotkey;
mod license;
mod model_manager;
mod notify;
mod onboarding;
mod overlay;
mod paste;
mod permissions;
mod report;
mod state;
mod templates;
mod transcribe;
mod tray;
mod updater;
mod util;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use tracing::info;

#[derive(Parser)]
#[command(name = "whisper-push", version, about)]
struct Cli {
    /// Subcommands (e.g. `dict`); when absent the daemon/tray runs.
    #[command(subcommand)]
    command: Option<Commands>,

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

    /// Print current macOS permission status as JSON and exit.
    #[arg(long, hide = true)]
    permissions_json: bool,

    /// Fire all missing OS permission prompts and exit.
    #[arg(long, hide = true)]
    permissions_prime: bool,

    /// Fire a single permission prompt ("mic" / "accessibility" /
    /// "input_monitoring") and exit. For mic the subprocess parks until
    /// the popup is resolved so macOS doesn't dismiss it.
    #[arg(long, hide = true, value_name = "KIND")]
    permissions_request: Option<String>,

    /// Post-update cleanup (remove old version backup, show notification).
    #[arg(long, hide = true)]
    post_update: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage the adaptive dictation dictionary (persistent, cross-model).
    Dict {
        #[command(subcommand)]
        action: DictAction,
    },
    /// Inspect/test the acoustic dictionary (correct words by their sound).
    Acoustic {
        #[command(subcommand)]
        action: AcousticAction,
    },
    /// End-to-end self-test: learn a word's SOUND from `wav1` with the real
    /// model, then verify it's recovered in `wav2` (proves the full pipeline).
    SelfTest { wav1: PathBuf, wav2: PathBuf },
    /// Autonomous test of the auto-capture feature: drives the real arm/capture
    /// core with simulated dictation+edit pairs and asserts the right
    /// corrections are (or aren't) learned. Deterministic, no GUI, no human.
    CaptureSelfTest,
    /// Manage the Lemon Squeezy license (activate, validate, status, …).
    License {
        #[command(subcommand)]
        action: LicenseAction,
    },
}

#[derive(Subcommand)]
enum LicenseAction {
    /// Activate this device with a license key + the purchase email.
    Activate {
        #[arg(long)]
        key: String,
        #[arg(long)]
        email: String,
    },
    /// Re-check the license against the server.
    Validate,
    /// Print the current license status (JSON).
    Status,
    /// Free this device's slot.
    Deactivate,
    /// Print the license.json path.
    Path,
}

#[derive(Subcommand)]
enum AcousticAction {
    /// Learn that a word recording sounds like `term`: `acoustic learn word.wav Kasar`.
    Learn { wav: PathBuf, term: String },
    /// Print the nearest stored term + DTW distance for a recording.
    Match { wav: PathBuf },
    /// Show how many acoustic fingerprints are stored.
    List,
}

#[derive(Subcommand)]
enum DictAction {
    /// List every entry (term, variants, source, counts).
    List,
    /// Add or update an entry: `dict add <term> [variants...]`.
    Add {
        term: String,
        variants: Vec<String>,
        /// Pin priority (wins collisions).
        #[arg(long)]
        starred: bool,
        /// Restrict to a language ("fr"/"en").
        #[arg(long)]
        lang: Option<String>,
    },
    /// Remove an entry by its canonical term.
    Remove { term: String },
    /// Teach from a correction without the GUI (also used by autonomous tests):
    /// `dict learn --finalized "..." --corrected "..."`.
    Learn {
        #[arg(long)]
        finalized: String,
        #[arg(long)]
        corrected: String,
        #[arg(long, default_value = "auto")]
        lang: String,
    },
    /// Print the dictionary.toml path.
    Path,
}

fn main() -> Result<()> {
    // Install panic hook early — catches panics and logs them + shows notification
    report::install_panic_hook();

    let cli = Cli::parse();

    // Permission CLI hooks handled BEFORE init_logging so stdout stays
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

    // Dictionary management subcommand (no daemon, stderr logging only).
    if let Some(Commands::Dict { action }) = &cli.command {
        init_logging(false);
        return cli_dict::run(action);
    }
    if let Some(Commands::Acoustic { action }) = &cli.command {
        init_logging(false);
        return cli_acoustic::run(action);
    }
    if let Some(Commands::SelfTest { wav1, wav2 }) = &cli.command {
        init_logging(false);
        return cli_selftest::run(wav1, wav2);
    }
    if let Some(Commands::CaptureSelfTest) = &cli.command {
        init_logging(false);
        return cli_capture_selftest::run();
    }
    if let Some(Commands::License { action }) = &cli.command {
        init_logging(false);
        return cli_license::run(action);
    }

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

    // Post-update cleanup: remove old app backup + show notification
    if cli.post_update {
        info!("Post-update cleanup");
        let backup = std::path::PathBuf::from("/Applications/Whisper Push.app.old");
        if backup.exists() {
            let _ = std::fs::remove_dir_all(&backup);
            info!("Removed old version backup");
        }
        notify::app(&format!("Updated to v{}!", env!("CARGO_PKG_VERSION")));
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

    // Cap retained rolled files at the appender level (belt-and-suspenders with
    // the startup `cleanup_old_logs` sweep) so the log dir can't grow unbounded.
    // Fall back to the simple daily appender if the builder can't initialize.
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("whisper-push.log")
        .max_log_files(7)
        .build(&log_dir)
        .unwrap_or_else(|_| tracing_appender::rolling::daily(&log_dir, "whisper-push.log"));

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

/// Largest the launchd stderr capture may grow before it's rotated aside.
const MAX_LAUNCHD_LOG: u64 = 5 * 1024 * 1024; // 5 MiB

/// Remove rolled log files older than 7 days and cap the launchd stderr capture.
fn cleanup_old_logs() {
    let log_dir = config::log_dir();
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(7 * 24 * 3600);

    let entries = match std::fs::read_dir(&log_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        // Rolled files are `whisper-push.log.YYYY-MM-DD` — the *date* is the
        // extension, so the old `extension() == "log"` test matched nothing and
        // never deleted anything. Match on the filename prefix instead.
        let is_app_log = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("whisper-push.log"));
        if is_app_log {
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if modified < cutoff {
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }

    // The launchd stderr capture is a single, continuously-appended file the age
    // sweep above can't catch (its mtime is always fresh). Rotate it aside when
    // it gets large; the next launch starts a fresh one. Renaming is safe even
    // while launchd holds the fd open — its writes follow the inode, not the path.
    let launchd = log_dir.join("launchd-stderr.log");
    if let Ok(meta) = launchd.metadata() {
        if meta.len() > MAX_LAUNCHD_LOG {
            let old = log_dir.join("launchd-stderr.log.old");
            let _ = std::fs::remove_file(&old);
            let _ = std::fs::rename(&launchd, &old);
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

        // Compiled features. Metal is auto-enabled on macOS via a target-specific
        // whisper-rs dependency, so report it on macOS regardless of the crate
        // `metal` feature flag (now off in the cross-platform default).
        let mut features = Vec::new();
        if cfg!(target_os = "macos") || cfg!(feature = "metal") {
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

        // Audio devices — enumeration is internally bounded (see
        // audio::list_devices / DEVICE_ENUM_TIMEOUT), so no wrapper needed here.
        println!("\nInput devices:");
        match crate::audio::list_devices() {
            Ok(devices) => {
                for (i, name) in devices.iter().enumerate() {
                    println!("  [{i}] {name}");
                }
            }
            Err(e) => println!("  ({e})"),
        }

        println!("\nOutput devices:");
        match crate::audio::list_output_devices() {
            Ok(devices) => {
                for (i, name) in devices.iter().enumerate() {
                    println!("  [{i}] {name}");
                }
            }
            Err(e) => println!("  ({e})"),
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
        // Arm the dictionary + acoustic store so the transcription is corrected.
        crate::dictionary::init(cfg.dictionary_enabled);
        crate::acoustic::init();
        println!("Loading model: {}...", cfg.model);
        let backend = crate::model_manager::resolve_backend(&cfg.model);
        crate::transcribe::ensure_loaded(&backend, &cfg.model)?;

        // Transcribe
        println!("Transcribing...");
        let start = Instant::now();
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

mod cli_dict {
    use crate::DictAction;
    use crate::dictionary::{self, Correction, EditKind, Source};
    use anyhow::{Result, anyhow};

    pub fn run(action: &DictAction) -> Result<()> {
        // Load the dictionary regardless of the runtime toggle — management
        // should always work.
        dictionary::init(true);

        match action {
            DictAction::Path => {
                println!("{}", dictionary::dictionary_path().display());
            }
            DictAction::List => {
                let entries = dictionary::list_entries();
                if entries.is_empty() {
                    println!("(dictionary is empty)");
                } else {
                    for e in &entries {
                        let star = if e.starred { "★ " } else { "  " };
                        let src = match e.source {
                            Source::Auto => "✨auto",
                            Source::Manual => "manual",
                        };
                        let lang = e.lang.as_deref().unwrap_or("*");
                        println!(
                            "{star}{}  [{src} {lang} ×{} ⌫{}]\n      ← {}",
                            e.term,
                            e.count,
                            e.undo_count,
                            if e.variants.is_empty() {
                                "(no variants yet)".to_string()
                            } else {
                                e.variants.join(", ")
                            }
                        );
                    }
                    println!(
                        "\n{} entr{}",
                        entries.len(),
                        if entries.len() == 1 { "y" } else { "ies" }
                    );
                }
            }
            DictAction::Add {
                term,
                variants,
                starred,
                lang,
            } => {
                dictionary::add_entry(term, variants, *starred, lang.as_deref())
                    .map_err(|e| anyhow!(e))?;
                println!("Added/updated: {term}");
            }
            DictAction::Remove { term } => {
                let removed = dictionary::remove_entry(term).map_err(|e| anyhow!(e))?;
                println!(
                    "{}",
                    if removed {
                        format!("Removed: {term}")
                    } else {
                        format!("Not found: {term}")
                    }
                );
            }
            DictAction::Learn {
                finalized,
                corrected,
                lang,
            } => match dictionary::correct(finalized, corrected, lang) {
                Correction::Done(report) => {
                    let kind = match report.kind {
                        Some(EditKind::Punctual) => "punctual correction",
                        Some(EditKind::Rewrite) => "rewrite (ignored)",
                        Some(EditKind::NoChange) => "no change",
                        None => "no change",
                    };
                    println!("Classified as: {kind}");
                    for (heard, term) in &report.learned {
                        println!("  learned: {heard:?} → {term:?}");
                    }
                    for term in &report.demoted {
                        println!("  demoted: {term:?}");
                    }
                    if !report.changed {
                        println!("  (dictionary unchanged)");
                    }
                }
                other => println!("{other:?}"),
            },
        }
        Ok(())
    }
}

mod cli_acoustic {
    use crate::AcousticAction;
    use anyhow::{Result, anyhow};
    use whisper_push_acoustic::{AcousticStore, fingerprint};

    fn store_path() -> std::path::PathBuf {
        crate::config::data_dir().join("acoustic.bin")
    }

    pub fn run(action: &AcousticAction) -> Result<()> {
        let path = store_path();
        let mut store = AcousticStore::load(&path);
        match action {
            AcousticAction::Learn { wav, term } => {
                let samples = crate::audio::decode::load_audio_file(wav)?;
                store.learn(term, fingerprint(&samples, 16_000));
                store.save(&path).map_err(|e| anyhow!(e.to_string()))?;
                println!(
                    "Learned the sound of \"{term}\" — {} fingerprint(s) stored",
                    store.len()
                );
            }
            AcousticAction::Match { wav } => {
                let samples = crate::audio::decode::load_audio_file(wav)?;
                let fp = fingerprint(&samples, 16_000);
                match store.nearest(&fp) {
                    Some((t, d)) => {
                        let verdict = if d <= 6.0 { "MATCH" } else { "no match (>6.0)" };
                        println!("nearest: \"{t}\"  distance={d:.2}  → {verdict}");
                    }
                    None => println!("(acoustic store is empty)"),
                }
            }
            AcousticAction::List => {
                println!(
                    "{} acoustic fingerprint(s) at {}",
                    store.len(),
                    path.display()
                );
            }
        }
        Ok(())
    }
}

mod cli_selftest {
    use anyhow::{Result, bail};
    use std::path::Path;

    /// Full-pipeline proof: learn a word's sound from `wav1` with the real model,
    /// then confirm `wav2` (another recording of the same word) is recovered by
    /// SOUND — exercises transcribe → timings → acoustic capture → learn → match.
    pub fn run(wav1: &Path, wav2: &Path) -> Result<()> {
        const MARKER: &str = "ACOUSTICPROOF";
        let cfg = crate::config::Config::load()?;
        // Disable the text dictionary so it can't mask the acoustic result, and
        // use an ephemeral acoustic store so we never touch the user's voiceprints.
        crate::dictionary::init(false);
        crate::acoustic::init_ephemeral();
        let backend = crate::model_manager::resolve_backend(&cfg.model);
        crate::transcribe::ensure_loaded(&backend, &cfg.model)?;

        // 1. Transcribe wav1 — this retains its audio + word spans in HISTORY.
        let s1 = crate::audio::decode::load_audio_file(wav1)?;
        let t1 = crate::transcribe::transcribe_with_backend(&s1, "auto", &backend)?;
        // Learn against the RAW first word (what HISTORY holds), so learn_word
        // can find that exact spoken segment regardless of text post-correction.
        let heard = match crate::acoustic::last_heard_word() {
            Some(w) if !w.is_empty() => w,
            _ => bail!("wav1 produced no word"),
        };
        let learned = crate::acoustic::learn_word(&heard, MARKER);
        if !learned {
            bail!("could not fingerprint {heard:?} from wav1 (segment too short?)");
        }
        println!("1) wav1 → {t1:?}; learned the sound of {heard:?} → {MARKER}");

        // 2. Transcribe wav2 — should be corrected by sound.
        let s2 = crate::audio::decode::load_audio_file(wav2)?;
        let t2 = crate::transcribe::transcribe_with_backend(&s2, "auto", &backend)?;
        println!("2) wav2 → {t2:?}");

        if t2.contains(MARKER) {
            println!("PASS: the spoken word was recovered by SOUND across two recordings");
            Ok(())
        } else {
            bail!("FAIL: acoustic correction did not apply (t1={t1:?}, t2={t2:?})")
        }
    }
}

/// Autonomous, no-human test of the **auto-capture** feature (the one the user
/// reported broken). It drives the *real* daemon-side capture core
/// (`arm_with_baseline` → `capture_with_current`) — the exact functions the
/// daemon calls after reading the focused field — with the field text injected
/// directly. This deterministically covers everything that decides what gets
/// learned: the PENDING snapshot, the unchanged-field guard, the diff/classify
/// (punctual-fix vs rewrite vs meaning-change) and the dictionary mutation.
///
/// Why injected text and not a live field: the literal `AXUIElementCopy…` read
/// only works in an Accessibility-*authorized* process (the installed daemon);
/// a freshly-built binary run from a shell is denied (-25204), so a "real field"
/// read here would be neither possible nor faithful. The field *content* is the
/// only thing that flows into the logic, so injecting it tests the real pipeline
/// with zero GUI flakiness. The AX read itself is hardened + validated in the
/// daemon (see `dictionary::ax`). This is the closed loop: change → run → fix.
mod cli_capture_selftest {
    use anyhow::{Result, bail};

    struct Scenario {
        name: &'static str,
        lang: &'static str,
        /// What the daemon pasted (the dictation, as it appeared in the field).
        baseline: String,
        /// What the field contains after the user edits it.
        edited: String,
        /// The term we expect learned, or `None` if nothing should be learned.
        expect_term: Option<&'static str>,
    }

    impl Scenario {
        fn new(
            name: &'static str,
            lang: &'static str,
            baseline: &str,
            edited: &str,
            expect_term: Option<&'static str>,
        ) -> Self {
            Scenario {
                name,
                lang,
                baseline: baseline.to_string(),
                edited: edited.to_string(),
                expect_term,
            }
        }
    }

    /// Embed a dictation in ~40k chars of unchanged filler — the size of a real
    /// document the user actually dictates into (and which the old MAX_FIELD=8000
    /// guard silently rejected). Local trimming must isolate the edit regardless.
    fn in_big_doc(sentence: &str) -> String {
        let filler = "Lorem ipsum dolor sit amet consectetur adipiscing elit sed do. ".repeat(300);
        format!("{filler}\n{sentence}\n{filler}")
    }

    pub fn run() -> Result<()> {
        // Don't spam real notifications while looping; capture's learn_word
        // no-ops without audio history. Safe: single-threaded at this point.
        unsafe { std::env::set_var("WHISPER_PUSH_SUPPRESS_NOTIFY", "1") };
        crate::acoustic::init_ephemeral();
        let tmp = std::env::temp_dir().join("wp_capture_selftest");
        let _ = std::fs::create_dir_all(&tmp);

        // The user's three described edit shapes, plus traps that must NOT learn,
        // plus the same shapes buried in a large document (the real-world bug).
        let scenarios = [
            Scenario::new(
                "add letters to a proper noun (punctual fix → learn)",
                "en",
                "I met Claud in Paris",
                "I met Claude in Paris",
                Some("Claude"),
            ),
            Scenario::new(
                "fix one mis-heard word inside a longer sentence (→ learn)",
                "en",
                "restart the Kafga cluster tonight please",
                "restart the Kafka cluster tonight please",
                Some("Kafka"),
            ),
            Scenario::new(
                "navigate in and add a letter to a name (→ learn)",
                "fr",
                "le dossier de Cruvelier est pret",
                "le dossier de Cruvellier est pret",
                Some("Cruvellier"),
            ),
            Scenario::new(
                "partial edit: rephrase one word AND fix a name (→ learn just the name)",
                "en",
                "tell Claud the report is due at noon",
                "tell Claude the report is due at two",
                Some("Claude"),
            ),
            Scenario::new(
                "full rephrase of the sentence (rewrite → learn nothing)",
                "en",
                "send me the report by noon",
                "please forward the document this afternoon",
                None,
            ),
            Scenario::new(
                "name replaced by a different word (meaning change → ignore)",
                "en",
                "call Khazar tomorrow morning",
                "call the supplier tomorrow morning",
                None,
            ),
            Scenario::new(
                "only appended a word (no substitution → learn nothing)",
                "en",
                "I went to the office",
                "I went to the office today",
                None,
            ),
            Scenario::new(
                "fixed an everyday word (sounds alike but common → don't promote)",
                "en",
                "a letter form you arrived",
                "a letter from you arrived",
                None,
            ),
            // ── Large-document cases: the real production bug ──────────────────
            // The user dictates into a ~40k-char document and fixes one name.
            // Before local trimming these were ALL rejected ("too large to diff").
            Scenario::new(
                "name fix buried in a 40k-char document (→ learn)",
                "en",
                &in_big_doc("I met Claud in Paris today."),
                &in_big_doc("I met Claude in Paris today."),
                Some("Claude"),
            ),
            Scenario::new(
                "second name fix in the same growing document (→ learn)",
                "fr",
                &in_big_doc("mon ami s'appelle Rodolf et il vient demain."),
                &in_big_doc("mon ami s'appelle Rodolphe et il vient demain."),
                Some("Rodolphe"),
            ),
            Scenario::new(
                "completely erase a name and retype another in a big doc (→ learn)",
                "en",
                &in_big_doc("restart the Kafga cluster tonight please."),
                &in_big_doc("restart the Kafka cluster tonight please."),
                Some("Kafka"),
            ),
            Scenario::new(
                "localized rewrite inside a big doc (→ learn nothing, not fooled by size)",
                "en",
                &in_big_doc("send me the report by noon."),
                &in_big_doc("send me the document this afternoon."),
                None,
            ),
            Scenario::new(
                "edit somewhere else in a big doc, dictation untouched (→ learn nothing)",
                "en",
                &in_big_doc("the meeting is on Tuesday."),
                &format!(
                    "EDITED PREAMBLE {}",
                    in_big_doc("the meeting is on Tuesday.")
                ),
                None,
            ),
        ];

        let mut fails = 0;
        for (i, s) in scenarios.iter().enumerate() {
            // Fresh, hermetic dictionary per scenario (never the user's file).
            let dict_path = tmp.join(format!("dict_{i}.toml"));
            let _ = std::fs::remove_file(&dict_path);
            whisper_push_dict::set_enabled(true);
            whisper_push_dict::init(dict_path.clone())
                .map_err(|e| anyhow::anyhow!("init temp dict: {e}"))?;

            // Exactly the daemon's sequence: record the dictation (so the capture
            // knows the language), arm with the pasted baseline, then capture the
            // edited field — only the field reads are injected instead of AX.
            let _ = whisper_push_dict::finalize_and_record(&s.baseline, s.lang);
            crate::dictionary::arm_with_baseline(s.baseline.clone(), s.lang.to_string(), false);
            crate::dictionary::capture_with_current(&s.edited);

            let learned: Vec<String> = whisper_push_dict::list_entries()
                .iter()
                .map(|e| e.term.clone())
                .collect();
            let ok = match s.expect_term {
                Some(term) => learned.iter().any(|t| t == term),
                None => learned.is_empty(),
            };
            if ok {
                println!("  PASS [{}] — learned {learned:?}", s.name);
            } else {
                fails += 1;
                println!(
                    "  FAIL [{}] — expected {:?}, got learned={learned:?}",
                    s.name, s.expect_term
                );
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
        println!();
        if fails == 0 {
            println!(
                "PASS: auto-capture learns the right edits and ignores rewrites/meaning-changes."
            );
            Ok(())
        } else {
            bail!("FAIL: {fails} capture scenario(s) failed")
        }
    }
}

mod cli_license {
    use crate::LicenseAction;
    use crate::license::{self, ActivateOutcome, DeactivateOutcome, ValidateOutcome};
    use anyhow::Result;

    pub fn run(action: &LicenseAction) -> Result<()> {
        license::init();
        match action {
            LicenseAction::Path => println!("{}", license::license_path().display()),
            LicenseAction::Status => println!("{}", license::status_json()),
            LicenseAction::Activate { key, email } => {
                let line = match license::activate(key, email) {
                    ActivateOutcome::Activated => "{\"activated\":true}".to_string(),
                    ActivateOutcome::Rejected(r) => {
                        format!("{{\"activated\":false,\"error\":{}}}", json_str(&r))
                    }
                    ActivateOutcome::Offline => {
                        "{\"activated\":false,\"error\":\"offline\"}".into()
                    }
                };
                println!("{line}");
            }
            LicenseAction::Validate => {
                let r = match license::validate() {
                    ValidateOutcome::Valid => "valid",
                    ValidateOutcome::Invalid => "invalid",
                    ValidateOutcome::Offline => "offline",
                };
                println!("{{\"result\":\"{r}\"}}");
            }
            LicenseAction::Deactivate => {
                let r = match license::deactivate() {
                    DeactivateOutcome::Done => "done",
                    DeactivateOutcome::Offline => "offline",
                };
                println!("{{\"result\":\"{r}\"}}");
            }
        }
        Ok(())
    }

    fn json_str(s: &str) -> String {
        serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
    }
}

mod app {
    use crate::config::Config;
    use anyhow::Result;

    /// Opt the daemon out of macOS App Nap for the whole process lifetime
    /// (without disabling system sleep, so the Mac still sleeps normally). The
    /// returned activity token must outlive the app, so we leak it.
    #[cfg(target_os = "macos")]
    fn disable_app_nap() {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        let token = NSProcessInfo::processInfo().beginActivityWithOptions_reason(
            NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
            &NSString::from_str("Whisper Push keeps its speech model warm for instant dictation"),
        );
        std::mem::forget(token);
    }

    pub fn run(mut cfg: Config) -> Result<()> {
        // Ensure single instance
        let _lock = crate::state::acquire_lock()?;

        // Arm the adaptive dictionary (load dictionary.toml, compile tables).
        crate::dictionary::init(cfg.dictionary_enabled);
        // Arm licensing (load license.json, anchor trial, kick bg revalidation).
        crate::license::init();
        // Arm the acoustic dictionary (load fingerprints).
        crate::acoustic::init();
        // Optional online enrichment (opt-in, default off).
        crate::enrich::set_enabled(cfg.online_enrichment);

        // Keep the speech model resident in RAM while a model is loaded, so the
        // first dictation after any idle gap (including the first of the day)
        // stays instant instead of paying a multi-second page-in of the (large)
        // model weights. Gated by config — see `keep_model_resident`.
        crate::transcribe::spawn_keep_warm(cfg.keep_model_resident);
        // macOS App Nap throttles background (LSUIElement) apps: it would delay
        // the keep-warm heartbeat and hasten eviction of the model's pages.
        // Opt out — without disabling system sleep, so battery is unaffected.
        #[cfg(target_os = "macos")]
        disable_app_nap();

        // First-launch onboarding. None = wizard exited without finishing
        // (e.g. user closed the window); exit cleanly without marking done.
        if crate::onboarding::check_first_launch() {
            match crate::onboarding::run() {
                Some(model) => cfg.model = model,
                None => {
                    tracing::info!("Onboarding interrupted, exiting");
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
