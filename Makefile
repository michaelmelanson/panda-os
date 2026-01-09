SHELL := /bin/bash
.PHONY: build panda-kernel init run test userspace-test

KERNEL_TESTS := basic heap pci memory scheduler process nx_bit raii apic
USERSPACE_TESTS := vfs_test preempt_test spawn_test

# Extra binaries needed for specific tests (space-separated)
spawn_test_EXTRAS := spawn_child

# Build targets
build: panda-kernel init
	cp target/x86_64-panda-uefi/debug/panda-kernel.efi build/efi/boot/bootx64.efi
	mkdir -p build/initrd
	cp target/x86_64-panda-userspace/debug/init build/initrd/init
	echo "Hello from the initrd!" > build/initrd/hello.txt
	tar --format=ustar -cf build/efi/initrd.tar -C build/initrd init hello.txt
	echo "fs0:\efi\boot\bootx64.efi" > build/efi/boot/startup.nsh

panda-kernel:
	cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json

init:
	cargo +nightly build -Z build-std=core --package init --target ./x86_64-panda-userspace.json

run: build
	$(QEMU_COMMON) \
		-drive format=raw,file=fat:rw:build \
		-no-shutdown -no-reboot \
		-display gtk \
		-monitor vc

# Kernel tests
test:
	@echo "Building kernel tests..."
	@cargo +nightly test --package panda-kernel --target ./x86_64-panda-uefi.json --no-run 2>&1 | grep -E "Compiling|Executable"
	@echo ""
	@for test in $(KERNEL_TESTS); do \
		./scripts/setup-kernel-test.sh $$test; \
	done
	@echo "Running kernel tests..."
	@./scripts/run-tests.sh kernel $(KERNEL_TESTS)

# Userspace tests
userspace-test: panda-kernel
	@echo "Building userspace tests..."
	@for test in $(USERSPACE_TESTS); do \
		cargo +nightly build -Z build-std=core --package $$test --target ./x86_64-panda-userspace.json; \
		extras_var=$${test}_EXTRAS; \
		for extra in $${!extras_var}; do \
			cargo +nightly build -Z build-std=core --package $$extra --target ./x86_64-panda-userspace.json; \
		done; \
	done
	@for test in $(USERSPACE_TESTS); do \
		extras_var=$${test}_EXTRAS; \
		./scripts/setup-userspace-test.sh $$test $${!extras_var}; \
	done
	@echo "Running userspace tests..."
	@./scripts/run-tests.sh userspace $(USERSPACE_TESTS)

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
