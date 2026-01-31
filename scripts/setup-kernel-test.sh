#!/bin/bash
# Set up build directory for a kernel test
# Usage: setup-kernel-test.sh <test-name>

set -e

TEST_NAME="$1"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
PROFILE_DIR="${PROFILE_DIR:-debug}"

BUILD_DIR="$PROJECT_DIR/build/test-$TEST_NAME"
DEPS_DIR="$PROJECT_DIR/target/x86_64-panda-uefi/$PROFILE_DIR/deps"

# Find the test binary (newest .efi matching the test name)
TEST_BIN=$(ls -t "$DEPS_DIR/$TEST_NAME"-*.efi 2>/dev/null | head -1)

if [ -z "$TEST_BIN" ]; then
    echo "ERROR: No test binary found for '$TEST_NAME' in $DEPS_DIR" >&2
    exit 1
fi

mkdir -p "$BUILD_DIR/efi/boot"
cp "$TEST_BIN" "$BUILD_DIR/efi/boot/bootx64.efi"
echo 'fs0:\efi\boot\bootx64.efi' > "$BUILD_DIR/efi/boot/startup.nsh"

# Create test disk for block tests
if [ "$TEST_NAME" = "block" ]; then
    dd if=/dev/zero of="$BUILD_DIR/test-disk.img" bs=1M count=1 2>/dev/null
fi
