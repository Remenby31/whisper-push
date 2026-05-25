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
- **NSEvent global monitor**: requires Accessibility permission on macOS
- **evdev on Linux**: requires user in 'input' group (`sudo usermod -aG input $USER`)
- **Windows keyboard hook**: WH_KEYBOARD_LL needs a message loop on the hook thread
