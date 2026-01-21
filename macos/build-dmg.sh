#!/bin/bash
#
# Build script for whisper-push macOS DMG
# Compatible with Apple Silicon (M1/M2/M3/M4) and Intel Macs
#
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$PROJECT_ROOT/build"
DIST_DIR="$PROJECT_ROOT/dist"
DMG_NAME="Whisper-Push-macOS"
APP_NAME="Whisper Push"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[BUILD]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

# Check architecture
ARCH=$(uname -m)
log "Building for architecture: $ARCH"

if [[ "$ARCH" == "arm64" ]]; then
    DMG_NAME="${DMG_NAME}-arm64"
    log "Apple Silicon detected (M1/M2/M3/M4)"
elif [[ "$ARCH" == "x86_64" ]]; then
    DMG_NAME="${DMG_NAME}-x86_64"
    log "Intel Mac detected"
else
    error "Unsupported architecture: $ARCH"
fi

# Check dependencies
log "Checking dependencies..."

if ! command -v python3 &> /dev/null; then
    error "Python 3 not found. Install with: brew install python"
fi

PYTHON_VERSION=$(python3 --version | cut -d' ' -f2 | cut -d'.' -f1,2)
log "Python version: $PYTHON_VERSION"

if ! command -v brew &> /dev/null; then
    warn "Homebrew not found. Some features may not work."
fi

# Check for sox (needed at runtime)
if ! command -v rec &> /dev/null; then
    warn "sox not found. Install with: brew install sox"
    warn "Users will need sox installed to use whisper-push"
fi

# Create/activate virtual environment
log "Setting up Python virtual environment..."
VENV_DIR="$BUILD_DIR/venv"
python3 -m venv "$VENV_DIR"
source "$VENV_DIR/bin/activate"

# Install dependencies
log "Installing Python dependencies..."
pip install --upgrade pip
pip install pyinstaller>=6.0
pip install faster-whisper>=1.0.0

# Generate .icns from SVG if needed
ICNS_FILE="$SCRIPT_DIR/whisper-push.icns"
SVG_FILE="$PROJECT_ROOT/icon.svg"

if [[ ! -f "$ICNS_FILE" ]] && [[ -f "$SVG_FILE" ]]; then
    log "Converting SVG to ICNS..."

    if command -v rsvg-convert &> /dev/null && command -v iconutil &> /dev/null; then
        ICONSET_DIR="$BUILD_DIR/whisper-push.iconset"
        mkdir -p "$ICONSET_DIR"

        # Generate different sizes for iconset
        for size in 16 32 64 128 256 512; do
            rsvg-convert -w $size -h $size "$SVG_FILE" -o "$ICONSET_DIR/icon_${size}x${size}.png"
            double=$((size * 2))
            rsvg-convert -w $double -h $double "$SVG_FILE" -o "$ICONSET_DIR/icon_${size}x${size}@2x.png"
        done

        iconutil -c icns "$ICONSET_DIR" -o "$ICNS_FILE"
        rm -rf "$ICONSET_DIR"
        log "Created $ICNS_FILE"
    else
        warn "rsvg-convert or iconutil not found. Skipping icon conversion."
        warn "Install with: brew install librsvg"
    fi
fi

# Clean previous builds
log "Cleaning previous builds..."
rm -rf "$BUILD_DIR/whisper-push"
rm -rf "$DIST_DIR/whisper-push"
rm -rf "$DIST_DIR/$APP_NAME.app"
rm -f "$DIST_DIR/$DMG_NAME.dmg"

# Build with PyInstaller
log "Building application with PyInstaller..."
cd "$PROJECT_ROOT"
pyinstaller --clean --noconfirm "$SCRIPT_DIR/whisper-push.spec"

# Verify the app was created
APP_PATH="$DIST_DIR/$APP_NAME.app"
if [[ ! -d "$APP_PATH" ]]; then
    error "Failed to create application bundle"
fi

log "Application bundle created: $APP_PATH"

# Create DMG
log "Creating DMG..."
DMG_PATH="$DIST_DIR/$DMG_NAME.dmg"
TMP_DMG="$BUILD_DIR/tmp.dmg"

# Calculate size needed (app size + 50MB buffer)
APP_SIZE=$(du -sm "$APP_PATH" | cut -f1)
DMG_SIZE=$((APP_SIZE + 50))

# Create temporary DMG
hdiutil create -size "${DMG_SIZE}m" -fs HFS+ -volname "$APP_NAME" "$TMP_DMG"

# Mount it
MOUNT_POINT=$(hdiutil attach "$TMP_DMG" | grep "Volumes" | cut -f3)
log "Mounted at: $MOUNT_POINT"

# Copy app
cp -R "$APP_PATH" "$MOUNT_POINT/"

# Create Applications symlink
ln -s /Applications "$MOUNT_POINT/Applications"

# Create background and position icons (optional, for nice appearance)
# This would require additional tools like create-dmg

# Unmount
hdiutil detach "$MOUNT_POINT"

# Convert to compressed DMG
hdiutil convert "$TMP_DMG" -format UDZO -o "$DMG_PATH"
rm -f "$TMP_DMG"

# Verify DMG
if [[ -f "$DMG_PATH" ]]; then
    DMG_SIZE_MB=$(du -h "$DMG_PATH" | cut -f1)
    log "DMG created successfully: $DMG_PATH ($DMG_SIZE_MB)"
else
    error "Failed to create DMG"
fi

# Optional: Sign the DMG (requires Apple Developer certificate)
if [[ -n "$DEVELOPER_ID" ]]; then
    log "Signing DMG with Developer ID..."
    codesign --deep --force --verify --verbose --sign "$DEVELOPER_ID" "$DMG_PATH"
fi

# Summary
echo ""
echo "=============================================="
echo -e "${GREEN}Build completed successfully!${NC}"
echo "=============================================="
echo ""
echo "Output files:"
echo "  App:  $APP_PATH"
echo "  DMG:  $DMG_PATH"
echo ""
echo "Architecture: $ARCH"
echo ""
echo "To test the app:"
echo "  open \"$APP_PATH\""
echo ""
echo "To install:"
echo "  1. Open $DMG_PATH"
echo "  2. Drag 'Whisper Push' to Applications"
echo "  3. Grant microphone and accessibility permissions"
echo ""
echo "Runtime dependencies (user must install):"
echo "  brew install sox"
echo ""
