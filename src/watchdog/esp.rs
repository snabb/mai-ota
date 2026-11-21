// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The ESP32's RTC watchdog, for [`watchdog`](super).
//!
//! The RWDT rather than a timer group's, because its stage 0 resets the whole system, RTC
//! domain included.
//!
//! `esp_hal::init` stops every watchdog, the bootloader's too, and whether this boot
//! is a trial is known only once [`esp::ota`](crate::esp::ota) has read the flash,
//! which can fail or hang too. So [`start`](super::Watchdog::start) it straight after
//! `init`, on every boot; then, once `ota` has answered, [`stop`](Watchdog::stop) it
//! again unless this is a trial, leaving an ordinary boot's to [`run`](super::run).
//! A trial's keeps running through bring-up, so a bring-up that fails or hangs also
//! ends in a reset and a rollback; `run` starting it again leaves it running.

use esp_hal::rtc_cntl::{Rtc, Rwdt, RwdtStage};

use super::TIMEOUT;

/// The RWDT, unarmed until [`start`](super::Watchdog::start).
pub struct Watchdog(Rwdt);

impl Watchdog {
    /// Take the RTC timer, whose driver owns the RWDT.
    pub fn new(rtc_timer: esp_hal::peripherals::RTC_TIMER<'static>) -> Self {
        Self(Rtc::new(rtc_timer).rwdt)
    }

    /// Disarm it: an ordinary boot's, started early to cover reading the flash.
    pub fn stop(&mut self) {
        self.0.disable();
    }
}

impl super::Watchdog for Watchdog {
    fn start(&mut self) {
        self.0.set_timeout(
            RwdtStage::Stage0,
            esp_hal::time::Duration::from_millis(TIMEOUT.as_millis()),
        );
        // Stage 0 resets the system; the other stages are off.
        self.0.enable();
    }

    fn feed(&mut self) {
        self.0.feed();
    }
}
