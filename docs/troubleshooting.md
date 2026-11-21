# Troubleshooting

## Refusals

The CLI prints the reply code; the device's log says why.

| Reply | Meaning |
| --- | --- |
| 4.00 | The image is wrong for this device (see the log messages below), or not the size its first block declared. |
| 4.01 | Not under OSCORE, or the credentials do not match. |
| 4.08 | The device lost the upload (no block for 120 s, or another upload began). The CLI starts over. |
| 4.13 | The image does not fit the slot. |
| 5.00 | A flash error, or a block that read back wrong. |
| 5.03 | The running image is on trial: send any authenticated request (`mai-ota info`), wait a moment, retry. Or an upload is already activated and the device is about to reboot. |

| Device log (`ota: image refused: …`) | Cause |
| --- | --- |
| `not an image for this slot` | the vector table points outside the active partition or RAM: built for another flash layout, or without the bootloader |
| `no image id` | no `image_id!`, a different magic, or a linker script without the `.mai_ota_image_id` section |
| `built for another board` | the image's package name (Cortex-M) or app descriptor project name (ESP) is not this board's |
| `built for another chip` | an ESP image for another chip; check `--chip` |
| `not an ESP application image` | an ELF or raw binary sent by a client other than the CLI, rather than `espflash save-image` output |
| `its reset vector lies past the end of the upload`, `the image runs past the end of the upload`, `too short` | the upload was cut short |

The Cortex-M magic cannot change for devices in the field; see
[the image ID](integration.md#the-image-id).

## A device that came back with the old version

* It rolled back. The device's log at boot says so (`ota: the uploaded image
  was rejected; running the previous one`), and its reset cause says why: a
  watchdog reset, or a clean reboot at the trial deadline. `Event::RolledBack`
  reaches the firmware's event hook. Common causes: the new image panics during
  bring-up, never gets on the network within 10 minutes, or nothing sent it an
  authenticated request in time.
* Or both builds report the same `version`. Put a build time in it.

## Wired flashes

* **A wired flash cancels a pending update** on embassy-boot boards. The
  application's ELF carries the bootloader's state partition, erased, so
  flashing one after an upload but before the reboot leaves the new image
  unused. That is deliberate: it also stops a stale swap from acting on an image
  just flashed by hand.
* **Every image needs the bootloader.** Applications link at the active
  partition, not at the start of flash, so a board without the bootloader runs
  nothing.
* **ESP32: erase `otadata` on every wired flash** once a board has been updated
  over the air (`espflash flash --erase-parts otadata …`). espflash writes
  `ota_0`; without the erase, the bootloader keeps booting whatever `otadata`
  last selected, which may be the image in `ota_1`.
* **ESP32: every flash writes the rollback bootloader** only if the firmware's
  `espflash.toml` names it (`[idf] bootloader = "bootloader.bin"`). Without it,
  espflash writes its bundled bootloader, which has no rollback: a new image is
  then kept even if it never confirms.

## Uploads that stall

* Use a client that sends each block as its own request and retries it, such as
  the [CLI](cli.md). aiocoap's own blockwise `PUT` abandons the whole body when
  one block's retransmissions run out (about 93 s, which a Thread child that
  loses its parent can exceed). It also runs EDHOC again for every request
  addressed by URI, so each block starts a new body on the device.
* The device abandons an upload after 120 s without a block and then answers
  4.08; the CLI starts over.

## Watchdog

* A boot that a watchdog reset caused leaves the watchdog unarmed for
  `DEFER_AFTER_RECOVERY` (10 minutes), so a fault that recurs straight after
  boot halts after one extra reboot instead of looping. A restored image after
  a rollback runs this way too.
* The feeder proves the executor schedules tasks, not that every task is healthy.
* A debug probe that halts the core pauses the watchdog (nRF, RP). On an ESP32,
  attaching a console over USB can reset the chip and hide the watchdog's cause.

## ESP32-C6

* **Every Wi-Fi update rolls back, and the new image never joins the network.**
  The software reset that boots it leaves the radio as the old image left it.
  Reset the modem before starting Wi-Fi; see the
  [integration guide](integration.md#the-application-1). The same leftover
  state makes a Wi-Fi image deaf after a Thread image ran, which only a power
  cycle or that reset clears.
* **No SLAAC address with embassy-net 0.9 (smoltcp).** On the Unit C6L it formed
  no IPv6 address after any boot, cold or not, though the router answered its
  solicitation at once; DHCPv4 worked. The examples use embassy-net from Git
  (xarxa), which forms the address within seconds.

## nRF54LM20A

* **An update rolls back about half the time, and the image on trial logs
  `panicked at 'MultiprotocolServiceLayer: 106:179'`** right after arming the
  watchdog. The reboot into it was a soft reset (`SCB::sys_reset`), which leaves
  the oscillators, the regulators and the GRTC as the old image left them, and
  MPSL's initialisation then asserts when the radio was busy at the reset. A
  watchdog reset resets them. Reboot with `watchdog::nrf::reset_chip`, which
  resets through WDT30 and marks itself, and decide the watchdog's recovery with
  `watchdog::nrf::after_watchdog_reset`, which tells that mark from a timeout.
  `probe-rs reset` is a soft reset too: a boot after it can hit the same assert,
  which a panic halting the core turns into a watchdog reset a few seconds later.
* **A swap takes a minute or two, and the debug port refuses memory access
  meanwhile.** embassy-boot swaps every page of the active partition, whatever
  the image's size, and the RRAM writes hold the bus. A probe read then fails
  with "lacked the permission to do so: erase_all"; it is not a locked part.
  `tools/nrf54-swap-test.sh` and `tools/nrf54-early-confirm-test.sh` start a swap
  over SWD, without an upload, to test this quickly. Both write the image given
  into the DFU partition first, since after a swap that partition holds the
  previous image one page further on, and both refuse while a swap is pending or
  the image is on trial.
* **The device's address changes at every boot, and the client cannot find the
  updated image.** With OpenThread's settings in RAM, its SLAAC secret is made
  afresh at each boot. The example stores one derived from the device's identity
  (settings key 7), as it does the SRP key (key 11).

## RP2040 and RP2350

* **The bootloaders differ in `embassy_rp::init`, measured once on each chip.**
  The RP2040 bootloader leaves the chip as the boot ROM left it: with `init`,
  the application's CYW43439 kept timing out Wi-Fi association. The RP2350 one
  runs `init`: without it, the first swap left the chip answering neither USB
  nor SWD.
* **A Pico 2 W wedged mid-swap** answers neither USB nor SWD. Hold BOOTSEL while
  plugging in its USB; SWD then works, and flashing the bootloader and an
  application by wire recovers it. In BOOTSEL the flash is not mapped, so
  `probe-rs verify` fails and `probe-rs read` returns zeros whatever is there.
* **Reset the whole chip, not the cores.** `watchdog::rp::reset_chip` is the
  reboot to give `Boot::reboot`: `SCB::sys_reset` on the RP2040 resets only the
  cores and leaves DMA and PIO running. `probe-rs reset` is core-only too, and a
  Pico W's CYW43439 can fail to associate for minutes afterwards; a full reset
  over SWD writes the watchdog's trigger: `probe-rs write --chip RP2040 b32
  0x40058000 0x80000000`.

## Known issues

* A Pico 2 W was once found halted about 5 hours after an upload. The cause was
  not captured. A full reset brought it back.
