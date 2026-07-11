#!/usr/bin/env bash
# Derive the kernel and userspace test lists that the Makefile drives.
#
# Usage: list-tests.sh <kernel|userspace|check-extras>
#
#   kernel        Print the names of panda-kernel's [[test]] integration
#                  tests (one per line), derived from panda-kernel/Cargo.toml.
#   userspace     Print the names of the userspace test crates (one per
#                  line), derived from the "userspace/tests/*" workspace
#                  members whose name does not end in "_child", "_producer"
#                  or "_consumer" (those are helper binaries spawned by a
#                  test, not tests themselves).
#   check-extras  Validate the Makefile's "<test>_EXTRAS" mappings: every
#                  crate referenced by an _EXTRAS variable must exist, and
#                  every helper crate (a "userspace/tests/*" member ending
#                  in "_child"/"_producer"/"_consumer") must be referenced
#                  by at least one _EXTRAS mapping. Exits non-zero on
#                  failure.
#
# This keeps the Makefile from silently skipping a test that someone added
# to Cargo.toml but forgot to hardcode into KERNEL_TESTS/USERSPACE_TESTS.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

list_kernel_tests() {
    # Matches:
    #   [[test]]
    #   name = "some_test"
    # immediately below each [[test]] header. Robust to leading/trailing
    # whitespace around the name.
    awk '
        /^\[\[test\]\]/ { want_name = 1; next }
        want_name && /^[[:space:]]*name[[:space:]]*=/ {
            line = $0
            sub(/^[^"]*"/, "", line)
            sub(/".*$/, "", line)
            print line
            want_name = 0
        }
    ' "$PROJECT_DIR/panda-kernel/Cargo.toml"
}

list_userspace_tests() {
    for dir in "$PROJECT_DIR"/userspace/tests/*/; do
        name="$(basename "$dir")"
        case "$name" in
            *_child|*_producer|*_consumer) ;; # helper crate, not a test
            *) echo "$name" ;;
        esac
    done
}

list_helper_crates() {
    for dir in "$PROJECT_DIR"/userspace/tests/*/; do
        name="$(basename "$dir")"
        case "$name" in
            *_child|*_producer|*_consumer) echo "$name" ;;
        esac
    done
}

check_extras() {
    local status=0
    local extras_line var name referenced_helpers all_helpers missing

    referenced_helpers="$(mktemp)"
    all_helpers="$(mktemp)"
    trap 'rm -f "$referenced_helpers" "$all_helpers"' RETURN

    list_helper_crates | sort -u > "$all_helpers"

    # Pull every "<test>_EXTRAS := a b c" line out of the Makefile and
    # validate that each referenced crate actually exists as a workspace
    # member directory under userspace/tests/.
    while IFS= read -r extras_line; do
        var="${extras_line%%:=*}"
        var="${var## }"
        var="${var%% }"
        values="${extras_line#*:=}"
        for name in $values; do
            if [ ! -d "$PROJECT_DIR/userspace/tests/$name" ]; then
                echo "error: $var references nonexistent crate '$name' (no userspace/tests/$name)" >&2
                status=1
            fi
            echo "$name"
        done
    done < <(grep -E '^[A-Za-z0-9_]+_EXTRAS[[:space:]]*:=' "$PROJECT_DIR/Makefile") > "$referenced_helpers"

    sort -u "$referenced_helpers" -o "$referenced_helpers"

    missing="$(comm -23 "$all_helpers" "$referenced_helpers")"
    if [ -n "$missing" ]; then
        echo "error: helper crate(s) not referenced by any _EXTRAS mapping:" >&2
        echo "$missing" >&2
        status=1
    fi

    return $status
}

case "${1:-}" in
    kernel) list_kernel_tests ;;
    userspace) list_userspace_tests ;;
    check-extras) check_extras ;;
    *)
        echo "Usage: $0 <kernel|userspace|check-extras>" >&2
        exit 1
        ;;
esac
