// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The XIAO nRF54LM20A example's build script: the linker scripts, the
//! credentials, the Thread network and the version, all fixed at build time.
//!
//! | Variable | |
//! | --- | --- |
//! | `MAI_OTA_DEVICE` | the device identity, a path without extension: `NAME.ccs` and `NAME.cosekey` |
//! | `MAI_OTA_CLIENT` | the client's credential, `client.ccs` |
//! | `THREAD_DATASET` | the network to join: the active operational dataset in hex, as `ot-ctl dataset active -x` prints it |

include!("../build-common.rs");

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    copy("memory.x".into(), &out, "memory.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    credentials(&out);
    let dataset = required("THREAD_DATASET");
    let dataset = (0..dataset.trim().len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&dataset.trim()[i..i + 2], 16))
        .collect::<Result<Vec<u8>, _>>()
        .expect("THREAD_DATASET is hex");
    fs::write(out.join("dataset.bin"), dataset).unwrap();

    version(&["src"]);
}
