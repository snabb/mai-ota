// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! mai-ota on an ESP32, ESP32-C6 or ESP32-S3: joins Wi-Fi and takes firmware on
//! `coap://<address>/fw`, behind ESP-IDF's bootloader with app rollback.

#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_net::udp::UdpSocket;
use embassy_net::{StackStorage, StaticPool};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use esp_bootloader_esp_idf::partitions::FlashStorage;
use esp_hal::clock::CpuClock;
use esp_hal::rtc_cntl::SocResetReason;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Interface, WifiController};
use log::{info, warn};
use mai_coap::transport::embassy_xarxa::EmbassyXarxaTransport;
use mai_ota::watchdog::Watchdog as _;
use mai_ota::watchdog::esp::Watchdog;
use mai_ota_example_common::{ChaCha20Rng, Credentials};
use rand_core::SeedableRng;
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

/// The Wi-Fi driver's heap; nothing else here allocates.
const HEAP_SIZE: usize = 72 * 1024;

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn watchdog_task(watchdog: Watchdog, after_watchdog_reset: bool) -> ! {
    mai_ota::watchdog::run(watchdog, after_watchdog_reset, mai_ota::no_events).await
}

#[embassy_executor::task]
async fn report(iface: embassy_net::iface::Iface<'static>) {
    iface.wait_config_up().await;
    for address in iface.ip_addrs().iter() {
        info!("address {:?} ({:?})", address.cidr, address.origin);
    }
}

#[embassy_executor::task]
async fn link(mut controller: WifiController<'static>) -> ! {
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi: associated");
                let _ = controller.wait_for_disconnect_async().await;
                warn!("wifi: disconnected");
            }
            Err(error) => warn!("wifi: association failed: {error:?}"),
        }
        Timer::after_secs(2).await;
    }
}

/// Reset the C6's radio MACs, baseband and front end before Wi-Fi starts.
///
/// The modem power domain survives the software reset that boots an uploaded
/// image, and esp-radio resets it at start-up only on chips without a PMU. Without
/// this, the new image's Wi-Fi comes up on top of the old image's radio state and
/// never associates, so every update over Wi-Fi rolls back.
#[cfg(feature = "esp32c6")]
fn reset_modem() {
    let syscon = esp_hal::peripherals::MODEM_SYSCON::regs();
    syscon.modem_rst_conf().modify(|_, w| {
        w.rst_wifibb().set_bit();
        w.rst_wifimac().set_bit();
        w.rst_fe().set_bit();
        w.rst_btmac().set_bit();
        w.rst_btbb().set_bit();
        w.rst_zbmac().set_bit()
    });
    syscon.modem_rst_conf().modify(|_, w| {
        w.rst_wifibb().clear_bit();
        w.rst_wifimac().clear_bit();
        w.rst_fe().clear_bit();
        w.rst_btmac().clear_bit();
        w.rst_btbb().clear_bit();
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
    #[cfg(any(feature = "esp32", feature = "esp32c6"))]
    return matches!(
        reason,
        Some(SocResetReason::CoreRtcWdt | SocResetReason::SysRtcWdt | SocResetReason::Cpu0RtcWdt)
    );
    #[cfg(feature = "esp32s3")]
    return matches!(
        reason,
        Some(SocResetReason::CoreRtcWdt | SocResetReason::SysRtcWdt | SocResetReason::CpuRtcWdt)
    );
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
    #[cfg(feature = "esp32c6")]
    reset_modem();
    let mut controller =
        WifiController::new(peripherals.WIFI, Default::default()).expect("the radio starts");
    let config = StationConfig::default()
        .with_ssid(env!("WIFI_SSID").try_into().expect("SSID length"))
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(
            env!("WIFI_PASSWORD").try_into().expect("password length"),
        ));
    controller
        .set_config(&esp_radio::wifi::Config::Station(config))
        .expect("the station configuration is valid");

    // With the radio running, esp-hal's generator is a true random source.
    let mut seed = [0u8; 32];
    esp_hal::rng::Rng::new().read(&mut seed);
    let rng = ChaCha20Rng::from_seed(seed);

    static STACK: StaticCell<StackStorage> = StaticCell::new();
    // Room for a 1514-byte Ethernet frame.
    static POOL: StaticPool<1536, 8> = StaticPool::new();
    let (stack, runner) = embassy_net::Stack::new(
        STACK.init(StackStorage::new()),
        &POOL,
        u64::from_le_bytes(seed[..8].try_into().unwrap()),
    );
    static DEVICE: StaticCell<Interface> = StaticCell::new();
    let iface = stack
        .add_iface_borrowed(DEVICE.init(Interface::station()))
        .expect("the stack has room for an interface");
    iface
        .set_dhcpv4(Some(Default::default()))
        .expect("DHCPv4 starts");
    iface
        .set_slaac(Some(Default::default()))
        .expect("SLAAC starts");
    spawner.spawn(net_task(runner).unwrap());
    spawner.spawn(link(controller).unwrap());
    spawner.spawn(report(iface).unwrap());

    // Served at once, not after the network is up: the trial's deadline runs
    // inside, and an image that never gets on the network must still roll back.
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
