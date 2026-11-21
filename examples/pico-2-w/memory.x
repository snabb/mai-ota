/* SPDX-License-Identifier: MIT OR Apache-2.0
 * Copyright (C) 2026 mai-ota contributors
 * Copyright Embassy project contributors
 *
 * The Pico 2 W's 4 MiB of flash, behind the bootloader in `bootloader/`:
 *
 *   0x10000000  bootloader        32 KiB
 *   0x10008000  bootloader state  4 KiB
 *   0x10009000  application       2028 KiB, this image
 *   0x10204000  DFU               2032 KiB, where an upload goes; one page more
 *                                 than the application, for the swap
 *
 * `bootloader/memory.x` states the same layout; change both together. The boot ROM
 * reads the bootloader's image definition, not this one, which is kept because
 * embassy-rp emits it. The boot-block sections are adapted from Embassy's
 * `examples/rp235x/memory.x` (MIT OR Apache-2.0).
 */
MEMORY
{
  BOOTLOADER       : ORIGIN = 0x10000000, LENGTH = 32K
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  FLASH            : ORIGIN = 0x10009000, LENGTH = 2028K
  DFU              : ORIGIN = 0x10204000, LENGTH = 2032K
  RAM              : ORIGIN = 0x20000000, LENGTH = 512K
}

/* Offsets into the flash, which is how its driver addresses it. */
__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOTLOADER);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOTLOADER);
__bootloader_active_start = ORIGIN(FLASH) - ORIGIN(BOOTLOADER);
__bootloader_active_end = ORIGIN(FLASH) + LENGTH(FLASH) - ORIGIN(BOOTLOADER);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOTLOADER);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOTLOADER);

SECTIONS
{
  /* Erased state, written by every wired flash, so that a swap pending from an
   * upload cannot replace the image just flashed. Not part of an upload: the CLI
   * leaves it out of the image. */
  .bootloader_state :
  {
    FILL(0xFFFFFFFF);
    LONG(0xFFFFFFFF);
    . += LENGTH(BOOTLOADER_STATE) - 4;
  } > BOOTLOADER_STATE
}

SECTIONS
{
  .start_block : ALIGN(4)
  {
    __start_block_addr = .;
    KEEP(*(.start_block));
    KEEP(*(.boot_info));
    /* The image ID an upload is checked for. */
    . = ALIGN(32);
    KEEP(*(.mai_ota_image_id));
  } > FLASH
} INSERT AFTER .vector_table;

/* Code after the image definition rather than over it; eight-byte aligned, as
 * rust-lld wants `.text`. */
_stext = ALIGN(ADDR(.start_block) + SIZEOF(.start_block), 8);

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
