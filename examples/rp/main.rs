// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) Embassy project contributors (the Wi-Fi bring-up)

//! mai-ota on a Pico W (RP2040) or Pico 2 W (RP2350): joins Wi-Fi and takes
//! firmware on `coap://<address>/fw`, behind the bootloader in `bootloader/`.
//!
//! The Wi-Fi bring-up is adapted from Embassy's
//! `examples/rp/src/bin/wifi_tcp_server.rs` (copyright Embassy project
//! contributors, MIT OR Apache-2.0).

#![no_std]
#![no_main]

use cyw43::{JoinOptions, aligned_bytes};
use cyw43_pio::{DEFAULT_CLOCK_DIVIDER, PioSpi};
use defmt::{info, unwrap, warn};
use embassy_executor::Spawner;
use embassy_net::udp::UdpSocket;
use embassy_net::{StackStorage, StaticPool};
use embassy_rp::flash::Flash;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, DMA_CH1, DMA_CH2, PIO0};
use embassy_rp::pio::{InterruptHandler, Pio};
use embassy_rp::{bind_interrupts, dma};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use mai_coap::transport::embassy_xarxa::EmbassyXarxaTransport;
use mai_ota::watchdog::rp::{Watchdog, reset_chip};
use mai_ota_example_common::{ChaCha20Rng, Credentials};
use rand_core::SeedableRng;
use static_cell::{ConstStaticCell, StaticCell};
use {defmt_rtt as _, panic_probe as _};

#[cfg(feature = "rp2040")]
const FLASH_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "rp235xa")]
const FLASH_SIZE: usize = 4 * 1024 * 1024;

/// Starts this image's ID; uploads must carry it too. Fixed for good: a device
/// refuses any image whose ID it cannot find.
const MAGIC: [u8; 8] = *b"MAIOTAEX";
mai_ota::image_id!(MAGIC);

static CREDENTIALS: Credentials = Credentials {
    device_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/device.ccs")),
    device_key: include_bytes!(concat!(env!("OUT_DIR"), "/device.cosekey")),
    client_ccs: include_bytes!(concat!(env!("OUT_DIR"), "/client.ccs")),
};

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => InterruptHandler<PIO0>;
    DMA_IRQ_0 => dma::InterruptHandler<DMA_CH0>, dma::InterruptHandler<DMA_CH1>,
        dma::InterruptHandler<DMA_CH2>;
    #[cfg(feature = "rp235xa")]
    TRNG_IRQ => embassy_rp::trng::InterruptHandler<embassy_rp::peripherals::TRNG>;
});

type FlashMutex = Mutex<NoopRawMutex, Flash<'static, embassy_rp::mode::Async, FLASH_SIZE>>;

#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<
        'static,
        cyw43::SpiBus<Output<'static>, PioSpi<'static, PIO0, 0>>,
        cyw43::Cyw43439,
    >,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: embassy_net::Runner<'static>) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn join(mut control: cyw43::Control<'static>, iface: embassy_net::iface::Iface<'static>) {
    while let Err(err) = control
        .join(
            env!("WIFI_SSID"),
            JoinOptions::new(env!("WIFI_PASSWORD").as_bytes()),
        )
        .await
    {
        warn!("wifi: join failed: {:?}", err);
        Timer::after_secs(2).await;
    }
    iface.wait_link_up().await;
    iface.wait_config_up().await;
    for address in iface.ip_addrs().iter() {
        info!("address {}", address);
    }
}

#[embassy_executor::task]
async fn watchdog_task(watchdog: Watchdog, after_watchdog_reset: bool) -> ! {
    mai_ota::watchdog::run(watchdog, after_watchdog_reset, mai_ota::no_events).await
}

/// 32 bytes to seed the EDHOC random generator.
///
/// The RP2350 has a TRNG. The RP2040 has only its ring oscillator's random bit,
/// which the datasheet says not to use for security: good enough for an
/// example, not for a product that cares about its keys.
#[cfg(feature = "rp2040")]
fn seed() -> [u8; 32] {
    let mut seed = [0u8; 32];
    rand_core::Rng::fill_bytes(&mut embassy_rp::clocks::RoscRng, &mut seed);
    seed
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());
    info!(
        "mai-ota example {}, {}",
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION")
    );

    let after_watchdog_reset = embassy_rp::pac::WATCHDOG.reason().read().timer();
    let watchdog = Watchdog::new(p.WATCHDOG);

    #[cfg(feature = "rp2040")]
    let seed = seed();
    #[cfg(feature = "rp235xa")]
    let seed = {
        let mut trng = embassy_rp::trng::Trng::new(p.TRNG, Irqs, Default::default());
        let mut seed = [0u8; 32];
        trng.fill_bytes(&mut seed).await;
        seed
    };
    let rng = ChaCha20Rng::from_seed(seed);

    static FLASH: StaticCell<FlashMutex> = StaticCell::new();
    let flash = FLASH.init(Mutex::new(Flash::new(p.FLASH, p.DMA_CH2, Irqs)));
    let ota = mai_ota::embassy_boot::ota(
        env!("CARGO_PKG_NAME"),
        env!("EXAMPLE_VERSION"),
        flash,
        embassy_rp::flash::FLASH_BASE as u32,
        MAGIC,
        reset_chip,
        mai_ota::no_events,
    )
    .await;
    // The bootloader starts the watchdog for a trial boot; `Watchdog::new` fed it.
    // Feeding it from here on covers the rest of bring-up.
    spawner.spawn(unwrap!(watchdog_task(watchdog, after_watchdog_reset)));

    let fw = aligned_bytes!("../rp/cyw43-firmware/43439A0.bin");
    let clm = aligned_bytes!("../rp/cyw43-firmware/43439A0_clm.bin");
    let nvram = aligned_bytes!("../rp/cyw43-firmware/nvram_rp2040.bin");
    let pwr = Output::new(p.PIN_23, Level::Low);
    let cs = Output::new(p.PIN_25, Level::High);
    let mut pio = Pio::new(p.PIO0, Irqs);
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        DEFAULT_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        dma::Channel::new(p.DMA_CH0, Irqs),
        dma::Channel::new(p.DMA_CH1, Irqs),
    );
    static STATE: ConstStaticCell<cyw43::State> = ConstStaticCell::new(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(STATE.take(), pwr, spi, fw, nvram).await;
    spawner.spawn(unwrap!(cyw43_task(runner)));
    control.init(clm, cyw43::Config::default()).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    static STACK: StaticCell<StackStorage> = StaticCell::new();
    // cyw43 receives each frame with its bus headers straight into these buffers.
    static POOL: StaticPool<{ cyw43::RX_BUF_SIZE }, 16, { cyw43::RX_BUF_ALIGN }> =
        StaticPool::new();
    let (stack, runner) = embassy_net::Stack::new(
        STACK.init(StackStorage::new()),
        &POOL,
        u64::from_le_bytes(seed[..8].try_into().unwrap()),
    );
    static DEVICE: StaticCell<cyw43::NetDriver<'static>> = StaticCell::new();
    let iface = unwrap!(stack.add_iface_borrowed(DEVICE.init(net_device)));
    unwrap!(iface.set_dhcpv4(Some(Default::default())));
    unwrap!(iface.set_slaac(Some(Default::default())));
    spawner.spawn(unwrap!(net_task(runner)));
    spawner.spawn(unwrap!(join(control, iface)));

    // Served at once, not after the network is up: the trial's deadline runs
    // inside, and an image that never gets on the network must still roll back.
    let mut socket = unwrap!(UdpSocket::new(stack));
    unwrap!(socket.bind_to_iface(Some(iface.handle())));
    unwrap!(socket.bind(5683, 0));
    let mut transport = unwrap!(EmbassyXarxaTransport::new(&socket, iface.handle(), 1));
    info!("serving /fw on port 5683");
    mai_ota_example_common::serve(&mut transport, &CREDENTIALS, rng, ota).await
}
