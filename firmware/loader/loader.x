ENTRY(Init)
EXTERN(Init Read Write SectorErase MassErase StorageInfo LoaderDiagnostics)
MEMORY { RAM (rwx) : ORIGIN = 0x24000004, LENGTH = 0xFFFC }
PHDRS { loader PT_LOAD; info PT_LOAD; }
SECTIONS {
 .text : { *(.text .text.*) *(.rodata .rodata.*) } > RAM :loader
 .data : { *(.data .data.*) } > RAM :loader
 .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } > RAM :loader
 .loader_diagnostics ALIGN(4) : { KEEP(*(.loader_diagnostics)) } > RAM :loader
 /* Measured Cube 2.24 entry MSP = loaded end + 0x3fc, not a universal ABI.
    Include real ELF bytes so that placement gives an eight-byte-aligned MSP. */
 .loader_tail : { . = ALIGN(8); LONG(0); __loader_code_end = .; } > RAM :loader
 .Dev_Info : { KEEP(*(.Dev_Info)) } > RAM :info
 Device_Info = StorageInfo;
 /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.comment) }
 ASSERT(SIZEOF(.data) == 0, "No runtime data initialization supplied")
 ASSERT(SIZEOF(.bss) == 0, "No runtime BSS initialization supplied")
 ASSERT(SIZEOF(.loader_diagnostics) == 84, "Loader diagnostic ABI mismatch")
 ASSERT(LoaderDiagnostics == ADDR(.loader_diagnostics), "Loader diagnostic placement mismatch")
 ASSERT(ADDR(.loader_tail) == ADDR(.loader_diagnostics) + SIZEOF(.loader_diagnostics), "Loader tail placement mismatch")
 ASSERT(__loader_code_end == ADDR(.loader_tail) + SIZEOF(.loader_tail), "Loader loaded-end mismatch")
 ASSERT(__loader_code_end % 8 == 4, "Cube 2.24 stack placement alignment mismatch")
 ASSERT(SIZEOF(.Dev_Info) == 200, "ST metadata ABI mismatch")
}
