<p align="center">
  <img src="icon.svg" width="80" height="80" alt="whisper-push icon">
</p>

<h1 align="center">whisper-push</h1>

<p align="center">
  <strong>Push-to-talk voice dictation, 100% local. Hold a key, speak, release — your words are typed wherever your cursor is.</strong>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> •
  <a href="#platform-support">Platforms</a> •
  <a href="#usage">Usage</a> •
  <a href="#configuration">Configuration</a> •
  <a href="#how-it-works">How it works</a>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-black?logo=apple" alt="macOS Apple Silicon">
  <img src="https://img.shields.io/badge/Linux-supported-blue?logo=linux&logoColor=white" alt="Linux">
  <img src="https://img.shields.io/badge/license-MIT-orange" alt="License">
</p>

---

No cloud, no account, no latency. Everything runs on your machine. On macOS it lives
in the menu bar and transcribes in **~0.15 s** with NVIDIA Parakeet via Apple MLX; on
Linux it's a CLI you bind to a hotkey, powered by faster-whisper.

## Quick start

### macOS (Apple Silicon — M1/M2/M3/M4)

**Option A — App (easiest):**
1. Download `Whisper-Push-macOS-arm64.dmg` from the [latest release](../../releases/latest).
2. Open it and drag **Whisper Push** to **Applications**.
3. The app isn't notarized, so the first launch is blocked. Clear the quarantine flag once:
   ```bash
   xattr -dr com.apple.quarantine "/Applications/Whisper Push.app"
   ```
   > No terminal? Double-click the app, then go to **System Settings → Privacy & Security**
   > and click **Open Anyway**. (On macOS 15+ the old right-click → Open trick is gone.)
4. Grant **Microphone**, **Accessibility**, and **Input Monitoring** when asked
   (System Settings → Privacy & Security).
5. Hold **Control**, speak, release. The first run downloads the model (~600 MB).

**Option B — From source:**
```bash
git clone https://github.com/Remenby31/whisper-push.git
cd whisper-push
./macos/install.sh
```

→ Full macOS docs: **[macos/README.md](macos/README.md)**

### Linux

```bash
git clone https://github.com/Remenby31/whisper-push.git
cd whisper-push
./install.sh
```

Then bind `whisper-push` to a hotkey (see [Linux setup](#linux-setup)).

## Platform support

| Platform | Status | Engine | Interface |
|----------|--------|--------|-----------|
| **macOS — Apple Silicon** | ✅ Supported | Parakeet (MLX) | Menu-bar app, hold-to-talk |
| **Linux** | ✅ Supported | faster-whisper | CLI + hotkey, toggle |
| macOS — Intel | ❌ Not supported | — | MLX requires an M-series chip |
| Windows | ❌ Not yet | — | Contributions welcome |

The macOS and Linux versions are independent implementations that share the same idea
and a TOML config format; they use different speech engines.

## macOS (Apple Silicon)

A menu-bar app — no terminal needed once installed.

- **Engine:** [NVIDIA Parakeet](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3) (`parakeet-tdt-0.6b-v3`) running on Apple MLX.
- **Latency:** ~0.15 s per phrase, kept warm in RAM (re-warms after sleep).
- **Languages:** auto-detects 25 European languages (incl. FR, EN, ES, DE, IT…).
- **Clipboard-safe:** your clipboard is restored after each paste.
- **Settings in the menu bar:** input/output device, hotkey, idle-unload, notifications,
  sounds — all written to a self-documenting `config.toml`.

Default hotkey: **hold Control**, speak, release. Configure everything from the 🎙 menu
bar icon or `Open Config (TOML)`.

See **[macos/README.md](macos/README.md)** for details, troubleshooting, and building the DMG.

## Linux

The original faster-whisper CLI. Press a hotkey to start recording, press again to
transcribe and type.

### Requirements

| Dependency | Purpose |
|------------|---------|
| [uv](https://github.com/astral-sh/uv) | Python package management |
| PipeWire | Audio recording (`pw-record`) |
| wl-clipboard | Clipboard access |
| ydotool | Keyboard simulation |
| NVIDIA GPU | CUDA acceleration (CPU fallback otherwise) |

### Linux setup

**One-file installer (recommended):**
```bash
./installer.sh
# or, auto-install deps + configure a GNOME hotkey:
WHISPER_PUSH_AUTO_DEPS=1 WHISPER_PUSH_HOTKEY="Super+V" ./installer.sh
```

**Manual:**
```bash
# 1. System dependencies (Arch example)
sudo pacman -S pipewire wl-clipboard ydotool
sudo usermod -aG input $USER   # required for ydotool; log out/in afterwards

# 2. Install
git clone https://github.com/Remenby31/whisper-push.git
cd whisper-push
./install.sh
```

Installer options: `WHISPER_PUSH_AUTO_DEPS=1` (auto deps), `WHISPER_PUSH_USE_CUDA=1`
(force CUDA), `WHISPER_PUSH_HOTKEY="Super+V"` (GNOME hotkey).

**Bind a hotkey:**
- GNOME: `./setup-hotkey.sh "Super+V"`
- KDE: System Settings → Shortcuts → Custom Shortcuts → command `~/.local/bin/whisper-push`

## Usage

**macOS:** hold the hotkey (default Control), speak, release → text is typed at the cursor.

**Linux:** press the hotkey to start (beep), speak, press again to transcribe and type.

```bash
whisper-push              # toggle recording/transcription (Linux)
whisper-push --status     # show status
whisper-push --stop       # force stop
whisper-push -l fr        # override language
whisper-push --doctor     # check dependencies
```

## Configuration

**macOS:** use the menu bar, or edit `~/Library/Application Support/whisper-push/config.toml`
(every option is documented inline).

**Linux:** edit `~/.config/whisper-push/config.toml`:

```toml
language = "auto"          # "auto" or ISO code ("fr", "en", "de", ...)
model = "large-v3-turbo"   # tiny, base, small, medium, large-v3, large-v3-turbo
compute_type = "int8"      # int8 (fast), float16, float32 (CPU)
device = "cuda"            # cuda, cpu, auto
notifications = true
sound_feedback = true
```

### Linux models (faster-whisper)

| Model | VRAM | Speed | Quality |
|:------|:----:|:-----:|:-------:|
| `tiny` | ~1 GB | ⚡⚡⚡⚡ | ★☆☆☆ |
| `base` | ~1 GB | ⚡⚡⚡ | ★★☆☆ |
| `small` | ~2 GB | ⚡⚡ | ★★★☆ |
| `medium` | ~3 GB | ⚡ | ★★★★ |
| `large-v3-turbo` | ~3 GB | ⚡⚡⚡ | ★★★★ |
| `large-v3` | ~5 GB | ⚡ | ★★★★★ |

**Recommended:** `large-v3-turbo` + `int8`. (macOS uses Parakeet and needs no model choice.)

## How it works

1. A global hotkey starts/stops capture (hold-to-talk on macOS, toggle on Linux).
2. Audio is recorded locally and fed straight to the speech model — no files, no network.
3. The transcript is placed at your cursor by simulating paste (clipboard restored on macOS).

Everything is offline after the one-time model download.

## Building the macOS DMG

```bash
brew install create-dmg          # optional, for a nicer DMG layout
./macos/build-dmg.sh             # → dist/Whisper-Push-macOS-arm64.dmg
```

The app is unsigned (no Apple Developer ID), so users de-quarantine it on first launch.

## Troubleshooting (Linux)

<details>
<summary><strong>ydotool not working</strong></summary>

Ensure you're in the `input` group (`groups | grep input`); if not, `sudo usermod -aG input $USER` and log out/in. Check the daemon: `systemctl --user status ydotool`.
</details>

<details>
<summary><strong>No sound recorded</strong></summary>

Verify `pw-record test.wav` (Ctrl+C to stop) then `pw-play test.wav`.
</details>

<details>
<summary><strong>CUDA errors</strong></summary>

Switch to CPU in config: `device = "cpu"`, `compute_type = "float32"`.
</details>

## Uninstall

- **macOS (app):** open the 🎙 menu → **Uninstall Whisper Push…**. This deletes
  your settings and the downloaded model (~600 MB), then moves the app to the
  Trash. Dragging the app to the Trash on its own leaves the model and settings
  behind, since macOS runs no cleanup code when an app is trashed.
- **macOS (source install):** `./macos/uninstall.sh`
- **Linux:** `./uninstall.sh`

## License

MIT
