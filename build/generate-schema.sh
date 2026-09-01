#!/bin/bash
# generate-schema.sh — Runs flatc to produce Rust and Swift bindings from FlatBuffers schemas.
#
# Prerequisites: flatc must be on PATH (install via `brew install flatbuffers`).

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
SCHEMA_DIR="$PROJECT_DIR/schema"

# Output directories
RUST_OUT="$PROJECT_DIR/crates/spire-code/src/generated"
SWIFT_OUT="$PROJECT_DIR/ui/swift/Sources/SpireUI/Generated"

mkdir -p "$RUST_OUT" "$SWIFT_OUT"

echo "=== Generating Rust bindings ==="
flatc --rust -o "$RUST_OUT" "$SCHEMA_DIR"/*.fbs

echo "=== Generating Swift bindings ==="
flatc --swift -o "$SWIFT_OUT" "$SCHEMA_DIR"/*.fbs

echo "=== Codegen complete ==="
echo "  Rust:  $RUST_OUT"
echo "  Swift: $SWIFT_OUT"