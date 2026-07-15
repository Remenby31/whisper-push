# CLAUDE.md — Whisper Push (Rust)

Push-to-talk voice dictation, 100% local. Cross-platform (macOS, Linux, Windows).

## Build & Run

```bash
# Prerequisites: Rust 1.83+, cmake

# Build (debug)
cargo build

# Build (release)
cargo build --release

# Build with CUDA (Linux/Windows, NVIDIA GPU)
cargo build --release --features cuda

# Build with Vulkan (Linux/Windows, AMD/Intel GPU)
cargo build --release --features vulkan

# macOS: create .app bundle + sign + launch
make deploy

# macOS: create DMG for distribution
make dmg

# Run directly
cargo run -- --doctor    # check environment
cargo run                # start daemon
```

## Structure

```
whisper-push/
├── Cargo.toml                # Workspace with features cuda/vulkan
├── Makefile                  # macOS build helpers (bundle, sign, dmg)
├── src/
│   ├── main.rs               # CLI (clap) + doctor + app entry
│   ├── config.rs             # TOML config (serde + dirs)
│   ├── state.rs              # State machine (Idle/Loading/Recording/Processing)
│   ├── permissions.rs        # macOS AXIsProcessTrusted
│   ├── notify.rs             # Cross-platform notifications (notify-rust)
│   ├── audio/
│   │   ├── mod.rs            # Device listing
│   │   ├── capture.rs        # cpal input → 16kHz mono f32 (rubato resampling)
│   │   └── playback.rs       # Start/stop sounds (embedded via include_bytes!)
│   ├── transcribe/
│   │   └── mod.rs            # whisper-rs load/unload/transcribe + HF model download
│   ├── hotkey/
│   │   ├── mod.rs            # Platform dispatch
│   │   ├── macos.rs          # NSEvent global monitor (objc2 + block2)
│   │   ├── linux.rs          # evdev keyboard reading
│   │   └── windows.rs        # WH_KEYBOARD_LL hook
│   ├── paste/
│   │   └── mod.rs            # arboard clipboard + enigo keystroke (Cmd/Ctrl+V)
│   └── tray/
│       └── mod.rs            # tray-icon + muda menu + event loop orchestration
├── resources/
│   ├── Info.plist            # macOS app bundle metadata
│   └── entitlements.plist    # macOS entitlements
├── sounds/
│   ├── start.wav             # Recording start sound
│   └── stop.wav              # Recording stop sound
└── .github/workflows/
    └── release.yml           # CI: macOS + Linux (CPU/CUDA) + Windows (CPU/CUDA)
```

## Architecture

### GPU backends (compile-time features)
- **macOS**: Metal (automatic, whisper.cpp detects Apple Silicon)
- **Linux/Windows CPU**: default (no feature flag)
- **Linux/Windows CUDA**: `--features cuda` (NVIDIA GPU, requires CUDA Toolkit)
- **Linux/Windows Vulkan**: `--features vulkan` (AMD/Intel GPU)

### Hotkey modes
- **hold** (default): hold modifier key → speak → release → text appears
  - Pre-roll: audio capture starts on key-down, committed after `hold_delay`
  - Quick taps (< hold_delay) are discarded (avoids triggering on Ctrl+C etc.)
- **toggle**: press once to start, press again to stop → text appears

### Model
- `ggml-large-v3-turbo-q5_0.bin` (~1.5GB) downloaded from HuggingFace on first run
- Stored in platform data dir (Application Support / XDG_DATA / AppData)
- Stays loaded in RAM for the daemon's lifetime (no idle unload). A keep-warm
  heartbeat (a silent inference every 90 s while a model is loaded) keeps macOS
  from compressing/swapping the weights, so the first dictation of the day is
  instant instead of paying an 11–18 s page-in. Gated by `config.keep_model_resident`
  (default true); see the "Keep-warm" note in `src/transcribe/mod.rs`. NB: mlock
  can't pin the weights on macOS (OS forbids wiring shared file-backed pages).
  Covers **Parakeet + Whisper only** — Voxtral is excluded (WGPU forbids using the
  model off its load thread), so Voxtral users still pay the cold-start page-in
  (plus the existing ~15 s first-transcription shader compile).

### Paste mechanism
1. Save current clipboard (arboard)
2. Set transcribed text to clipboard
3. Simulate Cmd+V (macOS) or Ctrl+V (Linux/Windows) via enigo
4. Restore original clipboard

### Config
TOML format, compatible with Python version. Platform-default paths:
- macOS: `~/Library/Application Support/whisper-push/config.toml`  
- Linux: `~/.config/whisper-push/config.toml`
- Windows: `%APPDATA%/whisper-push/config.toml`

## Codesign (macOS)

```bash
# Developer ID: Baptiste Cruvellier (3SNT64YKAS)
# Permissions TCC persist across rebuilds with this certificate
make sign    # sign the .app bundle
make dmg     # create distributable DMG
```

## Pièges

- **cpal macOS**: native sample rate is 44.1/48kHz, not 16kHz → rubato resampling required
- **whisper-rs build**: requires cmake for whisper.cpp compilation
- **macOS keyboard CGEventTap**: needs **Accessibility AND Input Monitoring** (kTCCServiceListenEvent). Accessibility alone is not enough — the tap silently receives nothing. The app checks both via `IOHIDCheckAccess` and requests them via `IOHIDRequestAccess`. The tap must be born *after* the grants → `permissions::guided_setup()` restarts the daemon (`launchctl kickstart -k`) once everything is granted.
- **Ad-hoc TCC reset**: every rebuild changes the binary's cdhash, so macOS invalidates the TCC grants. `guided_setup` is what makes the re-grant tolerable — it opens the right panes, polls, and auto-restarts. A real Developer ID would stop the resets entirely.
- **evdev on Linux**: requires user in 'input' group (`sudo usermod -aG input $USER`)
- **Windows keyboard hook**: WH_KEYBOARD_LL needs a message loop on the hook thread
- **Voxtral GPU shaders**: `transcribe_streaming` on silence hangs on M4 Pro Metal → warmup skipped, shaders compile lazily on first real transcription (~15s). Streaming mode disabled (blocks feed_chunk loop during compilation); batch mode works. cubecl stores autotune cache in `CWD/target/` → `load_model()` does `set_current_dir(data_dir)` so cache lands in `<data_dir>/target/autotune/`.

## Logging

Dual output: stderr + daily rolling file in `<data_dir>/logs/whisper-push.log.YYYY-MM-DD`.
`config.debug = true` sets level to `debug` (default `info`). Files > 7 days auto-deleted on startup.
LaunchAgent captures pre-tracing panics to `<data_dir>/logs/launchd-stderr.log`.

## Debugging

```bash
# Live tail the log
tail -f ~/Library/Application\ Support/whisper-push/logs/whisper-push.log.*

# Key log patterns to grep for:
#   "HotkeyDown" / "HotkeyUp"     — CGEventTap received the key
#   "Recording from"               — cpal opened the mic (device + sample rate)
#   "Captured Xs of audio"         — recording stopped (duration, RMS, max)
#   "Processing Xs with backend"   — transcription started (backend name, RMS)
#   "Parakeet:" / "Whisper:" / "Voxtral:"  — transcription result + time
#   "Pasting"                      — text sent to clipboard + Cmd+V
#   "model loaded (Xs)"            — model load time
#   "Too short, skipping"          — hold was too brief (< hold_delay)
#   "Transcription panicked"       — engine crashed (catch_unwind caught it)

# Common issues:
#   No HotkeyDown logged           → TCC: check Accessibility + Input Monitoring
#   HotkeyDown but no Recording    → hold_delay not reached (quick tap)
#   Recording but RMS ≈ 0          → wrong input device or mic permission denied
#   Transcription empty text       → audio too quiet or wrong language setting
#   "poisoned lock"                → previous panic corrupted Mutex; restart app
```

## E2E Testing (macOS)

**Prerequisites:** `brew install sox blackhole-2ch`

**Test harness binary** (`src/bin/test_harness.rs`):
```bash
cargo run --bin whisper-push-test -- hotkey-hold ctrl 3    # CGEvent: press, wait 3s, release
cargo run --bin whisper-push-test -- play-to "BlackHole 2ch" test.wav  # sox → virtual device
cargo run --bin whisper-push-test -- wait-log "Pasting" 30  # tail log, exit 0 on match
cargo run --bin whisper-push-test -- check-log "Ready!"     # grep log, exit 0 if found
```

**Full E2E script** (`tests/e2e.sh`): configures BlackHole as input, launches app, generates audio via `say`, plays to BlackHole while holding hotkey via CGEvent, verifies transcription in logs.
```bash
./tests/e2e.sh              # full run (builds + launches app)
./tests/e2e.sh --no-launch  # skip launch (app already running)
```

**How it works:** CGEvent posted at HID layer → real CGEventTap captures it → cpal records from BlackHole → rubato resamples → engine transcribes → clipboard + Cmd+V paste. Zero mocks — 100% production code path.

**Important**: modifier keys (ctrl, shift, cmd, alt) must be posted as `FlagsChanged` CGEvents, not `KeyDown`/`KeyUp` — the CGEventTap only listens for `FlagsChanged` in hold mode.

## Recent additions (branch `settings-and-brandkit`)

Enhancements layered on top of the existing modules — no new architectural pieces.

- **`tray/mod.rs`** — Engine / Hotkey / Input Device / Output Device / Permissions are now real `Submenu` dropdowns (needed `tray-icon 0.24` + `muda 0.19`: the old `0.16` had a Tahoe hover-close bug). Permissions submenu is always visible with a ✓ / ⚠ title and a "Run Guided Setup…" item.
- **`hotkey/macos.rs`** — match config is now live-mutable (`Mutex<Option<MatchConfig>>`), so preset switches and custom captures take effect without restart. `start_capture(tx)` arms a capture mode: tap a modifier → hold hotkey; press modifiers+key → toggle hotkey. Result arrives as `Event::HotkeyCaptured`. Keycode↔name table covers letters, digits, space, return, tab, escape.
- **`permissions.rs`** — adds Input Monitoring (`IOHIDCheckAccess`/`IOHIDRequestAccess`) to `PermissionStatus`. `guided_setup()` opens the relevant Settings panes, polls for grants, then `launchctl kickstart -k` to restart the daemon with permissions in place.
- **`audio/playback.rs`** — respects `output_device` via a static `RwLock<String>` set from config (was always using `default_output_device`). **`audio/mod.rs`** — `list_output_devices()` companion to `list_devices()`. Note that on macOS, device *enumeration* needs no mic permission — TCC only gates capture.
- **`transcribe/mod.rs`** — `model_path()` checks the `.app/Contents/Resources/models/` bundle path first (bundled DMG install), falls back to the user data dir (downloaded on first run). `transcribe_with_backend(Parakeet)` falls back to Whisper on any error, so transcription never hard-fails.
- **`transcribe/parakeet.rs`** — fixed the download repo to `onnx-community/parakeet-ctc-0.6b-ONNX` (`model.onnx` + `tokenizer.json` + optional `model.onnx_data`); the previous `nvidia/...` repo ships `.nemo` files only.
- **Sound feedback** — "start" sound is now played immediately on `HotkeyDown` (not after `hold_delay`), so the user gets an instant audio cue.
- **Menu-bar icons** (`tray/mod.rs`) — ONE master glyph (`resources/icons/icon-glyph.svg` → `icon-glyph.png`, the brand three-wave sound mark) is recoloured per state at runtime by `glyph_icon(GlyphStyle)`, so the geometry/size is byte-identical across states (no more squished or oversized variants). **Idle** = crisp macOS template (auto black/white); **Loading/Processing** = same template dimmed to ~43% (`BUSY_OPACITY`, reads as "working", visible on any bar); **Recording** = **citron #CEDC00** (`TINT_RECORDING`, the sole accent). State drives the icon via `set_tray_icon`; crucially the **pipeline thread emits `StateChanged`** on hotkey-driven record/stop too, so the icon updates identically whether recording starts from the menu or the key (previously only the menu path did). Start/stop sounds live at the trigger points only — never in the `StateChanged` handlers — to avoid doubling.
- **Makefile** — `make install` copies the bundle to `/Applications` and writes the login `LaunchAgent`. `make uninstall` reverses it. `make dmg` bundles `~/Library/Application Support/whisper-push/models/ggml-large-v3-turbo-q5_0.bin` into `Contents/Resources/models/` **before** signing, so the distributed DMG (~528 MB) gives a zero-download first launch. `make install` stays slim — only `make dmg` ships the model.
- **App icon** — `resources/AppIcon.icns` generated from the brand kit squircle PNGs, referenced by `Info.plist` (`CFBundleIconFile`).

## Adaptive dictation (learned word correction)

Persistent, cross-model, output-side correction that learns from user corrections — **no
prompts fed to any ASR model, no second local model**. Design: `docs/adaptive-dictation-plan.md`.

- **Lives in a pure workspace crate** `crates/whisper-push-dict/` (deps: serde, toml,
  unicode-normalization; serde_json dev-only) so `cargo test -p whisper-push-dict` runs in
  ~1.3 s without whisper.cpp/wgpu/onnx. Root `Cargo.toml` is now a `[workspace]` (resolver "3")
  with the crate as a path dep.
- **Hot path** — `whisper_push_dict::finalize_and_record(raw, lang)` is called at the end of
  `transcribe::transcribe_with_backend` (the single point all 3 backends pass through → model-
  agnostic). Exact n-gram longest-match (deterministic) + a heavily-guarded fuzzy layer
  (common-word blocklist `data/common_{en,fr}.txt` + similarity threshold). Empty/disabled dict
  ⇒ ~0-cost pass-through.
- **Cold path** — `learn.rs` diffs (finalized, corrected), classifies *punctual fix* vs
  *rewrite* (`sim_doc` + a per-span phonetic gate) and promotes proper-nouns/jargon only.
  **Partial edits** (rephrase part of a sentence AND fix a name) now learn the like-sounding fix
  and ignore the meaning-change spans — but when an unlearnable swap rides along, only
  high-confidence fixes (proper noun / fold-equal / `sim ≥ STRICT_SIM 0.85`) are kept, so a
  letter-similar content edit like "deployed→deleted" is NOT learned. The doc-level gate still
  rejects wholesale rewrites.
- **Auto-capture** (`src/dictionary.rs`) — after each paste, `arm_with_baseline` snapshots the
  focused field via the macOS AX C API; `capture_with_current` diffs it on the next paste / a 12 s
  timer and feeds the cold path. **Silently no-ops in terminals** (focused AXValue > `MAX_FIELD`
  8000 → logged, not silent now). The reader is split from the core (`arm_with_baseline` /
  `capture_with_current`) so the logic is testable without AX.
- **Glue** — `src/dictionary.rs` (path beside config.toml + `init`), `app::run` calls
  `dictionary::init(cfg.dictionary_enabled)`, config gained `dictionary_enabled` (default true),
  entries persist to `<config_dir>/whisper-push/dictionary.toml`.
- **CLI / autonomous test loop** — `whisper-push dict {list,add,remove,learn,path}` (`dict add
  <name>` works with NO variants — bare names self-correct), `whisper-push capture-self-test`
  (deterministic auto-capture edit→learn scenarios, no model/GUI), `whisper-push self-test
  wav1 wav2` (acoustic loop). `tools/test_correction.sh [--e2e]` runs all layers. Golden corpus:
  `fixtures/{finalize,learn}.jsonl` (~300 cases); scorecard `dict_eval` (`--emit` snapshots).
  NOTE: a shell-launched binary is NOT AX-authorized (`AXUIElementCopyAttributeValue` → -25204
  even though `AXIsProcessTrusted()`=true) — only the installed daemon reads fields; the
  capture test therefore injects field text rather than reading a live one.
- **Tuning knobs** (named consts): fuzzy phonetic 0.72 / base 0.84 (finalize.rs); `PHON_GATE` 0.6,
  `STRICT_SIM` 0.85, rewrite cutoffs in learn.rs. False-positives are the priority.
- **Pending (Phase B/D):** in-app tray "Dictionary" submenu + "Correct Last Dictation" panel exist;
  V2 = per-model input biasing (Whisper `set_initial_prompt`; Voxtral needs a fork).
