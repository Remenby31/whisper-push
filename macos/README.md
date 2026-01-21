# Whisper Push - macOS Build Guide

## Compatibility

- **macOS 11.0 (Big Sur)** or later
- **Apple Silicon (M1/M2/M3/M4)** - Native ARM64 support
- **Intel Macs** - x86_64 support

## Prerequisites

Install required tools on your Mac:

```bash
# Install Homebrew (if not already installed)
/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"

# Install runtime dependency
brew install sox

# Optional: for icon conversion from SVG
brew install librsvg
```

## Building the DMG

1. Clone the repository:
```bash
git clone <repository-url>
cd whisper-push
```

2. Run the build script:
```bash
./macos/build-dmg.sh
```

3. The DMG will be created in `dist/`:
   - Apple Silicon: `Whisper-Push-macOS-arm64.dmg`
   - Intel: `Whisper-Push-macOS-x86_64.dmg`

## Installation

1. Open the DMG file
2. Drag "Whisper Push" to Applications
3. First launch: Right-click → Open (to bypass Gatekeeper)
4. Grant permissions when prompted:
   - **Microphone**: Required for voice recording
   - **Accessibility**: Required for typing transcribed text

## Usage

### As CLI Tool
```bash
# Toggle recording (run once to start, again to stop and transcribe)
/Applications/Whisper\ Push.app/Contents/MacOS/whisper-push

# Check status
/Applications/Whisper\ Push.app/Contents/MacOS/whisper-push --status

# Force stop
/Applications/Whisper\ Push.app/Contents/MacOS/whisper-push --stop

# Override language
/Applications/Whisper\ Push.app/Contents/MacOS/whisper-push --language fr
```

### Keyboard Shortcut Setup

1. Open **System Settings** → **Keyboard** → **Keyboard Shortcuts**
2. Click **Services** → **General**
3. Add a new service or use Automator to create a Quick Action that runs:
   ```bash
   /Applications/Whisper\ Push.app/Contents/MacOS/whisper-push
   ```
4. Assign your preferred hotkey (e.g., `⌘⇧Space`)

Alternatively, use a tool like **Hammerspoon** or **Karabiner-Elements** for global hotkeys.

## Configuration

Configuration file location: `~/Library/Application Support/whisper-push/config.toml`

```toml
# Language: "auto" for auto-detection, or ISO code ("fr", "en", "de", ...)
language = "auto"

# Whisper model: tiny, base, small, medium, large-v3, large-v3-turbo
model = "large-v3-turbo"

# Precision: int8 (recommended for Apple Silicon), float32
compute_type = "int8"

# Device: cpu (recommended), auto
device = "cpu"

# Notifications on start/stop
notifications = true

# Sound feedback on start/stop
sound_feedback = true

# Transcription beam size (higher = more accurate, slower)
beam_size = 5
```

## Apple Silicon Performance

On M1/M2/M3/M4 Macs, the application uses CPU inference with int8 quantization, which provides excellent performance thanks to Apple's efficient ARM cores. The first transcription will download the Whisper model (~1.5GB for large-v3-turbo).

**Expected performance on M4:**
- Model loading: ~2-3 seconds (first run only)
- Transcription speed: ~10-20x real-time (10s audio → <1s processing)

## Troubleshooting

### "whisper-push" cannot be opened because the developer cannot be verified

Right-click the app → Open → Open

Or remove quarantine attribute:
```bash
xattr -d com.apple.quarantine /Applications/Whisper\ Push.app
```

### Microphone permission denied

Go to **System Settings** → **Privacy & Security** → **Microphone** and enable "Whisper Push"

### Keyboard simulation not working

Go to **System Settings** → **Privacy & Security** → **Accessibility** and enable "Whisper Push"

### sox/rec not found

```bash
brew install sox
```

## Code Signing (Optional)

To distribute the DMG, sign it with your Developer ID:

```bash
# Set your Developer ID
export DEVELOPER_ID="Developer ID Application: Your Name (TEAMID)"

# Run build script (will auto-sign)
./macos/build-dmg.sh
```

For notarization (required for distribution outside App Store):
```bash
xcrun notarytool submit dist/Whisper-Push-macOS-arm64.dmg \
    --apple-id "your@email.com" \
    --team-id "TEAMID" \
    --password "@keychain:AC_PASSWORD" \
    --wait

xcrun stapler staple dist/Whisper-Push-macOS-arm64.dmg
```
