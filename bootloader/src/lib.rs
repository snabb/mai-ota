// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) Embassy project contributors

//! embassy-boot bootloaders for [mai-ota](https://github.com/snabb/mai-ota): an
//! uploaded image is swapped into the active partition at the next boot, and swapped
//! back at the boot after that unless it marked itself booted.
//!
//! What this adds to embassy-boot's own is the watchdog. It is started only when the
//! state says swap, before the swap begins, with the settings
//! [`mai_ota::watchdog`] uses, so a trial image is covered from its first
//! instruction: a hang or a halted panic resets the chip, and the boot after that
//! reverts. An ordinary boot leaves the watchdog to the application, which defers it
//! after a watchdog reset.
//!
//! A binary calls its chip's `run` from its `#[entry]`; this crate supplies the
//! panic handler and the `HardFault` and `DefaultHandler` exceptions, each a reset.
//!
//! The binary's `memory.x` defines the partitions as embassy-boot reads them:
//! `__bootloader_{state,active,dfu}_{start,end}`, offsets into the flash as its driver
//! addresses it. Its `.bootloader_state` section fills the state partition with
//! erased bytes, so a wired flash of the bootloader cancels any pending swap; the
//! application's `memory.x` does the same, see the integration guide.
//!
//! Adapted from Embassy's `examples/boot/bootloader/{nrf,rp,rp235x}` and from
//! `embassy-boot-nrf`'s and `embassy-boot-rp`'s `WatchdogFlash` and
//! `BootLoader::load` (copyright Embassy project contributors, MIT OR Apache-2.0),
//! modified as above.

#![no_std]

#[cfg(feature = "nrf54lm20")]
pub mod nrf54lm20;
#[cfg(any(feature = "rp2040", feature = "rp235xa"))]
pub mod rp;

use core::cell::RefCell;

use embassy_boot::{BootLoader, BootLoaderConfig, State};
use embassy_sync::blocking_mutex::Mutex;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

/// A started watchdog.
trait Feed {
    fn feed(&mut self);
}

/// Swap if one is pending, with the watchdog `start_watchdog` gives fed throughout,
/// and return the active partition's offset. `buf` is an aligned page.
///
/// The watchdog is started only when the state says swap, before the swap begins.
/// `None` leaves the swap unwatched, as for a watchdog that already runs with
/// another configuration; a swap interrupted by that resumes at the next boot.
fn prepare<F: NorFlash, W: Feed>(
    flash: F,
    start_watchdog: impl FnOnce() -> Option<W>,
    buf: &mut [u8],
) -> u32 {
    let flash = Mutex::new(RefCell::new(WatchdogFlash {
        flash,
        watchdog: None,
    }));
    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active = config.active.offset();
    let mut boot = BootLoader::new(config);
    if boot.read_state(buf) == Ok(State::Swap) {
        let watchdog = start_watchdog();
        flash.lock(|flash| flash.borrow_mut().watchdog = watchdog);
    }
    // Not on an error: the active partition may hold part of each image, and that image
    // marking itself booted would end the swap there. Swap progress is kept in flash, so
    // the boot after the reset carries on where this one stopped.
    if boot.prepare_boot(buf).is_err() {
        cortex_m::peripheral::SCB::sys_reset();
    }
    // A full period for the application's bring-up, whatever the swap's last
    // operation was.
    flash.lock(|flash| flash.borrow_mut().feed());
    active
}

/// Jump to the image at `start`.
///
/// # Safety
///
/// `start` must hold a vector table whose reset vector is the image's entry point.
unsafe fn load(start: u32) -> ! {
    // SAFETY: the bootloader is done with the core.
    let p = unsafe { cortex_m::Peripherals::steal() };
    unsafe {
        p.SCB.vtor.write(start);
        cortex_m::asm::bootload(start as *const u32)
    }
}

/// The flash, feeding the watchdog on every operation once there is one: a swap takes
/// longer than its timeout.
struct WatchdogFlash<F, W> {
    flash: F,
    watchdog: Option<W>,
}

impl<F, W: Feed> WatchdogFlash<F, W> {
    fn feed(&mut self) {
        if let Some(watchdog) = &mut self.watchdog {
            watchdog.feed();
        }
    }
}

impl<F: ErrorType, W> ErrorType for WatchdogFlash<F, W> {
    type Error = F::Error;
}

impl<F: ReadNorFlash, W: Feed> ReadNorFlash for WatchdogFlash<F, W> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.feed();
        self.flash.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

impl<F: NorFlash, W: Feed> NorFlash for WatchdogFlash<F, W> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.feed();
        self.flash.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.feed();
        self.flash.write(offset, bytes)
    }
}

#[cortex_m_rt::exception]
unsafe fn HardFault(_: &cortex_m_rt::ExceptionFrame) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

#[cortex_m_rt::exception]
unsafe fn DefaultHandler(_: i16) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    cortex_m::peripheral::SCB::sys_reset();
}
