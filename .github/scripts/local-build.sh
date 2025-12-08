#!/bin/bash
set -e

BINARY_NAME="source-dumper"

echo "🔨 Building for current platform..."
cargo build --release

echo "📦 Preparing artifact..."
mkdir -p dist

# Detect OS
case "$(uname -s)" in
    Linux*)
        cp target/release/$BINARY_NAME dist/$BINARY_NAME-linux-x64
        strip dist/$BINARY_NAME-linux-x64 2>/dev/null || true
        chmod +x dist/$BINARY_NAME-linux-x64
        cd dist && sha256sum $BINARY_NAME-linux-x64 > $BINARY_NAME-linux-x64.sha256
        ;;
    Darwin*)
        ARCH=$(uname -m)
        if [ "$ARCH" = "arm64" ]; then
            SUFFIX="macos-arm64"
        else
            SUFFIX="macos-x64"
        fi
        cp target/release/$BINARY_NAME dist/$BINARY_NAME-$SUFFIX
        strip dist/$BINARY_NAME-$SUFFIX 2>/dev/null || true
        chmod +x dist/$BINARY_NAME-$SUFFIX
        cd dist && shasum -a 256 $BINARY_NAME-$SUFFIX > $BINARY_NAME-$SUFFIX.sha256
        ;;
    MINGW*|CYGWIN*|MSYS*)
        cp target/release/$BINARY_NAME.exe dist/$BINARY_NAME-windows-x64.exe
        ;;
esac

echo "✅ Done!"
ls -la dist/