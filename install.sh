#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_NAME="$(uname -s)"
AUTO_DEPS="${WHISPER_PUSH_AUTO_DEPS:-0}"
HOTKEY="${WHISPER_PUSH_HOTKEY:-}"

if [ "$OS_NAME" = "Darwin" ]; then
    echo "macOS detected. Use ./macos/install.sh instead."
    exit 1
fi

info() { echo "[*] $1"; }
warn() { echo "[!] $1"; }
die() { echo "[x] $1"; exit 1; }

echo "Installing whisper-push..."

install_system_deps() {
    local pm=""
    if command -v apt-get >/dev/null 2>&1; then
        pm="apt"
    elif command -v dnf >/dev/null 2>&1; then
        pm="dnf"
    elif command -v pacman >/dev/null 2>&1; then
        pm="pacman"
    fi

    if [ -z "$pm" ]; then
        warn "Could not detect package manager. Install dependencies manually."
        return
    fi

    info "Installing system dependencies with $pm..."
    case "$pm" in
        apt)
            sudo apt-get update
            sudo apt-get install -y pipewire wl-clipboard ydotool libnotify-bin pulseaudio-utils
            ;;
        dnf)
            sudo dnf install -y pipewire wl-clipboard ydotool libnotify pulseaudio-utils
            ;;
        pacman)
            sudo pacman -S --needed pipewire wl-clipboard ydotool libnotify
            ;;
    esac
}

# Check dependencies
if ! command -v uv >/dev/null 2>&1; then
    if [ "$AUTO_DEPS" = "1" ] && command -v curl >/dev/null 2>&1; then
        info "Installing uv..."
        curl -LsSf https://astral.sh/uv/install.sh | sh
        export PATH="$HOME/.local/bin:$PATH"
    else
        die "uv is required. Install from https://github.com/astral-sh/uv"
    fi
fi

required_cmds=(pw-record wl-copy wl-paste ydotool)
missing_cmds=()
for cmd in "${required_cmds[@]}"; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        missing_cmds+=("$cmd")
    fi
done

if [ ${#missing_cmds[@]} -gt 0 ] && [ "$AUTO_DEPS" = "1" ]; then
    install_system_deps
    missing_cmds=()
    for cmd in "${required_cmds[@]}"; do
        if ! command -v "$cmd" >/dev/null 2>&1; then
            missing_cmds+=("$cmd")
        fi
    done
fi

if [ ${#missing_cmds[@]} -gt 0 ]; then
    die "Missing dependencies: ${missing_cmds[*]}"
fi

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
PIP_PACKAGES=(faster-whisper tomli)

USE_CUDA="${WHISPER_PUSH_USE_CUDA:-auto}"
if [ "$USE_CUDA" = "auto" ]; then
    if command -v nvidia-smi >/dev/null 2>&1; then
        USE_CUDA="1"
    else
        USE_CUDA="0"
    fi
fi

if [ "$USE_CUDA" = "1" ]; then
    PIP_PACKAGES+=(nvidia-cublas-cu12 nvidia-cudnn-cu12)
else
    info "CUDA libs skipped (set WHISPER_PUSH_USE_CUDA=1 to force)."
fi

uv pip install --python "$SCRIPT_DIR/.venv/bin/python" "${PIP_PACKAGES[@]}"

# Make scripts executable
chmod +x "$SCRIPT_DIR/whisper-push"
chmod +x "$SCRIPT_DIR/whisper-push.py"
chmod +x "$SCRIPT_DIR/whisper_push.py"

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

# Optional GNOME hotkey setup
if [ -n "$HOTKEY" ] && [ -x "$SCRIPT_DIR/setup-hotkey.sh" ]; then
    "$SCRIPT_DIR/setup-hotkey.sh" "$HOTKEY" || true
fi

echo ""
echo "Installation complete!"
echo ""
echo "Usage:"
echo "  1. Bind 'whisper-push' to a keyboard shortcut"
echo "  2. Press to start recording"
echo "  3. Press again to transcribe and type"
echo ""
echo "Config: ~/.config/whisper-push/config.toml"
