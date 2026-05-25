#!/bin/bash
# Whisper Push — Universal installer
# Detects OS, architecture, and GPU, then downloads the right binary.
set -e

GREEN='\033[0;32m'; YELLOW='\033[1;33m'; RED='\033[0;31m'; NC='\033[0m'
log()  { echo -e "${GREEN}[✓]${NC} $1"; }
info() { echo -e "${YELLOW}[i]${NC} $1"; }
error(){ echo -e "${RED}[✗]${NC} $1"; exit 1; }

echo ""
echo "===================================="
echo -e "${GREEN}  Whisper Push Installer${NC}"
echo "===================================="
echo ""

# Detect OS
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Darwin) PLATFORM="macos" ;;
    Linux)  PLATFORM="linux" ;;
    *)      error "Unsupported OS: $OS" ;;
esac

case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) error "Unsupported architecture: $ARCH" ;;
esac

info "Platform: $PLATFORM $ARCH"

# Detect GPU
GPU_VARIANT=""
if [ "$PLATFORM" = "macos" ]; then
    GPU_VARIANT=""  # Metal is built-in
    info "GPU: Metal (Apple Silicon)"
elif command -v nvidia-smi > /dev/null 2>&1; then
    GPU_VARIANT="-cuda"
    info "GPU: NVIDIA (CUDA)"
elif command -v vulkaninfo > /dev/null 2>&1; then
    GPU_VARIANT="-vulkan"
    info "GPU: Vulkan"
else
    GPU_VARIANT=""
    info "GPU: CPU only"
fi

# Determine download URL
REPO="Remenby31/whisper-push"
VERSION=$(curl -sL "https://api.github.com/repos/$REPO/releases/latest" | grep '"tag_name"' | head -1 | cut -d'"' -f4)

if [ -z "$VERSION" ]; then
    error "Could not determine latest version"
fi

info "Latest version: $VERSION"

if [ "$PLATFORM" = "macos" ]; then
    FILENAME="Whisper-Push-macOS-arm64.dmg"
    DOWNLOAD_URL="https://github.com/$REPO/releases/download/$VERSION/$FILENAME"
    info "Downloading $FILENAME..."
    curl -L -o "/tmp/$FILENAME" "$DOWNLOAD_URL"
    log "Downloaded to /tmp/$FILENAME"
    echo ""
    echo "To install:"
    echo "  1. Open /tmp/$FILENAME"
    echo "  2. Drag 'Whisper Push' to Applications"
    open "/tmp/$FILENAME"
else
    FILENAME="whisper-push-${PLATFORM}-${ARCH}${GPU_VARIANT}.tar.gz"
    DOWNLOAD_URL="https://github.com/$REPO/releases/download/$VERSION/$FILENAME"
    info "Downloading $FILENAME..."
    curl -L -o "/tmp/$FILENAME" "$DOWNLOAD_URL"

    # Install
    mkdir -p ~/.local/bin
    tar xzf "/tmp/$FILENAME" -C ~/.local/bin/
    chmod +x ~/.local/bin/whisper-push
    rm "/tmp/$FILENAME"

    log "Installed to ~/.local/bin/whisper-push"

    # Check PATH
    if ! echo "$PATH" | grep -q "$HOME/.local/bin"; then
        info "Add to your shell profile: export PATH=\$HOME/.local/bin:\$PATH"
    fi
fi

echo ""
log "Installation complete!"
echo ""
echo "Usage:"
echo "  whisper-push --doctor    # check setup"
echo "  whisper-push --models    # list available models"
echo "  whisper-push             # start (menu bar/tray)"
echo ""
