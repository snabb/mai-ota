# Examples

One firmware per supported chip family. Each joins a network, serves `/fw` over
CoAP under EDHOC and OSCORE, and does nothing else. With the
[integration guide](../docs/integration.md), they show what a firmware needs.

| Example | Board | Network | Bootloader | Binary |
| --- | --- | --- | --- | --- |
| `esp32` | any ESP32 with 4 MiB of flash or more, chip revision v1.0 or later (M5Stack Atom Lite) | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `xtensa-esp32-none-elf/release/esp32-example` |
| `esp32c6` | any ESP32-C6 with 4 MiB of flash or more | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `riscv32imac-unknown-none-elf/release/esp32c6-example` |
| `esp32s3` | any ESP32-S3 with 4 MiB of flash or more | Wi-Fi | ESP-IDF's, from `tools/esp-bootloader` | `xtensa-esp32s3-none-elf/release/esp32s3-example` |
| `pico-w` | Raspberry Pi Pico W | Wi-Fi | `pico-w/bootloader` | `thumbv6m-none-eabi/release/pico-w` |
| `pico-2-w` | Raspberry Pi Pico 2 W | Wi-Fi | `pico-2-w/bootloader` | `thumbv8m.main-none-eabihf/release/pico-2-w` |
| `xiao-nrf54l15` | Seeed XIAO nRF54L15 | Thread | `xiao-nrf54l15/bootloader` | `thumbv8m.main-none-eabihf/release/xiao-nrf54l15` |
| `xiao-nrf54lm20a` | Seeed XIAO nRF54LM20A | Thread | `xiao-nrf54lm20a/bootloader` | `thumbv8m.main-none-eabihf/release/xiao-nrf54lm20a` |

Every example builds into `examples/target/`. `common/` is the CoAP server they
share; `esp/`, `rp/` and `nrf54l/` hold the source shared by the three ESP, the two
Pico and the two XIAO nRF54 examples.

## Prerequisites

All examples need Rust 1.97 or later, the
[mai-coap](https://github.com/snabb/mai-coap) CLI for the credentials, and the
`mai-ota` CLI for updates:

```sh
cargo install --locked --git https://github.com/snabb/mai-coap mai-coap-cli
cargo install --locked --git https://github.com/snabb/mai-ota mai-ota-cli
```

| Example | Also needs |
| --- | --- |
| `pico-w` | `rustup target add thumbv6m-none-eabi`, [probe-rs](https://probe.rs) and a debug probe |
| `pico-2-w` | `rustup target add thumbv8m.main-none-eabihf`, probe-rs and a debug probe |
| `xiao-nrf54l15`, `xiao-nrf54lm20a` | `rustup target add thumbv8m.main-none-eabihf`, probe-rs; CMake, clang and libclang of the same LLVM version (OpenThread is built from source) |
| `esp32c6` | `rustup target add riscv32imac-unknown-none-elf`, [espflash](https://github.com/esp-rs/espflash), Docker |
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
device's private key, and the nRF54 ones the Thread network key too. Keep
`examples/target/` and anything copied out of it private.

## 2. Build

Settings are fixed at build time through environment variables:

| Variable | |
| --- | --- |
| `MAI_OTA_DEVICE` | path to the device identity without extension, e.g. `examples/.credentials/device` |
| `MAI_OTA_CLIENT` | path to `client.ccs` |
| `WIFI_SSID`, `WIFI_PASSWORD` | the Wi-Fi examples' network |
| `THREAD_DATASET` | the nRF54 examples' network: the active dataset in hex, as `ot-ctl dataset active -x` prints it |
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

**Pico W, Pico 2 W, XIAO nRF54L15, XIAO nRF54LM20A:** the bootloader first, then the
application. The application is linked to run behind the bootloader and does
not start without it.

```sh
cd examples/pico-w/bootloader && cargo build --release
probe-rs download --chip RP2040 ../../target/thumbv6m-none-eabi/release/pico-w-bootloader
cd .. && cargo run --release          # probe-rs, with the defmt log
```

For the Pico 2 W use `--chip RP235x` and `thumbv8m.main-none-eabihf/release/pico-2-w-bootloader`;
for the XIAOs, `--chip nRF54L15` or `--chip nRF54LM20A` and
`thumbv8m.main-none-eabihf/release/xiao-nrf54l15-bootloader` or `xiao-nrf54lm20a-bootloader`.

**ESP32, ESP32-C6, ESP32-S3:** build ESP-IDF's bootloader with rollback once per chip.
The example's `espflash.toml` names it, so every flash writes it:

```sh
tools/esp-bootloader/build.sh esp32c6 examples/esp32c6/bootloader.bin   # from the repository root
cd examples/esp32c6 && cargo run --release   # espflash, with --erase-parts otadata
```

The log prints the device's addresses once it is on the network. The nRF54
examples also register `mai-ota-XXXXXXXX.local` with the Thread border router's
SRP server.

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
- Nothing persists across a reboot but the firmware itself: the Thread example
  rejoins from the dataset built into its image.
