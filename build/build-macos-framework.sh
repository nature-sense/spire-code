#!/bin/bash
# build-macos-framework.sh — Packages the Rust .dylib into a macOS .framework bundle.
#
# The framework can be embedded in the SwiftUI app or used directly.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="$PROJECT_DIR/build"
RELEASE_DIR="$PROJECT_DIR/target/release"

FRAMEWORK_NAME="SpireCore"
FRAMEWORK_DIR="$BUILD_DIR/$FRAMEWORK_NAME.framework"

echo "=== Building Rust release ==="
cargo build --release -p spire-code

echo "=== Creating .framework bundle ==="
mkdir -p "$FRAMEWORK_DIR/Versions/A"
mkdir -p "$FRAMEWORK_DIR/Resources"

# Copy the dylib into the framework
cp "$RELEASE_DIR/libspire_code.dylib" "$FRAMEWORK_DIR/Versions/A/$FRAMEWORK_NAME"

# Create symlinks
ln -sf "Versions/A/$FRAMEWORK_NAME" "$FRAMEWORK_DIR/$FRAMEWORK_NAME"
ln -sf "Versions/A" "$FRAMEWORK_DIR/Versions/Current"

# Create Info.plist
cat > "$FRAMEWORK_DIR/Resources/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key>
    <string>$FRAMEWORK_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>com.naturesense.spire-core</string>
    <key>CFBundleName</key>
    <string>$FRAMEWORK_NAME</string>
    <key>CFBundleVersion</key>
    <string>0.1.0</string>
    <key>CFBundlePackageType</key>
    <string>FMWK</string>
</dict>
</plist>
EOF

echo "=== Framework created at: $FRAMEWORK_DIR ==="