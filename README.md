<p align="center">
  <img src="icon.svg" width="80" height="80" alt="whisper-push icon">
</p>

<h1 align="center">whisper-push</h1>

<p align="center">
  <strong>Push-to-talk voice dictation powered by Whisper</strong>
</p>

<p align="center">
  <a href="#installation">Installation</a> •
  <a href="#usage">Usage</a> •
  <a href="#configuration">Configuration</a> •
  <a href="#models">Models</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/platform-Linux%20%7C%20macOS-blue" alt="Platform">
  <img src="https://img.shields.io/badge/license-MIT-orange" alt="License">
</p>

---

Press a hotkey to start recording, press again to transcribe and type. Local processing with [faster-whisper](https://github.com/SYSTRAN/faster-whisper) — no cloud, no latency.

## Features

- **Toggle mode** — Press to record, press again to transcribe
- **Local & private** — Runs entirely on your machine
- **Fast** — Uses faster-whisper with INT8 quantization
- **Streaming output** — Text appears segment by segment as it's transcribed
- **Multilingual** — Auto-detects language or use a fixed one
- **Clipboard restore** — Preserves your clipboard content

## Performance

Tested with `large-v3-turbo` + `int8` on RTX 3070:

| Metric | Value |
|--------|-------|
| VRAM usage | **~1.2 GB** |
| Transcription speed | **3.7x real-time** |
| First load | ~1.8s (model cached after) |

A 10-second recording transcribes in ~2.7s.

## macOS

For macOS (Apple Silicon & Intel), see **[macos/README.md](macos/README.md)**.

Quick install:
```bash
./macos/install.sh
```

---

## Linux

### Requirements

| Dependency | Purpose |
|------------|---------|
| [uv](https://github.com/astral-sh/uv) | Python package management |
| PipeWire | Audio recording (`pw-record`) |
| wl-clipboard | Clipboard access |
| ydotool | Keyboard simulation |
| NVIDIA GPU | CUDA acceleration (or CPU fallback) |

### Installation

#### 1. Install system dependencies

```bash
# Arch Linux
sudo pacman -S pipewire wl-clipboard ydotool

# Add yourself to the input group (required for ydotool)
sudo usermod -aG input $USER
# Log out and back in for this to take effect
```

#### 2. Clone and install

```bash
git clone https://github.com/Remenby31/whisper-push.git
cd whisper-push
./install.sh
```

#### 3. Set up keyboard shortcut

Bind `whisper-push` to a hotkey in your desktop settings (e.g., `Super+V`).

**KDE Plasma:**
1. System Settings → Shortcuts → Custom Shortcuts
2. Edit → New → Global Shortcut → Command/URL
3. Set trigger key and command: `/home/YOUR_USER/.local/bin/whisper-push`

## Usage

1. **Press** the hotkey → recording starts (you'll hear a beep)
2. **Speak**
3. **Press again** → transcribes and types the text

### CLI

```bash
whisper-push              # Toggle recording/transcription
whisper-push --status     # Show current status
whisper-push --stop       # Force stop recording
whisper-push -l fr        # Override language to French
```

## Configuration

Edit `~/.config/whisper-push/config.toml`:

```toml
# Language: "auto" or ISO code ("fr", "en", "de", ...)
language = "auto"

# Model: tiny, base, small, medium, large-v3, large-v3-turbo
model = "large-v3-turbo"

# Precision: int8 (fast), float16 (balanced), float32 (CPU)
compute_type = "int8"

# Device: cuda, cpu, auto
device = "cuda"

# Feedback
notifications = true
sound_feedback = true

# Debug: save recordings to ~/.cache/whisper-push-last.wav
debug = false
```

## Models

| Model | VRAM | Speed | Quality |
|:------|:----:|:-----:|:-------:|
| `tiny` | ~1 GB | ⚡⚡⚡⚡ | ★☆☆☆ |
| `base` | ~1 GB | ⚡⚡⚡ | ★★☆☆ |
| `small` | ~2 GB | ⚡⚡ | ★★★☆ |
| `medium` | ~3 GB | ⚡ | ★★★★ |
| `large-v3-turbo` | ~3 GB | ⚡⚡⚡ | ★★★★ |
| `large-v3` | ~5 GB | ⚡ | ★★★★★ |

**Recommended:** `large-v3-turbo` + `int8`

## Troubleshooting

<details>
<summary><strong>ydotool not working</strong></summary>

1. Ensure you're in the `input` group:
   ```bash
   groups | grep input
   ```
2. If not, add yourself and **log out/in**:
   ```bash
   sudo usermod -aG input $USER
   ```
3. Check the daemon is running:
   ```bash
   systemctl --user status ydotool
   ```
</details>

<details>
<summary><strong>No sound recorded</strong></summary>

Check that `pw-record` works:
```bash
pw-record test.wav  # Ctrl+C to stop
pw-play test.wav
```
</details>

<details>
<summary><strong>CUDA errors</strong></summary>

Try CPU mode in config:
```toml
device = "cpu"
compute_type = "float32"
```
</details>

## Uninstall

```bash
./uninstall.sh
```

## License

MIT
