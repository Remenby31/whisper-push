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

# --- Code-sign ---
# For DISTRIBUTION we sign ad-hoc by default. A self-signed cert lives only in
# the builder's keychain, so it gives downloaders no Gatekeeper trust (it isn't
# notarized) and a stale --deep seal can even trigger "app is damaged" -- which
# de-quarantine does NOT fix. Ad-hoc is the reliable path: users de-quarantine
# once (the DMG prints the command) and the app runs on every macOS version.
#
# Set WHISPER_PUSH_SIGN_IDENTITY to a real "Developer ID Application" cert to sign
# (and then notarize) properly. A stable identity also makes macOS (TCC) keep the
# Accessibility grant across updates instead of re-prompting -- but that only
# matters once you ship signed+notarized builds, not for ad-hoc distribution.
SIGN_IDENTITY="${WHISPER_PUSH_SIGN_IDENTITY:-}"
if [[ -n "$SIGN_IDENTITY" ]] && security find-identity -p codesigning 2>/dev/null | grep -q "$SIGN_IDENTITY"; then
    log "Signing with identity: $SIGN_IDENTITY"
    codesign --force --deep --sign "$SIGN_IDENTITY" "$APP_PATH" \
        || warn "Signing with '$SIGN_IDENTITY' failed (app may still run after de-quarantine)."
else
    [[ -n "$SIGN_IDENTITY" ]] && warn "Identity '$SIGN_IDENTITY' not found -- falling back to ad-hoc."
    log "Ad-hoc signing (unsigned distribution; users de-quarantine on first launch)."
    codesign --force --deep --sign - "$APP_PATH" || warn "Ad-hoc signing failed."
fi

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
        "$DMG_PATH" "$STAGE" || warn "create-dmg returned non-zero (recovering below)."

    # create-dmg sometimes fails to UNMOUNT the staging volume ("Resource busy",
    # usually Spotlight indexing it) and bails right before converting -- leaving
    # the final DMG missing but a fully laid-out rw.*.dmg next to it. Recover by
    # detaching any stuck "$APP_NAME" volume and converting that temp image,
    # rather than throwing away a good build.
    if [[ ! -f "$DMG_PATH" ]]; then
        RW_DMG=$(ls -t "$DIST_DIR"/rw.*"$DMG_NAME".dmg 2>/dev/null | head -1 || true)
        if [[ -n "$RW_DMG" ]]; then
            warn "Final DMG missing; converting leftover temp image ($RW_DMG)."
            for d in $(hdiutil info | awk -v v="/Volumes/$APP_NAME" '$0 ~ v {print $1}'); do
                hdiutil detach "$d" -force >/dev/null 2>&1 || true
            done
            hdiutil convert "$RW_DMG" -format UDZO -imagekey zlib-level=9 -o "$DMG_PATH" \
                && rm -f "$RW_DMG"
        fi
    fi
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
echo "  2. UNSIGNED app -> Gatekeeper blocks the first launch. Clear quarantine once:"
echo "       xattr -dr com.apple.quarantine \"/Applications/Whisper Push.app\""
echo "     This works on every macOS version. (On macOS 15+ the old right-click >"
echo "     Open trick is gone; without the command, use System Settings > Privacy &"
echo "     Security > 'Open Anyway' after the first blocked launch.)"
echo "  3. Grant Microphone + Accessibility + Input Monitoring when prompted"
echo "     (System Settings > Privacy & Security)."
echo "  4. First launch downloads the Parakeet model (~600 MB) -> needs internet."
echo ""
