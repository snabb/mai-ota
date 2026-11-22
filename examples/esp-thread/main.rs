// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! mai-ota on an ESP32-H2: joins a Thread network and takes firmware on
//! `coap://[<address>]/fw`, behind ESP-IDF's bootloader with app rollback.

#![no_std]
#![no_main]

// Linked for its symbols: OpenThread's C sources call `strtoul` and friends.
use tinyrlibc as _;

use embassy_executor::Spawner;
use embassy_net::udp::UdpSocket;
use embassy_net::{StackStorage, StaticPool};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use esp_bootloader_esp_idf::partitions::FlashStorage;
use esp_hal::clock::CpuClock;
use esp_hal::rng::{Trng, TrngSource};
use esp_hal::rtc_cntl::SocResetReason;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::ieee802154::Ieee802154;
use log::info;
use mai_coap::transport::embassy_xarxa::EmbassyXarxaTransport;
use mai_ota::watchdog::Watchdog as _;
use mai_ota::watchdog::esp::Watchdog;
use mai_ota_example_common::thread::{self, MTU};
use mai_ota_example_common::{ChaCha20Rng, Credentials, enet};
use openthread::esp::EspRadio;
use openthread::{OpenThread, OtResources, OtSrpResources, SimpleRamSettings};
use rand_core::{Rng as _, SeedableRng};
use static_cell::StaticCell;
use {esp_backtrace as _, esp_println as _};

// The board name an upload is checked against: the package name, which this puts
// in the app descriptor.
esp_bootloader_esp_idf::esp_app_desc!();

static CREDENTIALS: Credentials = Credentials {
    device_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/device.ccs")),
    device_key: include_bytes!(concat!(env!("OUT_DIR"), "/device.cosekey")),
    client_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/client.ccs")),
};
static DATASET: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/dataset.bin"));

/// The 802.15.4 driver's heap, for its receive queue; nothing else here
/// allocates.
const HEAP_SIZE: usize = 24 * 1024;

#[embassy_executor::task]
async fn thread_task(mut runner: enet::Runner, radio: EspRadio<'static>) -> ! {
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

/// Reset the radio MACs, baseband and front end before the radio starts.
///
/// The modem power domain survives the software reset that boots an uploaded
/// image, and esp-radio resets it at start-up only on chips without a PMU.
/// Without this, the new image's radio would come up on top of the old image's
/// 802.15.4 MAC state.
fn reset_modem() {
    let syscon = esp_hal::peripherals::MODEM_SYSCON::regs();
    syscon.modem_rst_conf().modify(|_, w| {
        w.rst_fe().set_bit();
        w.rst_btbb().set_bit();
        w.rst_btbb_apb().set_bit();
        w.rst_btmac().set_bit();
        w.rst_btmac_apb().set_bit();
        w.rst_zbmac().set_bit()
    });
    syscon.modem_rst_conf().modify(|_, w| {
        w.rst_fe().clear_bit();
        w.rst_btbb().clear_bit();
        w.rst_btbb_apb().clear_bit();
        w.rst_btmac().clear_bit();
        w.rst_btmac_apb().clear_bit();
        w.rst_zbmac().clear_bit()
    });
}

/// Which images this build takes: its own board's, and, when built with
/// `EXAMPLE_ALSO_ACCEPT`, that board's too, so a device can be handed to another
/// firmware over the air. That firmware then confirms its trial with its own
/// credentials; this build's client cannot.
fn policy(
    descriptor: &mai_ota::image::EspDescriptor<'_>,
    board: &str,
    _running: &str,
) -> Result<(), &'static str> {
    match option_env!("EXAMPLE_ALSO_ACCEPT") {
        Some(other) if descriptor.project == other.as_bytes() => Ok(()),
        _ => mai_ota::image::same_board(descriptor, board),
    }
}

fn after_watchdog_reset() -> bool {
    let reason = esp_hal::system::reset_reason();
    info!("reset reason: {reason:?}");
    matches!(
        reason,
        Some(SocResetReason::CoreRtcWdt | SocResetReason::SysRtcWdt | SocResetReason::Cpu0RtcWdt)
    )
}

/// The EUI-64 from the factory MAC, as ESP-IDF forms it: FF:FE in the middle.
fn eui64() -> [u8; 8] {
    let mac = esp_hal::efuse::base_mac_address();
    let m = mac.as_bytes();
    [m[0], m[1], m[2], 0xff, 0xfe, m[3], m[4], m[5]]
}

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));
    info!(
        "mai-ota example {}, {}",
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION")
    );

    let after_watchdog_reset = after_watchdog_reset();
    // `esp_hal::init` stops every watchdog, the bootloader's too, and whether this is
    // a trial is known only once the flash is read, which can panic or hang. So it
    // runs from here on every boot, and an ordinary boot stops it again below.
    let mut watchdog = Watchdog::new(peripherals.RTC_TIMER);
    watchdog.start();
    // The one flash handle: mai-ota borrows it for each operation, and a firmware
    // would use the same one for its settings.
    static FLASH: StaticCell<mai_ota::esp::Flash<NoopRawMutex>> = StaticCell::new();
    let flash = FLASH.init(Mutex::new(
        FlashStorage::new(peripherals.FLASH, Default::default()).expect("the flash answers"),
    ));
    let ota = mai_ota::esp::ota_with(
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION"),
        flash,
        mai_ota::no_events,
        policy,
    )
    .await
    .expect("the partition table has ota_0, ota_1 and otadata");
    // A trial's keeps running, so a bring-up that hangs also rolls back.
    if !ota.boot.trial {
        watchdog.stop();
    }
    spawner.spawn(watchdog_task(watchdog, after_watchdog_reset).unwrap());

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0);

    esp_alloc::heap_allocator!(size: HEAP_SIZE);

    // The SAR ADC as the entropy source: OpenThread needs its generator before
    // the radio, whose driver registers the other source, starts.
    static TRNG_SOURCE: StaticCell<TrngSource<'static>> = StaticCell::new();
    TRNG_SOURCE.init(TrngSource::new(peripherals.RNG, peripherals.ADC1));
    static OT_RNG: StaticCell<Trng> = StaticCell::new();
    let ot_rng = OT_RNG.init(Trng::try_new().expect("the entropy source is running"));
    let mut seed = [0u8; 32];
    ot_rng.fill_bytes(&mut seed);
    let rng = ChaCha20Rng::from_seed(seed);

    let eui64 = eui64();
    static RESOURCES: StaticCell<OtResources> = StaticCell::new();
    static SETTINGS: StaticCell<[u8; 1024]> = StaticCell::new();
    static STORE: StaticCell<SimpleRamSettings> = StaticCell::new();
    let store = STORE.init(SimpleRamSettings::new(SETTINGS.init([0; 1024])));
    thread::seed_settings(store, CREDENTIALS.device_key);
    static SRP: StaticCell<OtSrpResources<1, 128>> = StaticCell::new();
    let thread = OpenThread::new_with_srp(
        eui64,
        ot_rng,
        store,
        RESOURCES.init(OtResources::new()),
        SRP.init(OtSrpResources::new()),
    )
    .expect("OpenThread starts");

    // Room for a burst: OpenThread holds a single received message, and drops the
    // next one ("buffer full") while that waits for a slot here.
    static DRIVER: StaticCell<enet::DriverState<4, 2>> = StaticCell::new();
    let (link, runner, device) =
        enet::new(thread.clone(), DRIVER.init(enet::DriverState::new()), MTU);
    reset_modem();
    let radio = EspRadio::new(Ieee802154::new(peripherals.IEEE802154));
    spawner.spawn(thread_task(runner, radio).unwrap());

    static STACK: StaticCell<StackStorage> = StaticCell::new();
    // OpenThread's packets are at most the 1280-byte IPv6 minimum.
    static POOL: StaticPool<MTU, 12> = StaticPool::new();
    let (stack, net_runner) = embassy_net::Stack::new(
        STACK.init(StackStorage::new()),
        &POOL,
        u64::from_le_bytes(seed[24..].try_into().unwrap()),
    );
    static DEVICE: StaticCell<enet::Driver<'static>> = StaticCell::new();
    let iface = stack
        .add_iface_borrowed(DEVICE.init(device))
        .expect("the stack has room for an interface");
    spawner.spawn(net_task(net_runner).unwrap());

    thread
        .set_active_dataset_tlv(DATASET)
        .expect("THREAD_DATASET is a valid dataset");
    thread.enable_ipv6(true).expect("IPv6 starts");
    thread.enable_thread(true).expect("Thread starts");
    thread::register(&thread, eui64);
    spawner.spawn(follow(thread, link, iface).unwrap());

    // Served at once, not after attaching: the trial's deadline runs inside, and
    // an image that never joins the network must still roll back.
    let mut socket = UdpSocket::new(stack).expect("the stack has room for a socket");
    socket
        .bind_to_iface(Some(iface.handle()))
        .expect("the interface exists");
    socket.bind(5683, 0).expect("port 5683 is free");
    let mut transport =
        EmbassyXarxaTransport::new(&socket, iface.handle(), 1).expect("the transport starts");
    info!("serving /fw on port 5683");
    mai_ota_example_common::serve(&mut transport, &CREDENTIALS, rng, ota).await
}
