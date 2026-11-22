// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! Which board an uploaded image is for, read from the image itself.
//!
//! An ESP-IDF image already names it, in its app descriptor. A Cortex-M image
//! carries an [`ImageId`] that [`image_id!`](crate::image_id) puts there.

use crate::Refusal;

/// Bytes [`esp_descriptor`] needs: the image header, the first segment's header
/// and the app descriptor up to the end of its project name.
pub const ESP_PREFIX: usize = 112;

/// What an ESP image's app descriptor says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EspDescriptor<'a> {
    /// The project name, which `esp_app_desc!()` fills with the package name.
    pub project: &'a [u8],
    /// The version, which `esp_app_desc!()` fills with the package version.
    pub version: &'a [u8],
}

/// The app descriptor of an ESP-IDF application image for chip `chip_id`, with
/// the checks every image must pass: the image magic, the chip and the
/// descriptor's magic. Policy about the project and version is the caller's.
///
/// Layout: the 24-byte image header (magic 0xE9 at 0, the segment count at 1,
/// the chip ID at 12), the first segment's 8-byte header, then the app
/// descriptor (magic 0xABCD5432 at 32, the version at 48..80 and the project
/// name at 80..112, each NUL-padded).
pub fn esp_descriptor(prefix: &[u8], chip_id: u16) -> Result<EspDescriptor<'_>, &'static str> {
    if prefix.len() < ESP_PREFIX || prefix[0] != 0xe9 || !(1..=16).contains(&prefix[1]) {
        return Err("not an ESP application image");
    }
    if u16::from_le_bytes([prefix[12], prefix[13]]) != chip_id {
        return Err("built for another chip");
    }
    if prefix[32..36] != 0xabcd_5432u32.to_le_bytes() {
        return Err("no app descriptor");
    }
    Ok(EspDescriptor {
        project: until_nul(&prefix[80..112]),
        version: until_nul(&prefix[48..80]),
    })
}

/// Whether the first `len` bytes hold all of an ESP-IDF image, whose header
/// [`esp_descriptor`] already accepted, reading it through `read`.
///
/// After the 24-byte header come the segments, each an 8-byte header holding its
/// length, then its data. The checksum byte ends the next 16-byte boundary, and a
/// SHA-256 follows when header byte 23 says it was appended. The bootloader checks
/// those two; this only keeps an upload that was cut short from being activated.
///
/// [`Refusal::Flash`] when `read` failed, which says nothing about the image.
pub async fn esp_within(
    len: u32,
    mut read: impl AsyncFnMut(u32, &mut [u8]) -> Result<(), ()>,
) -> Result<(), Refusal> {
    const PAST: Refusal = Refusal::Image("the image runs past the end of the upload");
    let mut header = [0u8; 24];
    read(0, &mut header).await.map_err(|()| Refusal::Flash)?;
    let mut at: u32 = 24;
    for _ in 0..header[1] {
        if at + 8 > len {
            return Err(PAST);
        }
        let mut segment = [0u8; 8];
        read(at, &mut segment).await.map_err(|()| Refusal::Flash)?;
        let size = u32::from_le_bytes([segment[4], segment[5], segment[6], segment[7]]);
        match (at + 8).checked_add(size) {
            Some(end) if end <= len => at = end,
            _ => return Err(PAST),
        }
    }
    let digest = if header[23] == 1 { 32 } else { 0 };
    match (at + 1).next_multiple_of(16).checked_add(digest) {
        Some(end) if end <= len => Ok(()),
        _ => Err(PAST),
    }
}

/// Whether a Cortex-M image's reset vector, in `vectors` (its first 8 bytes), lies
/// within the `len` bytes that arrived of an image running from `start`.
/// [`cortex_m`] already placed it in the slot; a body cut short can still pass that.
pub fn cortex_m_within(vectors: [u8; 8], start: u32, len: u32) -> Result<(), &'static str> {
    let reset = u32::from_le_bytes([vectors[4], vectors[5], vectors[6], vectors[7]]) & !1;
    if start.checked_add(len).is_none_or(|end| reset >= end) {
        return Err("its reset vector lies past the end of the upload");
    }
    Ok(())
}

/// The default ESP policy: the project is `board`.
pub fn same_board(descriptor: &EspDescriptor<'_>, board: &str) -> Result<(), &'static str> {
    if descriptor.project != board.as_bytes() {
        return Err("built for another board");
    }
    Ok(())
}

fn until_nul(field: &[u8]) -> &[u8] {
    &field[..field.iter().position(|&b| b == 0).unwrap_or(field.len())]
}

/// Bytes [`cortex_m`] needs: room for the largest vector table here, the
/// nRF54LM20A's 1224 bytes, and the [`ImageId`] after it.
pub const CORTEX_M_PREFIX: usize = 2048;

/// Where a Cortex-M image names its board: an 8-byte magic chosen by the
/// firmware, then the board's name, NUL-padded. The board's linker script puts
/// section `.mai_ota_image_id` straight after the vector table, 32-byte
/// aligned; see [`image_id!`](crate::image_id).
///
/// The magic is what [`cortex_m`] looks for, so a firmware keeps it for good:
/// a board already in the field refuses an image whose record it cannot find.
#[repr(C, align(32))]
pub struct ImageId(pub [u8; 32]);

impl ImageId {
    /// The record for `board`.
    ///
    /// # Panics
    ///
    /// At build time, when `board` is longer than 24 bytes.
    pub const fn new(magic: [u8; 8], board: &str) -> Self {
        let board = board.as_bytes();
        assert!(board.len() <= 24, "board name too long for the image id");
        let mut id = [0u8; 32];
        let mut i = 0;
        while i < 8 {
            id[i] = magic[i];
            i += 1;
        }
        let mut i = 0;
        while i < board.len() {
            id[8 + i] = board[i];
            i += 1;
        }
        Self(id)
    }
}

/// Put this board's [`ImageId`], with `magic`, into the image. Invoke once, in
/// the board crate, whose package name it records.
#[macro_export]
macro_rules! image_id {
    ($magic:expr) => {
        #[used]
        #[unsafe(link_section = ".mai_ota_image_id")]
        static MAI_OTA_IMAGE_ID: $crate::image::ImageId =
            $crate::image::ImageId::new($magic, env!("CARGO_PKG_NAME"));
    };
}

/// A Cortex-M image for `board`, with the [`ImageId`] that starts with `magic`,
/// that runs from `slot` with its stack in `ram`.
///
/// Checks the vector table's initial stack pointer and reset vector, then
/// looks for the [`ImageId`] at 32-byte boundaries within `prefix`.
pub fn cortex_m(
    prefix: &[u8],
    magic: [u8; 8],
    board: &str,
    slot: core::ops::Range<u32>,
    ram: core::ops::RangeInclusive<u32>,
) -> Result<(), &'static str> {
    let word = |at: usize| {
        prefix
            .get(at..at + 4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
    };
    let (Some(sp), Some(reset)) = (word(0), word(4)) else {
        return Err("too short for a vector table");
    };
    if !ram.contains(&sp) || reset & 1 == 0 || !slot.contains(&(reset & !1)) {
        return Err("not an image for this slot");
    }
    let id = prefix
        .as_chunks::<32>()
        .0
        .iter()
        .find(|record| record[..8] == magic)
        .ok_or("no image id")?;
    let name = &id[8..];
    let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
    if name != board.as_bytes() {
        return Err("built for another board");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    const SLOT: core::ops::Range<u32> = 0xa000..0xfb000;
    const RAM: core::ops::RangeInclusive<u32> = 0x2000_0000..=0x2007_fd40;
    const MAGIC: [u8; 8] = *b"TESTIMID";

    fn cortex_m_image(
        sp: u32,
        reset: u32,
        id_at: Option<usize>,
        board: &str,
    ) -> [u8; CORTEX_M_PREFIX] {
        let mut prefix = [0u8; CORTEX_M_PREFIX];
        prefix[0..4].copy_from_slice(&sp.to_le_bytes());
        prefix[4..8].copy_from_slice(&reset.to_le_bytes());
        if let Some(at) = id_at {
            prefix[at..at + 32].copy_from_slice(&ImageId::new(MAGIC, board).0);
        }
        prefix
    }

    #[test]
    fn cortex_m_refuses_another_magic() {
        let image = cortex_m_image(0x2007_fd40, 0xa4c9, Some(1248), "pico");
        assert!(cortex_m(&image, *b"OTHERMAG", "pico", SLOT, RAM).is_err());
    }

    #[test]
    fn cortex_m_accepts_this_board() {
        let image = cortex_m_image(0x2007_fd40, 0xa4c9, Some(1248), "xiao-nrf54lm20a");
        assert_eq!(
            cortex_m(&image, MAGIC, "xiao-nrf54lm20a", SLOT, RAM),
            Ok(())
        );
    }

    #[test]
    fn cortex_m_refuses_another_board() {
        let image = cortex_m_image(0x2007_fd40, 0xa4c9, Some(1248), "pico");
        assert!(cortex_m(&image, MAGIC, "pico-2", SLOT, RAM).is_err());
        let image = cortex_m_image(0x2007_fd40, 0xa4c9, Some(1248), "pico-2");
        assert!(cortex_m(&image, MAGIC, "pico", SLOT, RAM).is_err());
    }

    #[test]
    fn cortex_m_refuses_a_bad_vector_table() {
        let board = "xiao-nrf54lm20a";
        // Linked for address 0, as before the bootloader.
        let image = cortex_m_image(0x2007_fd40, 0x04c9, Some(1248), board);
        assert!(cortex_m(&image, MAGIC, board, SLOT, RAM).is_err());
        // Not Thumb.
        let image = cortex_m_image(0x2007_fd40, 0xa4c8, Some(1248), board);
        assert!(cortex_m(&image, MAGIC, board, SLOT, RAM).is_err());
        let image = cortex_m_image(0x1000_0000, 0xa4c9, Some(1248), board);
        assert!(cortex_m(&image, MAGIC, board, SLOT, RAM).is_err());
        assert!(cortex_m(&image[..4], MAGIC, board, SLOT, RAM).is_err());
    }

    #[test]
    fn cortex_m_needs_the_id_where_it_belongs() {
        let board = "xiao-nrf54lm20a";
        assert!(
            cortex_m(
                &cortex_m_image(0x2007_fd40, 0xa4c9, None, board),
                MAGIC,
                board,
                SLOT,
                RAM
            )
            .is_err()
        );
        // Off a 32-byte boundary, so not where the linker script puts it.
        let image = cortex_m_image(0x2007_fd40, 0xa4c9, Some(1240), board);
        assert!(cortex_m(&image, MAGIC, board, SLOT, RAM).is_err());
    }

    fn image(chip: u16, project: &str) -> [u8; ESP_PREFIX] {
        let mut prefix = [0u8; ESP_PREFIX];
        prefix[0] = 0xe9;
        prefix[1] = 4;
        prefix[12..14].copy_from_slice(&chip.to_le_bytes());
        prefix[32..36].copy_from_slice(&0xabcd_5432u32.to_le_bytes());
        prefix[80..80 + project.len()].copy_from_slice(project.as_bytes());
        prefix
    }

    /// The default policy on top of the generic checks.
    fn esp(prefix: &[u8], chip_id: u16, board: &str) -> Result<(), &'static str> {
        same_board(&esp_descriptor(prefix, chip_id)?, board)
    }

    #[test]
    fn accepts_this_board() {
        assert_eq!(esp(&image(13, "unit-c6l"), 13, "unit-c6l"), Ok(()));
    }

    #[test]
    fn refuses_another_board_or_chip() {
        assert!(esp(&image(9, "heltec-wifi-lora-32-v3"), 9, "xiao-esp32s3").is_err());
        assert!(esp(&image(9, "unit-c6l"), 13, "unit-c6l").is_err());
        // The ESP32's chip ID is 0, which must not read as "any chip".
        assert!(esp(&image(9, "atom-lite"), 0, "atom-lite").is_err());
        assert!(esp(&image(0, "atom-lite"), 9, "atom-lite").is_err());
        assert_eq!(esp(&image(0, "atom-lite"), 0, "atom-lite"), Ok(()));
        // A name that merely starts with the board's must not pass.
        assert!(esp(&image(13, "unit-c6l-x"), 13, "unit-c6l").is_err());
    }

    #[test]
    fn reads_the_project_and_version_for_a_policy() {
        let mut prefix = image(13, "lamp-c6-thread");
        prefix[48..53].copy_from_slice(b"1.4.2");
        assert_eq!(
            esp_descriptor(&prefix, 13),
            Ok(EspDescriptor {
                project: b"lamp-c6-thread",
                version: b"1.4.2",
            })
        );
        // The generic checks stand whatever the policy.
        assert!(esp_descriptor(&prefix, 9).is_err());
    }

    #[test]
    fn refuses_what_is_not_an_image() {
        assert!(esp(&[0u8; ESP_PREFIX], 13, "unit-c6l").is_err());
        assert!(esp(&image(13, "unit-c6l")[..64], 13, "unit-c6l").is_err());
        let mut no_desc = image(13, "unit-c6l");
        no_desc[32] = 0;
        assert!(esp(&no_desc, 13, "unit-c6l").is_err());
    }

    /// An ESP image of segments with these data lengths, padded to its checksum
    /// byte, with a SHA-256 when `digest`.
    fn segmented(sizes: &[u32], digest: bool) -> std::vec::Vec<u8> {
        let mut image = std::vec![0u8; 24];
        image[0] = 0xe9;
        image[1] = sizes.len() as u8;
        image[23] = u8::from(digest);
        for &size in sizes {
            image.extend_from_slice(&0x4200_0000u32.to_le_bytes());
            image.extend_from_slice(&size.to_le_bytes());
            image.resize(image.len() + size as usize, 0x5a);
        }
        image.resize((image.len() + 1).next_multiple_of(16), 0);
        if digest {
            image.resize(image.len() + 32, 0);
        }
        image
    }

    fn within(image: &[u8], len: usize) -> Result<(), Refusal> {
        let within =
            embassy_futures::block_on(esp_within(len as u32, async |offset, buf: &mut [u8]| {
                let at = offset as usize;
                buf.copy_from_slice(image.get(at..at + buf.len()).ok_or(())?);
                Ok(())
            }));
        assert_ne!(within, Err(Refusal::Flash), "the test image reads");
        within
    }

    #[test]
    fn a_cortex_m_reset_vector_must_be_in_what_arrived() {
        let mut vectors = [0u8; 8];
        vectors[4..].copy_from_slice(&0xa4c9u32.to_le_bytes());
        assert_eq!(cortex_m_within(vectors, 0xa000, 0x4ca), Ok(()));
        assert!(cortex_m_within(vectors, 0xa000, 0x4c8).is_err());
        assert!(cortex_m_within(vectors, u32::MAX - 4, 0x10).is_err());
    }

    #[test]
    fn a_whole_esp_image_is_within_its_upload() {
        for digest in [false, true] {
            let image = segmented(&[0x40, 0x1000, 0x24], digest);
            assert_eq!(within(&image, image.len()), Ok(()));
        }
    }

    #[test]
    fn an_esp_image_cut_short_is_not() {
        for digest in [false, true] {
            let image = segmented(&[0x40, 0x1000, 0x24], digest);
            // Short of the digest, the checksum, a segment's data and its header.
            for cut in [1, 33, 0x30, 0x1000] {
                let len = image.len() - cut;
                assert!(within(&image[..len], len).is_err(), "cut {cut}");
            }
        }
        // Just the header and descriptor, as a body that ends there would be.
        let image = segmented(&[0x1000], false);
        assert!(within(&image[..112], 112).is_err());
    }

    #[test]
    fn an_esp_segment_length_that_overflows_is_refused() {
        let mut image = segmented(&[0x40], false);
        image[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(within(&image, image.len()).is_err());
    }
}
