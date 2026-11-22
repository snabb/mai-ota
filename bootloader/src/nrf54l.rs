// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) Embassy project contributors

//! The nRF54L15's and nRF54LM20's bootloader, on their RRAM. It touches no
//! peripheral but the RRAM controller and the watchdog (WDT31, embassy-nrf's `WDT0`
//! in a secure build).

use embassy_boot::AlignedBuffer;
use embassy_nrf::peripherals::{RRAMC, WDT0};
use embassy_nrf::rramc::{PAGE_SIZE, Rramc, Unbuffered};
use embassy_nrf::wdt::{self, WatchdogHandle};

impl crate::Feed for WatchdogHandle {
    fn feed(&mut self) {
        WatchdogHandle::feed(self);
    }
}

/// Swap if one is pending, then jump to the active partition.
///
/// The state partition holds a magic and one 16-byte progress record per step of a
/// swap; embassy-boot asserts that it fits.
pub fn run() -> ! {
    // SAFETY: nothing else runs; the application takes the peripherals afresh.
    let rram: Rramc<'static, Unbuffered> = Rramc::new(unsafe { RRAMC::steal() });
    let mut buf = AlignedBuffer([0; PAGE_SIZE]);
    let active = crate::prepare(rram, start_watchdog, &mut buf.0);
    // SAFETY: the bootloader is done with the core, and the active partition holds an
    // image linked to run there.
    unsafe {
        cortex_m::Peripherals::steal().SCB.invalidate_icache();
        crate::load(active)
    }
}

/// Start the watchdog as the application would, or adopt it if it already runs that way.
/// `None` when it runs with some other configuration, which cannot be changed until it
/// resets the chip.
fn start_watchdog() -> Option<WatchdogHandle> {
    // SAFETY: as for the RRAM controller.
    match wdt::Watchdog::try_new::<_, 1>(unsafe { WDT0::steal() }, mai_ota::watchdog::nrf::config())
    {
        Ok((_, [handle])) => Some(handle),
        Err(_) => None,
    }
}
