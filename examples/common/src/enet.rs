// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) the esp-rs contributors

//! OpenThread's IPv6 packets to and from embassy-net (xarxa), over its driver
//! channel.
//!
//! Adapted from `openthread` 0.4's `enet` module (copyright the esp-rs
//! contributors, MIT OR Apache-2.0), and modified: that targets the released
//! `embassy-net-driver-channel`, where the runner fills a slot in the channel;
//! the one from Git lends it a buffer from the stack's packet pool instead.

use defmt_or_log::warn;
use embassy_futures::select::{Either3, select3};
use embassy_net_driver_channel as ch;
use embassy_net_driver_channel::driver::HardwareAddress;
use openthread::{OpenThread, Radio};

pub use ch::{Device as Driver, State as DriverState, StateRunner};

/// Moves packets between OpenThread and the stack, and runs OpenThread.
pub struct Runner {
    ot: OpenThread<'static>,
    rx: ch::RxRunner<'static>,
    tx: ch::TxRunner<'static>,
}

/// The channel's three ends for OpenThread `ot`: bare IPv6 frames, no hardware
/// address, packets up to `mtu` bytes.
pub fn new<const N_RX: usize, const N_TX: usize>(
    ot: OpenThread<'static>,
    state: &'static mut ch::State<N_RX, N_TX>,
    mtu: usize,
) -> (StateRunner<'static>, Runner, Driver<'static>) {
    let (runner, device) = ch::new(state, HardwareAddress::Ip, mtu);
    let (state, rx, tx) = runner.split();
    (state, Runner { ot, rx, tx }, device)
}

impl Runner {
    /// Run OpenThread on `radio` and move packets both ways.
    pub async fn run<R: Radio>(&mut self, radio: R) -> ! {
        let Self { ot, rx, tx } = self;
        ot.enable_ipv6_rx(true);

        let receive = async {
            loop {
                if let Err(error) = ot.wait_rx_available().await {
                    warn!("thread: rx wait failed: {:?}", error);
                    continue;
                }
                let mut buf = rx.rx_buf().await;
                buf.set_len(buf.capacity());
                match ot.rx(&mut buf).await {
                    Ok(len) => {
                        buf.set_len(len);
                        rx.rx(buf).await;
                    }
                    // Dropping the buffer returns it to the pool.
                    Err(error) => warn!("thread: rx failed: {:?}", error),
                }
            }
        };
        let transmit = async {
            loop {
                let buf = tx.tx().await;
                if let Err(error) = ot.tx(&buf) {
                    warn!("thread: tx failed: {:?}", error);
                }
            }
        };

        match select3(receive, transmit, ot.run(radio)).await {
            Either3::First(never) | Either3::Second(never) | Either3::Third(never) => never,
        }
    }
}
