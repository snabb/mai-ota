// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The [`Slot`](crate::Slot) for a Cortex-M board behind an embassy-boot bootloader.
//!
//! An upload goes into the DFU partition, and activating it marks the state
//! partition for a swap. The bootloader swaps it into the active partition at the
//! next boot, and swaps it back at the boot after that unless the image has marked
//! itself booted. The partitions are the linker script's
//! `__bootloader_{active,dfu,state}_{start,end}`, the same symbols the bootloader
//! links against. They are offsets into the flash, as its driver addresses it; the
//! board says where offset 0 is mapped.
//!
//! Images are checked with [`image::cortex_m`], so each must carry an
//! [`image_id!`](crate::image_id) record.

use core::ops::{Range, RangeInclusive};

use embassy_boot::{FirmwareState, State};
use embassy_embedded_hal::flash::partition::Partition;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

use crate::fmt::Dbg;
use crate::{Boot, OnEvent, Ota, image};

unsafe extern "C" {
    static __bootloader_active_start: u32;
    static __bootloader_active_end: u32;
    static __bootloader_dfu_start: u32;
    static __bootloader_dfu_end: u32;
    static __bootloader_state_start: u32;
    static __bootloader_state_end: u32;
}

/// The addresses of two linker symbols. Raw pointers, not references: the
/// partition symbols are flash offsets, which name no `u32` in memory.
fn symbols(start: *const u32, end: *const u32) -> Range<u32> {
    start as u32..end as u32
}

/// The DFU partition, and the state partition that says what to boot.
pub struct Slot<M: RawMutex + 'static, F: NorFlash + 'static> {
    dfu: Partition<'static, M, F>,
    state: Partition<'static, M, F>,
    /// The active partition's addresses, where an image runs from.
    active: Range<u32>,
    /// Where an image's initial stack pointer may point.
    ram: RangeInclusive<u32>,
    /// Starts an image's [`image::ImageId`].
    magic: [u8; 8],
}

/// Everything an upload needs for `board`, running `version`, on `flash`: the
/// whole flash, which other users may share, and whose offset 0 the core sees at
/// address `base`. Uploads must carry an [`image::ImageId`] starting with `magic`
/// and have their initial stack pointer in cortex-m-rt's `_ram_start..=_ram_end`.
/// `reboot` must reset the whole chip.
///
/// A rollback is recorded once: the state is then marked booted, so the next boot
/// does not report it again.
pub async fn ota<M: RawMutex, F: NorFlash>(
    board: &'static str,
    version: &'static str,
    flash: &'static Mutex<M, F>,
    base: u32,
    magic: [u8; 8],
    reboot: fn() -> !,
    events: OnEvent,
) -> Ota<Slot<M, F>> {
    const { assert!(F::READ_SIZE <= 16) };
    unsafe extern "C" {
        static _ram_start: u32;
        static _ram_end: u32;
    }
    let (active, dfu, state, ram) = (
        symbols(
            &raw const __bootloader_active_start,
            &raw const __bootloader_active_end,
        ),
        symbols(
            &raw const __bootloader_dfu_start,
            &raw const __bootloader_dfu_end,
        ),
        symbols(
            &raw const __bootloader_state_start,
            &raw const __bootloader_state_end,
        ),
        symbols(&raw const _ram_start, &raw const _ram_end),
    );
    let slot = Slot {
        dfu: Partition::new(flash, dfu.start, dfu.len() as u32),
        state: Partition::new(flash, state.start, state.len() as u32),
        active: base + active.start..base + active.end,
        ram: ram.start..=ram.end,
        magic,
    };
    let mut aligned = Aligned::<F>::new();
    let mut boot = FirmwareState::new(slot.state.clone(), aligned.get());
    let (mut trial, mut rolled_back) = (false, false);
    match boot.get_state().await {
        Ok(State::Swap) => {
            trial = true;
            crate::start_trial();
        }
        Ok(State::Revert) => {
            rolled_back = true;
            if let Err(e) = boot.mark_booted().await {
                error!("ota: cannot clear the rollback: {:?}", Dbg(e));
            }
        }
        Ok(_) => {}
        // This may be a trial, whose fallback the DFU partition holds, so it is
        // treated as one: no uploads into that partition, a proof confirms it (and
        // the write that does so leaves the state known), and without one the
        // deadline reboots it, for the bootloader to decide.
        Err(e) => {
            error!(
                "ota: cannot read the boot state: {:?}; treating this boot as a trial",
                Dbg(e)
            );
            trial = true;
            crate::start_trial();
        }
    }
    Ota {
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
    }
}

/// A buffer of the state partition's word size, which `FirmwareState` reads and
/// writes through. Aligned because a DMA read wants it.
#[repr(C, align(32))]
struct Aligned<F> {
    buf: [u8; 32],
    _flash: core::marker::PhantomData<F>,
}

impl<F: NorFlash> Aligned<F> {
    const LEN: usize = {
        let len = if F::WRITE_SIZE > F::READ_SIZE {
            F::WRITE_SIZE
        } else {
            F::READ_SIZE
        };
        assert!(len <= 32);
        len
    };

    fn new() -> Self {
        Self {
            buf: [0; 32],
            _flash: core::marker::PhantomData,
        }
    }

    fn get(&mut self) -> &mut [u8] {
        &mut self.buf[..Self::LEN]
    }
}

impl<M: RawMutex, F: NorFlash> crate::Slot for Slot<M, F> {
    const ERASE: u32 = F::ERASE_SIZE as u32;
    const WRITE: u32 = F::WRITE_SIZE as u32;
    const PREFIX: usize = image::CORTEX_M_PREFIX;

    fn capacity(&self) -> u32 {
        // The DFU partition is a page larger, which the swap needs.
        self.active.len() as u32
    }

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
        self.dfu
            .erase(from, to)
            .await
            .map_err(|e| error!("ota: erase: {:?}", Dbg(e)))
    }

    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), ()> {
        self.dfu
            .write(offset, data)
            .await
            .map_err(|e| error!("ota: write: {:?}", Dbg(e)))
    }

    async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ()> {
        let granule = F::READ_SIZE;
        if (offset as usize).is_multiple_of(granule) && buf.len().is_multiple_of(granule) {
            return self
                .dfu
                .read(offset, buf)
                .await
                .map_err(|e| error!("ota: read: {:?}", Dbg(e)));
        }
        // An upload's last block can end off the read granularity (the RP's is 4
        // bytes, its write granularity 1); read whole granules around it.
        let mut at = 0;
        while at < buf.len() {
            let pos = offset as usize + at;
            let from = pos - pos % granule;
            let mut word = Aligned::<F>::new();
            self.dfu
                .read(from as u32, &mut word.buf[..16])
                .await
                .map_err(|e| error!("ota: read: {:?}", Dbg(e)))?;
            let skip = pos - from;
            let n = (16 - skip).min(buf.len() - at);
            buf[at..at + n].copy_from_slice(&word.buf[skip..skip + n]);
            at += n;
        }
        Ok(())
    }

    async fn activate(&mut self, _len: u32) -> Result<(), ()> {
        let mut aligned = Aligned::<F>::new();
        FirmwareState::new(self.state.clone(), aligned.get())
            .mark_updated()
            .await
            .map_err(|e| error!("ota: cannot activate: {:?}", Dbg(e)))
    }

    fn check(&self, prefix: &[u8], board: &str) -> Result<(), &'static str> {
        image::cortex_m(
            prefix,
            self.magic,
            board,
            self.active.clone(),
            self.ram.clone(),
        )
    }

    async fn verify(&mut self, len: u32) -> Result<(), crate::Refusal> {
        let mut vectors = [0u8; 8];
        crate::Slot::read(self, 0, &mut vectors)
            .await
            .map_err(|()| crate::Refusal::Flash)?;
        image::cortex_m_within(vectors, self.active.start, len).map_err(crate::Refusal::Image)
    }

    fn confirm(&self) -> impl Future<Output = Result<(), ()>> + 'static {
        let state = self.state.clone();
        async move {
            let mut aligned = Aligned::<F>::new();
            FirmwareState::new(state, aligned.get())
                .mark_booted()
                .await
                .map_err(|e| error!("ota: cannot confirm the image: {:?}", Dbg(e)))
        }
    }
}
