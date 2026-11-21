// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) Embassy project contributors

//! The RP2040's and RP2350's bootloader, on the QSPI flash.

use embassy_boot::AlignedBuffer;
use embassy_rp::flash::{Blocking, ERASE_SIZE, FLASH_BASE, Flash};
use embassy_rp::peripherals::FLASH;
use embassy_rp::watchdog::Watchdog;
use mai_ota::watchdog::TIMEOUT;

impl crate::Feed for Watchdog {
    fn feed(&mut self) {
        Watchdog::feed(self, TIMEOUT);
    }
}

/// Swap if one is pending, then jump to the active partition. `SIZE` is the flash
/// chip's size in bytes.
pub fn run<const SIZE: usize>() -> ! {
    // The two chips differ, each measured once on a Pico board (2026-11-08). On the
    // RP2040, with `embassy_rp::init` here the application's CYW43439 kept timing out
    // association, and without it the swap works; so the chip reaches the application
    // as the boot ROM left it. On the RP2350, without `init` the first swap left the
    // chip answering neither USB nor SWD until BOOTSEL, cause not found; with it, as
    // in Embassy's own example, the swap works.
    // SAFETY: nothing else runs; the application takes the peripherals afresh.
    #[cfg(feature = "rp2040")]
    let p = unsafe { embassy_rp::Peripherals::steal() };
    #[cfg(feature = "rp235xa")]
    let p = embassy_rp::init(Default::default());
    let flash: Flash<'static, FLASH, Blocking, SIZE> = Flash::new_blocking(p.FLASH);
    let start_watchdog = || {
        #[cfg(feature = "rp2040")]
        start_watchdog_tick();
        let mut watchdog = Watchdog::new(p.WATCHDOG);
        watchdog.pause_on_debug(true);
        watchdog.start(TIMEOUT);
        Some(watchdog)
    };
    let mut buf = AlignedBuffer([0; ERASE_SIZE]);
    let active = crate::prepare(flash, start_watchdog, &mut buf.0);
    // SAFETY: the active partition holds an image linked to run there.
    unsafe { crate::load(FLASH_BASE as u32 + active) }
}

/// Make the RP2040's watchdog count, which it does only on a tick derived from
/// `clk_ref`; without `init` nothing has set that up.
///
/// `clk_ref` is whatever the boot ROM left, not the crystal, so its rate is not known
/// here. The divider assumes 48 MHz, faster than the boot ROM runs it: a slower
/// `clk_ref` only lengthens the timeout, never shortens it under a swap that feeds
/// on every flash operation. The application's `init` sets the tick from the crystal
/// within milliseconds of the jump, after which the count it inherits runs at 1 MHz.
#[cfg(feature = "rp2040")]
fn start_watchdog_tick() {
    embassy_rp::pac::WATCHDOG.tick().write(|w| {
        w.set_cycles(48);
        w.set_enable(true);
    });
}
