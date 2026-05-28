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
- Stays loaded in RAM for instant transcription; idle unload after N minutes (configurable)

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

## Recent additions (branch `settings-and-brandkit`)

Enhancements layered on top of the existing modules — no new architectural pieces.

- **`tray/mod.rs`** — Engine / Hotkey / Input Device / Output Device / Permissions are now real `Submenu` dropdowns (needed `tray-icon 0.24` + `muda 0.19`: the old `0.16` had a Tahoe hover-close bug). Permissions submenu is always visible with a ✓ / ⚠ title and a "Run Guided Setup…" item.
- **`hotkey/macos.rs`** — match config is now live-mutable (`Mutex<Option<MatchConfig>>`), so preset switches and custom captures take effect without restart. `start_capture(tx)` arms a capture mode: tap a modifier → hold hotkey; press modifiers+key → toggle hotkey. Result arrives as `Event::HotkeyCaptured`. Keycode↔name table covers letters, digits, space, return, tab, escape.
- **`permissions.rs`** — adds Input Monitoring (`IOHIDCheckAccess`/`IOHIDRequestAccess`) to `PermissionStatus`. `guided_setup()` opens the relevant Settings panes, polls for grants, then `launchctl kickstart -k` to restart the daemon with permissions in place.
- **`audio/playback.rs`** — respects `output_device` via a static `RwLock<String>` set from config (was always using `default_output_device`). **`audio/mod.rs`** — `list_output_devices()` companion to `list_devices()`. Note that on macOS, device *enumeration* needs no mic permission — TCC only gates capture.
- **`transcribe/mod.rs`** — `model_path()` checks the `.app/Contents/Resources/models/` bundle path first (bundled DMG install), falls back to the user data dir (downloaded on first run). `transcribe_with_backend(Parakeet)` falls back to Whisper on any error, so transcription never hard-fails.
- **`transcribe/parakeet.rs`** — fixed the download repo to `onnx-community/parakeet-ctc-0.6b-ONNX` (`model.onnx` + `tokenizer.json` + optional `model.onnx_data`); the previous `nvidia/...` repo ships `.nemo` files only.
- **Sound feedback** — "start" sound is now played immediately on `HotkeyDown` (not after `hold_delay`), so the user gets an instant audio cue.
- **Menu-bar icons** — redrawn from the brand kit. Idle is a *template* image (auto-adapts to light/dark menu bar); recording is **citron #CEDC00** (brand glyph from `brandkit/icon/svg/whisperpush-icon-transparent-citron.svg`).
- **Makefile** — `make install` copies the bundle to `/Applications` and writes the login `LaunchAgent`. `make uninstall` reverses it. `make dmg` bundles `~/Library/Application Support/whisper-push/models/ggml-large-v3-turbo-q5_0.bin` into `Contents/Resources/models/` **before** signing, so the distributed DMG (~528 MB) gives a zero-download first launch. `make install` stays slim — only `make dmg` ships the model.
- **App icon** — `resources/AppIcon.icns` generated from the brand kit squircle PNGs, referenced by `Info.plist` (`CFBundleIconFile`).
