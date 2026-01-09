#!/bin/bash
# Set up build directory for a kernel test
# Usage: setup-kernel-test.sh <test-name>

set -e

TEST_NAME="$1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_DIR="$PROJECT_DIR/build/test-$TEST_NAME"
mkdir -p "$BUILD_DIR/efi/boot"

# Find the test binary
TEST_BIN=$(cargo +nightly test \
    --package panda-kernel \
    --target "$PROJECT_DIR/x86_64-panda-uefi.json" \
    --test "$TEST_NAME" \
    --no-run \
    --message-format=json 2>/dev/null | \
    jq -r 'select(.executable != null and .target.kind == ["test"]) | .executable')

cp "$TEST_BIN" "$BUILD_DIR/efi/boot/bootx64.efi"
echo 'fs0:\efi\boot\bootx64.efi' > "$BUILD_DIR/efi/boot/startup.nsh"
