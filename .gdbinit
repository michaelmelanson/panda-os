target remote :1234

set $base = 0x3D758000 - 0x0000000140000000 + 0x1000
add-symbol-file /home/michael/dev/panda/target/x86_64-panda-uefi/debug/panda-kernel.efi -o $base
add-symbol-file /home/michael/dev/panda/target/x86_64-panda-userspace/debug/init
break *0xa0000000000
break panda_kernel::breakpoint
