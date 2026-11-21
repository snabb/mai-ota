// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! One endpoint, EDHOC and OSCORE with the device, for as long as some work runs.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use futures_lite::future::{block_on, race};
use mai_coap::{
    codec::Code,
    crypto::SoftwareCrypto,
    edhoc::endpoint::Edhoc,
    endpoint::{Client, DefaultEndpoint, DefaultShared, Request, ResponseWriter, Target},
    security::ProtectedOnly,
    time::StdClock,
    transport::linux::StdTransport,
};

use crate::Failure;
use crate::credentials::{DEVICE, Files};

/// Requests to the device, protected with OSCORE.
pub type Device<'a> = Client<'a, { mai_coap::DEFAULT_DATAGRAM }, { mai_coap::DEFAULT_OUTBOUND }>;

/// Run `work` with requests to `remote`, on a new endpoint that drives itself
/// meanwhile. A fresh endpoint runs EDHOC again, which a rebooted device needs.
pub fn run<T>(
    files: &Files<'_>,
    remote: SocketAddr,
    work: impl AsyncFnOnce(Device<'_>) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let mut crypto = SoftwareCrypto::new(rand::rng());
    let (identity, peer) = files.load(&mut crypto)?;
    let mut protection = Edhoc::<_>::new(crypto, identity)?;
    protection.trust(peer)?;
    // This endpoint only initiates; it has no resources to offer the device.
    protection.set_responder_limit(0)?;

    let shared = DefaultShared::new();
    let mut endpoint = DefaultEndpoint::new(&shared, protection)?;
    let bind = SocketAddr::new(
        if remote.is_ipv4() {
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        } else {
            IpAddr::V6(Ipv6Addr::UNSPECIFIED)
        },
        0,
    );
    let mut socket =
        // A network failure, so that a host without IPv6 moves on to the IPv4 address.
        StdTransport::bind(bind).map_err(|e| Failure::network(format!("binding {bind}: {e}")))?;
    let clock = StdClock::default();
    let mut application = |_: Request<'_>, _: &mut ResponseWriter<'_>| Err(Code::NOT_FOUND);
    let driver = endpoint.run(&mut socket, &clock, &mut application, &ProtectedOnly);
    let mut target = Target::new(remote);
    target.peer = Some(DEVICE);
    let device = shared.handle().client(target);
    block_on(race(
        async {
            match driver.await {
                Ok(()) => Err(Failure::network("the endpoint driver stopped")),
                Err(error) => Err(Failure::network(error.to_string())),
            }
        },
        work(device),
    ))
}
