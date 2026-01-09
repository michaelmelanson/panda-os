#!/bin/bash
# Set up build directory for a userspace test
# Usage: setup-userspace-test.sh <test-name> [extra-binaries...]

set -e

TEST_NAME="$1"
shift
EXTRA_BINARIES=("$@")

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

BUILD_DIR="$PROJECT_DIR/build/utest-$TEST_NAME"
mkdir -p "$BUILD_DIR/efi/boot"
mkdir -p "$BUILD_DIR/initrd"

# Copy kernel
cp "$PROJECT_DIR/target/x86_64-panda-uefi/debug/panda-kernel.efi" \
    "$BUILD_DIR/efi/boot/bootx64.efi"

# Copy test binary as init
cp "$PROJECT_DIR/target/x86_64-panda-userspace/debug/$TEST_NAME" \
    "$BUILD_DIR/initrd/init"

# Create test files in initrd
echo "Hello from the initrd!" > "$BUILD_DIR/initrd/hello.txt"

# Build list of files to include in initrd
INITRD_FILES="init hello.txt"

# Copy extra binaries if specified
for binary in "${EXTRA_BINARIES[@]}"; do
    if [[ -n "$binary" ]]; then
        cp "$PROJECT_DIR/target/x86_64-panda-userspace/debug/$binary" \
            "$BUILD_DIR/initrd/$binary"
        INITRD_FILES="$INITRD_FILES $binary"
    fi
done

# Create initrd tar
tar --format=ustar -cf "$BUILD_DIR/efi/initrd.tar" \
    -C "$BUILD_DIR/initrd" $INITRD_FILES

echo 'fs0:\efi\boot\bootx64.efi' > "$BUILD_DIR/efi/boot/startup.nsh"
