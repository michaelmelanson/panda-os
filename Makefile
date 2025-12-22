
.PHONY: build
build:
	cargo build --target x86_64-unknown-uefi
	cp target/x86_64-unknown-uefi/debug/panda.efi build/efi/boot/bootx64.efi
	echo "fs0:\\\\efi\\\\boot\\\\bootx64.efi" > build/efi/boot/startup.nsh

.PHONY: run
run:
	qemu-system-x86_64 -nodefaults -enable-kvm -no-shutdown \
	    -machine pc-q35-9.2 -m 1G \
		-serial stdio -nographic \
		-device virtio-gpu \
		-device virtio-mouse \
		-device virtio-keyboard \
        -drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_CODE_4M.fd \
        -drive if=pflash,format=raw,readonly=on,file=firmware/OVMF_VARS_4M.fd \
        -drive format=raw,file=fat:rw:build
