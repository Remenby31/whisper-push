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
# Step 7: Install menu bar daemon
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
hotkey = "cmd+shift+space"

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
# Step 12: Request permissions
# =============================================================================

echo ""
warn "═══════════════════════════════════════════════════════════════"
warn "  IMPORTANT: Grant Accessibility permission when prompted!"
warn "═══════════════════════════════════════════════════════════════"
echo ""
info "The menu bar icon should now appear."
info "If prompted for Accessibility access, please allow it."
info ""
info "If no prompt appears, manually enable in:"
info "  System Settings → Privacy & Security → Accessibility"
info "  → Enable 'python3' or 'Python'"
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
echo -e "  ${BLUE}Hotkey:${NC}  ⌘⇧Space (Cmd+Shift+Space)"
echo -e "  ${BLUE}Config:${NC}  $CONFIG_FILE"
echo -e "  ${BLUE}Logs:${NC}    $SUPPORT_DIR/daemon.log"
echo ""
echo "Icon colors:"
echo "  Purple → Ready"
echo "  Red    → Recording"
echo "  Orange → Processing"
echo ""
echo "Usage:"
echo "  1. Press ⌘⇧Space to start recording"
echo "  2. Speak"
echo "  3. Press ⌘⇧Space again → text is typed at cursor"
echo ""
echo "To uninstall: $SCRIPT_DIR/uninstall.sh"
echo ""

# First run warning
if [[ ! -d "$HOME/.cache/huggingface" ]] || [[ -z "$(ls -A "$HOME/.cache/huggingface" 2>/dev/null)" ]]; then
    warn "First use will download the Whisper model (~1.5GB)."
fi
