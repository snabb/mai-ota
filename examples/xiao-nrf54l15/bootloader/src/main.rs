// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The XIAO nRF54L15 example's bootloader: [`mai_ota_bootloader::nrf54l`] on
//! the partitions in `memory.x`.

#![no_std]
#![no_main]

#[cortex_m_rt::entry]
fn main() -> ! {
    mai_ota_bootloader::nrf54l::run()
}
