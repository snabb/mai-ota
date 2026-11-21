/* The Pico W's 2 MiB of flash, behind the bootloader in `bootloader/`:
 *
 *   0x10000000  boot2 + bootloader  32 KiB
 *   0x10008000  bootloader state    4 KiB
 *   0x10009000  application         1004 KiB, this image
 *   0x10104000  DFU                 1008 KiB, where an upload goes; one page more
 *                                   than the application, for the swap
 *
 * `bootloader/memory.x` states the same layout; change both together. This image
 * has no boot2 (embassy-rp's `boot2-none`): the bootloader's has set up the flash.
 */
MEMORY
{
  BOOTLOADER       : ORIGIN = 0x10000000, LENGTH = 32K
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  FLASH            : ORIGIN = 0x10009000, LENGTH = 1004K
  DFU              : ORIGIN = 0x10104000, LENGTH = 1008K
  RAM              : ORIGIN = 0x20000000, LENGTH = 264K
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

/* The image ID an upload is checked for, right after the vector table. */
SECTIONS
{
  .mai_ota_image_id ALIGN(ADDR(.vector_table) + SIZEOF(.vector_table), 32) :
  {
    KEEP(*(.mai_ota_image_id));
    __mai_ota_image_id_end = .;
  } > FLASH
}
INSERT AFTER .vector_table;
/* Eight: rust-lld wants `.text` 8-byte aligned. */
_stext = ALIGN(__mai_ota_image_id_end, 8);
