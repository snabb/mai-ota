// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! A/B firmware updates for `no_std` embedded Rust, where a new image boots on
//! trial and is rolled back unless it proves itself.
//!
//! An upload goes into the slot the running image did not boot from ([`Slot`]),
//! and is read back as it is written. Before it is activated, the image must name
//! this board ([`image`]). The next boot runs it on trial. The first request that
//! passes the firmware's authentication confirms it ([`prove`]): that proves the
//! network, the server and the keys, which is everything the next update needs.
//! Without one within [`Boot::trial_timeout`] the node reboots, and the bootloader goes
//! back to the previous image. So does a hang or a panic, through the [`watchdog`].
//! Uploads are refused during a trial, because the other slot holds the fallback.
//!
//! What is here:
//!
//! * [`Upload`]: writes, reads back, checks and activates an image, whatever the
//!   transport. Module `coap` (feature `coap`) carries it over CoAP Block1 with
//!   [mai-coap](https://github.com/snabb/mai-coap), confirming on OSCORE, and
//!   reports the running image's version to a `GET`.
//! * [`supervise`]: confirms or rolls back the trial, then reboots after an upload.
//! * Slots: module `embassy_boot` (feature `embassy-boot`) behind an embassy-boot
//!   bootloader, and module `esp` (features `esp32`, `esp32c5`, `esp32c6`, `esp32h2`, `esp32s3`)
//!   behind ESP-IDF's.
//! * [`watchdog`]: a hardware watchdog fed while the executor runs, which turns a
//!   halt or a hang on trial into a rollback, with chip modules behind features.
//!
//! Logging goes to `log` or `defmt`, whichever feature is enabled.

#![no_std]
#![deny(missing_docs)]
#![allow(async_fn_in_trait)]

#[macro_use]
mod fmt;

#[cfg(feature = "coap")]
pub mod coap;
#[cfg(feature = "embassy-boot")]
pub mod embassy_boot;
#[cfg(any(
    feature = "esp32",
    feature = "esp32c5",
    feature = "esp32c6",
    feature = "esp32h2",
    feature = "esp32s3"
))]
pub mod esp;
pub mod image;
pub mod otadata;
#[cfg(test)]
mod test_clock;
mod upload;
pub mod watchdog;

pub use upload::{Refusal, Upload, Uploader};

use core::cell::Cell;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer};

/// How long a trial boot waits for an authenticated request, unless the firmware
/// sets [`Boot::trial_timeout`] otherwise.
pub const TRIAL_TIMEOUT: Duration = Duration::from_secs(600);

/// Between [`Upload::commit`] and the reboot: room for the final reply to
/// arrive, or for the client's retransmission to fetch it again.
pub const REBOOT_DELAY: Duration = Duration::from_secs(2);

/// Something worth recording, for a firmware that keeps a log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Event {
    /// This boot runs a new image on trial.
    Trial,
    /// The image on trial was confirmed.
    Confirmed,
    /// The bootloader rejected an uploaded image and went back to this one.
    RolledBack,
    /// An upload was refused or failed.
    Refused,
    /// This boot followed a watchdog reset.
    WatchdogRecovered,
}

/// Where [`Event`]s go: a plain function, so that it can reach a log in a static.
pub type OnEvent = fn(Event);

/// Ignores every [`Event`].
pub fn no_events(_: Event) {}

/// The inactive application slot, and how to boot what is written there.
pub trait Slot {
    /// Erase granularity in bytes.
    const ERASE: u32;
    /// Write granularity in bytes; writes start and end on it.
    const WRITE: u32;
    /// Bytes at the start of an image that [`check`](Self::check) needs.
    const PREFIX: usize;

    /// Bytes the slot holds.
    fn capacity(&self) -> u32;
    /// Erase `from..to`, both multiples of [`ERASE`](Self::ERASE).
    async fn erase(&mut self, from: u32, to: u32) -> Result<(), ()>;
    /// Write at `offset`, a multiple of [`WRITE`](Self::WRITE), as is `data.len()`.
    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), ()>;
    /// Read back what was written.
    async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ()>;
    /// Boot the slot's first `len` bytes next, on trial.
    async fn activate(&mut self, len: u32) -> Result<(), ()>;
    /// Whether the image starting with `prefix` is one this board can run.
    fn check(&self, prefix: &[u8], board: &str) -> Result<(), &'static str>;
    /// Whether the `len` bytes received, already [`check`](Self::check)ed, hold a
    /// whole image: what its headers point at lies within them. [`Refusal::Image`]
    /// when they do not, [`Refusal::Flash`] when it cannot be read back.
    async fn verify(&mut self, len: u32) -> Result<(), Refusal>;
    /// Mark the running image good, so the bootloader keeps it. Logs a failure;
    /// the trial then stays in force until it times out and rolls back, which is
    /// the safe direction.
    ///
    /// A future of its own because the slot itself moves to the upload.
    fn confirm(&self) -> impl Future<Output = Result<(), ()>> + 'static;
}

/// No slot: names the type for a firmware that is built with uploads but takes none.
pub enum NoSlot {}

impl Slot for NoSlot {
    const ERASE: u32 = 1;
    const WRITE: u32 = 1;
    const PREFIX: usize = 0;

    fn capacity(&self) -> u32 {
        match *self {}
    }
    async fn erase(&mut self, _: u32, _: u32) -> Result<(), ()> {
        match *self {}
    }
    async fn write(&mut self, _: u32, _: &[u8]) -> Result<(), ()> {
        match *self {}
    }
    async fn read(&mut self, _: u32, _: &mut [u8]) -> Result<(), ()> {
        match *self {}
    }
    async fn activate(&mut self, _: u32) -> Result<(), ()> {
        match *self {}
    }
    fn check(&self, _: &[u8], _: &str) -> Result<(), &'static str> {
        match *self {}
    }
    async fn verify(&mut self, _: u32) -> Result<(), Refusal> {
        match *self {}
    }
    fn confirm(&self) -> impl Future<Output = Result<(), ()>> + 'static {
        core::future::ready(Ok(()))
    }
}

/// What the boot already decided, from the slot's bootloader.
#[derive(Clone, Copy)]
pub struct Boot {
    /// The running image is on trial and must be confirmed.
    pub trial: bool,
    /// The bootloader rejected an uploaded image and went back to this one.
    pub rolled_back: bool,
    /// How long a trial waits for its proof before it reboots into the previous
    /// image; [`TRIAL_TIMEOUT`] unless the firmware wants another.
    pub trial_timeout: Duration,
    /// A clean reset, which the bootloader treats as the next boot.
    pub reboot: fn() -> !,
}

/// Everything an upload needs: one per firmware.
pub struct Ota<S> {
    /// Where an upload goes.
    pub slot: S,
    /// This board's name, which an image must carry.
    pub board: &'static str,
    /// What the running image calls itself, reported by the transport (`GET` on
    /// the upload path, for CoAP) so a client can tell whether the new image or
    /// the old one came back.
    pub version: &'static str,
    /// The trial state and the reboot.
    pub boot: Boot,
    /// Where trials, rollbacks and refused uploads are reported.
    pub events: OnEvent,
}

/// Past an activated upload's [`REBOOT_DELAY`], how long the [`watchdog`] feeder
/// waits for [`supervise`] to reboot before it stops feeding.
const REBOOT_GRACE: Duration = Duration::from_secs(10);

/// What this boot may still do, for every task to see. The slot's backend records
/// a trial as it reads the boot state, before any transport or task runs, so that
/// nothing can start an upload into the fallback in between. Behind a critical
/// section rather than in atomics, which thumbv6m has no read-modify-write for.
#[derive(Clone, Copy)]
struct BootState {
    /// When the trial began, while the running image is on one.
    trial_since: Option<Instant>,
    /// How long the trial may last: [`TRIAL_TIMEOUT`] until [`supervise`] sets the
    /// firmware's own.
    trial_timeout: Duration,
    /// The trial was confirmed: this boot is not a trial again, whoever asks.
    confirmed: bool,
    /// When an upload was activated: the other slot holds what boots next.
    activated_at: Option<Instant>,
}

const FRESH: BootState = BootState {
    trial_since: None,
    trial_timeout: TRIAL_TIMEOUT,
    confirmed: false,
    activated_at: None,
};

static BOOT_STATE: Mutex<CriticalSectionRawMutex, Cell<BootState>> = Mutex::new(Cell::new(FRESH));

static PROOF: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static REBOOT: Signal<CriticalSectionRawMutex, ()> = Signal::new();

fn boot_state() -> BootState {
    BOOT_STATE.lock(Cell::get)
}

fn update(f: impl FnOnce(&mut BootState)) {
    BOOT_STATE.lock(|cell| {
        let mut state = cell.get();
        f(&mut state);
        cell.set(state);
    });
}

/// Whether the running image is still on trial.
pub fn on_trial() -> bool {
    boot_state().trial_since.is_some()
}

/// When the trial ends unconfirmed, while there is one. Counted from when the
/// trial was found, not from whoever asks, so a supervisor started late or
/// restarted keeps it.
pub fn trial_deadline() -> Option<Instant> {
    let state = boot_state();
    state.trial_since.map(|since| since + state.trial_timeout)
}

/// Whether this boot has run past what it may: a trial past its deadline, or an
/// activated upload whose reboot is overdue. The [`watchdog`] feeder stops feeding
/// then, so the chip resets whatever kept [`supervise`] from rebooting it.
pub fn overdue() -> bool {
    let now = Instant::now();
    trial_deadline().is_some_and(|deadline| now >= deadline)
        || boot_state()
            .activated_at
            .is_some_and(|at| now >= at + REBOOT_DELAY + REBOOT_GRACE)
}

/// Mark this boot a trial, its deadline [`TRIAL_TIMEOUT`] from now until
/// [`supervise`] sets the firmware's own. A slot's backend calls it as it finds
/// the trial; a firmware with a backend of its own calls it as early as it can.
/// Once the trial is confirmed, it does nothing.
pub fn start_trial() {
    update(|state| {
        if state.trial_since.is_none() && !state.confirmed {
            state.trial_since = Some(Instant::now());
        }
    });
}

/// Whether an upload was activated this boot: the other slot holds what boots next.
fn activated() -> bool {
    boot_state().activated_at.is_some()
}

/// Take a request that passed authentication as proof the running image works.
/// Does nothing unless the image is on trial. For a transport's security policy.
pub fn prove() {
    if on_trial() {
        PROOF.signal(());
    }
}

/// Reboot into the activated image after [`REBOOT_DELAY`]; from [`Upload::commit`].
fn reboot_soon() {
    update(|state| {
        state.activated_at.get_or_insert_with(Instant::now);
    });
    REBOOT.signal(());
}

/// Confirm or roll back a trial boot, then reboot after an upload. Run it beside
/// the transport for as long as the firmware runs: it is what reboots on time. The
/// [`watchdog`] feeder enforces the same deadline should this stop running.
///
/// `confirm` is [`Slot::confirm`]'s future, taken before the slot moved to the
/// [`Upload`].
pub async fn supervise(
    boot: Boot,
    confirm: impl Future<Output = Result<(), ()>>,
    events: OnEvent,
) -> ! {
    if boot.trial {
        start_trial();
    }
    update(|state| state.trial_timeout = boot.trial_timeout);
    if boot.rolled_back {
        warn!("ota: the uploaded image was rejected; running the previous one");
        events(Event::RolledBack);
    }
    if let Some(deadline) = trial_deadline() {
        info!(
            "ota: trial boot; an authenticated request within {} s keeps this image",
            deadline.saturating_duration_since(Instant::now()).as_secs()
        );
        events(Event::Trial);
        // A proof taken at or after the deadline counts for nothing, even when it is
        // ready on the poll that ends the wait: embassy-time's `Timer` yields once
        // before it completes, so the other side of a `select` wins such a tie.
        let proved = select(Timer::at(deadline), PROOF.wait()).await;
        if matches!(proved, Either::First(())) || Instant::now() >= deadline {
            warn!("ota: no authenticated request; rebooting into the previous image");
            (boot.reboot)()
        }
        // Bounded too: the write may wait on something else, such as a radio
        // stack's flash arbitration, and the watchdog proves only that the
        // executor runs. A confirmation that starts writing before the deadline
        // and finishes after it still counts: it cannot be taken back.
        match select(Timer::at(deadline), confirm).await {
            Either::Second(Ok(())) => {}
            Either::First(()) | Either::Second(Err(())) => {
                // The bootloader still holds the previous image as the fallback, so
                // the trial, and the refusal of uploads into its slot, stand until
                // the deadline.
                Timer::at(deadline).await;
                warn!("ota: the image was never confirmed; rebooting into the previous image");
                (boot.reboot)()
            }
        }
        update(|state| {
            state.trial_since = None;
            state.confirmed = true;
        });
        info!("ota: image confirmed");
        events(Event::Confirmed);
    }
    // An upload activated before this supervisor ran, under one that was dropped,
    // has already taken the signal; the time it was activated is kept.
    let activated_at = match boot_state().activated_at {
        Some(at) => at,
        None => {
            REBOOT.wait().await;
            boot_state().activated_at.unwrap_or_else(Instant::now)
        }
    };
    info!("ota: rebooting into the new image");
    Timer::at(activated_at + REBOOT_DELAY).await;
    (boot.reboot)()
}

/// Clear what one test leaves in the shared boot state: the trial and the
/// activation, a commit's reboot request and a stray proof.
#[cfg(test)]
fn test_reset() {
    update(|state| *state = FRESH);
    PROOF.reset();
    REBOOT.reset();
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::panic::{AssertUnwindSafe, catch_unwind};

    use embassy_futures::select::select;

    use super::*;
    use crate::test_clock::{WindClock, exclusive};

    fn reboot() -> ! {
        panic!("reboot")
    }

    fn trial_boot() -> Boot {
        Boot {
            trial: true,
            rolled_back: false,
            trial_timeout: TRIAL_TIMEOUT,
            reboot,
        }
    }

    /// Run `supervise` for up to 1000 s of mock time. When it rebooted, if it did,
    /// and whether the trial still stood.
    fn supervised(
        boot: Boot,
        confirm: impl Future<Output = Result<(), ()>>,
    ) -> (Option<Instant>, bool) {
        let run = catch_unwind(AssertUnwindSafe(|| {
            embassy_futures::block_on(select(
                supervise(boot, confirm, no_events),
                WindClock(100, Duration::from_secs(10)),
            ))
        }));
        let rebooted = run.err().map(|payload| {
            assert_eq!(payload.downcast_ref::<&str>(), Some(&"reboot"));
            Instant::now()
        });
        let trial = on_trial();
        test_reset();
        (rebooted, trial)
    }

    /// A trial whose proof has arrived, confirming with `confirm`.
    fn trial(confirm: impl Future<Output = Result<(), ()>>) -> (Option<Instant>, bool) {
        test_reset();
        start_trial();
        prove();
        supervised(trial_boot(), confirm)
    }

    /// `at` is the deadline counted from when the trial started, `start` seconds
    /// into the mock clock.
    fn at_the_deadline(at: Option<Instant>, start: u64) -> bool {
        let deadline = Instant::from_secs(start + TRIAL_TIMEOUT.as_secs());
        at.is_some_and(|at| at >= deadline && at <= deadline + Duration::from_secs(10))
    }

    #[test]
    fn a_confirmation_that_never_finishes_rolls_back_at_the_deadline() {
        let _clock = exclusive();
        let (rebooted, trial) = trial(core::future::pending());
        assert!(at_the_deadline(rebooted, 0), "{rebooted:?}");
        assert!(trial);
    }

    #[test]
    fn a_failed_confirmation_keeps_the_trial_until_the_deadline() {
        let _clock = exclusive();
        let (rebooted, trial) = trial(core::future::ready(Err(())));
        assert!(at_the_deadline(rebooted, 0), "{rebooted:?}");
        assert!(trial);
    }

    #[test]
    fn a_confirmed_image_ends_the_trial() {
        let _clock = exclusive();
        assert_eq!(trial(core::future::ready(Ok(()))), (None, false));
    }

    #[test]
    fn no_proof_rolls_back_at_the_deadline() {
        let _clock = exclusive();
        test_reset();
        let (rebooted, _) = supervised(trial_boot(), core::future::ready(Ok(())));
        assert!(at_the_deadline(rebooted, 0), "{rebooted:?}");
    }

    #[test]
    fn the_deadline_counts_from_the_boot_not_from_the_supervisor() {
        let _clock = exclusive();
        test_reset();
        // The backend found the trial at boot; the supervisor starts 300 s later,
        // as after a bring-up that took long or a transport that was restarted.
        start_trial();
        crate::test_clock::advance(Duration::from_secs(300));
        let (rebooted, _) = supervised(trial_boot(), core::future::ready(Ok(())));
        assert!(at_the_deadline(rebooted, 0), "{rebooted:?}");
    }

    #[test]
    fn a_trial_is_overdue_for_the_watchdog_at_its_deadline() {
        let _clock = exclusive();
        test_reset();
        assert!(!overdue());
        start_trial();
        crate::test_clock::advance(TRIAL_TIMEOUT - Duration::from_secs(1));
        assert!(!overdue());
        crate::test_clock::advance(Duration::from_secs(1));
        assert!(overdue());
        test_reset();
    }

    #[test]
    fn an_activation_whose_reboot_does_not_come_is_overdue() {
        let _clock = exclusive();
        test_reset();
        reboot_soon();
        crate::test_clock::advance(REBOOT_DELAY + REBOOT_GRACE - Duration::from_secs(1));
        assert!(!overdue());
        crate::test_clock::advance(Duration::from_secs(1));
        assert!(overdue());
        test_reset();
    }

    #[test]
    fn a_supervisor_started_after_an_activation_still_reboots() {
        let _clock = exclusive();
        test_reset();
        reboot_soon();
        // A supervisor that was dropped took the signal.
        REBOOT.reset();
        let ordinary = Boot {
            trial: false,
            ..trial_boot()
        };
        let (rebooted, _) = supervised(ordinary, core::future::ready(Ok(())));
        let due = Instant::from_secs(REBOOT_DELAY.as_secs());
        assert!(
            rebooted.is_some_and(|at| at >= due && at <= due + Duration::from_secs(10)),
            "{rebooted:?}"
        );
    }

    #[test]
    fn a_confirmed_trial_stays_confirmed_under_a_new_supervisor() {
        let _clock = exclusive();
        test_reset();
        start_trial();
        prove();
        // Each run ends when the clock winder does: confirmed, it waits for an upload.
        for _ in 0..2 {
            embassy_futures::block_on(select(
                supervise(trial_boot(), core::future::ready(Ok(())), no_events),
                WindClock(5, Duration::from_secs(1)),
            ));
            assert!(!on_trial());
        }
        test_reset();
    }

    #[test]
    fn a_proof_taken_after_the_deadline_does_not_confirm() {
        let _clock = exclusive();
        test_reset();
        start_trial();
        crate::test_clock::advance(TRIAL_TIMEOUT);
        prove();
        let (rebooted, trial) = supervised(trial_boot(), core::future::ready(Ok(())));
        assert!(rebooted.is_some());
        assert!(trial);
    }
}
