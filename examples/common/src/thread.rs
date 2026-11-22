// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! What the Thread examples share once OpenThread runs on their radio: the
//! settings that keep the device's address and SRP name stable, the SRP
//! registration, and following OpenThread's state into embassy-net.

use core::net::Ipv6Addr;

use defmt_or_log::{assert, info, warn};
use embassy_net::driver::LinkState;
use embassy_net::wire::{IpCidr, Ipv6Cidr};
use embassy_time::Timer;
use openthread::{DeviceRole, OpenThread, SimpleRamSettings, SrpConf, SrpService};
use sha2::{Digest, Sha256};
use static_cell::StaticCell;

use crate::enet;

/// IPv6's minimum, which is what Thread carries.
pub const MTU: usize = 1280;

/// Where OpenThread keeps the SRP client's ECDSA key pair.
const SETTINGS_KEY_SRP_ECDSA: u16 = 11;

/// Where OpenThread keeps the 32-byte secret its SLAAC address identifiers
/// (RFC 7217) are derived from.
const SETTINGS_KEY_SLAAC_IID_SECRET: u16 = 7;

/// Fill OpenThread's settings with the SRP key pair and the SLAAC secret, both
/// derived from the device's identity key.
///
/// The settings are in RAM. Made afresh at each boot, the SLAAC secret would give
/// the device a new address every boot, so an updated image would come up where
/// the name still points to the old one, and the client could not confirm it. A
/// fresh SRP key would find the host name still registered to the last boot's
/// key, which the border router holds for days.
///
/// # Panics
///
/// If `store` has no room for them.
pub fn seed_settings(store: &mut SimpleRamSettings<'_>, device_key: &[u8]) {
    assert!(
        store
            .set(SETTINGS_KEY_SRP_ECDSA, &srp_key(device_key))
            .is_ok(),
        "the settings have room for the SRP key"
    );
    assert!(
        store
            .set(SETTINGS_KEY_SLAAC_IID_SECRET, &slaac_secret(device_key))
            .is_ok(),
        "the settings have room for the SLAAC secret"
    );
}

fn slaac_secret(device_key: &[u8]) -> [u8; 32] {
    Sha256::new()
        .chain_update(b"mai-ota example SLAAC secret")
        .chain_update(device_key)
        .finalize()
        .into()
}

/// The SRP client's key pair, in the form OpenThread stores it: an SEC1 DER
/// `ECPrivateKey` for P-256 in a 126-byte buffer whose last byte is its length.
fn srp_key(device_key: &[u8]) -> [u8; 126] {
    // The P-256 group order: a scalar must be in 1..n.
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63,
        0x25, 0x51,
    ];
    // SEQUENCE { INTEGER 1, OCTET STRING (32) ... [0] { OID prime256v1 } }.
    const HEAD: [u8; 7] = [0x30, 0x31, 0x02, 0x01, 0x01, 0x04, 0x20];
    const TAIL: [u8; 12] = [
        0xa0, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07,
    ];
    let mut scalar = [0u8; 32];
    for counter in 0u8.. {
        scalar = Sha256::new()
            .chain_update(b"mai-ota example SRP key")
            .chain_update(device_key)
            .chain_update([counter])
            .finalize()
            .into();
        if scalar != [0; 32] && scalar < N {
            break;
        }
    }
    let mut pair = [0u8; 126];
    let len = HEAD.len() + 32 + TAIL.len();
    pair[..HEAD.len()].copy_from_slice(&HEAD);
    pair[HEAD.len()..HEAD.len() + 32].copy_from_slice(&scalar);
    pair[HEAD.len() + 32..len].copy_from_slice(&TAIL);
    pair[125] = len as u8;
    pair
}

/// Register `mai-ota-XXXXXXXX` and its `_coap._udp` service with the border
/// router's SRP server, which publishes the name over mDNS as
/// `mai-ota-XXXXXXXX.local`. OpenThread's addresses change at every boot; the name
/// is how a client finds the device again after an update.
///
/// Call it once: the name lives in a static.
pub fn register(thread: &OpenThread<'static>, eui64: [u8; 8]) {
    use core::fmt::Write;
    static NAME: StaticCell<heapless::String<16>> = StaticCell::new();
    let name = NAME.init(heapless::String::new());
    let _ = write!(
        name,
        "mai-ota-{:02x}{:02x}{:02x}{:02x}",
        eui64[4], eui64[5], eui64[6], eui64[7]
    );
    let name: &'static str = name.as_str();
    let conf = SrpConf {
        host_name: name,
        ..SrpConf::new()
    };
    let service = SrpService {
        name: "_coap._udp",
        instance_name: name,
        subtype_labels: core::iter::empty::<&str>(),
        txt_entries: core::iter::empty::<(&str, &[u8])>(),
        port: 5683,
        priority: 0,
        weight: 0,
        lease_secs: 0,
        key_lease_secs: 0,
    };
    match thread
        .srp_set_conf(&conf)
        .and_then(|()| thread.srp_add_service(&service).map(|_| ()))
        .and_then(|()| thread.srp_autostart())
    {
        Ok(()) => info!("srp: registering {}.local", name),
        Err(e) => warn!("srp: {:?}", e),
    }
}

/// Follow OpenThread's state: the link is up while attached, and the stack's
/// address is the best one OpenThread has.
pub async fn follow(
    thread: OpenThread<'static>,
    link: enet::StateRunner<'static>,
    iface: embassy_net::iface::Iface<'static>,
) -> ! {
    // Any router address will do: on a bare-IP link nothing resolves it, and
    // OpenThread routes each packet by its destination. Without a route, the stack
    // sends nothing to clients beyond the prefix.
    let default = embassy_net::route::Route::new_ipv6_gateway(
        Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1),
        iface.handle(),
    );
    assert!(
        iface.stack().routes().add(default).is_ok(),
        "the route table has room"
    );
    let mut installed = None;
    loop {
        let attached = matches!(
            thread.device_role(),
            DeviceRole::Child | DeviceRole::Router | DeviceRole::Leader
        );
        link.set_link_state(if attached {
            LinkState::Up
        } else {
            LinkState::Down
        });
        let preferred = preferred_address(&thread);
        if preferred != installed {
            if let Some(address) = preferred {
                info!("address {}", address);
            }
            if iface.set_ip_addrs(preferred.map(IpCidr::V6)).is_err() {
                warn!("could not set the address");
            }
            installed = preferred;
        }
        embassy_futures::select::select(thread.wait_changed(), Timer::after_secs(10)).await;
    }
}

/// The address to serve on: one routable beyond the mesh (from a border router's
/// prefix) if there is one, else the mesh-local EID.
///
/// Never a routing or anycast locator: those sit in the mesh-local prefix with the
/// interface identifier `0000:00ff:fe00:xxxx`, and change with the topology.
fn preferred_address(thread: &OpenThread<'static>) -> Option<Ipv6Cidr> {
    let mesh_local = thread.mesh_local_prefix();
    let in_mesh = |address: &Ipv6Addr| address.octets()[..8] == mesh_local[..];
    let mut best: Option<(u8, Ipv6Addr, u8)> = None;
    let _ = thread.ipv6_addrs(|entry| {
        if let Some((address, prefix)) = entry {
            let locator = in_mesh(&address) && address.octets()[8..14] == [0, 0, 0, 0xff, 0xfe, 0];
            let rank = if address.is_unicast_link_local() {
                2
            } else if in_mesh(&address) {
                1
            } else {
                0
            };
            if !locator && best.is_none_or(|(best, _, _)| rank < best) {
                best = Some((rank, address, prefix));
            }
        }
        Ok(())
    });
    best.filter(|&(rank, _, _)| rank < 2)
        .map(|(_, address, prefix)| Ipv6Cidr::new(address, prefix))
}
