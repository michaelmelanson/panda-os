#!/usr/bin/env bash
# Headless full-boot repro: all four virtio devices, no display, serial to a
# log file. Mirrors the Makefile's `run` target but with `-display none` and
# a bounded timeout, for verifying a boot end-to-end without a GUI.
#
# Usage: boot-repro.sh <log-file> [timeout-seconds]
set -u

LOG_FILE="${1:-build/boot-repro.log}"
TIMEOUT="${2:-400}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

timeout "$TIMEOUT" qemu-system-x86_64 -nodefaults \
    -machine q35,kernel-irqchip=split -m 1G \
    -cpu qemu64,+smap \
    -device intel-iommu \
    -serial stdio \
    -boot menu=off \
    -display none \
    -device virtio-gpu,xres=1920,yres=1080 \
    -device virtio-mouse \
    -device virtio-keyboard \
    -drive "if=pflash,format=raw,readonly=on,file=$PROJECT_DIR/firmware/OVMF_CODE_4M.fd" \
    -drive "if=pflash,format=raw,readonly=on,file=$PROJECT_DIR/firmware/OVMF_VARS_4M.fd" \
    -drive "format=raw,file=fat:rw:$PROJECT_DIR/build/run" \
    -drive "file=$PROJECT_DIR/build/test.ext2,format=raw,if=none,id=ext2disk" \
    -device virtio-blk-pci,drive=ext2disk \
    -no-shutdown -no-reboot \
    > "$LOG_FILE" 2>&1

echo "boot repro exited with $? (log: $LOG_FILE)"
