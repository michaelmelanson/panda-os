SHELL := /bin/bash
.PHONY: build panda-kernel init run test kernel-test userspace-test

KERNEL_TESTS := basic heap pci memory scheduler process nx_bit raii apic resource
USERSPACE_TESTS := vfs_test preempt_test spawn_test yield_test heap_test print_test resource_test keyboard_test state_test readdir_test buffer_test surface_test window_test multi_window_test alpha_test partial_refresh_test window_move_test

# Extra binaries needed for specific tests (space-separated)
spawn_test_EXTRAS := spawn_child
yield_test_EXTRAS := yield_child
preempt_test_EXTRAS := preempt_child
export spawn_test_EXTRAS yield_test_EXTRAS preempt_test_EXTRAS

# Build targets
build: panda-kernel init terminal
	mkdir -p build/run/efi/boot
	mkdir -p build/run/initrd
	cp target/x86_64-panda-uefi/debug/panda-kernel.efi build/run/efi/boot/bootx64.efi
	cp target/x86_64-panda-userspace/debug/init build/run/initrd/init
	cp target/x86_64-unknown-none/debug/terminal build/run/initrd/terminal
	echo "Hello from the initrd!" > build/run/initrd/hello.txt
	tar --format=ustar -cf build/run/efi/initrd.tar -C build/run/initrd init terminal hello.txt
	echo 'fs0:\efi\boot\bootx64.efi' > build/run/efi/boot/startup.nsh

panda-kernel:
	cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json

init:
	cargo +nightly build -Z build-std=core,alloc --package init --target ./x86_64-panda-userspace.json

terminal:
	RUSTFLAGS="-C relocation-model=static -C code-model=large -C link-arg=-Tx86_64-panda-userspace.ld" cargo +nightly build --package terminal --target x86_64-unknown-none

run: build
	$(QEMU_COMMON) \
		-drive format=raw,file=fat:rw:build/run \
		-no-shutdown -no-reboot \
		-display gtk \
		-monitor vc

# All tests
test: kernel-test userspace-test

# Kernel tests
# Use 'cargo build --tests' instead of 'cargo test --no-run' to avoid dual-profile
# issues with build-std (cargo test builds deps in both test and dev profiles)
ifdef TEST
kernel-test:
	@if echo "$(KERNEL_TESTS)" | grep -q -w "$(TEST)"; then \
		echo "Building kernel test $(TEST)..."; \
		cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json --tests 2>&1 | grep -E "Compiling|Executable" || true; \
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
	@cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json --tests 2>&1 | grep -E "Compiling|Executable" || true
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
	-device virtio-gpu \
	-device virtio-mouse \
	-device virtio-keyboard \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd
