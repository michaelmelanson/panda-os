.PHONY: build panda-kernel init run

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
	qemu-system-x86_64 -nodefaults -no-shutdown -no-reboot \
	    -s -S \
	    -D qemu.log -d int,pcall,cpu_reset,guest_errors,strace \
	    -machine pc-q35-9.2 -m 1G \
		-display gtk \
		-serial stdio \
		-monitor vc \
		-device virtio-gpu \
		-device virtio-mouse \
		-device virtio-keyboard \
        -drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
        -drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd \
        -drive format=raw,file=fat:rw:build
