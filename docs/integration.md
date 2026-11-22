# Integration guide

What a firmware needs to take updates with mai-ota. Each step points at the
example that does it; [`examples/`](../examples/README.md) has a complete
firmware per chip family.

## The parts

1. **A slot**: where an upload goes, and how the next boot runs it. `esp::Slot`
   behind ESP-IDF's bootloader, or `embassy_boot::Slot` behind
   `mai-ota-bootloader`. Anything else implements `mai_ota::Slot`.
2. **An `Ota`**: the slot, the board name images are checked against, the
   version `GET /fw` reports, and the `Boot` the slot read (trial, rolled back,
   and how to reboot).
3. **The watchdog**: `watchdog::run` with the chip's `Watchdog`, from the first
   moments of a trial boot, so that a hang or a halted panic rolls back.
4. **A transport**: `coap::Resource` on a mai-coap router, behind
   `coap::Proving`, and `coap::run`, which drives the upload and the trial
   supervisor beside the endpoint. A transport other than CoAP drives
   `mai_ota::Upload` and `mai_ota::supervise` itself, and calls `mai_ota::prove`
   for each authenticated request.

## Dependency versions

- **RP and nRF54:** mai-ota's chip modules follow Embassy's Git revision
  `8a940dab0d56a910277b816987e45315ca7979aa`, which mai-coap's xarxa adapters
  also use: the RP watchdog uses embassy-rp's `Watchdog<'d>`, and the nRF54L
  needs embassy-nrf from Git. Patch the Embassy crates to that revision, as
  `examples/pico-w/Cargo.toml` and `examples/xiao-nrf54lm20a/Cargo.toml` do.
  `mai-ota-bootloader`'s RP module uses released embassy-rp 0.10; its nRF54
  bootloader needs the same Git patch as the application
  (`examples/xiao-nrf54lm20a/bootloader/Cargo.toml`).
- **ESP32:** the ESP modules need esp-hal's word-oriented `esp_hal::flash::Flash`,
  which no esp-hal release has yet. Patch `esp-hal` and `esp-bootloader-esp-idf`
  to esp-hal's main branch. The examples patch every esp-hal crate to a fork of
  main (`examples/esp32c6/Cargo.toml`) that also carries esp-radio changes they
  need for their network stack.

## Behind embassy-boot (nRF54L15, nRF54LM20A, RP2040, RP2350)

### Flash layout

One layout, written twice: in the bootloader's `memory.x` and in the
application's. Four regions:

| Region | Size |
| --- | --- |
| bootloader | what it needs: 24 KiB on the nRF54, 32 KiB on the RP parts, boot2 included |
| state | at least `(2 + 4 × active pages) × the flash's write size`, rounded up to a page (16 KiB on the nRF54, 4 KiB on the RP parts); embassy-boot asserts that it fits |
| active | where the application is linked and runs |
| DFU | where uploads go: **one page larger** than active, for the swap |

Both linker scripts define `__bootloader_{state,active,dfu}_{start,end}` as
offsets into the flash as its driver addresses it: from 0 on the nRF54, from the
start of XIP flash on the RP parts. `embassy_boot::ota` takes the address where
offset 0 is mapped (`0` on the nRF54, `embassy_rp::flash::FLASH_BASE` on the RP).

On the nRF54L, `RAM` stops short of the reserved top: `LENGTH = 0x3FE00` on the
nRF54L15, `0x7FD40` on the nRF54LM20A. A stack placed there faults at boot.

Both linker scripts also fill the state region with erased bytes, in a
`.bootloader_state` section with a memory region of its own. Every wired flash
then cancels a pending swap, so a stale swap cannot act on an image flashed by
hand. The CLI leaves that section out of an upload.

### The image ID

The running image checks every upload for an `image::ImageId`: an 8-byte magic
and the board name, within the image's first 2048 bytes, 32-byte aligned. Put one
in with

```rust
const MAGIC: [u8; 8] = *b"MYPRODID";
mai_ota::image_id!(MAGIC);
```

and give it a section right after the vector table in the application's
`memory.x`:

```text
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
```

On the RP2350 it goes inside `.start_block` instead (see
`examples/pico-2-w/memory.x`). `image_id!` records the package name, which must
be the `board` in `Ota`, at most 24 bytes.

**Choose the magic once.** A device refuses any image whose ID it cannot find,
so changing the magic means a wired flash for every device in the field.

### The bootloader

A binary per board, a workspace of its own so it unifies no features with the
application:

```rust
#[cortex_m_rt::entry]
fn main() -> ! {
    mai_ota_bootloader::rp::run::<{ 2 * 1024 * 1024 }>() // or nrf54l::run()
}
```

with the `memory.x` above and `cortex-m`'s `critical-section-single-core`. The
bootloader crate supplies the panic handler and fault handlers. The RP2040 one
also links embassy-rp's boot2 (`-Tlink-rp.x`), and the application is then built
with embassy-rp's `boot2-none` feature. See `examples/*/bootloader`.

### The application

```rust
static FLASH: StaticCell<Mutex<NoopRawMutex, Flash>> = StaticCell::new();
let flash = FLASH.init(Mutex::new(flash));      // shareable with other users
let ota = mai_ota::embassy_boot::ota(
    env!("CARGO_PKG_NAME"),
    env!("MY_VERSION"),                        // differs between builds
    flash, base, MAGIC, reboot, mai_ota::no_events,
).await;
```

The flash is any async `NorFlash`. On the nRF54 with MPSL running, that is
`nrf_mpsl::Flash`, because MPSL arbitrates the RRAM. `reboot` must reset the
whole chip: `watchdog::rp::reset_chip` on the RP parts (`SCB::sys_reset` resets
only the RP2040's cores), `watchdog::nrf::reset_chip` on the nRF54L (after a
plain `SCB::sys_reset` MPSL's initialisation can assert; see
[troubleshooting](troubleshooting.md#nrf54l)).

### The watchdog

The bootloader starts the watchdog for a trial boot, before the swap, with the
settings `mai_ota::watchdog` uses. The application must take it over:

```rust
let watchdog = mai_ota::watchdog::rp::Watchdog::new(p.WATCHDOG); // feeds it if running
spawner.spawn(watchdog_task(watchdog, after_watchdog_reset)?);
// in the task:
mai_ota::watchdog::run(watchdog, after_watchdog_reset, events).await
```

`after_watchdog_reset` comes from the chip's reset reason
(`WATCHDOG.reason().read().timer()` on the RP parts, see `examples/rp/main.rs`).
A boot after a watchdog reset leaves the watchdog unarmed for
`DEFER_AFTER_RECOVERY` (10 minutes), so a fault that recurs at boot does not
loop. On the nRF54L take it from `watchdog::nrf::after_watchdog_reset()`,
which does not count `reset_chip`'s own watchdog reset. On the nRF54,
`Watchdog::new` adopts the running watchdog only if its configuration matches
`watchdog::nrf::config()`, which the bootloader's does.

## Behind ESP-IDF's bootloader (ESP32, ESP32-C6, ESP32-S3)

### Bootloader and partition table

ESP-IDF's second-stage bootloader with `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`;
espflash's bundled one has no rollback. Build it once per chip with
`tools/esp-bootloader/build.sh` (needs Docker) and name it in `espflash.toml`, so
that every flash writes it:

```toml
[idf]
partition_table = "partitions.csv"
bootloader = "bootloader.bin"
```

The partition table needs `otadata`, `ota_0` and `ota_1`, and no `factory`:

```text
nvs,        data, nvs,     0x9000,   0x4000
otadata,    data, ota,     0xd000,   0x2000
phy_init,   data, phy,     0xf000,   0x1000
ota_0,      app,  ota_0,   0x10000,  0x180000
ota_1,      app,  ota_1,   0x190000, 0x180000
```

Flash with `espflash flash --erase-parts otadata` once a device has been updated
over the air: espflash writes `ota_0`, and the bootloader would otherwise keep
booting whatever `otadata` last selected. The examples' `.cargo/config.toml`
runner does this.

### The image check

An ESP image names its board in its app descriptor: `esp_app_desc!()` fills the
project name with the package name, which must be the `board` in `Ota`, at most
32 bytes. The chip is checked from the image header.

A firmware with its own rule passes a `Policy` to `esp::ota_with` instead. The
chip and descriptor checks stand; the policy gets the descriptor's project name
and version, the board and the running version, and says yes or why not: say, a
build of the same board for the other radio, taken only when that network's
settings are saved, and never the build already running.

### The application

```rust
// esp_hal::init stopped the bootloader's watchdog; reading the flash may fail or hang.
let mut watchdog = mai_ota::watchdog::esp::Watchdog::new(peripherals.RTC_TIMER);
watchdog.start();
static FLASH: StaticCell<mai_ota::esp::Flash<NoopRawMutex>> = StaticCell::new();
let flash = FLASH.init(Mutex::new(FlashStorage::new(peripherals.FLASH, Default::default())?));
let ota = mai_ota::esp::ota(env!("CARGO_PKG_NAME"), version, flash, events)
    .await
    .expect("ota_0, ota_1 and otadata");
if !ota.boot.trial {
    watchdog.stop(); // an ordinary boot's is watchdog::run's to arm
}
```

Start the watchdog before anything that touches the flash: a trial that panics or
hangs there must still reset, and the panic handler does not reset by itself.

**ESP32 before v3.0: lower esp-hal's minimum chip revision.** esp-hal requires
v3.0 on the ESP32 by default, and many boards are v1.x. Set
`ESP_HAL_CONFIG_MIN_CHIP_REVISION = "100"` in `.cargo/config.toml`'s `[env]`
(`examples/esp32/.cargo/config.toml`) if the firmware uses neither PSRAM nor
the second core, which the v1.x errata fixed in v3.0 concern.

**ESP32-C6 with Wi-Fi: reset the modem before starting Wi-Fi.** An uploaded
image boots by a software reset, which leaves the modem power domain as the
old image left it, and esp-radio resets it only on chips without a PMU. The new
image's Wi-Fi then never associates, and every update rolls back. Pulse the
Wi-Fi, Bluetooth and 802.15.4 MAC, baseband and front-end bits of
`MODEM_SYSCON.modem_rst_conf` before `WifiController::new`
(`reset_modem` in `examples/esp/main.rs`).

The flash is the firmware's: one `FlashStorage` behind a mutex, which mai-ota
locks for each operation and nothing longer, so settings and other partitions
use the same handle. Every `otadata` rewrite erases the entry's sector first and
leaves the other entry alone, so a write a power loss tears still leaves the
bootloader an image to boot.

## The CoAP side

```rust
static PROVING: Proving<ProtectedOnly> = Proving(&ProtectedOnly);

let queue = mai_ota::coap::Queue::<DATAGRAM>::new();
let mut fw = mai_ota::coap::Resource::new(&queue, &ota);
let mut router = Router::<DATAGRAM, ROUTES, 0, 0>::new(&PROVING);
router.route("/fw", &[Code::GET, Code::PUT], &mut fw)?;
let server = endpoint.run(transport, &clock, &mut router, &PROVING);
mai_ota::coap::run(ota, &queue, shared.handle(), &clock, server).await
```

* **Start this before the network is up.** `coap::run` runs the trial
  supervisor, which reboots at the trial's deadline. Bind the socket at once and
  let the network come up beside it.
* **Keep `server` from returning.** `coap::run` returns with it, dropping the
  supervisor; restart the endpoint inside the future instead, as
  `examples/common` does. The deadline counts from the boot, not from the
  supervisor, and `watchdog::run` stops feeding at it however the supervisor
  ended, so a trial rolls back on time either way, by a watchdog reset at worst.
* The endpoint needs room for deferred replies: the upload worker answers each
  block through the endpoint's command queue. `examples/common` sizes one for
  two uploads (`COMMANDS`, `OUTBOUND`, `DEFERRED` and `UPLOADS` of 2).
* The policy must allow nothing unprotected on `/fw`. `Proving` wraps it and
  confirms a trial on the first protected request it allows, on any path.
* Use OSCORE with EDHOC, so that only a client whose credential the firmware
  trusts can install images. The credential files are the ones
  `mai-coap --generate-identity` writes; `examples/common` loads them.
* EDHOC needs a cryptographically secure random generator.

### Owning the parts yourself

`coap::run` puts three things together, and a firmware that needs them apart can
run them itself:

* `mai_ota::supervise(ota.boot, ota.slot.confirm(), events)` from boot, whether
  or not the network ever comes up, for the trial and the reboot after an upload.
* `coap::worker(&queue, handle, &clock, uploader)`, which feeds the `PUT`s the
  resource queues into any `Uploader`: the `Upload` itself, or the firmware's own
  owner of it that also serves another transport, such as an HTTPS pull, and
  gives the slot to one of them at a time. An endpoint that restarts takes a new
  `handle`; the queue and the worker stay.
* `coap::Resource::with(&queue, board, version)` for the route, which needs no
  `Ota`.

Any other transport writes into the same `Uploader`: `begin` with the size when
it is known, so that a short or long image is refused (`Refusal::Length`),
`write` in order, then `commit`. Once one image is committed, every later `begin`
is refused (`Refusal::Unavailable`) until the reboot.

### The trial state

`ota.boot.trial_timeout` sets how long a trial waits for its proof:
`TRIAL_TIMEOUT` (600 s) unless the firmware changes it before handing `ota` on.

The slot backends (`embassy_boot::ota`, `esp::ota`) record a trial as they find
it, before any transport runs, so `on_trial()` and the refusal of uploads hold
from the start, and the deadline counts from then. A firmware with a backend of
its own calls `mai_ota::start_trial()` as early. When `embassy_boot::ota` cannot
read the boot state, it treats the boot as a trial: the DFU partition may hold
the image to fall back to, so uploads are refused, a proof confirms it, and
without one the deadline reboots it for the bootloader to decide. Once confirmed,
a boot is not a trial again, whoever calls `start_trial` or `supervise`.

`watchdog::run` stops feeding when the boot is `overdue()`: a trial past its
deadline, or an activated upload whose reboot has not come 10 s after
`REBOOT_DELAY`. A supervisor that started after the activation, because the one
before it was dropped, still reboots on time.

## Events

`Ota::events` and `watchdog::run`'s `events` receive `Event::Trial`,
`Confirmed`, `RolledBack`, `Refused` and `WatchdogRecovered`. They are a plain
`fn`, so a firmware can record them in a static log. `no_events` drops them.
