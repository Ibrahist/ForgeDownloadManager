#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Forge Download Manager - Linux & macOS Automated Installer
# Installs forge-dm CLI and registers the browser Native Messaging Host.
# ==============================================================================

REPO="Ibrahist/ForgeDownloadManager"
APP_NAME="forge-dm"

echo "=========================================================="
echo "      Forge Download Manager - Linux & macOS Installer    "
echo "=========================================================="

# 1. Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux)
        OS_TAG="linux"
        ;;
    Darwin)
        OS_TAG="macos"
        ;;
    *)
        echo "Error: Unsupported operating system: $OS"
        exit 1
        ;;
esac

case "$ARCH" in
    x86_64|amd64)
        ARCH_TAG="x86_64"
        ;;
    aarch64|arm64)
        ARCH_TAG="aarch64"
        ;;
    *)
        echo "Error: Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

echo "[1/4] Detected environment: $OS_TAG ($ARCH_TAG)"

# 2. Determine installation target
if [ "$(id -u)" -eq 0 ]; then
    INSTALL_DIR="/usr/local/bin"
else
    INSTALL_DIR="${HOME}/.local/bin"
fi
mkdir -p "$INSTALL_DIR"
DEST_PATH="${INSTALL_DIR}/${APP_NAME}"

# 3. Locate or download binary
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_BIN="${SCRIPT_DIR}/../target/release/${APP_NAME}"

if [ -f "$WORKSPACE_BIN" ]; then
    echo "[2/4] Found local workspace binary at $WORKSPACE_BIN"
    cp "$WORKSPACE_BIN" "$DEST_PATH"
else
    DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/forge-dm-${OS_TAG}-${ARCH_TAG}"
    echo "[2/4] Downloading ${APP_NAME} from GitHub: $DOWNLOAD_URL"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$DOWNLOAD_URL" -o "$DEST_PATH"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$DEST_PATH" "$DOWNLOAD_URL"
    else
        echo "Error: Neither curl nor wget is available."
        exit 1
    fi
fi

chmod +x "$DEST_PATH"

# 4. Register Native Messaging Host
echo "[3/4] Registering Native Messaging Host for browsers..."
if "$DEST_PATH" host --install; then
    echo "      Native messaging host registered successfully."
else
    echo "Warning: Native messaging host registration returned a non-zero code."
fi

# 5. Check PATH and verify
echo "[4/4] Verifying installation..."
if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
    echo ""
    echo "NOTE: $INSTALL_DIR is not in your PATH."
    echo "Add the following line to your ~/.bashrc or ~/.zshrc:"
    echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
fi

if "$DEST_PATH" --version; then
    echo ""
    echo "SUCCESS: Forge Download Manager is ready to use!"
    echo "Run 'forge-dm --help' to get started."
else
    echo "Warning: Installation completed but version check failed."
fi
