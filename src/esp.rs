// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The [`Slot`](crate::Slot) for an ESP32 behind ESP-IDF's bootloader: the two
//! application partitions and `otadata`.
//!
//! The partition table must have `ota_0`, `ota_1` and `otadata`. The bootloader
//! must be ESP-IDF's with app rollback (`CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`):
//! it boots an image marked new as pending, and an image still pending at its next
//! boot as aborted, falling back to the other slot. espflash's bundled bootloader
//! boots the new image for good on the first try.
//!
//! The flash is the application's: it hands over the mutex it shares the
//! [`FlashStorage`] through, and mai-ota takes it for each operation and nothing
//! longer, so settings and other partitions can use the same handle.
//!
//! `otadata` goes through [`otadata`] rather than esp-bootloader-esp-idf's `Ota`,
//! which writes a new selection's sequence and state separately and fails on an
//! entry a power loss tore. Every rewrite erases the entry's sector first: the
//! flash driver programs, it does not erase, and a state change sets bits.
//!
//! Images are checked with [`image::esp_descriptor`] and the [`Policy`]: the
//! chip, and by default the board's name as the app descriptor's project name,
//! which `esp_app_desc!()` fills with the package name.

use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use esp_bootloader_esp_idf::partitions::{
    self, AppPartitionSubType, DataPartitionSubType, FlashRegion, FlashStorage, PartitionEntry,
    PartitionType,
};

use crate::{Boot, OnEvent, Ota, image, otadata};

/// Room for the partition table.
const TABLE_LEN: usize = partitions::PARTITION_TABLE_MAX_LEN;

/// The flash erases in sectors of this many bytes.
const SECTOR: u32 = 4096;

#[cfg(feature = "esp32")]
const CHIP_ID: u16 = 0;
#[cfg(feature = "esp32c5")]
const CHIP_ID: u16 = 23;
#[cfg(feature = "esp32c6")]
const CHIP_ID: u16 = 13;
#[cfg(feature = "esp32h2")]
const CHIP_ID: u16 = 16;
#[cfg(feature = "esp32s3")]
const CHIP_ID: u16 = 9;

/// The application's flash, as mai-ota borrows it.
pub type Flash<M> = Mutex<M, FlashStorage<'static>>;

/// Which images a board takes, past the checks every image must pass (the chip,
/// the descriptor): given the image's app descriptor, the board's name and the
/// running image's version, `Ok` or why not. [`image::same_board`] is the default.
pub type Policy = fn(&image::EspDescriptor<'_>, &str, &str) -> Result<(), &'static str>;

fn default_policy(
    descriptor: &image::EspDescriptor<'_>,
    board: &str,
    _running: &str,
) -> Result<(), &'static str> {
    image::same_board(descriptor, board)
}

/// The slot the running image did not boot from.
pub struct Slot<M: RawMutex + 'static> {
    flash: &'static Flash<M>,
    /// The inactive application partition.
    target: PartitionEntry,
    /// Its OTA slot number, 0 or 1.
    index: u8,
    otadata: PartitionEntry,
    policy: Policy,
    /// The running image's version, for `policy`.
    version: &'static str,
}

/// Find the other slot and say whether this boot is a trial, and whether it
/// follows a rollback.
///
/// After a rollback the latest `otadata` entry is the aborted image's, not the
/// running one's; it is pointed back at the running image here, so that the next
/// upload's activation starts from what actually runs. A fresh USB flash that
/// erased `otadata` is recorded the same way.
///
/// `None` with an error logged when the flash is not laid out for this.
async fn init<M: RawMutex>(
    flash: &'static Flash<M>,
    policy: Policy,
    version: &'static str,
) -> Option<(Slot<M>, bool, bool)> {
    let mut storage = flash.lock().await;
    let mut buffer = [0u8; TABLE_LEN];
    let table = partitions::read_partition_table(&mut storage, &mut buffer)
        .inspect_err(|e| error!("ota: no partition table: {:?}", e))
        .ok()?;
    let Ok(Some(booted)) = table.booted_partition() else {
        error!("ota: cannot tell which partition is running");
        return None;
    };
    let booted = match booted.partition_type() {
        PartitionType::App(slot @ (AppPartitionSubType::Ota0 | AppPartitionSubType::Ota1)) => slot,
        other => {
            error!(
                "ota: running from {:?}; flash with an OTA partition table",
                other
            );
            return None;
        }
    };
    let target = if booted == AppPartitionSubType::Ota0 {
        AppPartitionSubType::Ota1
    } else {
        AppPartitionSubType::Ota0
    };
    let Ok(Some(slot)) = table.find_partition(PartitionType::App(target)) else {
        error!("ota: no {:?} partition", target);
        return None;
    };
    let Ok(Some(otadata)) = table.find_partition(PartitionType::Data(DataPartitionSubType::Ota))
    else {
        error!("ota: no otadata partition");
        return None;
    };

    let entries = with_otadata(&mut storage, otadata, |region| read_entries(region))
        .inspect_err(|e| error!("ota: otadata: {:?}", e))
        .ok()?;
    let latest = otadata::latest(&entries);
    let trial = latest == Some((index(booted), otadata::PENDING_VERIFY));
    if trial {
        crate::start_trial();
    }
    let rolled_back = matches!(
        latest,
        Some((slot, otadata::INVALID | otadata::ABORTED)) if slot == index(target)
    );
    if latest.map(|(slot, _)| slot) != Some(index(booted)) {
        warn!(
            "ota: otadata selects {:?} but {:?} runs; recording {:?}",
            latest, booted, booted
        );
        let (i, entry) = otadata::select(&entries, index(booted), otadata::VALID);
        if let Err(e) = with_otadata(&mut storage, otadata, |region| rewrite(region, i, &entry)) {
            error!("ota: cannot update otadata: {:?}", e);
            return None;
        }
    }
    info!("ota: running {:?}, next image goes to {:?}", booted, target);
    Some((
        Slot {
            flash,
            target: slot,
            index: index(target),
            otadata,
            policy,
            version,
        },
        trial,
        rolled_back,
    ))
}

/// Everything an upload needs for `board`, running `version`, on the
/// application's `flash`; `None` when the flash is not laid out for it. An image
/// must name `board` as its project.
pub async fn ota<M: RawMutex>(
    board: &'static str,
    version: &'static str,
    flash: &'static Flash<M>,
    events: OnEvent,
) -> Option<Ota<Slot<M>>> {
    ota_with(board, version, flash, events, default_policy).await
}

/// [`ota`], with the firmware's own [`Policy`] for which images it takes: say, a
/// build of the same board for another radio, or not the build already running.
pub async fn ota_with<M: RawMutex>(
    board: &'static str,
    version: &'static str,
    flash: &'static Flash<M>,
    events: OnEvent,
    policy: Policy,
) -> Option<Ota<Slot<M>>> {
    let (slot, trial, rolled_back) = init(flash, policy, version).await?;
    Some(Ota {
        slot,
        board,
        version,
        boot: Boot {
            trial,
            rolled_back,
            trial_timeout: crate::TRIAL_TIMEOUT,
            reboot,
        },
        events,
    })
}

/// The OTA slot number `otadata` uses.
fn index(slot: AppPartitionSubType) -> u8 {
    u8::from(slot == AppPartitionSubType::Ota1)
}

fn read_entries(
    region: &mut FlashRegion<'_, '_>,
) -> Result<[[u8; otadata::ENTRY_LEN]; 2], partitions::Error> {
    let mut entries = [[0u8; otadata::ENTRY_LEN]; 2];
    for (i, entry) in entries.iter_mut().enumerate() {
        region.read(i as u32 * otadata::SECTOR, entry)?;
    }
    Ok(entries)
}

/// Replace entry `i`: erase its sector, then program it. The other entry is not
/// touched, so a power loss in between leaves that one for the bootloader.
fn rewrite(
    region: &mut FlashRegion<'_, '_>,
    i: usize,
    entry: &[u8; otadata::ENTRY_LEN],
) -> Result<(), partitions::Error> {
    let at = i as u32 * otadata::SECTOR;
    region.erase(at, at + otadata::SECTOR)?;
    region.write(at, entry)
}

/// Run `f` on the `otadata` partition.
fn with_otadata<T>(
    storage: &mut FlashStorage<'static>,
    otadata: PartitionEntry,
    f: impl FnOnce(&mut FlashRegion<'_, 'static>) -> Result<T, partitions::Error>,
) -> Result<T, partitions::Error> {
    f(&mut otadata.as_flash_region(storage)?)
}

/// Mark the running image good, in place, as ESP-IDF's
/// `esp_ota_mark_app_valid_cancel_rollback` does. A write that tears leaves that
/// entry invalid, and the bootloader falls back to the other one, the previous image.
async fn confirm<M: RawMutex>(flash: &'static Flash<M>, otadata: PartitionEntry) -> Result<(), ()> {
    let mut storage = flash.lock().await;
    with_otadata(&mut storage, otadata, |region| {
        let entries = read_entries(region)?;
        let (i, entry) =
            otadata::with_state(&entries, otadata::VALID).ok_or(partitions::Error::InvalidState)?;
        rewrite(region, i, &entry)
    })
    .map_err(|e| error!("ota: cannot confirm the image: {:?}", e))
}

/// A clean reset, which the bootloader treats as the next boot.
pub fn reboot() -> ! {
    esp_hal::system::software_reset()
}

impl<M: RawMutex> Slot<M> {
    /// Run `f` on the inactive application partition.
    async fn with_target<T>(
        &self,
        what: &str,
        f: impl FnOnce(&mut FlashRegion<'_, 'static>) -> Result<T, partitions::Error>,
    ) -> Result<T, ()> {
        let mut storage = self.flash.lock().await;
        self.target
            .as_flash_region(&mut storage)
            .and_then(|mut region| f(&mut region))
            .map_err(|e| error!("ota: {}: {:?}", what, e))
    }
}

impl<M: RawMutex> crate::Slot for Slot<M> {
    const ERASE: u32 = SECTOR;
    const WRITE: u32 = 4;
    const PREFIX: usize = image::ESP_PREFIX;

    fn capacity(&self) -> u32 {
        self.target.len()
    }

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
        self.with_target("erase", |region| region.erase(from, to))
            .await
    }

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), ()> {
        self.with_target("write", |region| region.write(offset, data))
            .await
    }

    async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ()> {
        self.with_target("read", |region| region.read(offset, buf))
            .await
    }

    async fn activate(&mut self, _len: u32) -> Result<(), ()> {
        let target = self.index;
        let mut storage = self.flash.lock().await;
        with_otadata(&mut storage, self.otadata, |region| {
            let entries = read_entries(region)?;
            let (i, entry) = otadata::select(&entries, target, otadata::NEW);
            rewrite(region, i, &entry)
        })
        .map_err(|e| error!("ota: cannot activate: {:?}", e))
    }

    fn check(&self, prefix: &[u8], board: &str) -> Result<(), &'static str> {
        (self.policy)(
            &image::esp_descriptor(prefix, CHIP_ID)?,
            board,
            self.version,
        )
    }

    async fn verify(&mut self, len: u32) -> Result<(), crate::Refusal> {
        image::esp_within(len, async |offset, buf| {
            crate::Slot::read(self, offset, buf).await
        })
        .await
    }

    fn confirm(&self) -> impl Future<Output = Result<(), ()>> + 'static {
        let (flash, otadata) = (self.flash, self.otadata);
        async move { confirm(flash, otadata).await }
    }
}
