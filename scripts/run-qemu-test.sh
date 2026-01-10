#!/bin/bash
# Run a single test in QEMU and report pass/fail
# Usage: run-qemu-test.sh <test-name> <build-dir> <log-file> [timeout] [expected-file] [monitor-file]
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
MONITOR_FILE="${6:-}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Base QEMU command
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

# If we have monitor commands, run QEMU with a monitor socket
if [ -n "$MONITOR_FILE" ] && [ -f "$MONITOR_FILE" ]; then
    MONITOR_SOCK="/tmp/qemu-test-$$.sock"
    QEMU_CMD+=(-monitor "unix:$MONITOR_SOCK,server,nowait")

    cleanup() {
        rm -f "$MONITOR_SOCK"
        jobs -p | xargs -r kill 2>/dev/null
    }
    trap cleanup EXIT

    # Start QEMU in background
    "${QEMU_CMD[@]}" > "$LOG_FILE" 2>&1 &
    QEMU_PID=$!

    # Wait for monitor socket
    for i in {1..50}; do
        [ -S "$MONITOR_SOCK" ] && break
        sleep 0.1
    done

    if [ ! -S "$MONITOR_SOCK" ]; then
        echo "Failed to create QEMU monitor socket" >&2
        kill $QEMU_PID 2>/dev/null
        exit 1
    fi

    # Execute monitor commands
    while IFS= read -r line || [ -n "$line" ]; do
        # Skip comments and blank lines
        [[ "$line" =~ ^[[:space:]]*# ]] && continue
        [[ -z "${line// }" ]] && continue

        # Handle sleep command specially
        if [[ "$line" =~ ^sleep[[:space:]]+([0-9.]+) ]]; then
            sleep "${BASH_REMATCH[1]}"
        else
            # Use nc (netcat) for Unix socket if available, otherwise try socat
            if command -v nc >/dev/null 2>&1 && nc -h 2>&1 | grep -q "Unix"; then
                echo "$line" | nc -U "$MONITOR_SOCK" > /dev/null 2>&1
            elif command -v socat >/dev/null 2>&1; then
                echo "$line" | socat - "unix-connect:$MONITOR_SOCK" > /dev/null 2>&1
            else
                # Fallback: use bash /dev/tcp-like feature (requires special handling for unix sockets)
                # Use python as last resort
                python3 -c "import socket; s=socket.socket(socket.AF_UNIX); s.connect('$MONITOR_SOCK'); s.send(b'$line\n'); s.close()" 2>/dev/null
            fi
            sleep 0.1
        fi
    done < "$MONITOR_FILE"

    # Wait for QEMU to exit with timeout
    ELAPSED=0
    while kill -0 $QEMU_PID 2>/dev/null; do
        sleep 0.5
        ELAPSED=$((ELAPSED + 1))
        if [ $ELAPSED -ge $((TIMEOUT * 2)) ]; then
            kill $QEMU_PID 2>/dev/null
            EXIT_CODE=124
            break
        fi
    done

    if [ -z "$EXIT_CODE" ]; then
        wait $QEMU_PID
        EXIT_CODE=$?
    fi
else
    # No monitor commands - run normally with timeout
    timeout "$TIMEOUT" "${QEMU_CMD[@]}" > "$LOG_FILE" 2>&1
    EXIT_CODE=$?
fi

if [ $EXIT_CODE -eq 124 ]; then
    exit 2  # timeout
elif [ $EXIT_CODE -ne 33 ]; then
    exit 1  # failed
fi

# QEMU exited successfully, now check expected log patterns if specified
if [ -n "$EXPECTED_FILE" ] && [ -f "$EXPECTED_FILE" ]; then
    # Extract just the LOG: messages from the test output
    # Use -a to treat binary files (with escape sequences) as text
    LOG_MESSAGES=$(grep -a "INFO: LOG:" "$LOG_FILE" | sed 's/.*INFO: LOG: //')

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
