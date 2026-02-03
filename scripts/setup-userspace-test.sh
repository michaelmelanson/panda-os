#!/bin/bash
# Set up build directory for a userspace test
# Usage: setup-userspace-test.sh <test-name> [extra-binaries...]

set -e

TEST_NAME="$1"
shift
EXTRA_BINARIES=("$@")

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
PROFILE_DIR="${PROFILE_DIR:-debug}"

BUILD_DIR="$PROJECT_DIR/build/utest-$TEST_NAME"
mkdir -p "$BUILD_DIR/efi/boot"
mkdir -p "$BUILD_DIR/initrd"

# Copy kernel
cp "$PROJECT_DIR/target/x86_64-panda-uefi/$PROFILE_DIR/panda-kernel.efi" \
    "$BUILD_DIR/efi/boot/bootx64.efi"

# Copy test binary as init
cp "$PROJECT_DIR/target/x86_64-panda-userspace/$PROFILE_DIR/$TEST_NAME" \
    "$BUILD_DIR/initrd/init"

# Create test files in initrd
echo "Hello from the initrd!" > "$BUILD_DIR/initrd/hello.txt"

# Build list of files to include in initrd
INITRD_FILES="init hello.txt"

# Copy extra binaries if specified
for binary in "${EXTRA_BINARIES[@]}"; do
    if [[ -n "$binary" ]]; then
        cp "$PROJECT_DIR/target/x86_64-panda-userspace/$PROFILE_DIR/$binary" \
            "$BUILD_DIR/initrd/$binary"
        INITRD_FILES="$INITRD_FILES $binary"
    fi
done

# Create initrd tar
tar --format=ustar -cf "$BUILD_DIR/efi/initrd.tar" \
    -C "$BUILD_DIR/initrd" $INITRD_FILES

echo 'fs0:\efi\boot\bootx64.efi' > "$BUILD_DIR/efi/boot/startup.nsh"

TEST_SRC_DIR="$PROJECT_DIR/userspace/tests/$TEST_NAME"

# Create test disk for block tests (triggered by needs-block marker file)
if [ -f "$TEST_SRC_DIR/needs-block" ]; then
    dd if=/dev/zero of="$BUILD_DIR/test-disk.img" bs=1M count=1 2>/dev/null
fi

# Create ext2 disk (triggered by needs-ext2 marker file)
if [ -f "$TEST_SRC_DIR/needs-ext2" ]; then
    dd if=/dev/zero of="$BUILD_DIR/test-disk.img" bs=1M count=10 2>/dev/null
    mkfs.ext2 -F "$BUILD_DIR/test-disk.img" >/dev/null 2>&1
    # Populate with test files using debugfs
    echo "Hello from ext2!" > "$BUILD_DIR/hello.txt"
    echo "Nested file content" > "$BUILD_DIR/nested.txt"
    dd if=/dev/urandom of="$BUILD_DIR/large.bin" bs=1024 count=8 2>/dev/null
    echo "Deep file" > "$BUILD_DIR/deep.txt"
    # Create debugfs commands file
    cat > "$BUILD_DIR/debugfs_cmds.txt" << DEBUGFS_EOF
mkdir subdir
mkdir a
mkdir a/b
mkdir a/b/c
write $BUILD_DIR/hello.txt hello.txt
write $BUILD_DIR/nested.txt subdir/nested.txt
write $BUILD_DIR/large.bin large.bin
write $BUILD_DIR/deep.txt a/b/c/deep.txt
DEBUGFS_EOF
    debugfs -w "$BUILD_DIR/test-disk.img" -f "$BUILD_DIR/debugfs_cmds.txt" 2>/dev/null
    rm -f "$BUILD_DIR/hello.txt" "$BUILD_DIR/nested.txt" "$BUILD_DIR/large.bin" "$BUILD_DIR/deep.txt" "$BUILD_DIR/debugfs_cmds.txt"
fi
