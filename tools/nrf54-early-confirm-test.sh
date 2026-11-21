#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (C) 2026 mai-ota contributors
#
# Start a swap over SWD on a running nRF54LM20A example, as tools/nrf54-swap-test.sh
# does, while a client asks for /fw from the moment of the reset, the way the CLI
# does after an upload. The first answer from the image on trial confirms it. Logs
# the trial image's console, and prints which image answered.
#
#   tools/nrf54-early-confirm-test.sh PROBE ADDRESS TRIAL_ELF OUT_DIR soft|wdt [SECONDS|none|halt]
#
# TRIAL_ELF is written into the DFU partition first. It has to be: after a confirmed
# swap the partition holds the previous image one page further on, the layout the
# bootloader's revert reads, and swapping that in as an update boots garbage.
#
# The console is attached SECONDS (default 0) after the trial image starts, or not at
# all with `none`, so that a run can tell the debugger's effect from the image's.
# `halt` instead takes eight backtraces of the trial image through `probe-rs gdb`
# and gdb-multiarch, halting the core for each; the watchdog pauses meanwhile.
#
# soft resets with SYSRESETREQ, as `SCB::sys_reset` does; wdt through a WDT30
# timeout, as `watchdog::nrf::reset_chip` does.
#
# Run from examples/ with the example credentials.
set -euo pipefail

if (( $# < 5 )); then
    echo "usage: $0 PROBE ADDRESS TRIAL_ELF OUT_DIR soft|wdt [SECONDS|none|halt]" >&2
    exit 2
fi
probe=$1
address=$2
elf=$3
out=$4
how=$5
watch=${6:-0}
case $how in soft | wdt) ;; *) echo "soft or wdt, not $how" >&2; exit 2 ;; esac
case $watch in none | halt | [0-9]*) ;; *) echo "SECONDS, none or halt, not $watch" >&2; exit 2 ;; esac
[[ -f $elf ]] || { echo "$elf: no such file" >&2; exit 2; }
chip=nRF54LM20A
RRAMC=0x5004E000
state=0x6000
dfu=0x103000

w() { probe-rs write --chip $chip --probe "$probe" b32 "$@" >/dev/null; }
# Nothing is written while a swap is pending or the running image is on trial: the
# state partition then reads SWAP, and the DFU partition holds the fallback.
# A read that fails or says nothing stops here too (`set -e`, `pipefail`).
mark=$(probe-rs read --chip $chip --probe "$probe" b32 $state 1 | awk '{print $2}')
if [[ ! $mark =~ ^[0-9a-f]{8}$ ]]; then
    echo "cannot read the state partition (got '$mark')" >&2
    exit 1
fi
if [[ $mark == f0f0f0f0 ]]; then
    echo "a swap is pending or the image is on trial (state reads SWAP); confirm it first" >&2
    exit 1
fi

# The trial binary embeds the device's credentials: keep it to its owner.
mkdir -p "$out"
chmod 700 "$out"
objcopy=$(find "$(rustc --print sysroot)" -name llvm-objcopy | head -1)
"$objcopy" -O binary -R .bootloader_state "$elf" "$out/trial.bin"
probe-rs download --chip $chip --probe "$probe" --binary-format bin \
    --base-address $dfu "$out/trial.bin" > "$out/download.log" 2>&1
# Resume, should the download have left the core halted: WDT30 pauses while it is.
w 0xE000EDF0 0xA05F0001

vtor() { probe-rs read --chip $chip --probe "$probe" b32 0xE000ED08 1 2>/dev/null | awk '{print $2}'; }
# An array rather than a function: `timeout` runs programs, and a function named
# `info` would quietly run info(1) instead.
ask=(mai-ota --identity ~/.config/mai-ota/example/client.ccs
    --key ~/.config/mai-ota/example/client.cosekey
    --peer .credentials/device.ccs info "$address")

# The progress records first, the SWAP magic last.
w $((RRAMC + 0x500)) 1
for ((off = 16; off < 16384; off += 1008)); do
    n=$(( (16384 - off) / 4 < 252 ? (16384 - off) / 4 : 252 ))
    # shellcheck disable=SC2046
    w $((state + off)) $(printf '0xFFFFFFFF %.0s' $(seq $n))
done
w $state 0xF0F0F0F0 0xF0F0F0F0 0xF0F0F0F0 0xF0F0F0F0
w $((RRAMC + 0x500)) 0
case $how in
soft) w 0xE000ED0C 0x05FA0004 || true ;;
wdt)
    WDT30=0x50108000
    w 0x5010E504 0x4d # GPREGRET[1]: reset_chip's mark
    w $((WDT30 + 0x504)) 15
    w $((WDT30 + 0x508)) 1
    w $((WDT30 + 0x50c)) 1
    w $WDT30 1 || true
    ;;
esac
echo "reset ($how) $(date +%T)"

end=$(( $(date +%s) + 300 ))
(while (( $(date +%s) < end )); do
    echo "$(date +%T) $(timeout 4 "${ask[@]}" 2>&1 | tail -1)"
done > "$out/cli.log") &

# The swap takes a minute or more; then catch the trial image from its first line.
sleep 45
until [ "$(vtor)" = "0000a000" ]; do :; done
echo "trial image runs $(date +%T)"
if [ "$watch" = halt ]; then
    # Each gdb connection halts the core and its detach resumes it.
    probe-rs gdb --chip $chip --probe "$probe" --gdb-connection-string 127.0.0.1:1337 \
        > "$out/gdb-server.log" 2>&1 &
    server=$!
    sleep 2
    for ((i = 0; i < 8; i++)); do
        echo "=== $(date +%T)"
        timeout 20 gdb-multiarch -q -batch -nx "$elf" \
            -ex "target extended-remote 127.0.0.1:1337" \
            -ex "info registers pc lr sp" -ex "bt 40" -ex detach || true
        sleep 1
    done > "$out/samples.log" 2>&1
    kill $server || true
    wait $server || true
    w 0xE000EDF0 0xA05F0001 || true
elif [ "$watch" != none ]; then
    sleep "$watch"
    echo "console attached $(date +%T)"
    timeout -s INT 60 probe-rs attach --chip $chip --probe "$probe" "$elf" > "$out/console.log" 2>&1 || true
fi
wait
grep board "$out/cli.log" | sed 's/^[0-9:]* //' | uniq -c
