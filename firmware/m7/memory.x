MEMORY
{
  FLASH (rx)    : ORIGIN = 0x08000000, LENGTH = 1024K
  RAM (rwx)     : ORIGIN = 0x24000000, LENGTH = 512K
  EMBASSY (rwx) : ORIGIN = 0x38000000, LENGTH = 1K
  MAILBOX (rwx) : ORIGIN = 0x38000400, LENGTH = 1K
}

/* Match M4 exactly. Neither section belongs to runtime data/BSS initialization. */
SECTIONS
{
  .shared_hal (NOLOAD) : ALIGN(8)
  {
    KEEP(*(.shared_hal));
  } > EMBASSY

  .ipc_mailbox (NOLOAD) : ALIGN(32)
  {
    KEEP(*(.ipc_mailbox));
  } > MAILBOX
}

ASSERT(ADDR(.shared_hal) == ORIGIN(EMBASSY), "HAL metadata address mismatch");
ASSERT(SIZEOF(.shared_hal) <= LENGTH(EMBASSY), "HAL metadata exceeds reservation");
ASSERT(ADDR(.ipc_mailbox) == ORIGIN(MAILBOX), "IPC mailbox address mismatch");
ASSERT(SIZEOF(.ipc_mailbox) <= LENGTH(MAILBOX), "IPC mailbox exceeds reservation");
ASSERT(__sdata >= ORIGIN(RAM) && __edata <= ORIGIN(RAM) + LENGTH(RAM),
       "data initialization must stay in private RAM");
ASSERT(__sbss >= ORIGIN(RAM) && __ebss <= ORIGIN(RAM) + LENGTH(RAM),
       "BSS clearing must stay in private RAM");
ASSERT(__suninit >= ORIGIN(RAM) && __euninit <= ORIGIN(RAM) + LENGTH(RAM),
       "uninitialized storage must stay in private RAM");
ASSERT(__sheap >= ORIGIN(RAM) && __sheap <= ORIGIN(RAM) + LENGTH(RAM),
       "heap boundary must stay in private RAM");
ASSERT(_stack_end >= ORIGIN(RAM) && _stack_start <= ORIGIN(RAM) + LENGTH(RAM),
       "stack boundaries must stay in private RAM");
