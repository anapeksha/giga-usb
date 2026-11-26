/* Standalone boot target: all operational code, constants and exception vectors
 * execute from private AXI SRAM before either internal bank can become busy. */
ENTRY(_boot_reset)
MEMORY {
  BOOT (rx) : ORIGIN = 0x08000000, LENGTH = 128K
  RAM (rwx) : ORIGIN = 0x24000000, LENGTH = 512K
}
SECTIONS {
  .boot_vectors ORIGIN(BOOT) : {
    LONG(ORIGIN(RAM) + LENGTH(RAM));
    LONG(_boot_reset);
    KEEP(*(.boot_flash_exceptions));
    . = ALIGN(4);
  } > BOOT
  .boot_reset : { KEEP(*(.boot_reset)); } > BOOT
  .ram : ALIGN(1024) {
    __boot_ram_start = .;
    KEEP(*(.boot_ram_vectors));
    *(.text .text.*);
    *(.rodata .rodata.*);
    *(.data .data.*);
    . = ALIGN(4);
    __boot_ram_end = .;
  } > RAM AT> BOOT
  __boot_ram_load = LOADADDR(.ram);
  .bss (NOLOAD) : ALIGN(4) {
    __boot_bss_start = .;
    *(.bss .bss.*); *(COMMON);
    . = ALIGN(4);
    __boot_bss_end = .;
  } > RAM
  /* Boot never initializes or uses the application's BSP/IPC metadata. Do not
   * emit orphan file-backed RAM sections into this flash-only installation. */
  /DISCARD/ : { *(.ARM.exidx* .ARM.extab* .shared_hal .ipc_mailbox .defmt*); }
}
ASSERT(LOADADDR(.ram) + SIZEOF(.ram) <= 0x08020000, "bootloader exceeds 128 KiB");
ASSERT(__boot_bss_end < 0x24070000, "bootloader leaves less than 64 KiB stack");
