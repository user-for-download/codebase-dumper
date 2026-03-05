#!/bin/bash
set -euo pipefail

BINARY_NAME="source-dumper"
DIST_DIR="dist"
VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')

echo "🔨 Building $BINARY_NAME v$VERSION for current platform..."
cargo build --release

echo "📦 Preparing artifact..."
rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux*)
        case "$ARCH" in
            x86_64)  SUFFIX="linux-x64" ;;
            aarch64) SUFFIX="linux-arm64" ;;
            *)       SUFFIX="linux-$ARCH" ;;
        esac
        SRC="target/release/$BINARY_NAME"
        DST="$DIST_DIR/$BINARY_NAME-$SUFFIX"

        cp "$SRC" "$DST"
        strip "$DST" 2>/dev/null || true
        chmod +x "$DST"
        cd "$DIST_DIR" && sha256sum "$(basename "$DST")" > "$(basename "$DST").sha256"
        ;;

    Darwin*)
        case "$ARCH" in
            arm64)   SUFFIX="macos-arm64" ;;
            x86_64)  SUFFIX="macos-x64" ;;
            *)       SUFFIX="macos-$ARCH" ;;
        esac
        SRC="target/release/$BINARY_NAME"
        DST="$DIST_DIR/$BINARY_NAME-$SUFFIX"

        cp "$SRC" "$DST"
        strip "$DST" 2>/dev/null || true
        chmod +x "$DST"
        cd "$DIST_DIR" && shasum -a 256 "$(basename "$DST")" > "$(basename "$DST").sha256"
        ;;

    MINGW*|CYGWIN*|MSYS*)
        SUFFIX="windows-x64"
        SRC="target/release/$BINARY_NAME.exe"
        DST="$DIST_DIR/$BINARY_NAME-$SUFFIX.exe"

        cp "$SRC" "$DST"

        # SHA256 on Windows (Git Bash has sha256sum)
        if command -v sha256sum &>/dev/null; then
            cd "$DIST_DIR" && sha256sum "$(basename "$DST")" > "$(basename "$DST").sha256"
        elif command -v certutil &>/dev/null; then
            certutil -hashfile "$DST" SHA256 > "$DST.sha256"
        fi
        ;;

    *)
        echo "❌ Unsupported OS: $OS"
        exit 1
        ;;
esac

echo ""
echo "✅ Build complete!"
echo "   Version:  $VERSION"
echo "   Platform: $SUFFIX"
echo "   Output:"
ls -lh "$DIST_DIR"/ 2>/dev/null || ls -la dist/