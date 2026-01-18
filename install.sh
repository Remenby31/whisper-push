#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "Installing whisper-push..."

# Check dependencies
command -v uv >/dev/null 2>&1 || { echo "Error: uv is required. Install from https://github.com/astral-sh/uv"; exit 1; }
command -v pw-record >/dev/null 2>&1 || { echo "Error: pw-record (pipewire) is required"; exit 1; }
command -v wl-copy >/dev/null 2>&1 || { echo "Error: wl-clipboard is required"; exit 1; }
command -v ydotool >/dev/null 2>&1 || { echo "Error: ydotool is required"; exit 1; }

# Create directories
mkdir -p ~/.local/bin
mkdir -p ~/.config/whisper-push
mkdir -p ~/.local/share/applications
mkdir -p ~/.local/share/icons/hicolor/scalable/apps

# Create venv and install dependencies
if [ ! -d "$SCRIPT_DIR/.venv" ]; then
    echo "Creating virtual environment..."
    uv venv "$SCRIPT_DIR/.venv"
fi

echo "Installing Python dependencies..."
uv pip install --python "$SCRIPT_DIR/.venv/bin/python" \
    faster-whisper \
    nvidia-cublas-cu12 \
    nvidia-cudnn-cu12

# Make scripts executable
chmod +x "$SCRIPT_DIR/whisper-push"
chmod +x "$SCRIPT_DIR/whisper-push.py"

# Symlink executable
ln -sf "$SCRIPT_DIR/whisper-push" ~/.local/bin/whisper-push

# Copy config if not exists
if [ ! -f ~/.config/whisper-push/config.toml ]; then
    cp "$SCRIPT_DIR/config.toml" ~/.config/whisper-push/config.toml
    echo "Config created: ~/.config/whisper-push/config.toml"
fi

# Install icon and desktop file
cp "$SCRIPT_DIR/icon.svg" ~/.local/share/icons/hicolor/scalable/apps/whisper-push.svg
sed "s|Exec=.*|Exec=$HOME/.local/bin/whisper-push|" \
    "$SCRIPT_DIR/whisper-push.desktop" > ~/.local/share/applications/whisper-push.desktop

# Update caches
gtk-update-icon-cache -f -t ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database ~/.local/share/applications 2>/dev/null || true

# Check ydotool setup
if ! groups | grep -q input; then
    echo ""
    echo "WARNING: You are not in the 'input' group."
    echo "Run: sudo usermod -aG input \$USER"
    echo "Then log out and back in for ydotool to work."
fi

# Enable ydotool daemon
systemctl --user enable ydotool 2>/dev/null || true
systemctl --user start ydotool 2>/dev/null || true

echo ""
echo "Installation complete!"
echo ""
echo "Usage:"
echo "  1. Bind 'whisper-push' to a keyboard shortcut"
echo "  2. Press to start recording"
echo "  3. Press again to transcribe and type"
echo ""
echo "Config: ~/.config/whisper-push/config.toml"
