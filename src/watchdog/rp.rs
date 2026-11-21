// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The RP2040's and RP2350's watchdog, for [`watchdog`](super), and the whole-chip
//! reset it gives.

use embassy_rp::Peri;
use embassy_rp::peripherals::WATCHDOG;

use super::TIMEOUT;

/// Reset the whole chip, the way a watchdog timeout does. Not `SCB::sys_reset`, which
/// on the RP2040 resets only the core and leaves DMA and PIO running.
///
/// embassy-rp selects the RP2040's PSM.WDSEL bits on the RP2350 too, where they name
/// other blocks. The RP2350 still resets fully, cores included (measured on a Pico 2 W),
/// since the PSM restarts its sequence from the earliest block selected.
pub fn reset_chip() -> ! {
    // SAFETY: nothing runs after this; the watchdog it shares is about to reset.
    let watchdog = unsafe { WATCHDOG::steal() };
    embassy_rp::watchdog::Watchdog::new(watchdog).trigger_reset();
    loop {
        cortex_m::asm::nop();
    }
}

/// The watchdog, unarmed until [`start`](super::Watchdog::start).
pub struct Watchdog(embassy_rp::watchdog::Watchdog<'static>);

impl Watchdog {
    /// Take the watchdog without starting it. If it already runs — a bootloader starts
    /// it for a trial boot — it is fed now, which gives the rest of bring-up its whole
    /// timeout before the feeder task takes over.
    pub fn new(watchdog: Peri<'static, WATCHDOG>) -> Self {
        let mut this = Self(embassy_rp::watchdog::Watchdog::new(watchdog));
        if embassy_rp::pac::WATCHDOG.ctrl().read().enable() {
            info!("watchdog: already running; feeding it");
            this.0.feed(TIMEOUT);
        }
        this
    }
}

impl super::Watchdog for Watchdog {
    fn start(&mut self) {
        // A probe halting a core would otherwise end the session in a reset.
        self.0.pause_on_debug(true);
        self.0.start(TIMEOUT);
    }

    fn feed(&mut self) {
        self.0.feed(TIMEOUT);
    }
}
