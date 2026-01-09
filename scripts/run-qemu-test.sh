#!/bin/bash
# Run a single test in QEMU and report pass/fail
# Usage: run-qemu-test.sh <test-name> <build-dir> <log-file> [expected-file]
#
# Exit codes:
#   0 - test passed
#   1 - test failed
#   2 - test timed out

TEST_NAME="$1"
BUILD_DIR="$2"
LOG_FILE="$3"
TIMEOUT="${4:-60}"
EXPECTED_FILE="${5:-}"

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

if [ $EXIT_CODE -eq 124 ]; then
    exit 2  # timeout
elif [ $EXIT_CODE -ne 33 ]; then
    exit 1  # failed
fi

# QEMU exited successfully, now check expected log patterns if specified
if [ -n "$EXPECTED_FILE" ] && [ -f "$EXPECTED_FILE" ]; then
    # Extract just the LOG: messages from the test output
    LOG_MESSAGES=$(grep "INFO: LOG:" "$LOG_FILE" | sed 's/.*INFO: LOG: //')

    # Read expected patterns (skip comments and blank lines)
    EXPECTED_PATTERNS=$(grep -v '^#' "$EXPECTED_FILE" | grep -v '^[[:space:]]*$')

    # Check that each expected pattern appears in order
    LINE_NUM=0
    while IFS= read -r pattern; do
        LINE_NUM=$((LINE_NUM + 1))
        # Find the pattern in remaining log messages
        MATCH_LINE=$(echo "$LOG_MESSAGES" | grep -n -F "$pattern" | head -1 | cut -d: -f1)
        if [ -z "$MATCH_LINE" ]; then
            echo "Expected log not found: '$pattern' (expected.txt line $LINE_NUM)" >&2
            echo "Remaining log messages:" >&2
            echo "$LOG_MESSAGES" | head -5 >&2
            exit 1
        fi
        # Remove all lines up to and including the match
        LOG_MESSAGES=$(echo "$LOG_MESSAGES" | tail -n +$((MATCH_LINE + 1)))
    done <<< "$EXPECTED_PATTERNS"
fi

exit 0  # passed
