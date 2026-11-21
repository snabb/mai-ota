// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The mock clock, one test at a time.
//!
//! `MockDriver` is one global, and Cargo runs tests as threads of one process: a
//! `reset()` in one test moves time backwards under another, and an `advance()`
//! in one counts as elapsed time in another.

extern crate std;

use core::pin::Pin;
use core::task::{Context, Poll};

use embassy_time::{Duration, MockDriver};

/// Winds the mock clock by `.1` on each of `.0` polls, then completes, so a loop
/// waiting out a timer reaches it without the test sleeping.
///
/// `block_on` busy-polls, which is what makes this work: each poll moves time
/// forward and asks to be polled again.
pub(crate) struct WindClock(pub u32, pub Duration);

impl Future for WindClock {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 == 0 {
            return Poll::Ready(());
        }
        self.0 -= 1;
        MockDriver::get().advance(self.1);
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Move the mock clock forward by `by`.
pub(crate) fn advance(by: Duration) {
    MockDriver::get().advance(by);
}

static CLOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the clock for the duration of a test, and start it from zero.
///
/// Poison-tolerant: a test that panicked while holding it has already failed.
pub(crate) fn exclusive() -> std::sync::MutexGuard<'static, ()> {
    let guard = CLOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    MockDriver::get().reset();
    guard
}
