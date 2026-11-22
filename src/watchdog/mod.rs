// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! A hardware watchdog fed for as long as the executor runs.
//!
//! A panic that halts, or a hang, ends in a reset; on a trial image the bootloader
//! then goes back to the previous one. A boot that a watchdog reset caused leaves
//! it unarmed for [`DEFER_AFTER_RECOVERY`], so a fault that recurs straight after
//! boot halts after one extra reboot instead of looping. A lock-up after long
//! uptime is recovered every time.
//!
//! The feeder proves that the executor schedules tasks, not that every task is
//! healthy.
//!
//! Chip modules, each behind its feature: `esp` (`esp32`, `esp32c5`, `esp32c6`, `esp32h2`, `esp32s3`), `nrf`
//! (`nrf52840`, `nrf54l15`, `nrf54lm20`), `rp` (`rp2040`, `rp235xa`), `stm32` (`stm32`).

#[cfg(any(
    feature = "esp32",
    feature = "esp32c5",
    feature = "esp32c6",
    feature = "esp32h2",
    feature = "esp32s3"
))]
pub mod esp;
#[cfg(any(feature = "nrf52840", feature = "nrf54l15", feature = "nrf54lm20"))]
pub mod nrf;
#[cfg(any(feature = "rp2040", feature = "rp235xa"))]
pub mod rp;
#[cfg(feature = "stm32")]
pub mod stm32;

use embassy_time::{Duration, Timer};

use crate::{Event, OnEvent};

/// What a chip's watchdog has to offer the feeder.
pub trait Watchdog {
    /// Start the hardware timer with [`TIMEOUT`]. Called once, except by a board that
    /// also starts it before handing it over, to cover its bring-up (the ESP boards, on
    /// a trial boot); that board's watchdog must keep running through the second call.
    fn start(&mut self);
    /// Restart the countdown. Also called before [`start`](Self::start), which matters only
    /// for a watchdog that was already running at boot.
    fn feed(&mut self);
}

/// Time without a feed before the hardware resets the chip. Within the RP2040's limit of
/// about 8.3 s.
pub const TIMEOUT: Duration = Duration::from_secs(8);

const FEED_EVERY: Duration = Duration::from_secs(1);

/// How long a boot caused by the watchdog runs without one.
pub const DEFER_AFTER_RECOVERY: Duration = Duration::from_secs(600);

/// When the `watchdog-test-panic` feature panics, counted from the start of [`run`].
#[cfg(feature = "watchdog-test-panic")]
const TEST_PANIC_AFTER: Duration = Duration::from_secs(60);

/// Arm `watchdog` and feed it forever. `after_watchdog_reset` says a watchdog
/// timeout caused this boot, which is reported to `events`.
pub async fn run(watchdog: impl Watchdog, after_watchdog_reset: bool, events: OnEvent) -> ! {
    #[cfg(feature = "watchdog-test-panic")]
    {
        let panic = async {
            Timer::after(TEST_PANIC_AFTER).await;
            panic!("watchdog-test-panic");
        };
        match embassy_futures::select::select(feed(watchdog, after_watchdog_reset, events), panic)
            .await {}
    }
    #[cfg(not(feature = "watchdog-test-panic"))]
    feed(watchdog, after_watchdog_reset, events).await
}

async fn feed(mut watchdog: impl Watchdog, after_watchdog_reset: bool, events: OnEvent) -> ! {
    if after_watchdog_reset {
        warn!(
            "watchdog: this boot followed a watchdog reset; arming in {} s",
            DEFER_AFTER_RECOVERY.as_secs()
        );
        events(Event::WatchdogRecovered);
        // Still fed: a bootloader that went back to this image after a failed trial
        // leaves its watchdog running, and that one cannot be stopped.
        let armed = embassy_time::Instant::now() + DEFER_AFTER_RECOVERY;
        while embassy_time::Instant::now() < armed {
            feed_in_time(&mut watchdog).await;
        }
    }
    watchdog.start();
    info!("watchdog: armed, {} s timeout", TIMEOUT.as_secs());
    loop {
        feed_in_time(&mut watchdog).await;
    }
}

/// Feed, then wait for the next feed. A boot that is [`overdue`](crate::overdue),
/// a trial past its deadline or an activated upload whose reboot did not come, is
/// fed no more: whatever kept [`supervise`](crate::supervise) from rebooting it, the
/// watchdog then does.
async fn feed_in_time(watchdog: &mut impl Watchdog) {
    if crate::overdue() {
        warn!("watchdog: past the trial's deadline or an upload's reboot; no longer fed");
        core::future::pending::<()>().await;
    }
    watchdog.feed();
    Timer::after(FEED_EVERY).await;
}
