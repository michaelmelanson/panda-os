
SECTIONS {
    . = 0xA0000000000;
    .text ALIGN(4096) : { *(.text .text.*); }
    .data ALIGN(4096) : { *(.data .data.*); }
    .rodata ALIGN(4096) : { *(.rodata .rodata.*); }
    .bss ALIGN(4096) : { *(.bss .bss.*); }
}
