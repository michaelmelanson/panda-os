
SECTIONS {
    . = 0xA0000000000;
    .text ALIGN(4096) : { *(.text .text.*); }
    .rodata ALIGN(4096) : { *(.rodata .rodata.*); }
    .data ALIGN(4096) : { *(.data .data.*); }
    .bss ALIGN(4096) : { *(.bss .bss.*); }
    .got ALIGN(4096) : { *(.got .got.*); }

    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr .eh_frame.*)
        *(.note .note.*)
        *(.comment)
    }
}
