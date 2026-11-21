// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! ESP-IDF's `otadata` partition for two OTA slots, as its bootloader reads it.
//!
//! Two 32-byte entries, one at the start of each 4 KiB sector: `ota_seq`, a 20-byte
//! label, `ota_state`, and a CRC-32 of `ota_seq`, all little-endian. An entry is
//! valid when its sequence is not erased and its CRC matches. The bootloader boots
//! slot `(ota_seq - 1) % 2` of the valid entry with the higher sequence, skipping
//! one whose state is invalid or aborted.
//!
//! Pure functions over the two entries, so the board only reads and writes them.

/// Bytes in an entry.
pub const ENTRY_LEN: usize = 32;
/// Offset of entry `i` in the partition is `i * SECTOR`.
pub const SECTOR: u32 = 0x1000;

/// Set with a new selection: the bootloader turns it into [`PENDING_VERIFY`].
pub const NEW: u32 = 0;
/// Booted on trial; the application must confirm or reject it.
pub const PENDING_VERIFY: u32 = 1;
/// Confirmed.
pub const VALID: u32 = 2;
/// Rejected by the application.
pub const INVALID: u32 = 3;
/// Still on trial at the next boot; the bootloader gave up on it.
pub const ABORTED: u32 = 4;

const ERASED: u32 = u32::MAX;

/// `esp_rom_crc32_le(UINT32_MAX, &ota_seq, 4)`, which the bootloader checks.
fn crc(seq: u32) -> u32 {
    let mut crc = 0u32;
    for byte in seq.to_le_bytes() {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn word(entry: &[u8; ENTRY_LEN], at: usize) -> u32 {
    u32::from_le_bytes([entry[at], entry[at + 1], entry[at + 2], entry[at + 3]])
}

/// `(seq, state)` of an entry with a sequence and a matching CRC.
fn parse(entry: &[u8; ENTRY_LEN]) -> Option<(u32, u32)> {
    let seq = word(entry, 0);
    (seq != ERASED && word(entry, 28) == crc(seq)).then(|| (seq, word(entry, 24)))
}

/// Slot `(ota_seq - 1) % 2`.
fn slot_of(seq: u32) -> u8 {
    (seq.wrapping_sub(1) % 2) as u8
}

/// The slot and state of the latest selection, whether or not the bootloader
/// would still boot it: after a rollback this is the rejected image's entry.
/// `None` when neither entry is valid, as after a USB flash that erased it.
pub fn latest(entries: &[[u8; ENTRY_LEN]; 2]) -> Option<(u8, u32)> {
    entries
        .iter()
        .filter_map(parse)
        .max_by_key(|&(seq, _)| seq)
        .map(|(seq, state)| (slot_of(seq), state))
}

/// The entry that makes `slot` (0 or 1) the next to boot, in `state`, and its index.
///
/// Sequence and state go into one entry, as ESP-IDF's `esp_ota_set_boot_partition`
/// writes them. Written apart, a power loss between the two leaves the new sequence
/// beside the entry's previous state, which may be valid and skip the trial.
///
/// The sequence is past every valid entry's, so the new entry is the latest
/// whichever state the other holds. It replaces the entry the bootloader would boot
/// now only when there is none, so a write that fails halfway leaves that one.
pub fn select(entries: &[[u8; ENTRY_LEN]; 2], slot: u8, state: u32) -> (usize, [u8; ENTRY_LEN]) {
    debug_assert!(slot < 2);
    let parsed = entries.each_ref().map(parse);
    let bootable = |i: usize| parsed[i].is_some_and(|(_, s)| s != INVALID && s != ABORTED);
    let active = (0..2)
        .filter(|&i| bootable(i))
        .max_by_key(|&i| parsed[i].map(|(seq, _)| seq));
    let index = active.map_or(0, |i| 1 - i);
    let base = parsed
        .iter()
        .flatten()
        .map(|&(seq, _)| seq)
        .max()
        .unwrap_or(0);
    let mut seq = base + 1;
    if slot_of(seq) != slot {
        seq += 1;
    }
    let mut entry = [0xff; ENTRY_LEN];
    entry[0..4].copy_from_slice(&seq.to_le_bytes());
    entry[24..28].copy_from_slice(&state.to_le_bytes());
    entry[28..32].copy_from_slice(&crc(seq).to_le_bytes());
    (index, entry)
}

/// The latest entry in `state`, and its index: how the running image confirms
/// itself, in place, as ESP-IDF's `esp_ota_mark_app_valid_cancel_rollback` does.
///
/// A write that tears leaves that entry invalid, and the bootloader falls back to
/// the other one, the previous image. `None` when neither entry is valid.
pub fn with_state(entries: &[[u8; ENTRY_LEN]; 2], state: u32) -> Option<(usize, [u8; ENTRY_LEN])> {
    let index = (0..2)
        .filter_map(|i| parse(&entries[i]).map(|(seq, _)| (i, seq)))
        .max_by_key(|&(_, seq)| seq)?
        .0;
    let mut entry = entries[index];
    entry[24..28].copy_from_slice(&state.to_le_bytes());
    Some((index, entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: u32, state: u32) -> [u8; ENTRY_LEN] {
        let mut entry = [0xff; ENTRY_LEN];
        entry[0..4].copy_from_slice(&seq.to_le_bytes());
        entry[24..28].copy_from_slice(&state.to_le_bytes());
        entry[28..32].copy_from_slice(&crc(seq).to_le_bytes());
        entry
    }

    const ERASED_ENTRY: [u8; ENTRY_LEN] = [0xff; ENTRY_LEN];

    #[test]
    fn crc_matches_esp_idf() {
        // zlib.crc32(struct.pack('<I', seq), 0xffffffff)
        assert_eq!(crc(1), 0x4743_989a);
        assert_eq!(crc(2), 0x55f6_3774);
        assert_eq!(crc(0x1234_5678), 0x71d6_a731);
    }

    #[test]
    fn latest_is_the_higher_valid_sequence() {
        assert_eq!(latest(&[ERASED_ENTRY; 2]), None);
        assert_eq!(latest(&[entry(1, VALID), ERASED_ENTRY]), Some((0, VALID)));
        assert_eq!(
            latest(&[entry(3, VALID), entry(4, PENDING_VERIFY)]),
            Some((1, PENDING_VERIFY))
        );
        // After a rollback the rejected entry is still the latest.
        assert_eq!(
            latest(&[entry(3, VALID), entry(4, ABORTED)]),
            Some((1, ABORTED))
        );
    }

    #[test]
    fn latest_ignores_a_torn_entry() {
        let mut torn = entry(4, NEW);
        torn[28] ^= 1;
        assert_eq!(latest(&[entry(3, VALID), torn]), Some((0, VALID)));
    }

    #[test]
    fn select_writes_sequence_and_state_together() {
        let entries = [entry(1, VALID), ERASED_ENTRY];
        let (index, new) = select(&entries, 1, NEW);
        assert_eq!(index, 1);
        assert_eq!(new, entry(2, NEW));
        let mut after = entries;
        after[index] = new;
        assert_eq!(latest(&after), Some((1, NEW)));
    }

    #[test]
    fn select_keeps_the_bootable_entry() {
        // Slot 0 runs from entry 1 (seq 3); entry 0 holds an older selection.
        let entries = [entry(2, VALID), entry(3, VALID)];
        let (index, new) = select(&entries, 1, NEW);
        assert_eq!(index, 0);
        assert_eq!(new, entry(4, NEW));
    }

    #[test]
    fn select_after_a_rollback_passes_the_rejected_sequence() {
        // Slot 1 (seq 4) was aborted; slot 0 runs from seq 3. Pointing back at slot 0
        // must outrank seq 4, and must not overwrite the bootable seq 3.
        let entries = [entry(3, VALID), entry(4, ABORTED)];
        let (index, new) = select(&entries, 0, VALID);
        assert_eq!(index, 1);
        assert_eq!(new, entry(5, VALID));
    }

    #[test]
    fn confirming_rewrites_the_trial_entry_in_place() {
        let entries = [entry(5, PENDING_VERIFY), entry(4, VALID)];
        assert_eq!(with_state(&entries, VALID), Some((0, entry(5, VALID))));
        assert_eq!(with_state(&[ERASED_ENTRY; 2], VALID), None);
    }

    #[test]
    fn select_on_erased_otadata() {
        let (index, new) = select(&[ERASED_ENTRY; 2], 0, VALID);
        assert_eq!((index, new), (0, entry(1, VALID)));
        let (index, new) = select(&[ERASED_ENTRY; 2], 1, VALID);
        assert_eq!((index, new), (0, entry(2, VALID)));
    }
}
