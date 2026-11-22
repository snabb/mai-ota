// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The nRF52840's WDT, or the nRF54L15's or nRF54LM20's WDT31, for
//! [`watchdog`](super). Once started it runs until the next reset.
//!
//! On the nRF54L, embassy-nrf names WDT31 `WDT0` in a secure build; `WDT1` is
//! WDT30, which is secure-only.

use embassy_nrf::Peri;
use embassy_nrf::wdt::{self, HaltConfig, SleepConfig, WatchdogHandle};

use super::TIMEOUT;

#[cfg(feature = "nrf52840")]
type Wdt = embassy_nrf::peripherals::WDT;
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
type Wdt = embassy_nrf::peripherals::WDT0;

// Registers, secure aliases, written directly rather than through embassy-nrf's
// `pac`, which needs its `unstable-pac` feature.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
const WDT30: usize = 0x5010_8000;
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
const RESETREAS: *mut u32 = 0x5010_E600 as *mut u32;
/// `POWER.GPREGRET[1]`, which a watchdog reset keeps. `[0]` is left to bootloaders,
/// which commonly use it.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
const GPREGRET1: *mut u32 = 0x5010_E504 as *mut u32;
/// In [`GPREGRET1`]: the last watchdog reset was [`reset_chip`]'s.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
const DELIBERATE: u32 = 0x4d;

/// Reset the whole chip, as a watchdog timeout does: through WDT30, which nothing
/// else here uses, about half a millisecond from now.
///
/// Not `SCB::sys_reset`: a soft reset leaves the oscillators, the regulators and
/// the GRTC running as they were, and after one MPSL's initialisation asserts
/// (`MultiprotocolServiceLayer: 106:179`) about half the time when the radio was
/// busy. The reboot into an uploaded image is such a reset, so the image on trial
/// panicked at boot and was rolled back. A watchdog reset resets them as a pin
/// reset does. [`after_watchdog_reset`] tells this one from a real timeout.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
pub fn reset_chip() -> ! {
    // SAFETY: WDT30 is unused, and writing its registers only arms it; the marker
    // register is general purpose.
    unsafe {
        GPREGRET1.write_volatile(DELIBERATE);
        // CRV's minimum: 16 ticks of the 32.768 kHz clock.
        ((WDT30 + 0x504) as *mut u32).write_volatile(15);
        // RREN: one reload request, never written.
        ((WDT30 + 0x508) as *mut u32).write_volatile(1);
        // CONFIG: run while sleeping, pause while a probe halts the core.
        ((WDT30 + 0x50c) as *mut u32).write_volatile(1);
        // TASKS_START.
        (WDT30 as *mut u32).write_volatile(1);
    }
    loop {
        cortex_m::asm::nop();
    }
}

/// Whether the last reset was [`reset_chip`]'s, which the reset reasons show as a
/// watchdog's. Clears the mark, so ask once per boot, early.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
pub fn deliberate_reset() -> bool {
    // SAFETY: the marker register is read and cleared here alone.
    unsafe {
        let deliberate = GPREGRET1.read_volatile() & 0xff == DELIBERATE;
        GPREGRET1.write_volatile(0);
        deliberate
    }
}

/// Whether this boot follows a watchdog timeout, not counting [`reset_chip`]'s.
/// Clears the reset reasons, which accumulate across resets, so the next boot
/// reads its own, and [`deliberate_reset`]'s mark; call it once, early, instead of
/// both.
#[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
pub fn after_watchdog_reset() -> bool {
    // SAFETY: the reset reasons are read and cleared here alone.
    let reasons = unsafe {
        let reasons = RESETREAS.read_volatile();
        RESETREAS.write_volatile(reasons);
        reasons
    };
    let deliberate = deliberate_reset();
    info!("reset reasons {:#x}, deliberate {}", reasons, deliberate);
    // DOG0 and DOG1, bits 1 and 2.
    reasons & 0b110 != 0 && !deliberate
}

/// The WDT, unarmed until [`start`](super::Watchdog::start).
pub struct Watchdog {
    wdt: Option<Peri<'static, Wdt>>,
    handle: Option<WatchdogHandle>,
}

impl Watchdog {
    /// Take the WDT, starting it only if it already runs: a bootloader starts it for a
    /// trial boot, and a soft reset does not stop it. Then it is fed from here on.
    ///
    /// A bootloader that starts it must use [`config`]'s settings, or this cannot
    /// adopt it.
    pub fn new(wdt: Peri<'static, Wdt>) -> Self {
        let mut this = Self {
            wdt: Some(wdt),
            handle: None,
        };
        if this
            .wdt
            .as_ref()
            .is_some_and(|wdt| wdt::Config::try_new(wdt).is_some())
        {
            info!("watchdog: already running; feeding it");
            super::Watchdog::start(&mut this);
            // At once: the bootloader fed it last before its swap's final writes, and
            // the task that feeds it next runs only once bring-up first yields.
            super::Watchdog::feed(&mut this);
        }
        this
    }
}

/// The configuration [`Watchdog`] starts the WDT with.
pub fn config() -> wdt::Config {
    let mut config = wdt::Config::default();
    config.timeout_ticks = 32_768 * TIMEOUT.as_secs() as u32;
    config.action_during_sleep = SleepConfig::Run;
    // A probe halting the core would otherwise end the session in a reset.
    config.action_during_debug_halt = HaltConfig::Pause;
    config
}

impl super::Watchdog for Watchdog {
    fn start(&mut self) {
        let Some(wdt) = self.wdt.take() else {
            return;
        };
        match wdt::Watchdog::try_new::<_, 1>(wdt, config()) {
            Ok((_, [handle])) => self.handle = Some(handle),
            Err(_) => error!("watchdog: already running with another configuration"),
        }
    }

    fn feed(&mut self) {
        if let Some(handle) = &mut self.handle {
            // `pet` in embassy-nrf 0.11.0, `feed` in later git revisions, which the
            // nRF54L needs.
            #[cfg(feature = "nrf52840")]
            handle.pet();
            #[cfg(any(feature = "nrf54l15", feature = "nrf54lm20"))]
            handle.feed();
        }
    }
}
