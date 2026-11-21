// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) the Embassy contributors (original embassy-sync fmt.rs)

//! Logging macros that go to `log`, to `defmt`, or nowhere.
//!
//! Adapted from `embassy-sync`'s `fmt.rs` (copyright the Embassy contributors,
//! MIT OR Apache-2.0), trimmed to the logging macros and a `Debug` adapter.

// `error!` and `Dbg` are used only by the feature-gated backends.
#![allow(unused_macros, dead_code)]

use core::fmt::Debug;

#[cfg(all(feature = "defmt", feature = "log"))]
compile_error!("You may not enable both `defmt` and `log` features.");

macro_rules! info {
    ($s:literal $(, $x:expr)* $(,)?) => {
        {
            #[cfg(feature = "log")]
            ::log::info!($s $(, $x)*);
            #[cfg(feature = "defmt")]
            ::defmt::info!($s $(, $x)*);
            #[cfg(not(any(feature = "log", feature = "defmt")))]
            let _ = ($( & $x ),*);
        }
    };
}

macro_rules! warn {
    ($s:literal $(, $x:expr)* $(,)?) => {
        {
            #[cfg(feature = "log")]
            ::log::warn!($s $(, $x)*);
            #[cfg(feature = "defmt")]
            ::defmt::warn!($s $(, $x)*);
            #[cfg(not(any(feature = "log", feature = "defmt")))]
            let _ = ($( & $x ),*);
        }
    };
}

macro_rules! error {
    ($s:literal $(, $x:expr)* $(,)?) => {
        {
            #[cfg(feature = "log")]
            ::log::error!($s $(, $x)*);
            #[cfg(feature = "defmt")]
            ::defmt::error!($s $(, $x)*);
            #[cfg(not(any(feature = "log", feature = "defmt")))]
            let _ = ($( & $x ),*);
        }
    };
}

/// Formats a `Debug` value under either framework.
pub(crate) struct Dbg<T>(pub T);

impl<T: Debug> Debug for Dbg<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(feature = "defmt")]
impl<T: Debug> defmt::Format for Dbg<T> {
    fn format(&self, fmt: defmt::Formatter) {
        defmt::write!(fmt, "{}", defmt::Debug2Format(&self.0))
    }
}
