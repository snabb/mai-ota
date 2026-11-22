#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (C) 2026 mai-ota contributors
#
# Build the ESP-IDF second-stage bootloader with app rollback, which mai-ota
# needs and espflash's bundled bootloader lacks, for one chip:
#
#   tools/esp-bootloader/build.sh esp32 path/to/bootloader.bin
#   tools/esp-bootloader/build.sh esp32c6 path/to/bootloader.bin
#   tools/esp-bootloader/build.sh esp32s3 path/to/bootloader.bin
#
# Name the output in the firmware's espflash.toml (`[idf] bootloader = ...`) so
# that every flash writes it. Needs Docker. Only a fresh build directory is
# mounted into the container.
#
# The flash size, mode and frequency in sdkconfig.defaults are placeholders:
# espflash rewrites them in the image header for the board it flashes.
set -euo pipefail

chip=${1:?usage: build.sh esp32|esp32c6|esp32s3 OUTPUT}
out=${2:?usage: build.sh esp32|esp32c6|esp32s3 OUTPUT}
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
image=${IDF_IMAGE:-espressif/idf@sha256:81893c71bb5e570088901f21def8684c25cd2a9020281bd01b843a7655edb18c}

# Where the ROM loads the bootloader from: 0x1000 on the ESP32 (and S2), 0x0 on
# later chips. The size check needs it to find the room up to the partition table.
case $chip in
esp32 | esp32s2) offset=0x1000 ;;
*) offset=0x0 ;;
esac

work=$(mktemp -d "${TMPDIR:-/tmp}/esp-bootloader-$chip.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/main"
cp "$here/sdkconfig.defaults" "$work/sdkconfig.defaults"
printf 'cmake_minimum_required(VERSION 3.16)\ninclude($ENV{IDF_PATH}/tools/cmake/project.cmake)\nproject(bootloader_only)\n' \
    > "$work/CMakeLists.txt"
printf 'idf_component_register(SRCS "main.c")\n' > "$work/main/CMakeLists.txt"
printf 'void app_main(void) {}\n' > "$work/main/main.c"

docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$work:/project" -w /project "$image" bash -c "
set -euo pipefail
idf.py set-target $chip
idf.py bootloader
grep -qx CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y sdkconfig
python \$IDF_PATH/components/partition_table/check_sizes.py --offset 0x8000 bootloader $offset build/bootloader/bootloader.bin
git -C \$IDF_PATH describe --always --tags > idf-version.txt
" > "$work/build.log" 2>&1 || { tail -30 "$work/build.log"; exit 1; }

cp "$work/build/bootloader/bootloader.bin" "$out"
printf '%s: %s bytes, ESP-IDF %s, sha256 %s\n' "$out" "$(stat -c %s "$out")" \
    "$(cat "$work/idf-version.txt")" "$(sha256sum "$out" | cut -d' ' -f1)"
