# Whisper Push - macOS

Push-to-talk voice dictation using NVIDIA Parakeet, optimized for Apple Silicon.
Transcription is near-instant (~0.15s) and runs fully offline — audio is fed to
the model in-memory, with no ffmpeg or temporary files.

## Quick Install

```bash
# Clone and install
git clone <repository-url>
cd whisper-push
./macos/install.sh
```

That's it! The installer will:
- Install dependencies (Python packages: parakeet-mlx, sounddevice, ...)
- Add a **menu bar icon** (🎤) for status and control
- Set up auto-start on login
- Configure default hotkey: **Hold Control (ctrl)**
- Request necessary permissions

## Menu Bar

After installation, you'll see the Whisper Push icon in your menu bar. The icon changes color based on status:

| Color | Status |
|-------|--------|
| Purple | Idle - ready to record |
| Red | Recording in progress |
| Orange | Processing transcription |

Click the icon to access:
- Start/Stop Recording, Cancel Recording
- **Input / Output device** (submenus)
- **Hotkey** (submenu of presets — hold Control/Right-Control/…, or toggle ⌘⇧Space)
- **Idle unload** (free the model from RAM after N min idle)
- **Notifications / Sound feedback / Debug logging** (checkboxes)
- **Open Config (TOML)** — opens a fully self-documenting config file
- View Logs, Quit

All settings apply live and are written to `config.toml`. The device and hotkey
submenus refresh when the menu opens. Editing `config.toml` directly is also
fine — every option is documented in the file with its valid values.

Your clipboard is preserved: the transcription is pasted, then your previous
clipboard contents are restored automatically.

## Usage

1. Hold **Control** (or click 🎤 → Start Recording)
2. Speak your text
3. Release **Control** to stop
4. Text is automatically typed at cursor position

## Configuration

Edit `~/Library/Application Support/whisper-push/config.toml`:

```toml
# Global hotkey (toggle or hold)
# Modifiers: cmd, shift, alt (option), ctrl
# Keys: a-z, 0-9, space, return, f1-f12
hotkey = "ctrl"

# Hotkey mode: "toggle" or "hold"
# For hold-to-talk with Control only, use:
# hotkey_mode = "hold"
# hotkey = "rctrl"  # right control recommended to avoid conflicts
hotkey_mode = "hold"

# Language: Parakeet v3 auto-detects the language (no need to set it).
# Covers 25 European languages: bg, hr, cs, da, nl, en, et, fi, fr, de,
# el, hu, it, lv, lt, mt, pl, pt, ro, sk, sl, es, sv, ru, uk
language = "auto"

# Model (stays loaded in RAM for instant transcription):
#   parakeet-tdt-0.6b-v3 - multilingual, fastest, recommended
model = "parakeet-tdt-0.6b-v3"

# Free the model from RAM (~1.3GB) after N minutes idle (0 = always resident).
# When set, the model reloads while you record, so the cost is hidden.
idle_unload_minutes = 0

# Notifications and sound feedback
notifications = true
sound_feedback = true

# Audio device selection: "auto" or exact device name
# Pick devices interactively from the menu bar dropdown
input_device = "auto"
output_device = "auto"
```

After changing the hotkey, restart the daemon:
```bash
launchctl kickstart -k gui/$(id -u)/com.whisper-push.hotkey
```

## Permissions

The app requires two permissions:

1. **Microphone** - For voice recording
2. **Accessibility** - For global hotkeys and typing text

Go to **System Settings → Privacy & Security** to grant these permissions to Terminal or Python.

## Uninstall

```bash
./macos/uninstall.sh
```

## Manual Installation (DMG)

If you prefer to build a standalone app:

```bash
./macos/build-dmg.sh
```

Then:
1. Open `dist/Whisper-Push-macOS-*.dmg`
2. Drag to Applications
3. Run `./macos/install.sh` to set up hotkey daemon

## Code signing & first launch

`build-dmg.sh` produces an **ad-hoc signed (effectively unsigned)** app. It is
not notarized, so on first launch Gatekeeper blocks it. The user clears the
quarantine flag once:

```bash
xattr -dr com.apple.quarantine "/Applications/Whisper Push.app"
```

This works on **every macOS version**. On macOS 15+ the old right-click → Open
bypass is gone — without the command, the alternative is System Settings →
Privacy & Security → **Open Anyway** after the first blocked launch.

> **Don't ship a self-signed certificate.** It lives only in the builder's
> keychain, so it gives downloaders no Gatekeeper trust (it isn't notarized), and
> a stale `--deep` seal can trigger *"the app is damaged"* — which de-quarantine
> cannot fix. Ad-hoc is the reliable path for distribution.

**To make it just work (no terminal command):** get an Apple Developer account
($99/yr), and build + notarize with a real Developer ID:

```bash
WHISPER_PUSH_SIGN_IDENTITY="Developer ID Application: …" ./macos/build-dmg.sh
# then: xcrun notarytool submit … && xcrun stapler staple "dist/…dmg"
```

A real Developer ID is also what lets macOS (TCC) keep the **Accessibility**
grant across updates instead of re-prompting. After switching signing identity,
reset any stale grant once: `tccutil reset Accessibility com.whisper-push.app`,
then grant again.

## Compatibility

- **macOS 14.0+** (Sonoma or later) — required by the bundled MLX runtime
- **Apple Silicon only** (M1/M2/M3/M4) — MLX/Parakeet are arm64-only; no Intel

## Performance (Apple Silicon)

- First run downloads the Parakeet model (~600MB)
- Model loading + warmup: ~2 seconds at daemon startup (once)
- Transcription: ~0.15s per phrase (cost scales with clip length, not padded)

### Memory management

- The model stays warm in RAM (~1.3GB) so transcription is instant. The MLX GPU
  buffer cache is bounded and released after each transcription, so the idle
  footprint stays at the weights only.
- On **wake from sleep**, the model is re-warmed in the background (its pages may
  have been compressed while asleep), so the first transcription after waking
  isn't slow.
- Set `idle_unload_minutes` in the config to free the model after a period of
  inactivity. It reloads automatically while you record, so the reload time is
  hidden behind your speech.

## Troubleshooting

### Hotkey not working

1. Check Accessibility permission in System Settings
2. View logs: `cat ~/Library/Application\ Support/whisper-push/daemon.log`
3. Restart daemon: `launchctl kickstart -k gui/$(id -u)/com.whisper-push.hotkey`

### "Cannot be opened because the developer cannot be verified" / "is damaged"

```bash
xattr -dr com.apple.quarantine "/Applications/Whisper Push.app"
```

`-r` (recursive) matters: it clears the flag on the nested binaries too. This
works on every macOS version. On macOS 15+ you can also approve it via System
Settings → Privacy & Security → **Open Anyway** (the old right-click → Open
bypass was removed).

### Microphone not recording

Grant Microphone permission in System Settings → Privacy & Security → Microphone

### No audio / wrong device

Select the correct input/output device from the menu bar dropdown submenus. Set to "Auto" to use the built-in mic heuristic.

## Files

| Path | Description |
|------|-------------|
| `~/Library/Application Support/whisper-push/config.toml` | Configuration |
| `~/Library/Application Support/whisper-push/daemon.log` | App logs (rotating, max ~2MB ×3) |
| `~/Library/Application Support/whisper-push/daemon-stderr.log` | Native crash sink (uncaught errors) |
| `~/Library/LaunchAgents/com.whisper-push.hotkey.plist` | Auto-start config |
| `~/.cache/huggingface/` | Downloaded models |

For verbose per-keystroke logging when debugging the hotkey, set `debug = true`
in `config.toml` and restart the daemon.
