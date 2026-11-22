/* The XIAO nRF54L15's RRAM, behind the bootloader in `bootloader/`:
 *
 *   0x00000000  bootloader        24 KiB
 *   0x00006000  bootloader state  16 KiB
 *   0x0000A000  application       740 KiB, this image
 *   0x000C3000  DFU               744 KiB, where an upload goes; one page more
 *                                 than the application, for the swap
 *   0x0017D000  end of RRAM
 *
 * `bootloader/memory.x` states the same layout; change both together.
 *
 * RAM stops 512 bytes short of 256 KiB: the top holds the VPR's saved context.
 */
MEMORY
{
  BOOTLOADER       : ORIGIN = 0x00000000, LENGTH = 24K
  BOOTLOADER_STATE : ORIGIN = 0x00006000, LENGTH = 16K
  FLASH            : ORIGIN = 0x0000A000, LENGTH = 740K
  DFU              : ORIGIN = 0x000C3000, LENGTH = 744K
  RAM              : ORIGIN = 0x20000000, LENGTH = 0x3FE00
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE);
__bootloader_active_start = ORIGIN(FLASH);
__bootloader_active_end = ORIGIN(FLASH) + LENGTH(FLASH);
__bootloader_dfu_start = ORIGIN(DFU);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU);

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
_stext = ALIGN(__mai_ota_image_id_end, 8);
