#!/bin/bash
# assemble-app.sh — Builds the Rust core + Swift UI and assembles them into a
# double-clickable macOS app bundle.
#
#   build/Spire.app
#     Contents/
#       Info.plist
#       MacOS/SpireUI                    (the SwiftUI executable)
#       Frameworks/libspire_code.dylib   (the Rust core, loaded via dlopen)
#
# Dev-runnable only — unsigned, not notarized.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Spire"
APP_DIR="$ROOT/build/$APP_NAME.app"
EXEC_NAME="SpireUI"
BUNDLE_ID="com.naturesense.spire"
MARKETING_VERSION="0.1.0"

echo "=== 1/3 Building Rust core (cdylib) ==="
cargo build --release -p spire-code

echo "=== 2/3 Building Swift UI ==="
(cd "$ROOT/ui/swift" && swift build -c release)

echo "=== 3/3 Assembling $APP_NAME.app ==="
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Frameworks" "$APP_DIR/Contents/Resources"

cp "$ROOT/ui/swift/.build/release/$EXEC_NAME" "$APP_DIR/Contents/MacOS/$EXEC_NAME"
cp "$ROOT/target/release/libspire_code.dylib" "$APP_DIR/Contents/Frameworks/libspire_code.dylib"

cat > "$APP_DIR/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>Spire</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleExecutable</key>
    <string>$EXEC_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$MARKETING_VERSION</string>
    <key>CFBundleVersion</key>
    <string>$MARKETING_VERSION</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST

chmod +x "$APP_DIR/Contents/MacOS/$EXEC_NAME"

echo ""
echo "=== Done ==="
echo "  App: $APP_DIR"
echo "  Run: open $APP_DIR   (or $APP_DIR/Contents/MacOS/$EXEC_NAME)"
