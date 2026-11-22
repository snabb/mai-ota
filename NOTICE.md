# Third-party notices

mai-ota is Copyright (C) 2026 mai-ota contributors, licensed MIT OR Apache-2.0
([LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE)). It incorporates
work from the projects below, under the same choice of licences unless the table
says otherwise.

| Upstream | Licence | Where it lands here |
| --- | --- | --- |
| [embassy-sync](https://github.com/embassy-rs/embassy) `fmt.rs` | MIT OR Apache-2.0 | `src/fmt.rs` |
| [Embassy](https://github.com/embassy-rs/embassy) nRF and RP bootloaders | MIT OR Apache-2.0 | `bootloader/src/{lib,nrf54l,rp}.rs`, `examples/pico-2-w/bootloader/memory.x` |
| [Embassy](https://github.com/embassy-rs/embassy) RP examples | MIT OR Apache-2.0 | `examples/rp/main.rs`, `examples/pico-2-w/memory.x` |
| [nrf-802154](https://github.com/sysgrok/nrf-802154) `scrub_grtc` | MIT OR Apache-2.0 | `examples/nrf54l/main.rs` |
| [openthread](https://github.com/esp-rs/openthread) `enet` module | MIT OR Apache-2.0 | `examples/nrf54l/enet.rs` |
| [mai-coap](https://github.com/snabb/mai-coap) CLI credentials | MIT OR Apache-2.0 | `cli/src/credentials.rs` |
| [Infineon CYW43439 firmware](https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware) | Infineon Permissive Binary License 1.0 | `examples/rp/cyw43-firmware/` |

## embassy-sync — logging macros

Copyright (c) the Embassy contributors. `src/fmt.rs` is adapted from
`embassy-sync`'s `fmt.rs`, trimmed to the logging macros. Its `Dbg` adapter is
original to this repository.

## Embassy — bootloaders

Copyright Embassy project contributors.

`bootloader/src/nrf54l.rs` is adapted from
`examples/boot/bootloader/nrf/src/main.rs` in Embassy and from
`embassy-boot-nrf`'s `WatchdogFlash` and `BootLoader::load`, at revision
`2caf10d20c7e9962d627b91f2aebdeeca3790035`. `bootloader/src/rp.rs` is adapted
from `examples/boot/bootloader/rp/src/main.rs` and
`examples/boot/bootloader/rp235x/src/main.rs` and from `embassy-boot-rp`'s
`WatchdogFlash` and `BootLoader::load`, at revision
`522edd6463d3ecfa04ff6ed603ad22cab022766d`. Both are modified to start the
watchdog only when a swap is pending, with the application's settings; the nRF
one to touch no peripheral but the RRAM controller and the watchdog; the RP one
to leave the RP2040's clocks to the application rather than run
`embassy_rp::init`.

The boot-block sections of the RP2350 bootloader's `memory.x` are adapted from
`examples/boot/bootloader/rp235x/memory.x` at the same revision.

## Embassy — RP examples

Copyright Embassy project contributors. The Wi-Fi bring-up in
`examples/rp/main.rs` follows `examples/rp/src/bin/wifi_tcp_server.rs` in
Embassy at revision `8a940dab0d56a910277b816987e45315ca7979aa`. The boot-block
sections of `examples/pico-2-w/memory.x` are adapted from
`examples/rp235x/memory.x`.

## nrf-802154 — `scrub_grtc`

Copyright (c) nrf-sdc project contributors. `scrub_grtc` in
`examples/nrf54l/main.rs` is adapted from `scrub_grtc` in
`tests/src/lib.rs` in nrf-802154, with its documentation rewritten, and modified
to disable the compare channels as well.

## openthread — `enet` module

Copyright (c) the esp-rs contributors. `examples/nrf54l/enet.rs` is
adapted from `src/enet.rs` in `openthread` 0.4.0, and modified for the
`embassy-net-driver-channel` from Git, which lends the runner a buffer from the
stack's packet pool.

## Infineon CYW43439 firmware

The three blobs in `examples/rp/cyw43-firmware/` are copied from Embassy's
`cyw43-firmware/` directory, which records their source as
`georgerobotics/cyw43-driver`. They are licensed under the Infineon Permissive
Binary License, Version 1.0, whose text is
`examples/rp/cyw43-firmware/LICENSE-permissive-binary-license-1.0.txt`; it
travels with any image built from the Pico examples.

## mai-coap — credential files

Copyright (c) 2026 mai-coap contributors. `cli/src/credentials.rs` is adapted
from mai-coap's `cli/src/credentials.rs`, trimmed to loading the credential and
key files.
