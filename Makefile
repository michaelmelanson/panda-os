.PHONY: build panda-kernel init run test

# Common QEMU parameters
QEMU_COMMON = qemu-system-x86_64 -nodefaults \
	-machine pc-q35-9.2 -m 1G \
	-serial stdio \
	-device virtio-gpu \
	-device virtio-mouse \
	-device virtio-keyboard \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
	-drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd \
	-drive format=raw,file=fat:rw:build

build: panda-kernel init
	cp target/x86_64-panda-uefi/debug/panda-kernel.efi build/efi/boot/bootx64.efi
	cp target/x86_64-panda-userspace/debug/init build/efi/init
	echo "fs0:\\\\efi\\\\boot\\\\bootx64.efi" > build/efi/boot/startup.nsh

panda-kernel:
	cargo +nightly build --package panda-kernel --target ./x86_64-panda-uefi.json

init:
	cargo +nightly build -Z build-std=core --package init --target ./x86_64-panda-userspace.json

run:
	rm qemu.log
	$(QEMU_COMMON) \
		-no-shutdown -no-reboot \
		-s -S \
		-D qemu.log -d int,pcall,cpu_reset,guest_errors,strace \
		-display gtk \
		-monitor vc

test:
	@for test in basic heap pci memory scheduler process; do \
		echo "=== Running test: $$test ==="; \
		cargo +nightly test --package panda-kernel --target ./x86_64-panda-uefi.json --test $$test --no-run 2>&1 | tail -1; \
		TEST_BIN=$$(cargo +nightly test --package panda-kernel --target ./x86_64-panda-uefi.json --test $$test --no-run --message-format=json 2>/dev/null | jq -r 'select(.executable != null and .target.kind == ["test"]) | .executable'); \
		cp "$$TEST_BIN" build/efi/boot/bootx64.efi; \
		timeout 60 $(QEMU_COMMON) \
			-accel kvm -accel tcg \
			-display none \
			-device isa-debug-exit,iobase=0xf4,iosize=0x04; \
		EXIT_CODE=$$?; \
		if [ $$EXIT_CODE -eq 33 ]; then \
			echo ""; \
			echo "Test $$test: PASSED"; \
		elif [ $$EXIT_CODE -eq 124 ]; then \
			echo ""; \
			echo "Test $$test: TIMEOUT"; \
			exit 1; \
		else \
			echo ""; \
			echo "Test $$test: FAILED (exit code $$EXIT_CODE)"; \
			exit 1; \
		fi; \
	done
	@echo ""
	@echo "=== All tests passed ==="
