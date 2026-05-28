<p align="center">
  <img src="icon.svg" width="80" height="80" alt="whisper-push icon">
</p>

<h1 align="center">whisper-push</h1>

<p align="center">
  <strong>Push-to-talk voice dictation, 100% local. Hold a key, speak, release — your words are typed wherever your cursor is.</strong>
</p>

<p align="center">
  <a href="https://github.com/Remenby31/whisper-push/releases/latest"><img src="https://img.shields.io/github/v/release/Remenby31/whisper-push" alt="Release"></a>
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-black?logo=apple" alt="macOS">
  <img src="https://img.shields.io/badge/Linux-supported-blue?logo=linux&logoColor=white" alt="Linux">
  <img src="https://img.shields.io/badge/Windows-supported-blue?logo=windows&logoColor=white" alt="Windows">
  <img src="https://img.shields.io/badge/Rust-native-orange?logo=rust" alt="Rust">
  <img src="https://img.shields.io/badge/binary-35MB-green" alt="35MB">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-orange" alt="MIT"></a>
</p>

---

No cloud, no account, no latency. A **35MB native binary** that transcribes speech using GPU acceleration. Three transcription engines, every platform optimized.

## Transcription Engines

| | **macOS (Apple Silicon)** | **Linux NVIDIA** | **Linux AMD/Intel** | **Windows NVIDIA** | **Windows AMD/Intel** | **CPU (any)** |
|---|---|---|---|---|---|---|
| **Parakeet TDT v3** | Metal (WebGPU) | ONNX + CUDA | ONNX + WebGPU | ONNX + CUDA | ONNX + DirectML | ONNX CPU |
| **Voxtral Realtime 2602** | Burn + WGPU | Burn + Vulkan | Burn + Vulkan | Burn + Vulkan | Burn + Vulkan | Burn CPU |
| **Whisper large-v3-turbo** | whisper.cpp Metal | whisper.cpp CUDA | whisper.cpp Vulkan | whisper.cpp CUDA | whisper.cpp Vulkan | whisper.cpp CPU |

### Performance (10 seconds of audio)

| Engine | macOS Metal | CUDA (RTX 4070) | CPU |
|---|---|---|---|
| **Parakeet** | ~27ms | ~50ms | ~500ms |
| **Voxtral Q4** | ~400ms | ~300ms | ~3s |
| **Whisper turbo Q5** | ~1.2s | ~200ms | ~5-10s |

### Streaming

| Engine | Streaming? | How |
|---|---|---|
| **Voxtral Realtime** | **Yes** — words appear while speaking | Causal encoder, incremental decode |
| **Parakeet** (Nemotron) | Planned | Chunked audio, EOU detection |
| **Whisper** | No | Batch only |

### Accuracy (WER)

| Engine | English | Multilingual |
|---|---|---|
| **Parakeet** | **1.69%** | 25 EU languages |
| **Voxtral** | 4.90% | 13 languages |
| **Whisper** | 2.70% | **99+ languages** |

### Binary vs Python

| | Python (v1) | Rust (v2) |
|---|---|---|
| **Binary** | ~600MB (PyInstaller) | **35MB** |
| **Dependencies** | Python, PyObjC, sounddevice, scipy | **None** |
| **Startup** | ~3s | **<100ms** |
| **Memory (idle)** | ~200MB | **~15MB** |

## Quick Start

### macOS (Apple Silicon)

**Easiest — download the DMG** (the Whisper model is bundled, so first launch is instant):

1. Grab **[Whisper-Push-macOS-arm64.dmg](https://github.com/Remenby31/whisper-push/releases/latest)** (~528 MB)
2. Open it and drag **Whisper Push** to **Applications**
3. Launch from Launchpad / Applications — a guided setup opens to walk you through the three macOS permissions (Microphone, Accessibility, Input Monitoring). It detects when you toggle each one and restarts the app automatically.

> Ad-hoc signed for now. On first launch macOS shows "unverified" — open **System Settings → Privacy & Security → Open Anyway**, or, if you prefer the command line: `xattr -dr com.apple.quarantine "/Applications/Whisper Push.app"`. A notarized build (Developer ID) is on the roadmap.

**Build from source instead:**

```bash
git clone https://github.com/Remenby31/whisper-push.git
cd whisper-push
make install SIGN_ID="-"   # build + bundle + ad-hoc sign + copy to /Applications + login autostart
```

The model (~550MB) downloads automatically on first run when built from source (the bundled-model is only in the DMG path).

### Linux

```bash
curl -sSL https://raw.githubusercontent.com/Remenby31/whisper-push/main/scripts/install.sh | sh
```

### Usage

| Action | How |
|---|---|
| **Dictate** | Hold **Control** → speak → release. Menu-bar icon turns citron while you speak. |
| **Settings** | Click the menu-bar icon — submenus for **Engine**, **Hotkey**, **Input Device**, **Output Device**, **Permissions** |
| **Change hotkey** | Menu → **Hotkey** → pick a preset, or **Set Custom Hotkey…** then press any combo |
| **Switch engine** | Menu → **Engine** → Parakeet / Voxtral / Whisper |
| **Pick microphone / output** | Menu → **Input Device** / **Output Device** |
| **Fix permissions** | Menu → **Permissions → ⚙ Run Guided Setup…** (opens the right Settings panes + auto-restarts) |
| **Test** | Menu → "Test (record 3s + transcribe)" |
| **Check setup** | `whisper-push --doctor` |
| **Transcribe file** | `whisper-push --transcribe audio.mp3` |
| **List models** | `whisper-push --models` |

### Configuration

Settings are in the menu bar. Config file location:
- macOS: `~/Library/Application Support/whisper-push/config.toml`
- Linux: `~/.config/whisper-push/config.toml`
- Windows: `%APPDATA%/whisper-push/config.toml`

## Building from Source

```bash
# Prerequisites: Rust 1.83+, cmake

# macOS (all engines)
cargo build --release --features "metal,parakeet,voxtral"
make deploy

# Linux (CPU)
cargo build --release

# Linux (NVIDIA CUDA)
cargo build --release --features cuda

# Transcribe a file
./target/release/whisper-push --transcribe audio.mp3
```

## Architecture

```
src/
├── main.rs               # CLI + app entry
├── config.rs             # TOML config + platform paths
├── state.rs              # State machine + events
├── permissions.rs        # macOS TCC (mic, accessibility)
├── hardware.rs           # GPU detection + engine recommendation
├── model_manager.rs      # Model download + status
├── onboarding.rs         # First-launch wizard
├── autostart.rs          # Auto-start on login (all platforms)
├── notify.rs             # OS notifications
├── overlay.rs            # Floating overlay (live transcription)
├── audio/
│   ├── capture.rs        # cpal input → 16kHz mono f32
│   ├── decode.rs         # MP3/WAV/OGG/FLAC decoder (symphonia)
│   ├── playback.rs       # Start/stop sounds (embedded)
│   └── stream.rs         # Streaming capture (500ms chunks)
├── transcribe/
│   ├── mod.rs            # Whisper backend (whisper-rs)
│   ├── parakeet.rs       # Parakeet backend (ONNX)
│   └── voxtral_local.rs  # Voxtral Q4 backend (Burn + WGPU) + streaming
├── hotkey/
│   ├── macos.rs          # CGEventTap
│   ├── linux.rs          # evdev
│   └── windows.rs        # WH_KEYBOARD_LL
├── paste/
│   └── mod.rs            # CGEvent paste (macOS) / enigo (Linux/Windows)
└── tray/
    └── mod.rs            # System tray + menus (winit + tray-icon)
```

## License

MIT
