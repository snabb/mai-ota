// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) nrf-sdc project contributors (`scrub_grtc` only)

//! mai-ota on a Seeed XIAO nRF54LM20A: joins a Thread network and takes firmware
//! on `coap://[<address>]/fw`, behind the bootloader in `bootloader/`.
//!
//! [`scrub_grtc`] is adapted from `scrub_grtc` in `tests/src/lib.rs` in
//! [nrf-802154](https://github.com/sysgrok/nrf-802154) (copyright nrf-sdc project
//! contributors, MIT OR Apache-2.0), and modified to disable the compare channels
//! too.

#![no_std]
#![no_main]

mod enet;

// Linked for its symbols: OpenThread's C sources call `strtoul` and friends.
use tinyrlibc as _;

use core::net::Ipv6Addr;

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::driver::LinkState;
use embassy_net::udp::UdpSocket;
use embassy_net::wire::{IpCidr, Ipv6Cidr};
use embassy_net::{StackStorage, StaticPool};
use embassy_nrf::{bind_interrupts, pac};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use mai_coap::transport::embassy_xarxa::EmbassyXarxaTransport;
use mai_ota::watchdog::nrf::Watchdog;
use mai_ota_example_common::{ChaCha20Rng, Credentials};
use nrf_802154::{OpenThreadRadio, Radio};
use nrf_mpsl::{MultiprotocolServiceLayer, SessionMem};
use openthread::{
    DeviceRole, OpenThread, OtResources, OtSrpResources, SimpleRamSettings, SrpConf, SrpService,
};
use rand_core::SeedableRng;
use static_cell::StaticCell;
use {defmt_rtt as _, panic_probe as _};

/// Starts this image's ID; uploads must carry it too. Fixed for good: a device
/// refuses any image whose ID it cannot find.
const MAGIC: [u8; 8] = *b"MAIOTAEX";
mai_ota::image_id!(MAGIC);

static CREDENTIALS: Credentials = Credentials {
    device_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/device.ccs")),
    device_key: include_bytes!(concat!(env!("OUT_DIR"), "/device.cosekey")),
    client_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/client.ccs")),
};
static DATASET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dataset.bin"));

/// IPv6's minimum, which is what Thread carries.
const MTU: usize = 1280;

bind_interrupts!(struct Irqs {
    SWI00 => nrf_mpsl::LowPrioInterruptHandler;
    // MPSL sets these three to priority 0 itself, and its critical section leaves
    // them unmasked.
    RADIO_0 => nrf_mpsl::HighPrioInterruptHandler;
    TIMER10 => nrf_mpsl::HighPrioInterruptHandler;
    GRTC_3 => nrf_mpsl::HighPrioInterruptHandler;
    CLOCK_POWER => nrf_mpsl::ClockInterruptHandler;
    EGU10 => nrf_802154::EguInterruptHandler;
    GRTC_0 => nrf_802154::LpTimerInterruptHandler;
    // Frame encryption: without it the node associates, then fails on the
    // first secured frame.
    AAR00_CCM00 => nrf_802154::CcmInterruptHandler;
});

type Rram = Mutex<NoopRawMutex, nrf_mpsl::Flash<'static>>;

/// Disable every GRTC compare channel, and clear its interrupt enables and events,
/// before `embassy_nrf::init`.
///
/// A soft reset leaves the GRTC running with its state latched, and MPSL and the
/// 802.15.4 driver claim compare channels beyond embassy's own: an event left
/// from before the reset would otherwise fire forever in embassy's handler, and a
/// channel left enabled makes MPSL's initialisation assert
/// (`MultiprotocolServiceLayer: 106:179`). The reboot into an uploaded image is a
/// soft reset, often with the radio busy, so the image on trial panicked at boot
/// and the bootloader went back to the previous one.
fn scrub_grtc() {
    let r = pac::GRTC;
    for group in 0..4 {
        r.intenclr(group).write(|w| w.0 = u32::MAX);
    }
    for cc in 0..12 {
        r.cc(cc).ccen().write(|w| w.set_active(false));
        r.events_compare(cc).write_value(0);
    }
}

#[embassy_executor::task]
async fn mpsl_task(mpsl: &'static MultiprotocolServiceLayer<'static>) -> ! {
    mpsl.run().await
}

#[embassy_executor::task]
async fn thread_task(mut runner: enet::Runner, radio: OpenThreadRadio<'static>) -> ! {
    runner.run(radio).await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn watchdog_task(watchdog: Watchdog, after_watchdog_reset: bool) -> ! {
    mai_ota::watchdog::run(watchdog, after_watchdog_reset, mai_ota::no_events).await
}

/// Follow OpenThread's state: the link is up while attached, and the stack's
/// address is the best one OpenThread has.
#[embassy_executor::task]
async fn follow(
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
    unwrap!(iface.stack().routes().add(default).ok());
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
                defmt::warn!("could not set the address");
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

/// Where OpenThread keeps the SRP client's ECDSA key pair.
const SETTINGS_KEY_SRP_ECDSA: u16 = 11;

/// Where OpenThread keeps the 32-byte secret its SLAAC address identifiers
/// (RFC 7217) are derived from.
const SETTINGS_KEY_SLAAC_IID_SECRET: u16 = 7;

/// The SLAAC secret, derived from the device's identity key.
///
/// Fixed for the same reason as [`srp_key`]: made afresh at each boot, it would give
/// the device a new address every boot, and an updated image would come up where
/// the name still points to the old one, so the client could not confirm it.
fn slaac_secret(device_key: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::new()
        .chain_update(b"mai-ota example SLAAC secret")
        .chain_update(device_key)
        .finalize()
        .into()
}

/// The SRP client's key pair, derived from the device's identity key, in the form
/// OpenThread stores it: an SEC1 DER `ECPrivateKey` for P-256 in a 126-byte buffer
/// whose last byte is its length.
///
/// Fixed, because OpenThread's settings here are in RAM: a key made afresh at each
/// boot would find the host name still registered to the last boot's key, which
/// the border router holds for days.
fn srp_key(device_key: &[u8]) -> [u8; 126] {
    use sha2::{Digest, Sha256};
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
fn register(thread: &OpenThread<'static>, eui64: [u8; 8]) {
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
        Err(e) => defmt::warn!("srp: {:?}", e),
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    scrub_grtc();
    let mut config = embassy_nrf::config::Config::default();
    config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;
    config.lfclk_source = embassy_nrf::config::LfclkSource::ExternalXtal;
    config.clock_speed = embassy_nrf::config::ClockSpeed::CK128;
    let p = embassy_nrf::init(config);
    // Off by default; this part fetches from RRAM, which does not keep up at 128 MHz.
    pac::ICACHE.tasks_invalidatecache().write_value(1);
    pac::ICACHE.enable().write(|w| w.set_enable(true));
    info!(
        "mai-ota example {}, {}",
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION")
    );

    let after_watchdog_reset = mai_ota::watchdog::nrf::after_watchdog_reset();
    // Adopts the watchdog the bootloader started for a trial boot.
    let watchdog = Watchdog::new(p.WDT0);
    spawner.spawn(unwrap!(watchdog_task(watchdog, after_watchdog_reset)));

    static SESSION: StaticCell<SessionMem<1>> = StaticCell::new();
    static MPSL: StaticCell<MultiprotocolServiceLayer<'static>> = StaticCell::new();
    let lfclk = nrf_mpsl::raw::mpsl_clock_lfclk_cfg_t {
        source: nrf_mpsl::raw::MPSL_CLOCK_LF_SRC_XTAL as u8,
        rc_ctiv: 0,
        rc_temp_ctiv: 0,
        accuracy_ppm: nrf_mpsl::raw::MPSL_DEFAULT_CLOCK_ACCURACY_PPM as u16,
        skip_wait_lfclk_started: true,
    };
    let mpsl = MPSL.init(unwrap!(MultiprotocolServiceLayer::with_timeslots(
        nrf_mpsl::Peripherals::new(
            p.GRTC_CH7,
            p.GRTC_CH8,
            p.GRTC_CH9,
            p.GRTC_CH10,
            p.GRTC_CH11,
            p.TIMER10,
            p.TIMER20,
            p.TEMP,
            p.PPI10_CH0,
            p.PPI20_CH1,
            p.PPIB11_CH0,
            p.PPIB21_CH0,
        ),
        Irqs,
        lfclk,
        SESSION.init(SessionMem::new()),
    )));
    spawner.spawn(unwrap!(mpsl_task(mpsl)));
    // Let the watchdog and MPSL tasks run before the radio stack is built. A boot
    // that writes the boot state yields here anyway, in the RRAM write; a trial
    // boot only reads it, and without this, its executor was found starved after
    // bring-up, until the watchdog reset it.
    embassy_futures::yield_now().await;

    // With MPSL up, the RRAM is MPSL's to arbitrate.
    static RRAM: StaticCell<Rram> = StaticCell::new();
    let rram = RRAM.init(Mutex::new(nrf_mpsl::Flash::take(mpsl, p.RRAMC)));
    let ota = mai_ota::embassy_boot::ota(
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION"),
        rram,
        0,
        MAGIC,
        mai_ota::watchdog::nrf::reset_chip,
        mai_ota::no_events,
    )
    .await;

    static OT_RNG: StaticCell<embassy_nrf::crypto::rng::Rng<'static, embassy_nrf::mode::Blocking>> =
        StaticCell::new();
    let ot_rng = OT_RNG.init(embassy_nrf::crypto::rng::Rng::new_blocking(p.CRYPTO_RNG));
    let mut seed = [0u8; 32];
    ot_rng.blocking_fill_bytes(&mut seed);
    let rng = ChaCha20Rng::from_seed(seed);

    // The EUI-64 from the factory-programmed device address.
    let id = pac::FICR.deviceaddr(0).read().to_le_bytes();
    let id1 = pac::FICR.deviceaddr(1).read().to_le_bytes();
    let eui64 = [
        id[0] ^ 0x02,
        id[1],
        id[2],
        0xff,
        0xfe,
        id[3],
        id1[0],
        id1[1],
    ];

    static RESOURCES: StaticCell<OtResources> = StaticCell::new();
    static SETTINGS: StaticCell<[u8; 1024]> = StaticCell::new();
    static STORE: StaticCell<SimpleRamSettings> = StaticCell::new();
    let store = STORE.init(SimpleRamSettings::new(SETTINGS.init([0; 1024])));
    unwrap!(
        store
            .set(SETTINGS_KEY_SRP_ECDSA, &srp_key(CREDENTIALS.device_key))
            .ok()
    );
    unwrap!(
        store
            .set(
                SETTINGS_KEY_SLAAC_IID_SECRET,
                &slaac_secret(CREDENTIALS.device_key)
            )
            .ok()
    );
    static SRP: StaticCell<OtSrpResources<1, 128>> = StaticCell::new();
    let thread = unwrap!(OpenThread::new_with_srp(
        eui64,
        ot_rng,
        store,
        RESOURCES.init(OtResources::new()),
        SRP.init(OtSrpResources::new()),
    ));

    // Room for a burst: OpenThread holds a single received message, and drops the
    // next one ("buffer full") while that waits for a slot here.
    static DRIVER: StaticCell<enet::DriverState<4, 2>> = StaticCell::new();
    let (link, runner, device) =
        enet::new(thread.clone(), DRIVER.init(enet::DriverState::new()), MTU);
    let radio = OpenThreadRadio::new(Radio::new(
        p.RADIO,
        nrf_802154::RadioPeripherals::new(
            p.GRTC_CH3,
            p.GRTC_CH4,
            p.GRTC_CH5,
            p.PPI20_CH2,
            p.PPI20_CH3,
            p.PPIB11_CH1,
            p.PPIB21_CH1,
            p.PPIB11_CH2,
            p.PPIB21_CH2,
        ),
        Irqs,
        mpsl,
    ));
    spawner.spawn(unwrap!(thread_task(runner, radio)));

    static STACK: StaticCell<StackStorage> = StaticCell::new();
    // OpenThread's packets are at most the 1280-byte IPv6 minimum.
    static POOL: StaticPool<MTU, 12> = StaticPool::new();
    let (stack, net_runner) = embassy_net::Stack::new(
        STACK.init(StackStorage::new()),
        &POOL,
        u64::from_le_bytes(seed[24..].try_into().unwrap()),
    );
    static DEVICE: StaticCell<enet::Driver<'static>> = StaticCell::new();
    let iface = unwrap!(stack.add_iface_borrowed(DEVICE.init(device)).ok());
    spawner.spawn(unwrap!(net_task(net_runner)));

    unwrap!(thread.set_active_dataset_tlv(DATASET));
    unwrap!(thread.enable_ipv6(true));
    unwrap!(thread.enable_thread(true));
    register(&thread, eui64);
    spawner.spawn(unwrap!(follow(thread, link, iface)));

    // Served at once, not after attaching: the trial's deadline runs inside, and
    // an image that never joins the network must still roll back.
    let mut socket = unwrap!(UdpSocket::new(stack).ok());
    unwrap!(socket.bind_to_iface(Some(iface.handle())).ok());
    unwrap!(socket.bind(5683, 0).ok());
    let mut transport = unwrap!(EmbassyXarxaTransport::new(&socket, iface.handle(), 1).ok());
    info!("serving /fw on port 5683");
    mai_ota_example_common::serve(&mut transport, &CREDENTIALS, rng, ota).await
}
