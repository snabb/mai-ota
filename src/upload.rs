// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! Writing an image into the inactive slot, whatever carries it.

use crate::{Event, OnEvent, Slot, activated, image, on_trial, reboot_soon};

/// Largest [`Slot::PREFIX`] an [`Upload`] can check.
const PREFIX_MAX: usize = image::CORTEX_M_PREFIX;

/// Why an upload stopped. Nothing becomes active until [`Upload::commit`]
/// succeeds, so the running image and its fallback are untouched by any of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The running image is on trial; the other slot holds its fallback.
    OnTrial,
    /// Not this boot: an image is already activated and boots next.
    Unavailable,
    /// No upload was begun, or it was aborted.
    NotBegun,
    /// The image does not fit; the slot holds this many bytes.
    TooLarge(u32),
    /// The image is not one this board can run.
    Image(&'static str),
    /// Data arrived out of order.
    OutOfOrder,
    /// The image ended short of, or ran past, the size given to [`Upload::begin`].
    Length,
    /// The flash failed, or read back something other than was written.
    Flash,
}

/// Where a transport puts an image: [`Upload`], or a firmware's own owner of it,
/// such as one that serves several transports and gives the slot to one at a time.
///
/// The calls come in order: [`begin`](Self::begin), [`write`](Self::write)s at
/// increasing offsets, then [`commit`](Self::commit), or [`abort`](Self::abort) at
/// any point.
pub trait Uploader {
    /// Bytes an image may have.
    fn capacity(&self) -> u32;
    /// Start an image of `size` bytes, if the sender said.
    fn begin(&mut self, size: Option<u64>) -> Result<(), Refusal>;
    /// Write `bytes` at `offset`, which is where the last write ended.
    async fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Refusal>;
    /// Finish the image and make it the next to boot.
    async fn commit(&mut self) -> Result<(), Refusal>;
    /// Drop the image in progress.
    fn abort(&mut self);
}

impl<S: Slot> Uploader for Upload<S> {
    fn capacity(&self) -> u32 {
        Upload::capacity(self)
    }
    fn begin(&mut self, size: Option<u64>) -> Result<(), Refusal> {
        Upload::begin(self, size)
    }
    async fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Refusal> {
        Upload::write(self, offset, bytes).await
    }
    async fn commit(&mut self) -> Result<(), Refusal> {
        Upload::commit(self).await
    }
    fn abort(&mut self) {
        Upload::abort(self)
    }
}

/// One image at a time into the inactive slot: erased as it grows, every write
/// read back, the image checked as soon as its prefix is in, activated at the end.
///
/// A new [`begin`](Self::begin) abandons an upload in progress.
pub struct Upload<S> {
    slot: S,
    board: &'static str,
    events: OnEvent,
    /// Bytes written so far.
    len: u32,
    /// Bytes erased so far.
    erased: u32,
    /// The image passed [`Slot::check`].
    checked: bool,
    /// The size given to [`begin`](Self::begin), which the image must have.
    declared: Option<u32>,
    /// [`begin`](Self::begin) succeeded and nothing has ended the upload since.
    open: bool,
    buf: [u8; 256],
}

impl<S: Slot> Upload<S> {
    /// Write uploads for `board` into `slot`.
    pub fn new(slot: S, board: &'static str, events: OnEvent) -> Self {
        const { assert!(S::PREFIX <= PREFIX_MAX && S::WRITE <= 16) };
        Self {
            slot,
            board,
            events,
            len: 0,
            erased: 0,
            checked: false,
            declared: None,
            open: false,
            buf: [0; 256],
        }
    }

    /// Bytes an image may have.
    pub fn capacity(&self) -> u32 {
        self.slot.capacity()
    }

    fn refuse(&self, why: Refusal) -> Refusal {
        (self.events)(Event::Refused);
        why
    }

    /// Start an upload of `size` bytes, if the client said. Given a size, the
    /// upload refuses a write past it, and [`commit`](Self::commit) an image short
    /// of it.
    pub fn begin(&mut self, size: Option<u64>) -> Result<(), Refusal> {
        self.abort();
        if on_trial() {
            warn!("ota: upload refused during a trial boot");
            return Err(self.refuse(Refusal::OnTrial));
        }
        if activated() {
            warn!("ota: upload refused: an image is activated and boots next");
            return Err(self.refuse(Refusal::Unavailable));
        }
        if size.is_some_and(|size| size > u64::from(self.capacity())) {
            return Err(Refusal::TooLarge(self.capacity()));
        }
        // Fits in a u32: it is within the capacity.
        self.declared = size.map(|size| size as u32);
        self.open = true;
        info!("ota: upload started");
        Ok(())
    }

    /// Write `bytes` at `offset`, which must be where the last write ended.
    ///
    /// Only the final write may end off [`Slot::WRITE`]: its tail is padded with
    /// erased bytes.
    pub async fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Refusal> {
        if !self.open {
            return Err(Refusal::NotBegun);
        }
        if offset != self.len as usize {
            return Err(Refusal::OutOfOrder);
        }
        let end = self.len as usize + bytes.len();
        if end > self.capacity() as usize {
            return Err(Refusal::TooLarge(self.capacity()));
        }
        if let Some(declared) = self.declared
            && end > declared as usize
        {
            warn!("ota: data past the declared {} bytes", declared);
            return Err(self.refuse(Refusal::Length));
        }
        let end = end as u32;
        while self.erased < end {
            let to = (self.erased + S::ERASE).min(self.capacity());
            if self.slot.erase(self.erased, to).await.is_err() {
                return Err(self.flash());
            }
            self.erased = to;
        }
        let whole = bytes.len() - bytes.len() % S::WRITE as usize;
        self.store(self.len, &bytes[..whole]).await?;
        if whole < bytes.len() {
            let mut tail = [0xffu8; 16];
            let rest = &bytes[whole..];
            tail[..rest.len()].copy_from_slice(rest);
            self.store(self.len + whole as u32, &tail[..S::WRITE as usize])
                .await?;
        }
        self.len = end;
        if !self.checked && self.len as usize >= S::PREFIX {
            self.check().await?;
        }
        Ok(())
    }

    /// Check the image if that is not done yet, activate it, and reboot into it
    /// [`REBOOT_DELAY`](crate::REBOOT_DELAY) later, through [`supervise`](crate::supervise).
    pub async fn commit(&mut self) -> Result<(), Refusal> {
        if !self.open {
            return Err(Refusal::NotBegun);
        }
        if let Some(declared) = self.declared
            && declared != self.len
        {
            warn!("ota: {} bytes, not the declared {}", self.len, declared);
            return Err(self.refuse(Refusal::Length));
        }
        // The check reads its prefix back from flash, which past what arrived holds
        // only erased bytes; an image that short is no image.
        if (self.len as usize) < S::PREFIX {
            warn!("ota: {} bytes is too short for an image", self.len);
            return Err(self.refuse(Refusal::Image("too short")));
        }
        if !self.checked {
            self.check().await?;
        }
        match self.slot.verify(self.len).await {
            Ok(()) => {}
            Err(Refusal::Image(why)) => {
                warn!("ota: image refused: {}", why);
                return Err(self.refuse(Refusal::Image(why)));
            }
            Err(_) => return Err(self.flash()),
        }
        if self.slot.activate(self.len).await.is_err() {
            return Err(self.flash());
        }
        self.open = false;
        info!("ota: {} bytes written and activated", self.len);
        reboot_soon();
        Ok(())
    }

    /// Drop the upload in progress, if any.
    pub fn abort(&mut self) {
        if self.len > 0 {
            warn!("ota: upload abandoned at {} bytes", self.len);
        }
        self.len = 0;
        self.erased = 0;
        self.checked = false;
        self.declared = None;
        self.open = false;
    }

    fn flash(&self) -> Refusal {
        warn!("ota: flash error");
        self.refuse(Refusal::Flash)
    }

    async fn check(&mut self) -> Result<(), Refusal> {
        let mut prefix = [0u8; PREFIX_MAX];
        let prefix = &mut prefix[..S::PREFIX];
        if self.slot.read(0, prefix).await.is_err() {
            return Err(self.flash());
        }
        if let Err(why) = self.slot.check(prefix, self.board) {
            warn!("ota: image refused: {}", why);
            return Err(self.refuse(Refusal::Image(why)));
        }
        self.checked = true;
        Ok(())
    }

    /// Write `data` at `offset` and read it back.
    async fn store(&mut self, offset: u32, data: &[u8]) -> Result<(), Refusal> {
        if self.slot.write(offset, data).await.is_err() {
            return Err(self.flash());
        }
        let mut at = 0;
        while at < data.len() {
            let n = (data.len() - at).min(self.buf.len());
            if self
                .slot
                .read(offset + at as u32, &mut self.buf[..n])
                .await
                .is_err()
            {
                return Err(self.flash());
            }
            if self.buf[..n] != data[at..at + n] {
                warn!("ota: read-back mismatch at {}", offset as usize + at);
                return Err(self.refuse(Refusal::Flash));
            }
            at += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec;
    use std::vec::Vec;

    use super::*;
    use crate::test_clock::exclusive;

    const GOOD: &[u8; 8] = b"GOODIMG!";

    /// 256 bytes of NOR flash: erases to 0xff, writes only clear bits.
    struct Mock {
        mem: Vec<u8>,
        activated: Vec<u32>,
        /// Flip a bit in what is read back.
        corrupt_reads: bool,
        /// What [`Slot::verify`] says.
        verdict: Result<(), Refusal>,
    }

    impl Mock {
        fn new() -> Self {
            Self {
                mem: vec![0; 256],
                activated: Vec::new(),
                corrupt_reads: false,
                verdict: Ok(()),
            }
        }
    }

    impl Slot for Mock {
        const ERASE: u32 = 64;
        const WRITE: u32 = 4;
        const PREFIX: usize = 8;

        fn capacity(&self) -> u32 {
            self.mem.len() as u32
        }
        async fn erase(&mut self, from: u32, to: u32) -> Result<(), ()> {
            self.mem[from as usize..to as usize].fill(0xff);
            Ok(())
        }
        async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), ()> {
            for (cell, byte) in self.mem[offset as usize..].iter_mut().zip(data) {
                *cell &= byte;
            }
            Ok(())
        }
        async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ()> {
            buf.copy_from_slice(&self.mem[offset as usize..offset as usize + buf.len()]);
            if self.corrupt_reads {
                buf[0] ^= 1;
            }
            Ok(())
        }
        async fn activate(&mut self, len: u32) -> Result<(), ()> {
            self.activated.push(len);
            Ok(())
        }
        fn check(&self, prefix: &[u8], _: &str) -> Result<(), &'static str> {
            (prefix == GOOD).then_some(()).ok_or("not good")
        }
        async fn verify(&mut self, _: u32) -> Result<(), Refusal> {
            self.verdict
        }
        fn confirm(&self) -> impl Future<Output = Result<(), ()>> + 'static {
            core::future::ready(Ok(()))
        }
    }

    fn upload() -> Upload<Mock> {
        Upload::new(Mock::new(), "board", crate::no_events)
    }

    fn image(len: usize) -> Vec<u8> {
        let mut image: Vec<u8> = (0..len).map(|i| i as u8).collect();
        image[..8].copy_from_slice(GOOD);
        image
    }

    /// Blocks of 32 bytes, as Block1 would deliver them, then commit. What a
    /// commit leaves in the boot state is withdrawn, for the tests that share it.
    fn send(upload: &mut Upload<Mock>, image: &[u8]) -> Result<(), Refusal> {
        let sent = embassy_futures::block_on(async {
            upload.begin(Some(image.len() as u64))?;
            for (i, block) in image.chunks(32).enumerate() {
                upload.write(i * 32, block).await?;
            }
            upload.commit().await
        });
        crate::test_reset();
        sent
    }

    #[test]
    fn writes_reads_back_and_activates_once() {
        let _clock = exclusive();
        let mut upload = upload();
        let image = image(100);
        assert_eq!(send(&mut upload, &image), Ok(()));
        assert_eq!(&upload.slot.mem[..100], &image[..]);
        assert_eq!(upload.slot.activated, [100]);
    }

    #[test]
    fn holds_the_image_to_its_declared_size() {
        let _clock = exclusive();
        let image = image(100);
        // Short: nothing activates.
        let mut short = upload();
        let result = embassy_futures::block_on(async {
            short.begin(Some(120))?;
            short.write(0, &image).await?;
            short.commit().await
        });
        assert_eq!(result, Err(Refusal::Length));
        assert!(short.slot.activated.is_empty());
        // Long: the write past the size is refused.
        let mut long = upload();
        let result = embassy_futures::block_on(async {
            long.begin(Some(64))?;
            long.write(0, &image[..64]).await?;
            long.write(64, &image[64..]).await
        });
        assert_eq!(result, Err(Refusal::Length));
        // No size: whatever arrived is the image.
        let mut open = upload();
        let result = embassy_futures::block_on(async {
            open.begin(None)?;
            open.write(0, &image).await?;
            open.commit().await
        });
        crate::test_reset();
        assert_eq!(result, Ok(()));
        assert_eq!(open.slot.activated, [100]);
    }

    #[test]
    fn pads_a_tail_off_the_write_size() {
        let _clock = exclusive();
        let mut upload = upload();
        let image = image(37);
        assert_eq!(send(&mut upload, &image), Ok(()));
        assert_eq!(&upload.slot.mem[..37], &image[..]);
        assert_eq!(upload.slot.mem[37..40], [0xff; 3]);
    }

    #[test]
    fn refuses_during_a_trial() {
        let _clock = exclusive();
        crate::start_trial();
        let refused = upload().begin(None);
        crate::test_reset();
        assert_eq!(refused, Err(Refusal::OnTrial));
    }

    #[test]
    fn refuses_a_second_image_once_one_is_activated() {
        let _clock = exclusive();
        let mut upload = upload();
        let image = image(100);
        let second = embassy_futures::block_on(async {
            upload.begin(None)?;
            upload.write(0, &image).await?;
            upload.commit().await?;
            upload.begin(None)
        });
        crate::test_reset();
        assert_eq!(second, Err(Refusal::Unavailable));
        assert_eq!(upload.slot.activated, [100]);
    }

    #[test]
    fn refuses_writes_and_commits_without_a_begin() {
        let _clock = exclusive();
        let mut upload = upload();
        let image = image(100);
        let write = embassy_futures::block_on(upload.write(0, &image));
        assert_eq!(write, Err(Refusal::NotBegun));
        // A refused begin opens nothing either.
        assert_eq!(upload.begin(Some(257)), Err(Refusal::TooLarge(256)));
        assert_eq!(
            embassy_futures::block_on(upload.commit()),
            Err(Refusal::NotBegun)
        );
        assert!(upload.slot.activated.is_empty());
    }

    #[test]
    fn refuses_an_image_shorter_than_its_prefix() {
        let _clock = exclusive();
        let mut upload = upload();
        assert_eq!(
            send(&mut upload, &image(8)[..4]),
            Err(Refusal::Image("too short"))
        );
        assert!(upload.slot.activated.is_empty());
    }

    #[test]
    fn refuses_an_image_that_does_not_verify() {
        let _clock = exclusive();
        let mut upload = upload();
        upload.slot.verdict = Err(Refusal::Image("truncated"));
        assert_eq!(
            send(&mut upload, &image(100)),
            Err(Refusal::Image("truncated"))
        );
        assert!(upload.slot.activated.is_empty());
        // A flash that will not read back says nothing about the image.
        let mut upload = self::upload();
        upload.slot.verdict = Err(Refusal::Flash);
        assert_eq!(send(&mut upload, &image(100)), Err(Refusal::Flash));
        assert!(upload.slot.activated.is_empty());
    }

    #[test]
    fn refuses_what_does_not_fit() {
        let _clock = exclusive();
        let mut upload = upload();
        assert_eq!(upload.begin(Some(257)), Err(Refusal::TooLarge(256)));
        // A client that gives no size learns at the block that crosses the end.
        upload.begin(None).unwrap();
        let too_far = embassy_futures::block_on(async {
            for i in 0..8 {
                upload.write(i * 32, &image(256)[i * 32..][..32]).await?;
            }
            upload.write(256, &[0; 32]).await
        });
        assert_eq!(too_far, Err(Refusal::TooLarge(256)));
        assert!(upload.slot.activated.is_empty());
    }

    #[test]
    fn refuses_blocks_out_of_order() {
        let _clock = exclusive();
        let mut upload = upload();
        upload.begin(None).unwrap();
        let skipped = embassy_futures::block_on(upload.write(32, &[0; 32]));
        assert_eq!(skipped, Err(Refusal::OutOfOrder));
    }

    #[test]
    fn refuses_another_image_and_activates_nothing() {
        let _clock = exclusive();
        let mut upload = upload();
        let mut image = image(100);
        image[0] = b'B';
        assert_eq!(send(&mut upload, &image), Err(Refusal::Image("not good")));
        assert!(upload.slot.activated.is_empty());
    }

    #[test]
    fn a_read_back_mismatch_fails_and_activates_nothing() {
        let _clock = exclusive();
        let mut upload = upload();
        upload.slot.corrupt_reads = true;
        assert_eq!(send(&mut upload, &image(100)), Err(Refusal::Flash));
        assert!(upload.slot.activated.is_empty());
    }
}
