#!/bin/bash
#
# Whisper Push - macOS Complete Installation Script
# Installs the app, configures hotkey daemon, and sets up auto-start
#
# Tested on: macOS 11+ (Big Sur, Monterey, Ventura, Sonoma, Sequoia)
# Architectures: Apple Silicon (M1/M2/M3/M4) and Intel
#
set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[✓]${NC} $1"; }
info() { echo -e "${BLUE}[i]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }
error() { echo -e "${RED}[✗]${NC} $1"; exit 1; }

# Paths
APP_NAME="Whisper Push"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
SUPPORT_DIR="$HOME/Library/Application Support/whisper-push"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
PLIST_NAME="com.whisper-push.hotkey.plist"
VENV_DIR="$SUPPORT_DIR/venv"
HOTKEY_SCRIPT="$SUPPORT_DIR/hotkey-daemon.py"
CONFIG_FILE="$SUPPORT_DIR/config.toml"

echo ""
echo "=========================================="
echo -e "${GREEN}  Whisper Push - macOS Installer${NC}"
echo "=========================================="
echo ""

# =============================================================================
# Step 1: Check macOS version and architecture
# =============================================================================

MACOS_VERSION=$(sw_vers -productVersion)
MACOS_MAJOR=$(echo "$MACOS_VERSION" | cut -d. -f1)
info "macOS version: $MACOS_VERSION"

if [[ "$MACOS_MAJOR" -lt 11 ]]; then
    error "macOS 11.0 (Big Sur) or later required"
fi

ARCH=$(uname -m)
info "Architecture: $ARCH"

# =============================================================================
# Step 2: Check/Install Homebrew
# =============================================================================

echo ""
info "Checking Homebrew..."

# Determine Homebrew path based on architecture
if [[ "$ARCH" == "arm64" ]]; then
    BREW_PATH="/opt/homebrew/bin/brew"
else
    BREW_PATH="/usr/local/bin/brew"
fi

if [[ -x "$BREW_PATH" ]]; then
    eval "$("$BREW_PATH" shellenv)"
    log "Homebrew found at $BREW_PATH"
elif command -v brew &> /dev/null; then
    log "Homebrew found in PATH"
else
    echo ""
    warn "Homebrew is not installed."
    warn "Homebrew is required to install dependencies (sox, python)."
    echo ""
    echo "To install Homebrew, run:"
    echo '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
    echo ""
    echo "Then run this installer again."
    exit 1
fi

# =============================================================================
# Step 3: Install system dependencies
# =============================================================================

echo ""
info "Checking system dependencies..."

# ffmpeg is optional: the daemon feeds recorded audio to the model in-memory
# and never shells out to ffmpeg. We only install it as a convenience.
if ! command -v ffmpeg &> /dev/null; then
    info "Installing ffmpeg (optional convenience)..."
    brew install ffmpeg || warn "ffmpeg install skipped (not required)"
fi

# Python 3 (prefer Homebrew Python for consistency)
PYTHON_CMD=""
PYTHON_VERSION=""

# Try Homebrew Python first
for py in python3.12 python3.11 python3; do
    if command -v "$py" &> /dev/null; then
        ver=$("$py" --version 2>&1 | cut -d' ' -f2)
        major=$(echo "$ver" | cut -d. -f1)
        minor=$(echo "$ver" | cut -d. -f2)
        if [[ "$major" -ge 3 ]] && [[ "$minor" -ge 11 ]]; then
            PYTHON_CMD="$py"
            PYTHON_VERSION="$ver"
            break
        fi
    fi
done

if [[ -z "$PYTHON_CMD" ]]; then
    info "Installing Python 3.12 (required for tomllib)..."
    brew install python@3.12
    PYTHON_CMD="python3.12"
    PYTHON_VERSION=$("$PYTHON_CMD" --version 2>&1 | cut -d' ' -f2)
fi

log "Python $PYTHON_VERSION found ($PYTHON_CMD)"

# librsvg for SVG to PNG conversion (optional but recommended)
if ! command -v rsvg-convert &> /dev/null; then
    info "Installing librsvg (for menu bar icons)..."
    brew install librsvg || warn "Could not install librsvg, will use fallback icons"
fi

# =============================================================================
# Step 4: Create directories
# =============================================================================

echo ""
info "Creating directories..."
mkdir -p "$SUPPORT_DIR"
mkdir -p "$SUPPORT_DIR/icons"
mkdir -p "$LAUNCH_AGENTS_DIR"
log "Directories created"

# =============================================================================
# Step 5: Create Python virtual environment with dependencies
# =============================================================================

echo ""
info "Setting up Python environment..."

# Always recreate venv to ensure clean state
if [[ -d "$VENV_DIR" ]]; then
    rm -rf "$VENV_DIR"
fi

"$PYTHON_CMD" -m venv "$VENV_DIR"
source "$VENV_DIR/bin/activate"

# Upgrade pip silently
pip install --quiet --upgrade pip

# Install PyObjC for menu bar and hotkeys
info "Installing PyObjC (this may take a minute)..."
pip install --quiet pyobjc-framework-Cocoa pyobjc-framework-Quartz

# Install parakeet-mlx for transcription (NVIDIA Parakeet on Apple Silicon GPU)
info "Installing parakeet-mlx (Apple Silicon optimized)..."
pip install --quiet parakeet-mlx

# Install sounddevice for audio recording (uses CoreAudio natively)
info "Installing audio dependencies..."
pip install --quiet sounddevice soundfile numpy scipy

deactivate

log "Python environment ready"

# =============================================================================
# Step 6: Install sound effects
# =============================================================================

echo ""
info "Installing sound effects..."

# Copy sounds (start/stop feedback)
if [[ -d "$PROJECT_ROOT/sounds" ]]; then
    cp -r "$PROJECT_ROOT/sounds" "$SUPPORT_DIR/"
    log "Sounds installed"
else
    warn "sounds/ not found in $PROJECT_ROOT, sound feedback disabled"
fi

# =============================================================================
# Step 7: Create Spotlight-visible app in /Applications
# =============================================================================

echo ""
info "Creating Whisper Push app for Spotlight..."

APP_PATH="/Applications/Whisper Push.app"

# Remove old app if exists
rm -rf "$APP_PATH" 2>/dev/null || true

# Create app bundle structure
mkdir -p "$APP_PATH/Contents/MacOS"
mkdir -p "$APP_PATH/Contents/Resources"

# Create the launcher script (runs Python directly as a GUI app)
cat > "$APP_PATH/Contents/MacOS/Whisper Push" << LAUNCHER
#!/bin/bash
# Check if daemon already running (match python process only, not shell scripts)
if pgrep -xf '.*[Pp]ython.* hotkey-daemon.py' > /dev/null 2>&1; then
    exit 0
fi

export PYTHONUNBUFFERED=1
# The daemon owns daemon.log (rotating). Native stdout/stderr (uncaught
# exceptions, Metal/native crashes) go to a separate, small crash sink.
exec "${VENV_DIR}/bin/python3" -u "${HOTKEY_SCRIPT}" >> "${SUPPORT_DIR}/daemon-stderr.log" 2>&1
LAUNCHER

chmod +x "$APP_PATH/Contents/MacOS/Whisper Push"

# Create Info.plist
cat > "$APP_PATH/Contents/Info.plist" << 'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>Whisper Push</string>
    <key>CFBundleIdentifier</key>
    <string>com.whisper-push.app</string>
    <key>CFBundleName</key>
    <string>Whisper Push</string>
    <key>CFBundleDisplayName</key>
    <string>Whisper Push</string>
    <key>CFBundleVersion</key>
    <string>1.0.0</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSUIElement</key>
    <true/>
</dict>
</plist>
PLIST

# Copy app icon
if [[ -f "$SCRIPT_DIR/whisper-push.icns" ]]; then
    cp "$SCRIPT_DIR/whisper-push.icns" "$APP_PATH/Contents/Resources/AppIcon.icns"
    log "App icon installed"
else
    warn "whisper-push.icns not found, app will use default icon"
fi

# Force Spotlight to index the app
touch "$APP_PATH"
mdimport "$APP_PATH" 2>/dev/null || true

log "Whisper Push.app created (searchable via Spotlight)"

# =============================================================================
# Step 8: Install menu bar daemon
# =============================================================================

echo ""
info "Installing menu bar daemon..."

if [[ -f "$SCRIPT_DIR/menubar-daemon.py" ]]; then
    cp "$SCRIPT_DIR/menubar-daemon.py" "$HOTKEY_SCRIPT"
else
    error "menubar-daemon.py not found in $SCRIPT_DIR"
fi

chmod +x "$HOTKEY_SCRIPT"
log "Menu bar daemon installed"

# =============================================================================
# Step 8: Convert and install menu bar icons
# =============================================================================

echo ""
info "Installing menu bar icons..."

ICONS_SRC="$SCRIPT_DIR/icons"
ICONS_DST="$SUPPORT_DIR/icons"

if command -v rsvg-convert &> /dev/null; then
    for state in idle recording processing; do
        if [[ -f "$ICONS_SRC/icon-${state}.svg" ]]; then
            # Generate 36px PNG (18pt @2x for Retina)
            rsvg-convert -w 36 -h 36 "$ICONS_SRC/icon-${state}.svg" -o "$ICONS_DST/icon-${state}.png"
        fi
    done
    log "Icons installed"
else
    warn "rsvg-convert not found, using text fallback for menu bar"
fi

# =============================================================================
# Step 9: Create LaunchAgent
# =============================================================================

echo ""
info "Creating LaunchAgent for auto-start..."

# Use the venv Python, not system Python
VENV_PYTHON="$VENV_DIR/bin/python3"

cat > "$LAUNCH_AGENTS_DIR/$PLIST_NAME" << PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.whisper-push.hotkey</string>
    <key>ProgramArguments</key>
    <array>
        <string>${VENV_PYTHON}</string>
        <string>${HOTKEY_SCRIPT}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>${SUPPORT_DIR}/daemon-stderr.log</string>
    <key>StandardErrorPath</key>
    <string>${SUPPORT_DIR}/daemon-stderr.log</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PYTHONUNBUFFERED</key>
        <string>1</string>
        <key>PATH</key>
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
PLIST

log "LaunchAgent created"

# =============================================================================
# Step 10: Create default config
# =============================================================================

if [[ ! -f "$CONFIG_FILE" ]]; then
    info "Creating default configuration..."
    cat > "$CONFIG_FILE" << 'CONFIG'
# Whisper Push Configuration
# Uses parakeet-mlx (NVIDIA Parakeet, optimized for Apple Silicon GPU)

# Global hotkey (toggle or hold)
# Format: modifier+modifier+key
# Modifiers: cmd, shift, alt (option), ctrl
# Keys: a-z, 0-9, space, return, tab, escape, f1-f12
hotkey = "ctrl"

# Hotkey mode: "toggle" or "hold"
# For hold-to-talk with Control only, use:
# hotkey_mode = "hold"
# hotkey = "rctrl"  # right control recommended to avoid conflicts
hotkey_mode = "hold"

# Language: Parakeet v3 auto-detects the language automatically.
# It covers 25 European languages: bg, hr, cs, da, nl, en, et, fi, fr, de,
# el, hu, it, lv, lt, mt, pl, pt, ro, sk, sl, es, sv, ru, uk
language = "auto"

# Model (stays loaded in RAM for instant transcription):
#   parakeet-tdt-0.6b-v3  - multilingual (25 EU langs), fastest, recommended
model = "parakeet-tdt-0.6b-v3"

# Free the model from memory (~1.3GB) after N minutes of inactivity.
# 0 = always resident (instant). If set, the model reloads while you record,
# so the only cost is a slightly longer first transcription after a long pause.
idle_unload_minutes = 0

# Notifications on start/stop
notifications = true

# Sound feedback on start/stop
sound_feedback = true

# Audio device selection: "auto" or exact device name (e.g. "MacBook Pro Microphone")
# Use the menu bar dropdown to pick devices interactively
input_device = "auto"
output_device = "auto"
CONFIG
    log "Configuration created"
else
    log "Configuration already exists"
fi

# =============================================================================
# Step 11: Start the daemon via app (uses Terminal for Accessibility permissions)
# =============================================================================

echo ""
info "Starting menu bar daemon..."

# Unload LaunchAgent if already running
launchctl bootout gui/$(id -u)/com.whisper-push.hotkey 2>/dev/null || true

# Kill any existing daemon
pkill -f 'hotkey-daemon.py' 2>/dev/null || true
sleep 0.5

# Launch via the app (opens Terminal briefly for Accessibility permissions)
info "Opening Whisper Push via Terminal (for permissions)..."
open "$APP_PATH"

# Wait for daemon to start
sleep 3

# Wait a bit more for Terminal to launch daemon
sleep 2

# Verify it started
if pgrep -f 'hotkey-daemon.py' > /dev/null; then
    log "Daemon started successfully"
else
    warn "Daemon may not have started. Try launching 'Whisper Push' from Spotlight."
    warn "Check logs: $SUPPORT_DIR/daemon.log"
fi

# Note: LaunchAgent plist is already in place, will auto-load on next login
# We don't bootstrap it now because we want the app (via Terminal) to be the primary launcher

# =============================================================================
# Step 12: Verify permissions
# =============================================================================

echo ""
# Terminal should already have Accessibility permissions
# If not, macOS will prompt the user when Terminal tries to monitor keystrokes
info "Whisper Push uses Terminal's Accessibility permissions for hotkey detection."
info "If the hotkey doesn't work, ensure Terminal has Accessibility permission in:"
info "  System Settings → Privacy & Security → Accessibility"
echo ""

# =============================================================================
# Summary
# =============================================================================

echo ""
echo "=========================================="
echo -e "${GREEN}  Installation Complete!${NC}"
echo "=========================================="
echo ""
echo "Look for the Whisper Push icon in your menu bar (top right)."
echo ""
echo -e "  ${BLUE}Hotkey:${NC}  Hold Control (ctrl)"
echo -e "  ${BLUE}Config:${NC}  $CONFIG_FILE"
echo -e "  ${BLUE}Logs:${NC}    $SUPPORT_DIR/daemon.log"
echo ""
echo "Icon colors:"
echo "  Purple → Ready"
echo "  Red    → Recording"
echo "  Orange → Processing"
echo ""
echo "Usage:"
echo "  1. Press ⌃⇧Space to start recording"
echo "  2. Speak"
echo "  3. Press ⌃⇧Space again → text is typed at cursor"
echo ""
echo "To uninstall: $SCRIPT_DIR/uninstall.sh"
echo ""

# First run warning
if [[ ! -d "$HOME/.cache/huggingface" ]] || [[ -z "$(ls -A "$HOME/.cache/huggingface" 2>/dev/null)" ]]; then
    warn "First use will download the Parakeet model (~600MB)."
fi
