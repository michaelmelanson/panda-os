#!/bin/bash
# Run the keyboard test in QEMU with automated key injection
# Usage: run-qemu-keyboard-test.sh <build-dir> <log-file>

BUILD_DIR="$1"
LOG_FILE="$2"
TIMEOUT="${3:-30}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Create a named pipe for QEMU monitor
MONITOR_SOCK="/tmp/qemu-keyboard-test-$$.sock"

QEMU_CMD=(
    qemu-system-x86_64 -nodefaults
    -machine q35 -m 1G
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
    -monitor unix:$MONITOR_SOCK,server,nowait
)

cleanup() {
    rm -f "$MONITOR_SOCK"
    # Kill any background jobs
    jobs -p | xargs -r kill 2>/dev/null
}
trap cleanup EXIT

# Start QEMU in background
"${QEMU_CMD[@]}" > "$LOG_FILE" 2>&1 &
QEMU_PID=$!

# Wait for monitor socket to be ready
for i in {1..50}; do
    if [ -S "$MONITOR_SOCK" ]; then
        break
    fi
    sleep 0.1
done

if [ ! -S "$MONITOR_SOCK" ]; then
    echo "Failed to create QEMU monitor socket" >&2
    kill $QEMU_PID 2>/dev/null
    exit 1
fi

# Wait a bit for the kernel to boot and keyboard driver to initialize
sleep 2

# Send key events via the monitor
# We need to send 5 key press/release pairs for the test
send_key() {
    echo "sendkey $1" | socat - unix-connect:$MONITOR_SOCK > /dev/null 2>&1
    sleep 0.2
}

# Send 5 key events (each sendkey sends press+release)
send_key a
send_key b
send_key c
send_key d
send_key e

# Wait for QEMU to exit (with timeout)
ELAPSED=0
while kill -0 $QEMU_PID 2>/dev/null; do
    sleep 0.5
    ELAPSED=$((ELAPSED + 1))
    if [ $ELAPSED -ge $((TIMEOUT * 2)) ]; then
        kill $QEMU_PID 2>/dev/null
        exit 2  # timeout
    fi
done

# Get exit code
wait $QEMU_PID
EXIT_CODE=$?

if [ $EXIT_CODE -eq 33 ]; then
    exit 0  # passed (QEMU exit code 33 = success via isa-debug-exit)
else
    exit 1  # failed
fi
