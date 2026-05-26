#!/bin/bash
#
# Build a self-contained Whisper Push.app and package it into a DMG.
# Apple Silicon only (MLX/Parakeet require an M-series chip).
#
# Note: this produces an UNSIGNED app. Without an Apple Developer ID we cannot
# notarize, so users must remove the quarantine flag on first launch (the DMG
# prints the command). See the end of this script.
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$PROJECT_ROOT/build"
DIST_DIR="$PROJECT_ROOT/dist"
APP_NAME="Whisper Push"
APP_PATH="$DIST_DIR/$APP_NAME.app"
DMG_NAME="Whisper-Push-macOS-arm64"
DMG_PATH="$DIST_DIR/$DMG_NAME.dmg"

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}[BUILD]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC}  $1"; }
error(){ echo -e "${RED}[ERROR]${NC} $1"; exit 1; }

[[ "$(uname -m)" == "arm64" ]] || error "This app requires Apple Silicon (MLX is arm64-only)."
command -v python3 >/dev/null || error "python3 not found (brew install python)."

# --- Regenerate the app icon (.icns) and menu-bar PNGs from the brand SVGs ---
# The wave glyph is bundled as SVG; load_icon_image() reads PNGs, so render them
# here (and always rebuild the .icns) to keep the app in sync with icon.svg.
ICNS_FILE="$SCRIPT_DIR/whisper-push.icns"
SVG_FILE="$PROJECT_ROOT/icon.svg"
if command -v rsvg-convert >/dev/null; then
    if [[ -f "$SVG_FILE" ]] && command -v iconutil >/dev/null; then
        log "Generating app icon (.icns) from $SVG_FILE..."
        ICONSET="$BUILD_DIR/icon.iconset"; rm -rf "$ICONSET"; mkdir -p "$ICONSET"
        for size in 16 32 128 256 512; do
            rsvg-convert -w $size -h $size "$SVG_FILE" -o "$ICONSET/icon_${size}x${size}.png"
            rsvg-convert -w $((size*2)) -h $((size*2)) "$SVG_FILE" -o "$ICONSET/icon_${size}x${size}@2x.png"
        done
        iconutil -c icns "$ICONSET" -o "$ICNS_FILE" && rm -rf "$ICONSET"
    fi
    log "Rendering menu-bar icons (PNG) from SVG..."
    for st in idle recording processing; do
        [[ -f "$SCRIPT_DIR/icons/icon-${st}.svg" ]] && \
            rsvg-convert -w 48 -h 48 "$SCRIPT_DIR/icons/icon-${st}.svg" -o "$SCRIPT_DIR/icons/icon-${st}.png"
    done
else
    warn "rsvg-convert not found; using committed icon assets as-is."
fi

# --- Build venv with the runtime deps + PyInstaller ---
log "Setting up build virtualenv..."
VENV="$BUILD_DIR/venv"
python3 -m venv "$VENV"
# shellcheck disable=SC1091
source "$VENV/bin/activate"
pip install --quiet --upgrade pip
# MLX is pinned to 0.30.6 on purpose: 0.31.x raises
#   "There is no Stream(gpu, 0) in current thread."
# when transcription runs on a thread other than the one that warmed the model.
# We also re-pin to the macosx_14_0 wheels below so the app runs on macOS 14+
# (building on macOS 26 otherwise pulls macosx_26_0 wheels whose libs set
# minos 26.0, locking out every older Apple Silicon Mac).
MLX_VERSION="0.30.6"
MACOS_WHEEL_TARGET="macosx_14_0_arm64"

log "Installing dependencies (this downloads MLX, PyObjC, etc.)..."
pip install --quiet \
    "pyinstaller>=6.6" \
    "mlx==${MLX_VERSION}" "mlx-metal==${MLX_VERSION}" \
    parakeet-mlx sounddevice soundfile numpy scipy \
    pyobjc-framework-Cocoa pyobjc-framework-Quartz

log "Re-pinning MLX to ${MACOS_WHEEL_TARGET} wheels (macOS 14+ compatibility)..."
WHEELS="$BUILD_DIR/mlx-wheels"; rm -rf "$WHEELS"; mkdir -p "$WHEELS"
pip download --quiet --no-deps --only-binary :all: \
    --platform "$MACOS_WHEEL_TARGET" --python-version 3.13 \
    "mlx==${MLX_VERSION}" "mlx-metal==${MLX_VERSION}" -d "$WHEELS"
pip install --quiet --force-reinstall --no-deps "$WHEELS"/*.whl

# --- Build the .app ---
log "Cleaning previous builds..."
rm -rf "$BUILD_DIR/Whisper Push" "$APP_PATH" "$DMG_PATH"

log "Running PyInstaller..."
cd "$PROJECT_ROOT"
pyinstaller --clean --noconfirm "$SCRIPT_DIR/whisper-push.spec"
deactivate

[[ -d "$APP_PATH" ]] || error "PyInstaller did not produce $APP_PATH"
log "App bundle: $APP_PATH"

# --- Ad-hoc sign so it launches once de-quarantined ---
log "Ad-hoc signing the app..."
codesign --force --deep --sign - "$APP_PATH" || warn "Ad-hoc signing failed (app may still run after de-quarantine)."

# --- Package into a DMG ---
# create-dmg wants a SOURCE FOLDER (its contents go into the image), so stage
# the app on its own. create-dmg adds the Applications drop-link itself.
log "Creating DMG..."
STAGE="$BUILD_DIR/dmg-stage"; rm -rf "$STAGE"; mkdir -p "$STAGE"
cp -R "$APP_PATH" "$STAGE/"
if command -v create-dmg >/dev/null; then
    # Note: create-dmg drives Finder via AppleScript to lay out the window; the
    # first run may ask to allow controlling Finder -- approve it.
    create-dmg \
        --volname "$APP_NAME" \
        --window-size 600 380 \
        --icon-size 120 \
        --icon "$APP_NAME.app" 150 190 \
        --app-drop-link 450 190 \
        --hide-extension "$APP_NAME.app" \
        "$DMG_PATH" "$STAGE" || warn "create-dmg returned non-zero (DMG may still be usable)."
else
    warn "create-dmg not found (brew install create-dmg) -- using hdiutil fallback."
    ln -s /Applications "$STAGE/Applications"
    hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH"
fi
rm -rf "$STAGE"

[[ -f "$DMG_PATH" ]] || error "Failed to create DMG."

echo ""
echo "=================================================="
echo -e "${GREEN}Done.${NC}  DMG: $DMG_PATH  ($(du -h "$DMG_PATH" | cut -f1))"
echo "=================================================="
echo ""
echo "Install:"
echo "  1. Open the DMG, drag 'Whisper Push' to Applications."
echo "  2. UNSIGNED app -> first launch is blocked by Gatekeeper. Run once:"
echo "       xattr -dr com.apple.quarantine \"/Applications/Whisper Push.app\""
echo "     (or right-click the app > Open > Open)."
echo "  3. Grant Microphone + Accessibility + Input Monitoring when prompted"
echo "     (System Settings > Privacy & Security)."
echo "  4. First launch downloads the Parakeet model (~600 MB) -> needs internet."
echo ""
