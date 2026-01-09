#!/bin/bash
# Run a single test in QEMU and report pass/fail
# Usage: run-qemu-test.sh <test-name> <build-dir> <log-file>
#
# Exit codes:
#   0 - test passed
#   1 - test failed
#   2 - test timed out

TEST_NAME="$1"
BUILD_DIR="$2"
LOG_FILE="$3"
TIMEOUT="${4:-60}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

QEMU_CMD=(
    qemu-system-x86_64 -nodefaults
    -machine pc-q35-9.2 -m 1G
    -serial stdio
    -boot menu=off
    -device virtio-gpu
    -device virtio-mouse
    -device virtio-keyboard
    -drive "if=pflash,format=raw,readonly=on,file=$PROJECT_DIR/firmware/OVMF_CODE_4M.fd"
    -drive "if=pflash,format=raw,readonly=on,file=$PROJECT_DIR/firmware/OVMF_VARS_4M.fd"
    -drive "format=raw,file=fat:rw:$BUILD_DIR"
    -display none
    -accel kvm -accel tcg
    -device isa-debug-exit,iobase=0xf4,iosize=0x04
)

timeout "$TIMEOUT" "${QEMU_CMD[@]}" > "$LOG_FILE" 2>&1
EXIT_CODE=$?

if [ $EXIT_CODE -eq 33 ]; then
    exit 0  # passed
elif [ $EXIT_CODE -eq 124 ]; then
    exit 2  # timeout
else
    exit 1  # failed
fi
