SHELL := /bin/bash
.PHONY: build panda-kernel init run test kernel-test userspace-test ext2-image clean-ext2

KERNEL_TESTS := basic heap pci memory scheduler process nx_bit raii apic resource block device_path
USERSPACE_TESTS := vfs_test preempt_test spawn_test yield_test heap_test print_test resource_test keyboard_test mailbox_keyboard_test state_test readdir_test buffer_test surface_test window_test multi_window_test alpha_test partial_refresh_test window_move_test block_test ext2_test device_path_test channel_test mailbox_test args_test

# Ext2 test disk image
EXT2_IMAGE = build/test.ext2

# Extra binaries needed for specific tests (space-separated)
spawn_test_EXTRAS := spawn_child
yield_test_EXTRAS := yield_child
preempt_test_EXTRAS := preempt_child
channel_test_EXTRAS := channel_child
mailbox_test_EXTRAS := mailbox_child
args_test_EXTRAS := args_child
export spawn_test_EXTRAS yield_test_EXTRAS preempt_test_EXTRAS channel_test_EXTRAS mailbox_test_EXTRAS args_test_EXTRAS

# Build targets
build: panda-kernel init terminal hello ls cat
	mkdir -p build/run/efi/boot
	mkdir -p build/run/initrd
	cp target/x86_64-panda-uefi/debug/panda-kernel.efi build/run/efi/boot/bootx64.efi
	cp target/x86_64-panda-userspace/debug/init build/run/initrd/init
	echo "Hello from the initrd!" > build/run/initrd/hello.txt
	tar --format=ustar -cf build/run/efi/initrd.tar -C build/run/initrd init hello.txt
	echo 'fs0:\efi\boot\bootx64.efi' > build/run/efi/boot/startup.nsh

panda-kernel:
	cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json

init:
	cargo +nightly build -Z build-std=core,alloc --package init --target ./x86_64-panda-userspace.json

terminal:
	cargo +nightly build -Z build-std=core,alloc --package terminal --target ./x86_64-panda-userspace.json

hello:
	cargo +nightly build -Z build-std=core,alloc --package hello --target ./x86_64-panda-userspace.json

ls:
	cargo +nightly build -Z build-std=core,alloc --package ls --target ./x86_64-panda-userspace.json

cat:
	cargo +nightly build -Z build-std=core,alloc --package cat --target ./x86_64-panda-userspace.json

run: build ext2-image
	$(QEMU_COMMON) \
		-drive format=raw,file=fat:rw:build/run \
		-drive file=$(EXT2_IMAGE),format=raw,if=none,id=ext2disk \
		-device virtio-blk-pci,drive=ext2disk \
		-no-shutdown -no-reboot \
		-display gtk \
		-monitor vc

# Create ext2 test disk image
ext2-image: $(EXT2_IMAGE)

$(EXT2_IMAGE): terminal hello ls cat
	@echo "Creating ext2 test image..."
	@mkdir -p build
	dd if=/dev/zero of=$(EXT2_IMAGE) bs=1M count=32 2>/dev/null
	mkfs.ext2 -F $(EXT2_IMAGE) >/dev/null 2>&1
	@# Populate the image using debugfs (no root required)
	@echo "Hello from ext2!" > build/hello.txt
	@echo "Nested file content" > build/nested.txt
	@dd if=/dev/urandom of=build/large.bin bs=1024 count=8 2>/dev/null
	@echo "Deep file" > build/deep.txt
	@debugfs -w $(EXT2_IMAGE) -f /dev/stdin <<< $$'mkdir subdir\nmkdir a\nmkdir a/b\nmkdir a/b/c\nwrite build/hello.txt hello.txt\nwrite build/nested.txt subdir/nested.txt\nwrite build/large.bin large.bin\nwrite build/deep.txt a/b/c/deep.txt\nwrite target/x86_64-panda-userspace/debug/terminal terminal\nwrite target/x86_64-panda-userspace/debug/hello hello\nwrite target/x86_64-panda-userspace/debug/ls ls\nwrite target/x86_64-panda-userspace/debug/cat cat' 2>/dev/null
	@rm -f build/hello.txt build/nested.txt build/large.bin build/deep.txt
	@echo "Ext2 image created: $(EXT2_IMAGE)"

clean-ext2:
	rm -f $(EXT2_IMAGE)

# All tests
test: kernel-test userspace-test

# Kernel tests
# Use 'cargo build --tests' instead of 'cargo test --no-run' to avoid dual-profile
# issues with build-std (cargo test builds deps in both test and dev profiles)
ifdef TEST
kernel-test:
	@if echo "$(KERNEL_TESTS)" | grep -q -w "$(TEST)"; then \
		echo "Building kernel test $(TEST)..."; \
		cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json --tests --features testing 2>&1 | grep -E "Compiling|Executable" || true; \
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
	@cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json --tests --features testing 2>&1 | grep -E "Compiling|Executable" || true
	@echo ""
	@for test in $(KERNEL_TESTS); do \
		./scripts/setup-kernel-test.sh $$test; \
	done
	@echo "Running kernel tests..."
	@./scripts/run-tests.sh kernel $(KERNEL_TESTS)
endif

# Userspace tests
ifdef TEST
userspace-test: panda-kernel
	@if echo "$(USERSPACE_TESTS)" | grep -q -w "$(TEST)"; then \
		echo "Building userspace test $(TEST)..."; \
		cargo +nightly build -Z build-std=core,alloc --package $(TEST) --target ./x86_64-panda-userspace.json; \
		extras_var=$(TEST)_EXTRAS; \
		for extra in $${!extras_var}; do \
			cargo +nightly build -Z build-std=core,alloc --package $$extra --target ./x86_64-panda-userspace.json; \
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
	@for test in $(USERSPACE_TESTS); do \
		cargo +nightly build -Z build-std=core,alloc --package $$test --target ./x86_64-panda-userspace.json; \
		extras_var=$${test}_EXTRAS; \
		for extra in $${!extras_var}; do \
			cargo +nightly build -Z build-std=core,alloc --package $$extra --target ./x86_64-panda-userspace.json; \
		done; \
	done
	@for test in $(USERSPACE_TESTS); do \
		extras_var=$${test}_EXTRAS; \
		./scripts/setup-userspace-test.sh $$test $${!extras_var}; \
	done
	@echo "Running userspace tests..."
	@./scripts/run-tests.sh userspace $(USERSPACE_TESTS)
endif

# QEMU command for interactive use
QEMU_COMMON = qemu-system-x86_64 -nodefaults \
	-machine pc-q35-9.2 -m 1G \
	-serial stdio \
	-boot menu=off \
	-display gtk,zoom-to-fit=on \
	-device virtio-gpu,xres=1920,yres=1080 \
	-device virtio-mouse \
	-device virtio-keyboard \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd
