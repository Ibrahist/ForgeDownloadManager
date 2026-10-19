#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Forge Download Manager - Build Release Assets (Linux & macOS)
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
DIST_DIR="${ROOT_DIR}/dist/installers"

echo "=========================================================="
echo "    Forge Download Manager - Build Release Assets (Unix)  "
echo "=========================================================="
echo "Output Directory: ${DIST_DIR}"

mkdir -p "${DIST_DIR}"

# 1. Detect OS and architecture
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"
case "$ARCH" in
    x86_64|amd64) ARCH="x86_64" ;;
    aarch64|arm64) ARCH="aarch64" ;;
esac

# 2. Build CLI binary
echo "[1/4] Building forge-dm CLI release binary..."
cargo build --release -p forgedownloadmanager-cli

CLI_SRC="${ROOT_DIR}/target/release/forge-dm"
CLI_DEST="${DIST_DIR}/forge-dm-${OS}-${ARCH}"
cp "${CLI_SRC}" "${CLI_DEST}"
chmod +x "${CLI_DEST}"
echo "      CLI binary staged: ${CLI_DEST}"

# 3. Package extension
echo "[2/4] Packaging Browser Extension..."
node "${SCRIPT_DIR}/package-extension.mjs"
EXT_SRC="${ROOT_DIR}/dist/forge-dm-extension.zip"
if [ -f "${EXT_SRC}" ]; then
    cp "${EXT_SRC}" "${DIST_DIR}/forge-dm-extension.zip"
    echo "      Extension archive staged: ${DIST_DIR}/forge-dm-extension.zip"
fi

# 4. Build Desktop App & Installers (if node & tauri available)
echo "[3/4] Building Desktop Web frontend..."
(cd "${ROOT_DIR}/apps/desktop" && npm run build)

echo "      Building Tauri application bundle..."
if (cd "${ROOT_DIR}/apps/desktop" && npm run tauri -- build); then
    TAURI_BUNDLE_DIR="${ROOT_DIR}/apps/desktop/src-tauri/target/release/bundle"
    if [ -d "${TAURI_BUNDLE_DIR}" ]; then
        find "${TAURI_BUNDLE_DIR}" -type f \( -name "*.deb" -o -name "*.AppImage" -o -name "*.dmg" -o -name "*.tar.gz" \) -exec cp {} "${DIST_DIR}/" \;
        echo "      Tauri bundles copied to ${DIST_DIR}"
    fi
else
    echo "Warning: Tauri build did not produce bundles (platform build dependencies may be missing)."
fi

# Stage installer scripts
cp "${ROOT_DIR}/scripts/install.ps1" "${DIST_DIR}/install.ps1"
cp "${ROOT_DIR}/scripts/install.sh" "${DIST_DIR}/install.sh"
echo "      Installer scripts staged: install.ps1, install.sh"

# 5. Generate Checksums
echo "[4/4] Generating SHA256SUMS.txt..."
CHECKSUM_FILE="${DIST_DIR}/SHA256SUMS.txt"
rm -f "${CHECKSUM_FILE}"

(cd "${DIST_DIR}" && sha256sum * > SHA256SUMS.txt 2>/dev/null || shasum -a 256 * > SHA256SUMS.txt)
echo ""
echo "SUCCESS: All release assets staged in: ${DIST_DIR}"
cat "${CHECKSUM_FILE}"
