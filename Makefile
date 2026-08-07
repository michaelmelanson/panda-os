# Resolve bash from PATH (NixOS has no /bin/bash); $(shell) itself uses /bin/sh which is universal.
SHELL := $(shell command -v bash)
.PHONY: build panda-kernel init compositor run test kernel-test userspace-test unit-test check-extras ext2-image clean-ext2 release

# Set PROFILE=release for optimized builds: make build PROFILE=release
PROFILE ?= dev
ifeq ($(PROFILE),release)
  CARGO_PROFILE := --release
  PROFILE_DIR := release
else
  CARGO_PROFILE :=
  PROFILE_DIR := debug
endif

# Test lists are derived from the workspace itself (see scripts/list-tests.sh)
# rather than hardcoded, so a new test can't silently fail to run because
# someone forgot to add it here.
KERNEL_TESTS := $(shell ./scripts/list-tests.sh kernel)
USERSPACE_TESTS := $(shell ./scripts/list-tests.sh userspace)

# Ext2 test disk image
EXT2_IMAGE = build/test.ext2

# Extra binaries needed for specific tests (space-separated)
spawn_test_EXTRAS := spawn_child
yield_test_EXTRAS := yield_child
preempt_test_EXTRAS := preempt_child
channel_test_EXTRAS := channel_child
mailbox_test_EXTRAS := mailbox_child
mailbox_overflow_test_EXTRAS := mailbox_overflow_child
args_test_EXTRAS := args_child
pipeline_test_EXTRAS := pipeline_producer pipeline_consumer
control_plane_test_EXTRAS := control_plane_child
env_test_EXTRAS := env_child
fault_recovery_test_EXTRAS := fault_child
handle_transfer_test_EXTRAS := handle_transfer_child
# claim_test acts as its own `init` (no service manager runs to spawn
# `block:`'s/`display:`'s providers from `initrd:/drivers`) and needs both
# a real `block:/pci/storage/0` (M4 Phase 2) and a real
# `display:/pci/display/0` (roadmap M4) to open for its claim-lifecycle
# cases, so it spawns both userspace drivers directly, the same way
# block_test/ext2_*_test and display_test/compositor_start_test do (see
# those below).
claim_test_EXTRAS := claim_child virtio-blk virtio-gpu
buffer_transfer_test_EXTRAS := buffer_transfer_child
buffer_owner_test_EXTRAS := buffer_owner_child
scheme_provider_test_EXTRAS := scheme_provider_child
scheme_provider_concurrency_test_EXTRAS := scheme_provider_conc_provider_child scheme_provider_conc_client_child
# Roadmap M4: `compositor_test_child` (the shared helper every screenshot-
# verified compositor test spawns) now spawns the userspace virtio-gpu
# driver itself before starting the compositor (see that binary's doc
# comment) — it needs `virtio-gpu` present in the same initrd, and since
# `_EXTRAS` is not transitive (each top-level test's initrd is only the test
# binary plus its own `_EXTRAS` list — see `userspace-test`'s recipe below),
# every test that spawns `compositor_test_child` needs `virtio-gpu` listed
# too, not just `compositor_test_child`.
window_test_EXTRAS := compositor_test_child virtio-gpu
multi_window_test_EXTRAS := compositor_test_child virtio-gpu
alpha_test_EXTRAS := compositor_test_child virtio-gpu
partial_refresh_test_EXTRAS := compositor_test_child virtio-gpu
window_move_test_EXTRAS := compositor_test_child virtio-gpu
compositor_protocol_test_EXTRAS := compositor_test_child virtio-gpu
# keyboard_test/mailbox_keyboard_test act as their own `init` (no service
# manager runs to spawn drivers from `initrd:/drivers` — see
# `userspace/init/src/driver_registry.rs`), so they spawn the
# virtio-keyboard driver directly by path, the same way `scheme_provider_test`
# spawns its provider child. It needs to be present in the initrd for that
# `environment::spawn("file:/initrd/virtio-keyboard")` call to resolve.
keyboard_test_EXTRAS := virtio-keyboard
mailbox_keyboard_test_EXTRAS := virtio-keyboard
# block_test/ext2_*_test act as their own `init` too (see above), so they
# spawn the virtio-blk driver directly by path
# (`environment::spawn("file:/initrd/virtio-blk")`) before opening `block:`
# or calling `environment::mount("ext2", ...)` — M4 Phase 2,
# plans/device-driver-model.md.
block_test_EXTRAS := virtio-blk
ext2_test_EXTRAS := virtio-blk
ext2_create_test_EXTRAS := virtio-blk
ext2_mkdir_test_EXTRAS := virtio-blk
ext2_write_test_EXTRAS := virtio-blk
# display_test/compositor_start_test also act as their own `init` (see
# their doc comments) and spawn the userspace virtio-gpu driver (roadmap M4)
# directly.
display_test_EXTRAS := virtio-gpu
compositor_start_test_EXTRAS := virtio-gpu
export spawn_test_EXTRAS yield_test_EXTRAS preempt_test_EXTRAS channel_test_EXTRAS mailbox_test_EXTRAS mailbox_overflow_test_EXTRAS args_test_EXTRAS pipeline_test_EXTRAS control_plane_test_EXTRAS env_test_EXTRAS fault_recovery_test_EXTRAS handle_transfer_test_EXTRAS claim_test_EXTRAS buffer_transfer_test_EXTRAS buffer_owner_test_EXTRAS scheme_provider_test_EXTRAS scheme_provider_concurrency_test_EXTRAS window_test_EXTRAS multi_window_test_EXTRAS alpha_test_EXTRAS partial_refresh_test_EXTRAS window_move_test_EXTRAS compositor_protocol_test_EXTRAS keyboard_test_EXTRAS mailbox_keyboard_test_EXTRAS block_test_EXTRAS ext2_test_EXTRAS ext2_create_test_EXTRAS ext2_mkdir_test_EXTRAS ext2_write_test_EXTRAS display_test_EXTRAS compositor_start_test_EXTRAS
export PROFILE_DIR CARGO_PROFILE

# Cargo commands for custom targets (require build-std for no_std targets)
CARGO := cargo
CARGO_BUILD_STD := -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem
KERNEL_TARGET := --target ./x86_64-panda-uefi.json
USERSPACE_TARGET := --target ./x86_64-panda-userspace.json

# Build targets
build: panda-kernel init compositor terminal hello ls cat virtio-keyboard virtio-blk virtio-gpu
	mkdir -p build/run/efi/boot
	mkdir -p build/run/initrd/drivers
	cp target/x86_64-panda-uefi/$(PROFILE_DIR)/panda-kernel.efi build/run/efi/boot/bootx64.efi
	cp target/x86_64-panda-userspace/$(PROFILE_DIR)/init build/run/initrd/init
	# virtio-blk lives in the initrd (not the ext2 image, unlike
	# virtio-keyboard): the block driver that mounts ext2 must itself be
	# readable before any filesystem is mounted (M4 Phase 2,
	# plans/device-driver-model.md). `init`'s driver registry scans
	# "initrd:/drivers" (see userspace/init/src/driver_registry.rs) and
	# spawns everything found there before calling `environment::mount`.
	cp target/x86_64-panda-userspace/$(PROFILE_DIR)/virtio-blk build/run/initrd/drivers/virtio-blk
	echo "Hello from the initrd!" > build/run/initrd/hello.txt
	tar --format=ustar -cf build/run/efi/initrd.tar -C build/run/initrd init hello.txt drivers/virtio-blk
	echo 'fs0:\efi\boot\bootx64.efi' > build/run/efi/boot/startup.nsh

release:
	$(MAKE) build PROFILE=release

panda-kernel:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package panda-kernel $(KERNEL_TARGET)

init:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package init $(USERSPACE_TARGET)

compositor:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package compositor $(USERSPACE_TARGET)

terminal:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package terminal $(USERSPACE_TARGET)

hello:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package hello $(USERSPACE_TARGET)

ls:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package ls $(USERSPACE_TARGET)

cat:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package cat $(USERSPACE_TARGET)

virtio-keyboard:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package virtio-keyboard $(USERSPACE_TARGET)

virtio-blk:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package virtio-blk $(USERSPACE_TARGET)

virtio-gpu:
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package virtio-gpu $(USERSPACE_TARGET)

run: build ext2-image
	$(QEMU_COMMON) \
		-drive format=raw,file=fat:rw:build/run \
		-drive file=$(EXT2_IMAGE),format=raw,if=none,id=ext2disk \
		-device virtio-blk-pci,drive=ext2disk \
		-no-shutdown -no-reboot \
		-monitor vc

# Create ext2 test disk image
ext2-image: $(EXT2_IMAGE)

$(EXT2_IMAGE): compositor terminal hello ls cat virtio-keyboard virtio-gpu
	@echo "Creating ext2 test image..."
	@mkdir -p build
	# 32M used to be enough, but debug (unstripped) userspace binaries have
	# grown considerably since M4 added virtio-keyboard/virtio-blk/virtio-gpu
	# alongside compositor/terminal/hello/ls/cat — the full debug-profile set
	# written below now totals ~43MB, comfortably over a 32M image. debugfs's
	# `write` silently succeeds even when the filesystem is out of free
	# blocks: it still records the source file's correct *size* in the
	# inode, but leaves the file with zero allocated blocks (an
	# indistinguishable-looking sparse file), so every read of it returns
	# zeros instead of an error — the failure is invisible until something
	# actually parses the content (e.g. `userspace/init/src/driver_registry.rs`
	# reading an ELF section from a driver spawned via `file:/mnt/drivers`,
	# which found a `.panda_devices.pci`-less all-zero "binary" and silently
	# skipped it, so `init` never spawned virtio-gpu/virtio-keyboard from a
	# real ext2-mounted boot at all). 128M leaves comfortable headroom for
	# further growth without depending on exact binary sizes staying put.
	dd if=/dev/zero of=$(EXT2_IMAGE) bs=1M count=128 2>/dev/null
	mkfs.ext2 -F $(EXT2_IMAGE) >/dev/null 2>&1
	@# Populate the image using debugfs (no root required)
	@echo "Hello from ext2!" > build/hello.txt
	@echo "Nested file content" > build/nested.txt
	@dd if=/dev/urandom of=build/large.bin bs=1024 count=8 2>/dev/null
	@echo "Deep file" > build/deep.txt
	@debugfs -w $(EXT2_IMAGE) -f /dev/stdin <<< $$'mkdir subdir\nmkdir a\nmkdir a/b\nmkdir a/b/c\nmkdir drivers\nwrite build/hello.txt hello.txt\nwrite build/nested.txt subdir/nested.txt\nwrite build/large.bin large.bin\nwrite build/deep.txt a/b/c/deep.txt\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/compositor compositor\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/terminal terminal\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/hello hello\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/ls ls\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/cat cat\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/virtio-keyboard drivers/virtio-keyboard\nwrite target/x86_64-panda-userspace/$(PROFILE_DIR)/virtio-gpu drivers/virtio-gpu' 2>/dev/null
	@rm -f build/hello.txt build/nested.txt build/large.bin build/deep.txt
	@echo "Ext2 image created: $(EXT2_IMAGE)"

clean-ext2:
	rm -f $(EXT2_IMAGE)

# All tests
test: check-extras unit-test kernel-test userspace-test

# Validate the <test>_EXTRAS mappings against the workspace: every
# referenced crate must exist, and every helper crate (*_child/*_producer/
# *_consumer) must be referenced by at least one _EXTRAS mapping.
check-extras:
	@./scripts/list-tests.sh check-extras

# Rust unit tests and doctests (run on host without build-std)
unit-test:
	@echo "Running panda-abi unit tests..."
	@cargo test -p panda-abi --features std
	@echo ""
	@echo "Running ring-buffer unit tests..."
	@cargo test -p ring-buffer
	@echo ""
	@echo "Running panda-elf unit tests..."
	@cargo test -p panda-elf
	@echo ""
	@echo "Running compositor-protocol unit tests..."
	@cargo test -p compositor-protocol
	@echo ""
	@echo "Running compositor unit tests..."
	@cargo test -p compositor --no-default-features
	@echo ""
	@echo "Running libpanda doctests..."
	@cargo test -p libpanda --doc --no-default-features
	@echo ""

# Kernel tests
ifdef TEST
kernel-test:
	@if echo "$(KERNEL_TESTS)" | grep -q -w "$(TEST)"; then \
		echo "Building kernel test $(TEST)..."; \
		$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package panda-kernel $(KERNEL_TARGET) \
			--test $(TEST) --features testing; \
		./scripts/setup-kernel-test.sh $(TEST); \
		echo "Running kernel test $(TEST)..."; \
		./scripts/run-tests.sh kernel $(TEST); \
	else \
		echo "Error: Test '$(TEST)' not found in KERNEL_TESTS"; \
		exit 1; \
	fi
else
kernel-test:
	@echo "Building kernel tests..."
	$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package panda-kernel $(KERNEL_TARGET) \
		--tests --features testing
	@for test in $(KERNEL_TESTS); do \
		./scripts/setup-kernel-test.sh $$test || exit 1; \
	done
	@echo "Running kernel tests..."
	@./scripts/run-tests.sh kernel $(KERNEL_TESTS)
endif

# Userspace tests
ifdef TEST
userspace-test: panda-kernel
	@if echo "$(USERSPACE_TESTS)" | grep -q -w "$(TEST)"; then \
		echo "Building userspace test $(TEST)..."; \
		$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package $(TEST) $(USERSPACE_TARGET); \
		extras_var=$(TEST)_EXTRAS; \
		for extra in $${!extras_var}; do \
			$(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package $$extra $(USERSPACE_TARGET); \
		done; \
		./scripts/setup-userspace-test.sh $(TEST) $${!extras_var}; \
		echo "Running userspace test $(TEST)..."; \
		./scripts/run-tests.sh userspace $(TEST); \
	else \
		echo "Error: Test '$(TEST)' not found in USERSPACE_TESTS"; \
		exit 1; \
	fi
else
userspace-test: panda-kernel
	@echo "Building userspace tests..."
	@build_failed=""; \
	for test in $(USERSPACE_TESTS); do \
		if ! $(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package $$test $(USERSPACE_TARGET); then \
			build_failed="$$build_failed $$test"; \
		fi; \
		extras_var=$${test}_EXTRAS; \
		for extra in $${!extras_var}; do \
			if ! $(CARGO) build $(CARGO_BUILD_STD) $(CARGO_PROFILE) --package $$extra $(USERSPACE_TARGET); then \
				build_failed="$$build_failed $$extra"; \
			fi; \
		done; \
	done; \
	if [ -n "$$build_failed" ]; then \
		echo ""; \
		echo "BUILD FAILED for:$$build_failed"; \
		exit 1; \
	fi
	@for test in $(USERSPACE_TESTS); do \
		extras_var=$${test}_EXTRAS; \
		./scripts/setup-userspace-test.sh $$test $${!extras_var}; \
	done
	@echo "Running userspace tests..."
	@./scripts/run-tests.sh userspace $(USERSPACE_TESTS)
endif

# QEMU command for interactive use
QEMU_COMMON = qemu-system-x86_64 -nodefaults \
	-machine q35,kernel-irqchip=split -m 1G \
	-cpu qemu64,+smap \
	-device intel-iommu \
	-serial stdio \
	-boot menu=off \
	-display gtk,zoom-to-fit=off \
	-device virtio-gpu,xres=1920,yres=1080 \
	-device virtio-mouse \
	-device virtio-keyboard \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd
