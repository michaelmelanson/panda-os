#!/bin/bash
# Run multiple tests in parallel and report results
# Usage: run-tests.sh <test-type> <test1> [test2] ...
#
# test-type: "kernel" or "userspace"
#
# Environment variables:
#   MAX_PARALLEL - max concurrent tests (default: unlimited)
#   TEST_TIMEOUT - per-test timeout in seconds (default: 60)

set -e

TEST_TYPE="$1"
shift
TESTS=("$@")

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
MAX_PARALLEL="${MAX_PARALLEL:-0}"
TEST_TIMEOUT="${TEST_TIMEOUT:-60}"

if [ ${#TESTS[@]} -eq 0 ]; then
    echo "No tests specified"
    exit 1
fi

# Run tests with optional parallelism limit
pids=()
for test in "${TESTS[@]}"; do
    if [ "$TEST_TYPE" = "kernel" ]; then
        BUILD_DIR="$PROJECT_DIR/build/test-$test"
        LOG_FILE="$PROJECT_DIR/build/test-$test.log"
        EXPECTED_FILE=""
        MONITOR_FILE=""
    else
        BUILD_DIR="$PROJECT_DIR/build/utest-$test"
        LOG_FILE="$PROJECT_DIR/build/utest-$test.log"
        EXPECTED_FILE="$PROJECT_DIR/userspace/tests/$test/expected.txt"
        MONITOR_FILE="$PROJECT_DIR/userspace/tests/$test/monitor.txt"
    fi

    "$SCRIPT_DIR/run-qemu-test.sh" "$test" "$BUILD_DIR" "$LOG_FILE" "$TEST_TIMEOUT" "$EXPECTED_FILE" "$MONITOR_FILE" &
    pids+=($!)

    # Throttle parallelism if MAX_PARALLEL is set
    if [ "$MAX_PARALLEL" -gt 0 ] 2>/dev/null; then
        while [ "$(jobs -rp | wc -l)" -ge "$MAX_PARALLEL" ]; do
            sleep 0.5
        done
    fi
done

# Wait for all tests and collect exit codes
declare -A results
for i in "${!TESTS[@]}"; do
    wait "${pids[$i]}" && results[${TESTS[$i]}]=0 || results[${TESTS[$i]}]=$?
done

# Report results
failed=0
for test in "${TESTS[@]}"; do
    exit_code=${results[$test]}
    if [ "$TEST_TYPE" = "kernel" ]; then
        LOG_FILE="$PROJECT_DIR/build/test-$test.log"
    else
        LOG_FILE="$PROJECT_DIR/build/utest-$test.log"
    fi

    case $exit_code in
        0)
            if [ "$TEST_TYPE" = "kernel" ]; then
                grep -E "^(Running |.*\.\.\.|All tests)" "$LOG_FILE" 2>/dev/null || true
            fi
            echo "Test $test: PASSED"
            [ "$TEST_TYPE" = "kernel" ] && echo ""
            ;;
        2)
            echo "Test $test: TIMEOUT"
            echo "Full log: $LOG_FILE"
            failed=1
            ;;
        *)
            if [ "$TEST_TYPE" = "kernel" ]; then
                grep -E "^(Running |.*\.\.\.|All tests|\[failed\]|Error:)" "$LOG_FILE" 2>/dev/null || true
            fi
            echo "Test $test: FAILED (exit code $exit_code)"
            echo "Full log: $LOG_FILE"
            failed=1
            ;;
    esac
done

if [ $failed -eq 1 ]; then
    exit 1
fi

echo "=== All tests passed ==="
