/* The XIAO nRF54L15's RRAM, as the bootloader sees it:
 *
 *   0x00000000  bootloader        24 KiB, this crate
 *   0x00006000  bootloader state  16 KiB
 *   0x0000A000  active image      740 KiB, what runs
 *   0x000C3000  DFU               744 KiB, where an upload goes
 *
 * `../memory.x` states the same layout for the application; change both together.
 *
 * The state partition holds a magic and one 16-byte progress record per step of
 * a swap: 2 + 4 x 185 pages, 11872 bytes. embassy-boot asserts that it fits.
 */
MEMORY
{
  FLASH            : ORIGIN = 0x00000000, LENGTH = 24K
  BOOTLOADER_STATE : ORIGIN = 0x00006000, LENGTH = 16K
  ACTIVE           : ORIGIN = 0x0000A000, LENGTH = 740K
  DFU              : ORIGIN = 0x000C3000, LENGTH = 744K
  RAM              : ORIGIN = 0x20000000, LENGTH = 0x3FE00
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE);
__bootloader_active_start = ORIGIN(ACTIVE);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE);
__bootloader_dfu_start = ORIGIN(DFU);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU);

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
