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

# Check if this test needs screenshot verification
SCREENSHOT_TEST=0
EXPECTED_PNG=""
if [ -n "$EXPECTED_FILE" ]; then
    EXPECTED_PNG="${EXPECTED_FILE%.txt}.png"
    if [ -f "$EXPECTED_PNG" ]; then
        SCREENSHOT_TEST=1
    fi
fi

# Base QEMU command
QEMU_CMD=(
    qemu-system-x86_64 -nodefaults
    -machine q35 -m 1G
    -cpu qemu64,+smap
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
)

# Add virtio-blk drive for block tests and ext2 tests
if [ "$TEST_NAME" = "block" ] || [ "$TEST_NAME" = "block_test" ] || [ "$TEST_NAME" = "ext2_test" ]; then
    # Use explicit virtio-blk-pci device with MSI-X enabled (default vectors=3)
    QEMU_CMD+=(-drive "file=$BUILD_DIR/test-disk.img,if=none,format=raw,id=blk0")
    QEMU_CMD+=(-device "virtio-blk-pci,drive=blk0")
fi

# For screenshot tests, use monitor socket instead of isa-debug-exit
if [ $SCREENSHOT_TEST -eq 1 ]; then
    MONITOR_SOCK="/tmp/qemu-test-$$.sock"
    QEMU_CMD+=(-monitor "unix:$MONITOR_SOCK,server,nowait")
else
    QEMU_CMD+=(-device isa-debug-exit,iobase=0xf4,iosize=0x04)
fi

# Handle screenshot tests
if [ $SCREENSHOT_TEST -eq 1 ]; then
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

    # Watch for screenshot ready marker in log
    SCREENSHOT_TIMEOUT=$((TIMEOUT * 2))
    for i in $(seq 1 $SCREENSHOT_TIMEOUT); do
        if grep -q "<<<SCREENSHOT_READY>>>" "$LOG_FILE" 2>/dev/null; then
            # Test is ready for screenshot
            sleep 0.2  # Brief delay to ensure log is flushed

            # Capture screenshot
            ACTUAL_PNG_TMP="$BUILD_DIR/${TEST_NAME}_actual.ppm"
            ACTUAL_PNG="$BUILD_DIR/${TEST_NAME}_actual.png"

            # Send commands to monitor with proper formatting
            {
                sleep 0.1
                echo "screendump $ACTUAL_PNG_TMP"
                sleep 0.3
                echo "quit"
            } | nc -U "$MONITOR_SOCK" > /dev/null 2>&1

            # Wait for QEMU to exit
            wait $QEMU_PID 2>/dev/null || true

            # Verify screenshot was captured
            if [ ! -f "$ACTUAL_PNG_TMP" ]; then
                echo "Screenshot not captured: $ACTUAL_PNG_TMP" >&2
                exit 1
            fi

            # Convert PPM to PNG
            if command -v convert >/dev/null 2>&1; then
                convert "$ACTUAL_PNG_TMP" "$ACTUAL_PNG"
                rm -f "$ACTUAL_PNG_TMP"
            else
                # No ImageMagick - keep PPM format
                mv "$ACTUAL_PNG_TMP" "$ACTUAL_PNG"
            fi

            # If expected.png doesn't exist, this is the first run
            if [ ! -f "$EXPECTED_PNG" ]; then
                echo "No expected.png found - saving actual screenshot for review" >&2
                echo "Screenshot saved to: $ACTUAL_PNG" >&2
                echo "If it looks correct, copy it to: $EXPECTED_PNG" >&2
                exit 1
            fi

            # Compare screenshots
            SCREENSHOT_PASSED=0
            if command -v compare >/dev/null 2>&1; then
                DIFF_PNG="$BUILD_DIR/${TEST_NAME}_diff.png"
                # Allow 1% fuzz for anti-aliasing differences
                DIFF_PIXELS=$(compare -metric AE -fuzz 1% "$EXPECTED_PNG" "$ACTUAL_PNG" "$DIFF_PNG" 2>&1 | head -1 | awk '{print $1}' || echo "999999")
                if [ "$DIFF_PIXELS" -gt 1000 ] 2>/dev/null; then
                    echo "Screenshot differs from expected (${DIFF_PIXELS} pixels different)" >&2
                    echo "Expected: $EXPECTED_PNG" >&2
                    echo "Actual: $ACTUAL_PNG (preserved for inspection)" >&2
                    echo "Diff: $DIFF_PNG" >&2
                    echo "" >&2
                    echo "To accept new screenshot: cp $ACTUAL_PNG $EXPECTED_PNG" >&2
                    exit 1
                fi
                rm -f "$DIFF_PNG"
                SCREENSHOT_PASSED=1
            else
                # Pixel-perfect comparison
                if ! cmp -s "$EXPECTED_PNG" "$ACTUAL_PNG"; then
                    echo "Screenshot differs from expected (install ImageMagick for fuzzy compare)" >&2
                    echo "Expected: $EXPECTED_PNG" >&2
                    echo "Actual: $ACTUAL_PNG (preserved for inspection)" >&2
                    echo "" >&2
                    echo "To accept new screenshot: cp $ACTUAL_PNG $EXPECTED_PNG" >&2
                    exit 1
                fi
                SCREENSHOT_PASSED=1
            fi

            # Only clean up actual screenshot if it passed
            if [ $SCREENSHOT_PASSED -eq 1 ]; then
                rm -f "$ACTUAL_PNG"
            fi

            # Screenshot test passed - now check expected.txt if it exists
            break
        fi

        # Check if QEMU died
        if ! kill -0 $QEMU_PID 2>/dev/null; then
            echo "QEMU exited before screenshot ready" >&2
            exit 1
        fi

        sleep 0.5
    done

    # If we got here without seeing the marker, it's a timeout
    if ! grep -q "<<<SCREENSHOT_READY>>>" "$LOG_FILE" 2>/dev/null; then
        kill $QEMU_PID 2>/dev/null
        exit 2
    fi

    EXIT_CODE=0
elif [ -n "$MONITOR_FILE" ] && [ -f "$MONITOR_FILE" ]; then
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

    # Wait for the test to be ready before sending monitor commands
    # Look for a readiness marker in the log instead of using a fixed sleep
    READY_TIMEOUT=$((TIMEOUT))
    READY_MARKER=""
    # Check if the monitor file has a "sleep" as first command - replace with log-based wait
    FIRST_CMD=$(grep -v '^#' "$MONITOR_FILE" | grep -v '^[[:space:]]*$' | head -1)
    if [[ "$FIRST_CMD" =~ ^sleep ]]; then
        # Use the log to detect readiness: wait for the test to print something
        # indicating it's ready for input
        for i in $(seq 1 $((READY_TIMEOUT * 2))); do
            if grep -qa "Waiting for key events\|ready for input\|Keyboard opened" "$LOG_FILE" 2>/dev/null; then
                READY_MARKER=1
                sleep 1  # Extra settle time after readiness
                break
            fi
            if ! kill -0 $QEMU_PID 2>/dev/null; then
                break
            fi
            sleep 0.5
        done
    fi

    # Send monitor commands over a single persistent connection using Python
    python3 -c "
import socket, time, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect('$MONITOR_SOCK')
# Read initial prompt
s.settimeout(2.0)
try:
    s.recv(4096)
except:
    pass
first_sleep_skipped = ${READY_MARKER:-0}
for line in open('$MONITOR_FILE'):
    line = line.strip()
    if not line or line.startswith('#'): continue
    if line.startswith('sleep '):
        if first_sleep_skipped:
            first_sleep_skipped = 0
            continue  # Already waited via log marker
        time.sleep(float(line.split()[1]))
    else:
        s.send((line + '\n').encode())
        time.sleep(0.3)
        try:
            s.settimeout(0.5)
            s.recv(4096)
        except:
            pass
time.sleep(1.0)
s.close()
" 2>&1 | head -5 >&2 || echo "Warning: failed to send monitor commands" >&2

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
elif [ $EXIT_CODE -ne 33 ] && [ $EXIT_CODE -ne 0 ]; then
    # Exit code 33 = isa-debug-exit success
    # Exit code 0 = screenshot test success
    exit 1  # failed
fi

# QEMU exited successfully, now check expected log patterns if specified
if [ -n "$EXPECTED_FILE" ] && [ -f "$EXPECTED_FILE" ]; then
    # Extract just the LOG: messages from the test output
    # Use -a to treat binary files (with escape sequences) as text
    LOG_MESSAGES=$(grep -a "INFO: LOG:" "$LOG_FILE" | sed 's/.*INFO: LOG: //')

    # Number each log line for position tracking
    LOG_WITH_NUMS=$(echo "$LOG_MESSAGES" | nl -ba)

    # Check for @unordered mode
    if grep -q '^# @unordered' "$EXPECTED_FILE"; then
        # Unordered mode: patterns within sections can match in any order
        # Sections are separated by "# @barrier" lines

        LAST_BARRIER_POS=0
        SECTION_PATTERNS=""

        while IFS= read -r line; do
            # Skip empty lines and regular comments
            [[ -z "${line// }" ]] && continue
            [[ "$line" =~ ^#[[:space:]]*$ ]] && continue
            [[ "$line" =~ ^#[[:space:]][^@] ]] && continue

            # Skip @unordered directive
            [[ "$line" == "# @unordered" ]] && continue

            # Handle barrier
            if [[ "$line" == "# @barrier" ]]; then
                # Verify all patterns in current section, find max position
                MAX_POS=$LAST_BARRIER_POS
                if [ -n "$SECTION_PATTERNS" ]; then
                    while IFS= read -r pattern; do
                        [ -z "$pattern" ] && continue
                        # Find pattern in log with line number
                        MATCH=$(echo "$LOG_WITH_NUMS" | grep -F "$pattern" | head -1)
                        if [ -z "$MATCH" ]; then
                            echo "Expected log not found: '$pattern'" >&2
                            echo "Log messages:" >&2
                            echo "$LOG_MESSAGES" | head -10 >&2
                            exit 1
                        fi
                        POS=$(echo "$MATCH" | awk '{print $1}')
                        if [ "$POS" -le "$LAST_BARRIER_POS" ]; then
                            echo "Pattern '$pattern' found at position $POS, but barrier requires position > $LAST_BARRIER_POS" >&2
                            exit 1
                        fi
                        if [ "$POS" -gt "$MAX_POS" ]; then
                            MAX_POS=$POS
                        fi
                    done <<< "$SECTION_PATTERNS"
                fi
                LAST_BARRIER_POS=$MAX_POS
                SECTION_PATTERNS=""
                continue
            fi

            # Accumulate pattern
            SECTION_PATTERNS="${SECTION_PATTERNS}${line}"$'\n'
        done < "$EXPECTED_FILE"

        # Verify final section (after last barrier or if no barriers)
        if [ -n "$SECTION_PATTERNS" ]; then
            while IFS= read -r pattern; do
                [ -z "$pattern" ] && continue
                MATCH=$(echo "$LOG_WITH_NUMS" | grep -F "$pattern" | head -1)
                if [ -z "$MATCH" ]; then
                    echo "Expected log not found: '$pattern'" >&2
                    echo "Log messages:" >&2
                    echo "$LOG_MESSAGES" | head -10 >&2
                    exit 1
                fi
                POS=$(echo "$MATCH" | awk '{print $1}')
                if [ "$POS" -le "$LAST_BARRIER_POS" ]; then
                    echo "Pattern '$pattern' found at position $POS, but barrier requires position > $LAST_BARRIER_POS" >&2
                    exit 1
                fi
            done <<< "$SECTION_PATTERNS"
        fi
    else
        # Ordered mode (default): patterns must appear in strict order
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
fi

# Check expected fault patterns (kernel ERROR: messages) if specified
if [ -n "$EXPECTED_FILE" ]; then
    EXPECTED_FAULT_FILE="${EXPECTED_FILE%.txt}_fault.txt"
    if [ -f "$EXPECTED_FAULT_FILE" ]; then
        # Extract ERROR: messages from kernel log output
        FAULT_MESSAGES=$(grep -a "ERROR:" "$LOG_FILE" | sed 's/.*ERROR: //')

        # Read expected patterns (skip comments and blank lines)
        EXPECTED_FAULT_PATTERNS=$(grep -v '^#' "$EXPECTED_FAULT_FILE" | grep -v '^[[:space:]]*$')

        # Check that each expected pattern appears in order
        FAULT_LINE_NUM=0
        while IFS= read -r pattern; do
            [ -z "$pattern" ] && continue
            FAULT_LINE_NUM=$((FAULT_LINE_NUM + 1))
            MATCH_LINE=$(echo "$FAULT_MESSAGES" | grep -n -F "$pattern" | head -1 | cut -d: -f1)
            if [ -z "$MATCH_LINE" ]; then
                echo "Expected fault not found: '$pattern' (expected_fault.txt line $FAULT_LINE_NUM)" >&2
                echo "Kernel error messages:" >&2
                echo "$FAULT_MESSAGES" | head -5 >&2
                exit 1
            fi
            # Remove all lines up to and including the match
            FAULT_MESSAGES=$(echo "$FAULT_MESSAGES" | tail -n +$((MATCH_LINE + 1)))
        done <<< "$EXPECTED_FAULT_PATTERNS"
    fi
fi

exit 0  # passed
