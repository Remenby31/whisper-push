#!/bin/bash
#
# Whisper Push - macOS Uninstallation Script
#
set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${GREEN}[✓]${NC} $1"; }
info() { echo -e "${BLUE}[i]${NC} $1"; }
warn() { echo -e "${YELLOW}[!]${NC} $1"; }

SUPPORT_DIR="$HOME/Library/Application Support/whisper-push"
LAUNCH_AGENTS_DIR="$HOME/Library/LaunchAgents"
PLIST_NAME="com.whisper-push.hotkey.plist"

echo ""
echo "=========================================="
echo -e "${YELLOW}  Whisper Push - Uninstaller${NC}"
echo "=========================================="
echo ""

read -p "Uninstall Whisper Push? [y/N] " -n 1 -r
echo
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    echo "Cancelled."
    exit 0
fi

echo ""

# Stop daemon
info "Stopping daemon..."
launchctl bootout gui/$(id -u)/com.whisper-push.hotkey 2>/dev/null || true
log "Daemon stopped"

# Remove LaunchAgent
if [[ -f "$LAUNCH_AGENTS_DIR/$PLIST_NAME" ]]; then
    rm -f "$LAUNCH_AGENTS_DIR/$PLIST_NAME"
    log "LaunchAgent removed"
fi

# Remove temp files
rm -f "$TMPDIR/whisper-push.wav" 2>/dev/null || true
rm -f "$TMPDIR/whisper-push.lock" 2>/dev/null || true

# Ask about app data
echo ""
read -p "Remove configuration and app data? [y/N] " -n 1 -r
echo
if [[ $REPLY =~ ^[Yy]$ ]]; then
    if [[ -d "$SUPPORT_DIR" ]]; then
        rm -rf "$SUPPORT_DIR"
        log "App data removed"
    fi
else
    info "Kept: $SUPPORT_DIR"
fi

# Ask about Whisper models
echo ""
MODELS_DIR="$HOME/.cache/huggingface"
if [[ -d "$MODELS_DIR" ]]; then
    SIZE=$(du -sh "$MODELS_DIR" 2>/dev/null | cut -f1 || echo "unknown")
    warn "Whisper models: $MODELS_DIR ($SIZE)"
    warn "These are shared with other Hugging Face apps."
    read -p "Remove Whisper models? [y/N] " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        # Only remove whisper models, not all HF models
        find "$MODELS_DIR" -type d -name "*whisper*" -exec rm -rf {} + 2>/dev/null || true
        log "Whisper models removed"
    fi
fi

echo ""
echo "=========================================="
echo -e "${GREEN}  Uninstallation Complete${NC}"
echo "=========================================="
echo ""
info "sox was kept (may be used by other apps)"
info "To remove: brew uninstall sox"
echo ""
