target remote :1234
set $base = 0
add-symbol-file /home/michael/dev/panda/target/x86_64-panda-uefi/debug/panda-kernel.efi -o $base
add-symbol-file /home/michael/dev/panda/target/x86_64-panda-userspace/debug/init
set disassemble-next-line on
break *0xa0000000000
