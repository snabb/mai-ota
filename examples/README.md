# Examples

One firmware per supported chip family. Each joins a network, serves `/fw` over
CoAP under EDHOC and OSCORE, and does nothing else. With the
[integration guide](../docs/integration.md), they show what a firmware needs.

| Example | Board | Network | Bootloader | Binary |
| --- | --- | --- | --- | --- |
| `esp32` | any ESP32 with 4 MiB of flash or more, chip revision v1.0 or later (M5Stack Atom Lite) | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `xtensa-esp32-none-elf/release/esp32-example` |
| `esp32c5` | any ESP32-C5 with 4 MiB of flash or more (Seeed XIAO ESP32-C5) | Wi-Fi, 2.4 or 5 GHz | ESP-IDF's, from `tools/esp-bootloader` | `riscv32imac-unknown-none-elf/release/esp32c5-example` |
| `esp32c6` | any ESP32-C6 with 4 MiB of flash or more | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `riscv32imac-unknown-none-elf/release/esp32c6-example` |
| `esp32h2` | any ESP32-H2 with 4 MiB of flash or more (M5Stack NanoH2) | Thread | ESP-IDF's, from `tools/esp-bootloader` | `riscv32imac-unknown-none-elf/release/esp32h2-example` |
| `esp32s3` | any ESP32-S3 with 4 MiB of flash or more | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `xtensa-esp32s3-none-elf/release/esp32s3-example` |
| `pico-w` | Raspberry Pi Pico W | Wi-Fi | `pico-w/bootloader` | `thumbv6m-none-eabi/release/pico-w` |
| `pico-2-w` | Raspberry Pi Pico 2 W | Wi-Fi | `pico-2-w/bootloader` | `thumbv8m.main-none-eabihf/release/pico-2-w` |
| `xiao-nrf54l15` | Seeed XIAO nRF54L15 | Thread | `xiao-nrf54l15/bootloader` | `thumbv8m.main-none-eabihf/release/xiao-nrf54l15` |
| `xiao-nrf54lm20a` | Seeed XIAO nRF54LM20A | Thread | `xiao-nrf54lm20a/bootloader` | `thumbv8m.main-none-eabihf/release/xiao-nrf54lm20a` |

Every example builds into `examples/target/`. `common/` is the CoAP server they
share, and its `thread` feature the Thread code the nRF54 and ESP32-H2 examples
share. `esp/`, `rp/` and `nrf54l/` hold the source shared by the four ESP Wi-Fi,
the two Pico and the two XIAO nRF54 examples; `esp-thread/` is the ESP32-H2's.

## Prerequisites

All examples need Rust 1.97 or later (the ESP ones 1.100, esp-hal main's MSRV), the
[mai-coap](https://github.com/snabb/mai-coap) CLI for the credentials, and the
`mai-ota` CLI for updates:

```sh
cargo install --locked --git https://github.com/snabb/mai-coap mai-coap-cli
cargo install --locked --git https://github.com/snabb/mai-ota mai-ota-cli
```

| Example | Also needs |
| --- | --- |
| `pico-w` | `rustup target add thumbv6m-none-eabi`, a USB data cable, and [picotool](https://github.com/raspberrypi/pico-sdk-tools/releases) 2.3.1 or later |
| `pico-2-w` | `rustup target add thumbv8m.main-none-eabihf`, a USB data cable, and picotool 2.3.1 or later |
| `xiao-nrf54l15`, `xiao-nrf54lm20a` | `rustup target add thumbv8m.main-none-eabihf`, [probe-rs](https://probe.rs); CMake, clang and libclang of the same LLVM version (OpenThread is built from source) |
| `esp32c5`, `esp32c6` | `rustup target add riscv32imac-unknown-none-elf`, [espflash](https://github.com/esp-rs/espflash), Docker |
| `esp32h2` | as `esp32c6`, plus CMake, clang and libclang of the same LLVM version (OpenThread is built from source) |
| `esp32`, `esp32s3` | Espressif's Xtensa toolchain (below), espflash, Docker |

The ESP32 and ESP32-S3 need Espressif's Rust toolchain at esp-hal main's MSRV, a
pre-release that `espup` installs only when asked, and `. ~/export-esp.sh` in
every shell that builds:

```sh
espup install --targets esp32,esp32s3 --toolchain-version 1.100.0.0
```

## 1. Credentials

The device and the client each have an EDHOC identity. `examples/.credentials/`
is ignored by Git:

```sh
mkdir -p examples/.credentials && cd examples/.credentials
mai-coap --generate-identity device    # device.ccs, device.cosekey
mai-coap --generate-identity client    # client.ccs, client.cosekey
```

The firmware is built with the device's identity and the client's credential;
the `mai-ota` CLI gets the client's identity and the device's credential.

**The images carry the credentials.** Every ELF and binary built here embeds the
device's private key, and the Thread ones (nRF54, ESP32-H2) the Thread network key too. Keep
`examples/target/` and anything copied out of it private.

## 2. Build

Settings are fixed at build time through environment variables:

| Variable | |
| --- | --- |
| `MAI_OTA_DEVICE` | path to the device identity without extension, e.g. `examples/.credentials/device` |
| `MAI_OTA_CLIENT` | path to `client.ccs` |
| `WIFI_SSID`, `WIFI_PASSWORD` | the Wi-Fi examples' network |
| `THREAD_DATASET` | the Thread examples' network (nRF54, ESP32-H2): the active dataset in hex, as `ot-ctl dataset active -x` prints it |
| `EXAMPLE_ALSO_ACCEPT` | optional, ESP examples: one more board name whose images the build takes, to hand a device over to another firmware |

```sh
cd examples/pico-w
export MAI_OTA_DEVICE=$PWD/../.credentials/device
export MAI_OTA_CLIENT=$PWD/../.credentials/client.ccs
export WIFI_SSID=… WIFI_PASSWORD=…
cargo build --release
```

Each build reports a version that includes its build time, which is how the CLI
tells the new image from a rollback.

## 3. Flash once, by wire

### Pico W and Pico 2 W

Both boards can flash over their USB port in BOOTSEL mode. From the example's
directory, with the build settings above, build both ELFs and combine them into
one UF2. The combined file uses the linked flash addresses and includes the
erased OTA state page, so a pending swap cannot replace the application on boot.

```sh
# Pico W. For Pico 2 W set target=thumbv8m.main-none-eabihf and family=rp2350-arm-s.
target=thumbv6m-none-eabi
family=rp2040
board=$(basename "$PWD")
out="../target/$target/release"
cargo build --release
(cd bootloader && cargo build --release)
picotool uf2 convert "$out/$board-bootloader" -t elf "$out/$board-bootloader.uf2" --family "$family"
picotool uf2 convert "$out/$board" -t elf "$out/$board-app.uf2" --family "$family"
picotool uf2 combine "$out/$board-bootloader.uf2" "$out/$board-app.uf2" "$out/$board.uf2"
```

Unplug the board, hold BOOTSEL while plugging in the USB data cable, then
release BOOTSEL when the drive appears (`RPI-RP2` on Pico W, `RP2350` on Pico 2 W).
Copy only the combined `$out/$board.uf2` to that drive. The drive disappears and
the board reboots. Find its address in your router's DHCP leases.

For SWD flashing and defmt logs, connect a debug probe. From the same directory,
run `probe-rs download --chip RP2040 "$out/$board-bootloader"` (use `RP235x` on
Pico 2 W), then `cargo run --release` to flash and start the application.

### XIAO nRF54L15 and XIAO nRF54LM20A

From the example's directory, build and flash the bootloader first, then run
the application:

```sh
(cd bootloader && cargo build --release)
probe-rs download --chip nRF54L15 ../target/thumbv8m.main-none-eabihf/release/xiao-nrf54l15-bootloader
cargo run --release
```

For the XIAO nRF54LM20A, use `--chip nRF54LM20A` and `xiao-nrf54lm20a-bootloader`.

### ESP32, ESP32-C5, ESP32-C6, ESP32-H2, ESP32-S3

Build ESP-IDF's bootloader with rollback once per chip.
The example's `espflash.toml` names it, so every flash writes it:

```sh
tools/esp-bootloader/build.sh esp32c6 examples/esp32c6/bootloader.bin   # from the repository root
cd examples/esp32c6 && cargo run --release   # espflash, with --erase-parts otadata
```

When using the logging runner, the log prints the device's addresses once it is
on the network. The Thread examples also register `mai-ota-XXXXXXXX.local` with
the Thread border router's SRP server.

## 4. Update over the air

Rebuild first, so the new image reports a new version. From the example's
directory:

```sh
K=../.credentials
mai-ota --identity $K/client.ccs --key $K/client.cosekey --peer $K/device.ccs \
  upload <address> ../target/thumbv6m-none-eabi/release/pico-w
```

Use the binary from the table above for the other examples. To see a rollback,
upload with `--no-confirm`, or build with `mai-ota`'s `watchdog-test-panic`
feature. See the [CLI reference](../docs/cli.md) for the other options.

## Caveats

- The RP2040 has no random generator fit for keys. Its example seeds EDHOC from
  the ring oscillator, which the datasheet says not to use for security: fine
  for an example, not for a product. The RP2350 example uses the TRNG.
- The Cortex-M examples' image magic is `MAIOTAEX`. A product picks its own and
  keeps it.
- Nothing persists across a reboot but the firmware itself: the Thread examples
  rejoin from the dataset built into their image.
