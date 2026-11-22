// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) nrf-sdc project contributors (`scrub_grtc` only)

//! mai-ota on a Seeed XIAO nRF54LM20A or nRF54L15: joins a Thread network and
//! takes firmware on `coap://[<address>]/fw`, behind the bootloader in each
//! example's `bootloader/`.
//!
//! [`scrub_grtc`] is adapted from `scrub_grtc` in `tests/src/lib.rs` in
//! [nrf-802154](https://github.com/sysgrok/nrf-802154) (copyright nrf-sdc project
//! contributors, MIT OR Apache-2.0), and modified to disable the compare channels
//! too.

#![no_std]
#![no_main]

// Linked for its symbols: OpenThread's C sources call `strtoul` and friends.
use tinyrlibc as _;

use defmt::{info, unwrap};
use embassy_executor::Spawner;
use embassy_net::udp::UdpSocket;
use embassy_net::{StackStorage, StaticPool};
use embassy_nrf::{bind_interrupts, pac};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use mai_coap::transport::embassy_xarxa::EmbassyXarxaTransport;
use mai_ota::watchdog::nrf::Watchdog;
use mai_ota_example_common::thread::{self, MTU};
use mai_ota_example_common::{ChaCha20Rng, Credentials, enet};
use nrf_802154::{OpenThreadRadio, Radio};
use nrf_mpsl::{MultiprotocolServiceLayer, SessionMem};
use openthread::{OpenThread, OtResources, OtSrpResources, SimpleRamSettings};
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

#[embassy_executor::task]
async fn follow(
    thread: OpenThread<'static>,
    link: enet::StateRunner<'static>,
    iface: embassy_net::iface::Iface<'static>,
) -> ! {
    thread::follow(thread, link, iface).await
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
    thread::seed_settings(store, CREDENTIALS.device_key);
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
    thread::register(&thread, eui64);
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
