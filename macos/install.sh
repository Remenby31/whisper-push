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

# sox (for audio recording)
if ! command -v rec &> /dev/null; then
    info "Installing sox (audio recording)..."
    brew install sox
fi
log "sox installed"

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

# Install faster-whisper for transcription
info "Installing faster-whisper..."
pip install --quiet faster-whisper

deactivate

log "Python environment ready"

# =============================================================================
# Step 6: Install whisper-push script
# =============================================================================

echo ""
info "Installing whisper-push..."

# Copy the main Python script
if [[ -f "$PROJECT_ROOT/whisper-push-macos.py" ]]; then
    cp "$PROJECT_ROOT/whisper-push-macos.py" "$SUPPORT_DIR/"
else
    error "whisper-push-macos.py not found in $PROJECT_ROOT"
fi

# Copy sounds
if [[ -d "$PROJECT_ROOT/sounds" ]]; then
    cp -r "$PROJECT_ROOT/sounds" "$SUPPORT_DIR/"
fi

# Create launcher script that uses the venv
cat > "$SUPPORT_DIR/whisper-push" << LAUNCHER
#!/bin/bash
source "$VENV_DIR/bin/activate"
exec python3 "$SUPPORT_DIR/whisper-push-macos.py" "\$@"
LAUNCHER
chmod +x "$SUPPORT_DIR/whisper-push"

log "whisper-push installed"

# =============================================================================
# Step 7: Create Spotlight-visible app in /Applications
# =============================================================================

echo ""
info "Creating Whisper Push app for Spotlight..."

APP_PATH="/Applications/Whisper Push.app"
mkdir -p "$APP_PATH/Contents/MacOS"
mkdir -p "$APP_PATH/Contents/Resources"

# Create executable script with absolute paths (evaluated at install time)
cat > "$APP_PATH/Contents/MacOS/Whisper Push" << APPSCRIPT
#!/bin/bash
export HOME="$HOME"
export PATH="/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:\$PATH"
source "$VENV_DIR/bin/activate"
exec python3 "$SUPPORT_DIR/whisper-push-macos.py" "\$@"
APPSCRIPT
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
    <key>CFBundleShortVersionString</key>
    <string>1.0.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>Whisper Push needs microphone access to record your voice.</string>
</dict>
</plist>
PLIST

# Force Spotlight to index the app
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
    <string>${SUPPORT_DIR}/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>${SUPPORT_DIR}/daemon.log</string>
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

# Global hotkey to toggle recording
# Format: modifier+modifier+key
# Modifiers: cmd, shift, alt (option), ctrl
# Keys: a-z, 0-9, space, return, tab, escape, f1-f12
hotkey = "ctrl+shift+space"

# Language: "auto" for auto-detection, or ISO code ("fr", "en", "de", ...)
language = "auto"

# Whisper model: tiny, base, small, medium, large-v3, large-v3-turbo
model = "large-v3-turbo"

# Precision: int8 (recommended for Apple Silicon), float32
compute_type = "int8"

# Device: cpu (recommended for macOS)
device = "cpu"

# Notifications on start/stop
notifications = true

# Sound feedback on start/stop
sound_feedback = true

# Transcription beam size (higher = more accurate, slower)
beam_size = 5
CONFIG
    log "Configuration created"
else
    log "Configuration already exists"
fi

# =============================================================================
# Step 11: Start the daemon
# =============================================================================

echo ""
info "Starting menu bar daemon..."

# Unload if already running
launchctl bootout gui/$(id -u)/com.whisper-push.hotkey 2>/dev/null || true

# Small delay to ensure clean unload
sleep 0.5

# Load the agent
launchctl bootstrap gui/$(id -u) "$LAUNCH_AGENTS_DIR/$PLIST_NAME"

# Verify it started
sleep 1
if launchctl print gui/$(id -u)/com.whisper-push.hotkey &>/dev/null; then
    log "Daemon started successfully"
else
    warn "Daemon may not have started. Check: $SUPPORT_DIR/daemon.log"
fi

# =============================================================================
# Step 12: Request permissions (Accessibility)
# =============================================================================

echo ""
info "Configuring permissions..."

# Function to check if we have accessibility permission
check_accessibility() {
    # Try to use osascript to check accessibility - this will prompt if needed
    osascript -e 'tell application "System Events" to return true' &>/dev/null
    return $?
}

# Function to show macOS notification
show_notification() {
    local title="$1"
    local message="$2"
    osascript -e "display notification \"$message\" with title \"$title\" sound name \"default\"" 2>/dev/null || true
}

# Check if we already have accessibility permission
if check_accessibility; then
    log "Accessibility permission already granted"
else
    # Show notification and open System Settings
    show_notification "Whisper Push" "Please enable Accessibility permission for Python"

    warn "═══════════════════════════════════════════════════════════════"
    warn "  ACTION REQUIRED: Enable Accessibility permission"
    warn "═══════════════════════════════════════════════════════════════"
    echo ""
    info "Opening System Settings → Accessibility..."

    # Open System Settings directly to Accessibility pane
    open "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"

    echo ""
    info "Please enable 'Python' or 'python3.12' in the list."
    info "You may need to click the lock 🔒 to make changes."
    echo ""

    # Wait for user to grant permission (with timeout)
    info "Waiting for permission to be granted..."
    TIMEOUT=60
    ELAPSED=0
    while ! check_accessibility && [[ $ELAPSED -lt $TIMEOUT ]]; do
        sleep 2
        ELAPSED=$((ELAPSED + 2))
        echo -n "."
    done
    echo ""

    if check_accessibility; then
        log "Accessibility permission granted!"
        show_notification "Whisper Push" "Setup complete! Press Cmd+Shift+Space to use."

        # Restart daemon to pick up new permissions
        info "Restarting daemon with new permissions..."
        launchctl kickstart -k gui/$(id -u)/com.whisper-push.hotkey 2>/dev/null || true
        sleep 1
    else
        warn "Permission not yet granted. Hotkey won't work until you enable it."
        warn "Go to: System Settings → Privacy & Security → Accessibility"
        warn "Then enable 'Python' and restart with:"
        warn "  launchctl kickstart -k gui/\$(id -u)/com.whisper-push.hotkey"
    fi
fi

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
echo -e "  ${BLUE}Hotkey:${NC}  ⌃⇧Space (Ctrl+Shift+Space)"
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
    warn "First use will download the Whisper model (~1.5GB)."
fi
