// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The STM32's independent watchdog, for [`watchdog`](super). Once started it runs
//! until the next reset.

use embassy_stm32::Peri;
use embassy_stm32::peripherals::IWDG;
use embassy_stm32::wdg::IndependentWatchdog;

use super::TIMEOUT;

/// The IWDG, configured but not started.
pub struct Watchdog(IndependentWatchdog<'static, IWDG>);

impl Watchdog {
    /// Configure the IWDG for [`TIMEOUT`], without starting it.
    pub fn new(iwdg: Peri<'static, IWDG>) -> Self {
        Self(IndependentWatchdog::new(iwdg, TIMEOUT.as_micros() as u32))
    }
}

impl super::Watchdog for Watchdog {
    fn start(&mut self) {
        self.0.unleash();
    }

    fn feed(&mut self) {
        self.0.pet();
    }
}
