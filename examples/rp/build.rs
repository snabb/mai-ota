// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The Pico examples' build script: the linker scripts, the credentials, the
//! Wi-Fi network and the version, all fixed at build time.
//!
//! | Variable | |
//! | --- | --- |
//! | `MAI_OTA_DEVICE` | the device identity, a path without extension: `NAME.ccs` and `NAME.cosekey` |
//! | `MAI_OTA_CLIENT` | the client's credential, `client.ccs` |
//! | `WIFI_SSID`, `WIFI_PASSWORD` | the network to join |

include!("../build-common.rs");

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());

    copy("memory.x".into(), &out, "memory.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");
    println!("cargo:rustc-link-arg-bins=-Tdefmt.x");

    credentials(&out);
    required("WIFI_SSID");
    required("WIFI_PASSWORD");

    version(&["../rp"]);
}
