#!/bin/sh
set -e

REPO="iamngoni/mimir"
BINARY="mimir"
INSTALL_DIR="/usr/local/bin"

# Detect OS and architecture
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-linux" ;;
      aarch64) TARGET="aarch64-linux" ;;
      *)       echo "Unsupported architecture: $ARCH" && exit 1 ;;
    esac
    ;;
  Darwin)
    case "$ARCH" in
      x86_64)  TARGET="x86_64-macos" ;;
      arm64)   TARGET="aarch64-macos" ;;
      *)       echo "Unsupported architecture: $ARCH" && exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS"
    echo "For Windows, download manually from: https://github.com/$REPO/releases/latest"
    exit 1
    ;;
esac

ARTIFACT="mimir-${TARGET}"
URL="https://github.com/${REPO}/releases/latest/download/${ARTIFACT}"

echo "Detected: $OS/$ARCH → downloading $ARTIFACT"
echo "Downloading from $URL..."

TMP="$(mktemp)"
curl -fsSL "$URL" -o "$TMP"
chmod +x "$TMP"

echo "Installing to $INSTALL_DIR/$BINARY..."
if [ -w "$INSTALL_DIR" ]; then
  mv "$TMP" "$INSTALL_DIR/$BINARY"
else
  sudo mv "$TMP" "$INSTALL_DIR/$BINARY"
fi

echo ""
echo "✅ Mimir installed successfully!"
echo "Run: mimir --help"
