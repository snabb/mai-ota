// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The Pico 2 W example's bootloader: [`mai_ota_bootloader::rp`] on the partitions
//! in `memory.x`.

#![no_std]
#![no_main]

#[cortex_m_rt::entry]
fn main() -> ! {
    mai_ota_bootloader::rp::run::<{ 4 * 1024 * 1024 }>()
}
