# mai-ota

A/B over-the-air firmware updates for `no_std` embedded Rust with Embassy. A new
image boots **on trial** and is rolled back automatically unless it proves it
can still take the next update. Images are uploaded over CoAP, authenticated
with EDHOC and OSCORE ([mai-coap](https://github.com/snabb/mai-coap)).

- Two slots: the running image stays intact until the new one is confirmed.
- Automatic rollback on a hang, a panic, or a new image that never gets back on
  the network, enforced by the hardware watchdog.
- Each upload is read back as it is written and checked for the right board and
  chip before it is activated.
- A host CLI that uploads an ELF and reports whether the device came back
  running it.

## Supported hardware

| Chip | Bootloader | Network in the example | Measured upload rate |
| --- | --- | --- | --- |
| ESP32 | ESP-IDF's, with app rollback | Wi-Fi | ~14–22 KiB/s (1 MiB in ~45–75 s) |
| ESP32-C5 | ESP-IDF's, with app rollback | Wi-Fi (2.4 or 5 GHz) | ~18 KiB/s (1 MiB in ~1 min) |
| ESP32-C6 | ESP-IDF's, with app rollback | Wi-Fi | ~29 KiB/s (1 MiB in ~35 s) |
| ESP32-H2 | ESP-IDF's, with app rollback | Thread | ~3.5–3.8 KiB/s (1 MiB in ~5 min) |
| ESP32-S3 | ESP-IDF's, with app rollback | Wi-Fi | ~23 KiB/s (1 MiB in ~45 s) |
| nRF54L15 | `mai-ota-bootloader` (embassy-boot) | Thread | ~2.6 KiB/s (1 MiB in ~7 min) |
| nRF54LM20A | `mai-ota-bootloader` (embassy-boot) | Thread | ~1.7 KiB/s (1 MiB in ~10 min) |
| RP2040 (Pico W) | `mai-ota-bootloader` (embassy-boot) | Wi-Fi | ~17 KiB/s (1 MiB in ~1 min) |
| RP2350 (Pico 2 W) | `mai-ota-bootloader` (embassy-boot) | Wi-Fi | ~18 KiB/s (1 MiB in ~1 min) |

Measured with the examples and the CLI's default 1024-byte blocks, not counting
the reboot and confirmation afterwards. The 1 MiB times are extrapolated from the
rate.

The nRF52840 and STM32 have watchdog modules only: there is no bootloader or
example for them.

## How it works

1. An upload is written into the slot the running image did not boot from.
2. After the last block the device reboots, and the bootloader swaps the new
   image in (embassy-boot) or selects its slot (ESP-IDF). It runs on trial.
3. The first authenticated request **confirms** it: that proves the network, the
   CoAP server and the keys work. The CLI sends that request after the reboot.
4. Without one within 10 minutes, or on a hang or panic, the device resets and
   the bootloader goes back to the previous image.

Uploads are refused while an image is on trial, because the other slot holds the
fallback.

## Quick start: try an example

This walks through the Raspberry Pi Pico W. The [examples guide](examples/README.md)
covers every board, including the ESP32 and nRF54 variants.

You need Rust 1.97 or later, a USB data cable, and
[picotool](https://github.com/raspberrypi/pico-sdk-tools/releases) 2.3.1 or later.
Install the two host tools and the target:

```sh
rustup target add thumbv6m-none-eabi
cargo install --locked --git https://github.com/snabb/mai-coap mai-coap-cli
cargo install --locked --git https://github.com/snabb/mai-ota mai-ota-cli
```

Make an EDHOC identity for the device and one for the client
(`examples/.credentials/` is ignored by Git):

```sh
mkdir -p examples/.credentials && cd examples/.credentials
mai-coap --generate-identity device   # device.ccs, device.cosekey
mai-coap --generate-identity client   # client.ccs, client.cosekey
cd ../pico-w
```

Set the build environment:

```sh
export MAI_OTA_DEVICE=$PWD/../.credentials/device
export MAI_OTA_CLIENT=$PWD/../.credentials/client.ccs
export WIFI_SSID=… WIFI_PASSWORD=…
```

Follow the [Pico USB flashing steps](examples/README.md#pico-w-and-pico-2-w)
from the `examples/pico-w` directory to install the bootloader and application.
Find the board's address in your router's DHCP leases.

Change something, rebuild, and update over the air:

```sh
cargo build --release
K=../.credentials
mai-ota --identity $K/client.ccs --key $K/client.cosekey --peer $K/device.ccs \
  upload <address> ../target/thumbv6m-none-eabi/release/pico-w
```

The CLI exits 0 once the new version answered and is confirmed. To see a
rollback instead, add `--no-confirm` and wait 10 minutes.

## Using it in your firmware

mai-ota is not on crates.io; depend on it from Git:

```toml
[dependencies]
mai-ota = { git = "https://github.com/snabb/mai-ota.git", rev = "…", features = ["coap", "embassy-boot", "rp2040", "defmt"] }
```

| Feature | Enables |
| --- | --- |
| `coap` | `coap::{Resource, Proving, run}`: uploads over mai-coap |
| `embassy-boot` | `embassy_boot::{ota, Slot}`, for nRF54L15, nRF54LM20A and RP2040/RP2350 |
| `esp32`, `esp32c5`, `esp32c6`, `esp32h2`, `esp32s3` | `esp::{ota, Slot}` and `watchdog::esp` |
| `nrf52840`, `nrf54l15`, `nrf54lm20` | `watchdog::nrf`; on the nRF54L, with embassy-nrf's secure chip feature (`nrf54l15-app-s`, `nrf54lm20-app-s`) |
| `rp2040`, `rp235xa` | `watchdog::rp` |
| `stm32` | `watchdog::stm32` |
| `log`, `defmt` | logging through either |
| `watchdog-test-panic` | panics a minute after the watchdog starts, to test a rollback |

The [integration guide](docs/integration.md) lists what a firmware needs: the
flash layout or partition table, the bootloader, the image ID, the watchdog and
the CoAP route. Each step points at the example that does it.

## Documentation

| Document | For |
| --- | --- |
| [examples/README.md](examples/README.md) | building, flashing and updating the example firmware |
| [docs/integration.md](docs/integration.md) | adding mai-ota to your own firmware |
| [docs/cli.md](docs/cli.md) | the `mai-ota` command: credentials, options, exit status |
| [docs/protocol.md](docs/protocol.md) | what the device answers on `/fw`, for other clients |
| [docs/troubleshooting.md](docs/troubleshooting.md) | refusals, rollbacks, stalled uploads, per-chip issues |

## Repository layout

| Path | Contents |
| --- | --- |
| `src/` | the `mai-ota` library |
| `bootloader/` | `mai-ota-bootloader`: embassy-boot bootloaders that start the watchdog for a trial boot |
| `cli/` | `mai-ota-cli`, the host tool (binary `mai-ota`) |
| `examples/` | one firmware per chip family, with its bootloader |
| `tools/esp-bootloader/` | builds ESP-IDF's bootloader with app rollback (needs Docker) |
| `tools/nrf54-*.sh` | hardware tests that start a swap over SWD |

## Limitations

- **Anyone holding an authorised client credential can install firmware.** There
  is no image signature; the board and chip checks guard against a wrong image,
  not a malicious one.
- Every build must report a different version, or the CLI cannot tell a
  successful update from a rollback. The examples put the build time in it.
- The Cortex-M image magic cannot change once devices are in the field: a device
  refuses images without the magic it was built with.
- The CLI runs on Unix-like hosts only.

## Licence

MIT OR Apache-2.0, at your option ([LICENSE-MIT](LICENSE-MIT),
[LICENSE-APACHE](LICENSE-APACHE)). Third-party code and the CYW43439 firmware
blobs are listed in [NOTICE.md](NOTICE.md).
