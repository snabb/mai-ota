// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The application image to upload, from the firmware's ELF.

use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};

use object::{
    Endianness, FileKind, Object, ObjectSection,
    elf::{EM_ARM, EM_RISCV, EM_XTENSA, PT_LOAD},
    read::elf::{ElfFile32, FileHeader, ProgramHeader},
};

use crate::Failure;

/// Erased bootloader state that an embassy-boot application carries for wired
/// flashes; it lies outside the image.
const BOOTLOADER_STATE: &str = ".bootloader_state";

/// The image for `elf`. An ESP32 ELF goes through `espflash save-image` for `chip`,
/// or for the chip its architecture implies; a Cortex-M one is its loadable bytes
/// from the lowest load address up, as `objcopy -O binary` makes them.
pub fn from_elf(elf: &Path, chip: Option<&str>) -> Result<Vec<u8>, Failure> {
    let data = std::fs::read(elf).map_err(|e| Failure::local(format!("{}: {e}", elf.display())))?;
    let not_elf = || Failure::local(format!("{}: not a 32-bit ELF file", elf.display()));
    if FileKind::parse(&*data).map_err(|_| not_elf())? != FileKind::Elf32 {
        return Err(not_elf());
    }
    let file = ElfFile32::<Endianness>::parse(&*data).map_err(|_| not_elf())?;
    let machine = file.elf_header().e_machine(file.endian());
    let chip = match (machine, chip) {
        (EM_ARM, None) => {
            return cortex_m(&file).map_err(|e| Failure::local(format!("{}: {e}", elf.display())));
        }
        (EM_ARM, Some(_)) => return Err(Failure::local("--chip is for ESP32 images")),
        // espflash writes the chip given into the image header, which is what the
        // device checks; an ELF for the other architecture must not get that far.
        (EM_RISCV | EM_XTENSA, Some(chip)) => {
            let xtensa = matches!(chip, "esp32" | "esp32s2" | "esp32s3");
            if xtensa != (machine == EM_XTENSA) {
                return Err(Failure::local(format!(
                    "{}: a {} ELF, not one for {chip}",
                    elf.display(),
                    if machine == EM_XTENSA {
                        "Xtensa"
                    } else {
                        "RISC-V"
                    }
                )));
            }
            chip
        }
        // The ESP32 chips mai-ota has a slot for.
        (EM_RISCV, None) => "esp32c6",
        (EM_XTENSA, None) => "esp32s3",
        _ => {
            return Err(Failure::local(format!(
                "{}: machine {machine:#x} has no image format here",
                elf.display()
            )));
        }
    };
    esp(elf, chip)
}

fn esp(elf: &Path, chip: &str) -> Result<Vec<u8>, Failure> {
    // A private directory rather than a fixed name in the shared temp dir, which
    // another user could pre-create as a symlink. Mode 0700 explicitly: tempfile
    // otherwise leaves it to the umask, and the image can embed device keys.
    let mut builder = tempfile::Builder::new();
    builder.permissions(std::fs::Permissions::from_mode(0o700));
    let dir = builder
        .tempdir()
        .map_err(|e| Failure::local(format!("temporary directory: {e}")))?;
    let out = dir.path().join("app.bin");
    let status = Command::new("espflash")
        .args(["save-image", "--chip", chip])
        .arg(elf)
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .status()
        .map_err(|e| Failure::local(format!("espflash: {e}")))?;
    let image = std::fs::read(&out);
    if !status.success() {
        return Err(Failure::local(format!(
            "espflash save-image failed: {status}"
        )));
    }
    image.map_err(|e| Failure::local(format!("{}: {e}", out.display())))
}

fn cortex_m(file: &ElfFile32<'_, Endianness>) -> Result<Vec<u8>, String> {
    let endian = file.endian();
    let skip = file
        .section_by_name(BOOTLOADER_STATE)
        .map(|s| s.address()..s.address() + s.size());
    let mut parts = Vec::new();
    for header in file.elf_program_headers() {
        let len = u64::from(header.p_filesz(endian));
        if header.p_type(endian) != PT_LOAD || len == 0 {
            continue;
        }
        let at = u64::from(header.p_paddr(endian));
        if let Some(skip) = &skip
            && at < skip.end
            && skip.start < at + len
        {
            if skip.start <= at && at + len <= skip.end {
                continue;
            }
            // Dropping the segment would drop the rest; keeping it, the state.
            return Err(format!(
                "{BOOTLOADER_STATE} shares a load segment with other data; give it a \
                 memory region of its own"
            ));
        }
        let bytes = header
            .data(endian, file.data())
            .map_err(|_| "a segment lies outside the file")?;
        parts.push((at, bytes));
    }
    image(parts)
}

/// Load-address-ordered `parts` as one image, gaps filled with zeros.
fn image(mut parts: Vec<(u64, &[u8])>) -> Result<Vec<u8>, String> {
    parts.sort_by_key(|&(at, _)| at);
    let Some(&(start, _)) = parts.first() else {
        return Err("nothing to load".into());
    };
    let mut image = Vec::new();
    for (at, bytes) in parts {
        let offset = usize::try_from(at - start).map_err(|_| "segments too far apart")?;
        if offset < image.len() {
            return Err(format!("segments overlap at {at:#x}"));
        }
        image.resize(offset, 0);
        image.extend_from_slice(bytes);
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_are_placed_by_load_address_with_zeroed_gaps() {
        let parts = vec![(0x1008, &[3u8, 4][..]), (0x1000, &[1u8, 2][..])];
        assert_eq!(image(parts).unwrap(), [1, 2, 0, 0, 0, 0, 0, 0, 3, 4]);
    }

    #[test]
    fn overlapping_segments_are_refused() {
        let parts = vec![(0x1000, &[1u8, 2, 3][..]), (0x1002, &[4u8][..])];
        assert!(image(parts).is_err());
    }
}
