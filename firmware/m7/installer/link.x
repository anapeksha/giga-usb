ENTRY(_start)
MEMORY { CODE (rx) : ORIGIN = 0x24000000, LENGTH = 64K }
SECTIONS {
  .entry : { KEEP(*(.entry)) } > CODE
  .text : { *(.text .text.*) *(.rodata .rodata.*) } > CODE
  .data : { *(.data .data.*) } > CODE
  .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } > CODE
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}
ASSERT(SIZEOF(.data) == 0, "loader must not require initialized writable globals")
ASSERT(SIZEOF(.bss) == 0, "loader must not require BSS initialization")
ASSERT(ADDR(.entry) == 0x24000000, "wrong vector address")
ASSERT(SIZEOF(.entry) >= 8, "missing entry vectors")
