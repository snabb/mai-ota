#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (C) 2026 mai-ota contributors
#
# Start an embassy-boot swap on a running nRF54L over SWD, without an upload:
# write ELF's image into the DFU partition, mark the state partition SWAP while
# the application runs, then reset the chip the way the application would, and
# watch the bootloader swap it in.
#
#   tools/nrf54-swap-test.sh PROBE ELF soft|wdt [STATE_ADDR [DFU_ADDR]]
#
# soft: SYSRESETREQ through AIRCR, what `SCB::sys_reset` does.
# wdt:  a WDT30 timeout after about 1 ms.
#
# STATE_ADDR and DFU_ADDR are the state and DFU partitions (default 0x6000 and
# the example's DFU start for CHIP). The image is written each time because after a
# swap the DFU partition holds the previous image one page further on, the layout
# a revert reads, and swapping that in again boots garbage.
set -euo pipefail

if (( $# < 3 )); then
    echo "usage: $0 PROBE ELF soft|wdt [STATE_ADDR [DFU_ADDR]]" >&2
    exit 2
fi
probe=$1
elf=$2
how=$3
# CHIP picks the chip, nRF54LM20A by default; the defaults below are the
# examples' layout on it.
chip=${CHIP:-nRF54LM20A}
case $chip in
nRF54LM20A) RRAMC=0x5004E000 default_dfu=0x103000 ram_end=0x2007FD40 ;;
nRF54L15) RRAMC=0x5004B000 default_dfu=0xC3000 ram_end=0x2003FE00 ;;
*) echo "CHIP: nRF54LM20A or nRF54L15, not $chip" >&2; exit 2 ;;
esac
state=${4:-0x6000}
dfu=${5:-$default_dfu}
case $how in soft | wdt) ;; *) echo "soft or wdt, not $how" >&2; exit 2 ;; esac
[[ -f $elf ]] || { echo "$elf: no such file" >&2; exit 2; }
WDT30=0x50108000

w() { probe-rs write --chip $chip --probe "$probe" b32 "$@" >/dev/null; }
r() { probe-rs read --chip $chip --probe "$probe" b32 "$1" "${2:-1}" | cut -d: -f2-; }

# Nothing is written while a swap is pending or the running image is on trial: the
# state partition then reads SWAP, and the DFU partition holds the fallback.
# A read that fails or says nothing stops here too (`set -e`, `pipefail`).
mark=$(probe-rs read --chip $chip --probe "$probe" b32 "$state" 1 | awk '{print $2}')
if [[ ! $mark =~ ^[0-9a-f]{8}$ ]]; then
    echo "cannot read the state partition (got '$mark')" >&2
    exit 1
fi
if [[ $mark == f0f0f0f0 ]]; then
    echo "a swap is pending or the image is on trial (state reads SWAP); confirm it first" >&2
    exit 1
fi

# In a private directory: the binary embeds the device's credentials.
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
objcopy=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
"$objcopy" -O binary -R .bootloader_state "$elf" "$tmp/image.bin"
# Both chips' examples link at 0xA000; only the initial stack pointer tells an
# image for the other chip apart.
sp=$(od -An -tx4 -N4 "$tmp/image.bin" | tr -d ' ')
if (( 16#$sp <= 0x20000000 || 16#$sp > ram_end )); then
    echo "$elf: initial stack pointer 0x$sp is outside $chip's RAM" >&2
    exit 2
fi
probe-rs download --chip $chip --probe "$probe" --binary-format bin \
    --base-address "$dfu" "$tmp/image.bin" > "$tmp/download.log" 2>&1 ||
    { cat "$tmp/download.log" >&2; exit 1; }
# The download can leave the core halted, and WDT30 pauses while it is.
w 0xE000EDF0 0xA05F0001

# RRAM has no erase: writing all-ones is the erase. 16 KiB of state, in chunks:
# the progress records first, the SWAP magic last, so a reset in between leaves no
# swap marked over stale progress.
w $((RRAMC + 0x500)) 1 # CONFIG.WEN, unbuffered
for ((off = 16; off < 16384; off += 1008)); do
    n=$(( (16384 - off) / 4 < 252 ? (16384 - off) / 4 : 252 ))
    # shellcheck disable=SC2046
    w $((state + off)) $(printf '0xFFFFFFFF %.0s' $(seq $n))
done
w "$state" 0xF0F0F0F0 0xF0F0F0F0 0xF0F0F0F0 0xF0F0F0F0
w $((RRAMC + 0x500)) 0
echo "state: $(r "$state" 5)"

case $how in
soft) w 0xE000ED0C 0x05FA0004 || true ;;
wdt)
    w $((WDT30 + 0x504)) 0x20 # CRV: 32 ticks of 32.768 kHz
    w $((WDT30 + 0x508)) 1    # RREN: RR0
    w $((WDT30 + 0x000)) 1    # TASKS_START
    ;;
esac

# Uptime from the GRTC, and which vector table runs: 0 is the bootloader.
# Until the application has run for 5 s, or 120 s at most.
start=$(date +%s)
ok=
while (( $(date +%s) - start < 120 )); do
    up=$(r 0x500E2720 2>/dev/null || echo "noaccess")
    vtor=$(r 0xE000ED08 2>/dev/null || echo "noaccess")
    echo "$(( $(date +%s) - start ))s up=$up vtor=$vtor"
    if [[ $vtor != *00000000* && $vtor != noaccess && $(( 16#${up// /} )) -gt 5000000 ]]; then
        ok=1
        break
    fi
    sleep 1
done
echo "state: $(r "$state" 5)"
[[ $ok ]] || { echo "the application did not come up within 120 s" >&2; exit 1; }
