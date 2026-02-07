#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_URL="${WHISPER_PUSH_REPO:-https://github.com/Remenby31/whisper-push.git}"
INSTALL_DIR="${WHISPER_PUSH_DIR:-$HOME/.local/share/whisper-push}"
UPDATE="${WHISPER_PUSH_UPDATE:-0}"
OS_NAME="$(uname -s)"

info() { echo "[*] $1"; }
die() { echo "[x] $1"; exit 1; }

PROJECT_DIR=""

if [ -f "$SCRIPT_DIR/whisper_push.py" ] && [ -f "$SCRIPT_DIR/install.sh" ]; then
    PROJECT_DIR="$SCRIPT_DIR"
else
    if ! command -v git >/dev/null 2>&1; then
        die "git is required to download the project. Install git and re-run."
    fi
    if [ -d "$INSTALL_DIR/.git" ]; then
        PROJECT_DIR="$INSTALL_DIR"
        if [ "$UPDATE" = "1" ]; then
            info "Updating existing install in $INSTALL_DIR..."
            git -C "$INSTALL_DIR" pull --ff-only
        else
            info "Using existing install in $INSTALL_DIR..."
        fi
    else
        info "Cloning to $INSTALL_DIR..."
        git clone "$REPO_URL" "$INSTALL_DIR"
        PROJECT_DIR="$INSTALL_DIR"
    fi
fi

if [ "$OS_NAME" = "Darwin" ]; then
    info "Running macOS installer..."
    "$PROJECT_DIR/macos/install.sh"
else
    info "Running Linux installer..."
    WHISPER_PUSH_AUTO_DEPS="${WHISPER_PUSH_AUTO_DEPS:-1}" \
    WHISPER_PUSH_HOTKEY="${WHISPER_PUSH_HOTKEY:-}" \
    "$PROJECT_DIR/install.sh"
fi
