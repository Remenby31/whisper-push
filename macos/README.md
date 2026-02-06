# Whisper Push - macOS

Push-to-talk voice dictation using Whisper, optimized for Apple Silicon.

## Quick Install

```bash
# Clone and install
git clone <repository-url>
cd whisper-push
./macos/install.sh
```

That's it! The installer will:
- Install dependencies (sox, Python packages)
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
- Start/Stop Recording
- Cancel Recording
- Open Config
- View Logs
- Quit

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

# Language: "auto" or ISO code ("fr", "en", "de", ...)
language = "auto"

# Whisper model: tiny, base, small, medium, large-v3, large-v3-turbo
model = "large-v3-turbo"

# Precision: int8 (recommended), float32
compute_type = "int8"

# Notifications and sound feedback
notifications = true
sound_feedback = true
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

## Compatibility

- **macOS 11.0+** (Big Sur or later)
- **Apple Silicon** (M1/M2/M3/M4) - Native ARM64
- **Intel Macs** - x86_64 support

## Performance (Apple Silicon)

- First run downloads Whisper model (~1.5GB)
- Model loading: ~2-3 seconds (first transcription only)
- Transcription: ~10-20x real-time

## Troubleshooting

### Hotkey not working

1. Check Accessibility permission in System Settings
2. View logs: `cat ~/Library/Application\ Support/whisper-push/hotkey.log`
3. Restart daemon: `launchctl kickstart -k gui/$(id -u)/com.whisper-push.hotkey`

### "Cannot be opened because the developer cannot be verified"

```bash
xattr -d com.apple.quarantine /Applications/Whisper\ Push.app
```

### Microphone not recording

Grant Microphone permission in System Settings → Privacy & Security → Microphone

### sox/rec not found

```bash
brew install sox
```

## Files

| Path | Description |
|------|-------------|
| `~/Library/Application Support/whisper-push/config.toml` | Configuration |
| `~/Library/Application Support/whisper-push/hotkey.log` | Daemon logs |
| `~/Library/LaunchAgents/com.whisper-push.hotkey.plist` | Auto-start config |
| `~/.cache/huggingface/` | Whisper models |
