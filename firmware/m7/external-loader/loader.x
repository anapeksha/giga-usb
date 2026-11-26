ENTRY(Init)
EXTERN(Init Read Write SectorErase MassErase StorageInfo)
MEMORY { RAM (rwx) : ORIGIN = 0x24000004, LENGTH = 0xFFFC }
PHDRS { loader PT_LOAD; info PT_LOAD; }
SECTIONS {
 .text : { *(.text .text.*) *(.rodata .rodata.*) } > RAM :loader
 .data : { *(.data .data.*) } > RAM :loader
 .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } > RAM :loader
 .Dev_Info : { KEEP(*(.Dev_Info)) } > RAM :info
 Device_Info = StorageInfo;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) }
 ASSERT(SIZEOF(.data) == 0, "No runtime data initialization supplied")
 ASSERT(SIZEOF(.bss) == 0, "No runtime BSS initialization supplied")
 ASSERT(SIZEOF(.Dev_Info) == 200, "ST metadata ABI mismatch")
}
