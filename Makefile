SHELL := /bin/bash
.PHONY: build panda-kernel init run test

TESTS := basic heap pci memory scheduler process nx_bit raii

# Common QEMU parameters (without drive, added per-test)
QEMU_COMMON = qemu-system-x86_64 -nodefaults \
	-machine pc-q35-9.2 -m 1G \
	-serial stdio \
	-device virtio-gpu \
	-device virtio-mouse \
	-device virtio-keyboard \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd

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

run:
	rm qemu.log
	$(QEMU_COMMON) \
		-drive format=raw,file=fat:rw:build \
		-no-shutdown -no-reboot \
		-display gtk \
		-monitor vc

test:
	@# Build all tests first (sequential due to cargo lock)
	@echo "Building all tests..."
	@cargo +nightly test --package panda-kernel --target ./x86_64-panda-uefi.json --no-run 2>&1 | grep -E "Compiling|Executable"
	@echo ""
	@# Set up per-test build directories
	@for test in $(TESTS); do \
		mkdir -p build/test-$$test/efi/boot; \
		TEST_BIN=$$(cargo +nightly test --package panda-kernel --target ./x86_64-panda-uefi.json --test $$test --no-run --message-format=json 2>/dev/null | jq -r 'select(.executable != null and .target.kind == ["test"]) | .executable'); \
		cp "$$TEST_BIN" build/test-$$test/efi/boot/bootx64.efi; \
		echo "fs0:\\efi\\boot\\bootx64.efi" > build/test-$$test/efi/boot/startup.nsh; \
	done
	@# Run tests in parallel
	@echo "Running tests in parallel..."
	@failed=0; \
	pids=""; \
	for test in $(TESTS); do \
		( \
			timeout 60 $(QEMU_COMMON) \
				-drive format=raw,file=fat:rw:build/test-$$test \
				-accel kvm -accel tcg \
				-display none \
				-device isa-debug-exit,iobase=0xf4,iosize=0x04 \
				> build/test-$$test.log 2>&1; \
			echo $$? > build/test-$$test.exit \
		) & \
		pids="$$pids $$!"; \
	done; \
	wait $$pids; \
	echo ""; \
	for test in $(TESTS); do \
		EXIT_CODE=$$(cat build/test-$$test.exit); \
		if [ $$EXIT_CODE -eq 33 ]; then \
			grep -E "^(Running |.*\.\.\.|All tests)" build/test-$$test.log; \
			echo "Test $$test: PASSED"; \
			echo ""; \
		elif [ $$EXIT_CODE -eq 124 ]; then \
			echo "Test $$test: TIMEOUT"; \
			echo "Full log: build/test-$$test.log"; \
			failed=1; \
		else \
			grep -E "^(Running |.*\.\.\.|All tests|\[failed\]|Error:)" build/test-$$test.log; \
			echo "Test $$test: FAILED (exit code $$EXIT_CODE)"; \
			echo "Full log: build/test-$$test.log"; \
			failed=1; \
		fi; \
	done; \
	if [ $$failed -eq 1 ]; then exit 1; fi
	@echo "=== All tests passed ==="
