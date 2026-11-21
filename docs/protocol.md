# Upload protocol

What a device built with mai-ota's `coap` feature answers on its upload path,
`/fw` by convention. The [`mai-ota` CLI](cli.md) speaks it; any CoAP client
that can send Block1 under OSCORE can too.

## Security

Both methods must be served under OSCORE only: the firmware's endpoint policy
allows nothing unprotected on the upload path (mai-coap's `ProtectedOnly`, or
`PublicPaths` that does not list it), so an unprotected request gets 4.01. The
firmware wraps that policy in `coap::Proving`, which takes the first protected
request it allows on a trial boot as proof that the image works. There is no other
authentication and no manifest. OSCORE authenticates every block, Block1
numbers them, and the last block says it is last, so a body that arrives
complete arrived intact. The image check (below) guards against the wrong
image, not against a malicious one: whoever holds an authorised OSCORE
credential can install firmware.

## `GET /fw`

2.05 with Content-Format 60 (CBOR), a map:

| Key | Type | Value |
| --- | --- | --- |
| 0 | text | the board name the device checks images against |
| 1 | text | `Ota::version`, at most 64 bytes, what the firmware calls itself |
| 2 | bool | whether the running image is still on trial |

Being an authenticated request, a `GET` on a trial boot also confirms the image.
Its own answer can still say `true`, because the confirmation is written after
the request is allowed; a second `GET` a moment later shows `false`.

A client tells a rollback from a successful update by the version. That only
works if every build reports a different one: include a build time or a build
number, not just a release name.

## `PUT /fw`

The body is the raw application image:

* **ESP32:** the ESP-IDF application image, as `espflash save-image` makes it.
* **Cortex-M:** the bytes from the active partition's start, as `objcopy -O
  binary` makes them, without the `.bootloader_state` section.

Blocks go one per request with Block1, Content-Format 42, the same Request-Tag
on every block of one attempt, and Size1 on the first. Each block is written and
read back before its reply. The device checks the image as soon as the first
bytes are in (112 bytes for ESP, 2048 for Cortex-M), and activates it only after
the last block.

| Reply | When |
| --- | --- |
| 2.31 | block written; send the next |
| 2.04 on the last block | written, checked and activated; the device reboots 2 s later |
| 4.00 | not an image for this board or chip, cut short, or not the size Size1 gave; the device logs why |
| 4.01 | not protected by OSCORE |
| 4.08 | the device lost the upload (timeout, another upload); start over with a new Request-Tag |
| 4.13 | larger than the slot, or a last block that ends off the Size1 given (mai-coap answers that one itself) |
| 5.00 | flash error or read-back mismatch |
| 5.03 | the running image is still on trial (confirm it first; a device that could not read its boot state counts as one), or an image is already activated and waits for the reboot |

A device may answer with a smaller Block1 size, and later blocks then follow it;
mai-coap's upload worker currently answers with the size it was sent.

### Timeouts

* An upload with no block for **120 s** is abandoned. That is longer than one
  CON request retransmits (`MAX_TRANSMIT_WAIT`, 93 s), so a block that got
  through only on its last retransmission, or is sent again after that, still
  finds the upload open.
* A block may be sent again as a new request: the device recognises a block it
  has already written. A client should do that rather than give up the whole
  body when one block's retransmissions run out, which a Thread child that loses
  its parent for a minute can cause.
* The device keys a body by its OSCORE context and Request-Tag. A client that
  runs EDHOC again in the middle of an upload starts a new body.

## After the upload

The device reboots `REBOOT_DELAY` (2 s) after the final reply, so the reply can
arrive or a retransmission fetch it. The new image boots on trial:

* The first protected request within `TRIAL_TIMEOUT` (600 s, counted from the
  boot) confirms it.
* Without one, the device reboots and the bootloader goes back to the previous
  image. Should the supervisor not reboot it, the watchdog does: its feeder stops
  at the deadline.
* A hang or a halted panic does the same through the watchdog, which runs from
  the trial image's first instruction (embassy-boot) or straight after
  `esp_hal::init` (ESP32).
* A confirmation that fails to write keeps the trial until the deadline, then
  rolls back. On embassy-boot, `mark_booted` erases the swap state before it
  writes the booted mark, and an erased state reads as booted: a write that fails
  after the erase keeps the new image, though the device reports the failure.

Uploads are refused (5.03) during a trial, because the other slot holds the
fallback.
