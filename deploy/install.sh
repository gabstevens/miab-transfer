#!/usr/bin/env bash
# ==============================================================================
# miab-transfer Installation Script
# https://github.com/gabstevens/miab-transfer
# ==============================================================================

set -euo pipefail

REPO="gabstevens/miab-transfer"
PREFIX="${PREFIX:-/usr/local}"
BIN_DIR="${PREFIX}/bin"
INSTALL_SERVICE=0
UNINSTALL=0

# Parse CLI options
while [[ $# -gt 0 ]]; do
  case "$1" in
    --service)
      INSTALL_SERVICE=1
      shift
      ;;
    --uninstall)
      UNINSTALL=1
      shift
      ;;
    --prefix)
      PREFIX="$2"
      BIN_DIR="${PREFIX}/bin"
      shift 2
      ;;
    -h|--help)
      echo "Usage: install.sh [OPTIONS]"
      echo ""
      echo "Options:"
      echo "  --service      Install and enable the miabd systemd service"
      echo "  --uninstall    Remove miab, miabd, and associated service"
      echo "  --prefix PATH  Set installation prefix (default: /usr/local)"
      echo "  -h, --help     Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

# Need sudo if target directory is not writeable
SUDO=""
if [[ ! -w "${BIN_DIR}" ]] && [[ $EUID -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    SUDO="sudo"
  else
    echo "Error: Need write permissions to ${BIN_DIR} and 'sudo' is not available." >&2
    exit 1
  fi
fi

# ------------------------------------------------------------------------------
# Uninstall procedure
# ------------------------------------------------------------------------------
if [[ "${UNINSTALL}" -eq 1 ]]; then
  echo "==> Uninstalling miab and miabd..."
  if command -v systemctl >/dev/null 2>&1 && systemctl is-active --quiet miabd 2>/dev/null; then
    echo "Stopping miabd service..."
    ${SUDO} systemctl stop miabd || true
    ${SUDO} systemctl disable miabd || true
  fi

  ${SUDO} rm -f "${BIN_DIR}/miab" "${BIN_DIR}/miabd"
  ${SUDO} rm -f /etc/systemd/system/miabd.service
  if command -v systemctl >/dev/null 2>&1; then
    ${SUDO} systemctl daemon-reload || true
  fi
  echo "✓ Successfully uninstalled."
  exit 0
fi

# ------------------------------------------------------------------------------
# Detect OS and architecture
# ------------------------------------------------------------------------------
OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "${ARCH}" in
  x86_64|amd64)
    TARGET_ARCH="x86_64"
    ;;
  aarch64|arm64)
    TARGET_ARCH="aarch64"
    ;;
  armv7l|armv7)
    TARGET_ARCH="armv7"
    ;;
  *)
    echo "Unsupported architecture: ${ARCH}" >&2
    exit 1
    ;;
esac

case "${OS}" in
  linux)
    if [[ "${TARGET_ARCH}" == "armv7" ]]; then
      TARGET="armv7-unknown-linux-musleabihf"
    else
      TARGET="${TARGET_ARCH}-unknown-linux-musl"
    fi
    ;;
  darwin)
    TARGET="${TARGET_ARCH}-apple-darwin"
    ;;
  *)
    echo "Unsupported operating system: ${OS}" >&2
    exit 1
    ;;
esac

echo "==> Detected platform: ${OS} (${TARGET_ARCH}) -> Target: ${TARGET}"

# ------------------------------------------------------------------------------
# Install binaries
# ------------------------------------------------------------------------------
${SUDO} mkdir -p "${BIN_DIR}"

# Check if pre-compiled binaries are present locally (e.g. from local repo build)
LOCAL_BIN_DIR="$(dirname "$(dirname "$(readlink -f "$0" 2>/dev/null || echo "$0")")")/target/release"
if [[ -f "${LOCAL_BIN_DIR}/miab" ]] && [[ -f "${LOCAL_BIN_DIR}/miabd" ]]; then
  echo "==> Installing locally built binaries from ${LOCAL_BIN_DIR}..."
  ${SUDO} install -m 0755 "${LOCAL_BIN_DIR}/miab" "${BIN_DIR}/miab"
  ${SUDO} install -m 0755 "${LOCAL_BIN_DIR}/miabd" "${BIN_DIR}/miabd"
else
  echo "==> Fetching latest release asset for ${TARGET} from GitHub..."
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "${TMP_DIR}"' EXIT

  RELEASE_URL="https://github.com/${REPO}/releases/latest/download/miab-transfer-${TARGET}.tar.gz"
  echo "Downloading from ${RELEASE_URL}..."

  if command -v curl >/dev/null 2>&1; then
    curl -sSfL "${RELEASE_URL}" -o "${TMP_DIR}/archive.tar.gz"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "${TMP_DIR}/archive.tar.gz" "${RELEASE_URL}"
  else
    echo "Error: Neither curl nor wget is available." >&2
    exit 1
  fi

  tar -xzf "${TMP_DIR}/archive.tar.gz" -C "${TMP_DIR}"
  ${SUDO} install -m 0755 "${TMP_DIR}/miab" "${BIN_DIR}/miab"
  ${SUDO} install -m 0755 "${TMP_DIR}/miabd" "${BIN_DIR}/miabd"
fi

echo "✓ Installed ${BIN_DIR}/miab"
echo "✓ Installed ${BIN_DIR}/miabd"

# ------------------------------------------------------------------------------
# Install systemd service if requested
# ------------------------------------------------------------------------------
if [[ "${INSTALL_SERVICE}" -eq 1 ]]; then
  if ! command -v systemctl >/dev/null 2>&1; then
    echo "Warning: systemctl not found, skipping service installation."
  else
    echo "==> Installing miabd systemd service..."
    SCRIPT_DIR="$(dirname "$(readlink -f "$0" 2>/dev/null || echo "$0")")"
    SERVICE_FILE="${SCRIPT_DIR}/miabd.service"
    DEFAULT_CONFIG="${SCRIPT_DIR}/miabd.default"

    if [[ -f "${SERVICE_FILE}" ]]; then
      ${SUDO} install -m 0644 "${SERVICE_FILE}" /etc/systemd/system/miabd.service
    else
      echo "Downloading miabd.service from repository..."
      ${SUDO} curl -sSfL "https://raw.githubusercontent.com/${REPO}/main/deploy/miabd.service" \
        -o /etc/systemd/system/miabd.service
    fi

    if [[ ! -f /etc/default/miabd ]] && [[ -f "${DEFAULT_CONFIG}" ]]; then
      ${SUDO} install -m 0644 "${DEFAULT_CONFIG}" /etc/default/miabd
    fi

    ${SUDO} systemctl daemon-reload
    ${SUDO} systemctl enable --now miabd
    echo "✓ Enabled and started miabd.service"
  fi
fi

echo ""
echo "miab-transfer installation complete!"
echo "Run 'miab --help' or 'miabd --help' to get started."
