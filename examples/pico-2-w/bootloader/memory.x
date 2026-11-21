/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (C) 2026 mai-ota contributors
 * Copyright Embassy project contributors
 *
 * The Pico 2 W's 4 MiB of flash, as the bootloader sees it:
 *
 *   0x10000000  bootloader        32 KiB, this crate
 *   0x10008000  bootloader state  4 KiB
 *   0x10009000  active image      2028 KiB, what runs
 *   0x10204000  DFU               2032 KiB, where an upload goes
 *
 * `../memory.x` states the same layout for the application; change both together.
 * The `__bootloader_*` symbols are offsets into the flash, which is how its driver
 * addresses it.
 *
 * The boot-block sections are adapted from Embassy's
 * `examples/boot/bootloader/rp235x/memory.x` (MIT OR Apache-2.0): the boot ROM
 * looks for the image definition in the first 4 KiB, so it goes after the vector
 * table.
 */
MEMORY
{
  FLASH            : ORIGIN = 0x10000000, LENGTH = 32K
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  ACTIVE           : ORIGIN = 0x10009000, LENGTH = 2028K
  DFU              : ORIGIN = 0x10204000, LENGTH = 2032K
  RAM              : ORIGIN = 0x20000000, LENGTH = 512K
}

SECTIONS
{
  .start_block : ALIGN(4)
  {
    __start_block_addr = .;
    KEEP(*(.start_block));
    KEEP(*(.boot_info));
  } > FLASH
} INSERT AFTER .vector_table;

_stext = ADDR(.start_block) + SIZEOF(.start_block);

SECTIONS
{
  .end_block : ALIGN(4)
  {
    __end_block_addr = .;
    KEEP(*(.end_block));
  } > FLASH
} INSERT AFTER .uninit;

PROVIDE(start_to_end = __end_block_addr - __start_block_addr);
PROVIDE(end_to_start = __start_block_addr - __end_block_addr);

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);

/* Erased state, written by every wired flash: whatever was there before, a pending
 * swap included, must not act on the image just flashed. */
SECTIONS
{
  .bootloader_state :
  {
    FILL(0xFFFFFFFF);
    LONG(0xFFFFFFFF);
    . += LENGTH(BOOTLOADER_STATE) - 4;
  } > BOOTLOADER_STATE
}
