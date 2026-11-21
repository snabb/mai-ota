// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! What every example shares: a CoAP server that serves `/fw` under EDHOC and
//! OSCORE, and nothing else.
//!
//! A board brings up its network, opens a UDP socket on port 5683 as a mai-coap
//! transport, and calls [`serve`] with its [`Ota`]. The credentials are the files
//! `mai-coap --generate-identity` writes, built into the image; see the examples'
//! README.

#![no_std]

use mai_coap::InlineStorage;
use mai_coap::codec::Code;
use mai_coap::crypto::SoftwareCrypto;
use mai_coap::edhoc::endpoint::Edhoc;
use mai_coap::edhoc::{Identity, TrustedPeer, TrustedPeers};
use mai_coap::endpoint::{Endpoint, Shared, role};
use mai_coap::security::{PeerId, ProtectedOnly};
use mai_coap::server::Router;
use mai_coap::time::EmbassyClock;
use mai_coap::transport::Transport;
use mai_ota::coap::{Proving, Queue, Resource};
use mai_ota::{Ota, Slot};

pub use chacha20::ChaCha20Rng;

/// Largest datagram either way: a 1024-byte block with OSCORE's option and tag.
pub const DATAGRAM: usize = 1152;
/// Uploads in flight, each answering its blocks as deferred replies.
const UPLOADS: usize = 2;

type ExampleShared = Shared<DATAGRAM, UPLOADS>;
/// No observers; a two-entry reply cache and send queue, sharing two datagram
/// buffers; the rest sized for the uploads.
type ExampleEndpoint<'a, P> = Endpoint<
    'a,
    P,
    DATAGRAM,
    UPLOADS,
    8,
    0,
    2,
    2,
    UPLOADS,
    UPLOADS,
    UPLOADS,
    2,
    InlineStorage<DATAGRAM, 2>,
    role::Server,
>;
/// Two OSCORE contexts and two handshakes, one trusted client.
type Protection<R> = Edhoc<SoftwareCrypto<R>, 2, 2, TrustedPeers<1>, 0>;

/// Nothing unprotected; any protected request proves a trial image.
static PROVING: Proving<ProtectedOnly> = Proving(&ProtectedOnly);

/// The COSE_Key `{1: 2, -1: 1, -4: d}` up to `d`, as `mai-coap --generate-identity`
/// and aiocoap-keygen write it.
const COSE_KEY_HEADER: [u8; 8] = [0xa3, 0x01, 0x02, 0x20, 0x01, 0x23, 0x58, 0x20];

/// The device's EDHOC identity and the one client it trusts.
pub struct Credentials {
    /// The device's credential (CCS), `device.ccs`.
    pub device_ccs: &'static [u8],
    /// The device's private key, `device.cosekey`.
    pub device_key: &'static [u8],
    /// The client's credential (CCS), `client.ccs`.
    pub client_ccs: &'static [u8],
}

/// Serve `/fw` over `transport` forever.
///
/// `rng` must be cryptographically secure: EDHOC's ephemeral keys come from it.
///
/// # Panics
///
/// When the credentials are not what `mai-coap --generate-identity` writes.
pub async fn serve<T: Transport, S: Slot, R: rand_core::CryptoRng>(
    transport: &mut T,
    credentials: &Credentials,
    rng: R,
    ota: Ota<S>,
) -> ! {
    let mut crypto = SoftwareCrypto::new(rng);
    let private: [u8; 32] = credentials
        .device_key
        .strip_prefix(&COSE_KEY_HEADER)
        .and_then(|d| d.try_into().ok())
        .expect("device.cosekey is a P-256 COSE_Key");
    let identity = Identity::new(credentials.device_ccs, private, &mut crypto)
        .expect("device.ccs matches device.cosekey");
    let mut protection: Protection<R> = Edhoc::with_store(crypto, identity, TrustedPeers::new())
        .expect("the EDHOC capacities are nonzero");
    protection
        .trust(TrustedPeer::new(PeerId(1), credentials.client_ccs).expect("client.ccs is a CCS"))
        .expect("one credential fits a one-entry store");

    let shared = ExampleShared::new();
    let queue = Queue::<DATAGRAM>::new();
    let mut fw = Resource::new(&queue, &ota);
    let mut router = Router::<DATAGRAM, 1, 0, 0>::new(&PROVING);
    router
        .route("/fw", &[Code::GET, Code::PUT], &mut fw)
        .expect("the router holds one route");
    let mut endpoint = ExampleEndpoint::new_with_role(&shared, protection)
        .expect("a server-only endpoint is a valid configuration");
    let server = drive(&mut endpoint, transport, &mut router);
    mai_ota::coap::run(ota, &queue, shared.handle(), &EmbassyClock, server).await
}

async fn drive<T: Transport, P: mai_coap::endpoint::Protection>(
    endpoint: &mut ExampleEndpoint<'_, P>,
    transport: &mut T,
    router: &mut Router<'_, DATAGRAM, 1, 0, 0>,
) -> ! {
    loop {
        // A transport error ends a run; the endpoint keeps its state.
        let _ = endpoint
            .run(transport, &EmbassyClock, router, &PROVING)
            .await;
        embassy_time::Timer::after_secs(1).await;
    }
}
