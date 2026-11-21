# `mai-ota` CLI

Uploads an image to a device and checks that it came back running it.

```sh
cargo install --locked --git https://github.com/snabb/mai-ota mai-ota-cli
```

It builds on Unix-like hosts only. `upload` converts an ESP32 ELF with
`espflash save-image`, so `espflash` must be on `PATH` for those; a Cortex-M ELF
is converted by the tool itself.

## Credentials

A device takes firmware over OSCORE only, so every command needs EDHOC
credentials, in the files [`mai-coap`](https://github.com/snabb/mai-coap)'s CLI
uses:

| Flag | File |
| --- | --- |
| `--identity` | this client's credential (CCS), byte for byte what the device has |
| `--key` | this client's private key: a COSE_Key `{1: 2, -1: 1, -4: d}` as `aiocoap-keygen` or `mai-coap --generate-identity` writes it, readable by its owner only |
| `--peer` | the device's credential (CCS) |

`mai-coap --generate-identity client` makes a client pair (install it with
`cargo install --locked --git https://github.com/snabb/mai-coap mai-coap-cli`).
The device's firmware is built with `client.ccs` as its trusted peer. The
credential options may also follow the command.

## Commands

```sh
mai-ota --identity client.ccs --key client.cosekey --peer device.ccs info device.local
```

Prints the board, version and trial state (`GET /fw`). On an image that is on
trial, this confirms it.

```sh
mai-ota --identity client.ccs --key client.cosekey --peer device.ccs \
  upload device.local path/to/firmware.elf
```

1. `GET /fw`, to learn the running version.
2. `PUT /fw`, one Block1 request per block. A block whose retransmissions run
   out is sent again, up to 6 tries in all; a 4.08 starts the upload over, up to
   3 times. Every block but the last must be answered 2.31 and the last 2.04,
   each acknowledging the block sent. The last block goes without EDHOC
   recovery: should its reply be lost and the device reboot, the new image
   answers 4.01, and a fresh session sending the block again would confirm the
   trial. The tool then goes on as below, or with `--no-confirm` reports that
   it cannot tell whether the device took the image.
3. Waits for the reboot, then `GET /fw` on the address that took the upload and
   on every other the name resolves to, every 5 s for up to 5 minutes. A Wi-Fi
   device can come back without the address its name pointed at. The first
   answer confirms the trial, and the tool keeps asking until the device says it
   is no longer on trial.

| Option | |
| --- | --- |
| `--chip` | the ESP32 chip, for `espflash save-image`; by default esp32c6 for RISC-V and esp32s3 for Xtensa; refused when the ELF is for the other architecture |
| `--block-size` | a power of two from 16 to 1024, default 1024; the device may ask for smaller |
| `--no-confirm` | stop after the upload and send nothing more, so the trial times out and the device rolls back; for testing |
| `--path` | the upload path, default `/fw` |
| `--port` | default 5683 |
| `-v`, `-vv` | the CoAP library's log |

A host is a name, an IPv4 or IPv6 address, or a link-local IPv6 address with
its zone (`fe80::1%eth0`).

## Exit status

| Status | Means |
| --- | --- |
| 0 | done: the new version answered and is confirmed (or `--no-confirm`) |
| 1 | the device refused, or the old version answered (rolled back), or the new one is still on trial at the deadline |
| 2 | bad arguments, files or local tools |
| 3 | the device did not answer, or with `--no-confirm`, the last block's reply was lost and whether the device took the image is unknown |

"The old version answered" also appears when the new image reports the same
version as the old one; see [the protocol](protocol.md#get-fw).

A device whose running image does not serve `GET` on the upload path (it
answers 4.04 or 4.05) still takes an upload; its version shows as not reported.
If it answers that way again after the reboot, the old image is back, and the
tool says it rolled back.
