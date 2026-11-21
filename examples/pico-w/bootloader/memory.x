/* The Pico W's 2 MiB of flash, as the bootloader sees it:
 *
 *   0x10000000  boot2             256 bytes, embassy-rp's
 *   0x10000100  bootloader        32 KiB with boot2, this crate
 *   0x10008000  bootloader state  4 KiB
 *   0x10009000  active image      1004 KiB, what runs
 *   0x10104000  DFU               1008 KiB, where an upload goes
 *
 * `../memory.x` states the same layout for the application; change both together.
 * The `__bootloader_*` symbols are offsets into the flash, which is how its driver
 * addresses it.
 */
MEMORY
{
  BOOT2            : ORIGIN = 0x10000000, LENGTH = 0x100
  FLASH            : ORIGIN = 0x10000100, LENGTH = 32K - 0x100
  BOOTLOADER_STATE : ORIGIN = 0x10008000, LENGTH = 4K
  ACTIVE           : ORIGIN = 0x10009000, LENGTH = 1004K
  DFU              : ORIGIN = 0x10104000, LENGTH = 1008K
  RAM              : ORIGIN = 0x20000000, LENGTH = 264K
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_state_end = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(BOOT2);
__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(BOOT2);
__bootloader_active_end = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(BOOT2);
__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(BOOT2);
__bootloader_dfu_end = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(BOOT2);

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
