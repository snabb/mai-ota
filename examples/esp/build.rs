// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! The ESP32 examples' build script: esp-hal's linker script, the credentials,
//! the Wi-Fi network and the version, all fixed at build time.
//!
//! | Variable | |
//! | --- | --- |
//! | `MAI_OTA_DEVICE` | the device identity, a path without extension: `NAME.ccs` and `NAME.cosekey` |
//! | `MAI_OTA_CLIENT` | the client's credential, `client.ccs` |
//! | `WIFI_SSID`, `WIFI_PASSWORD` | the network to join |

include!("../build-common.rs");

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        // The Xtensa image maps IRAM writable and executable by design; GNU ld,
        // which links it, warns about that on every build.
        println!("cargo:rustc-link-arg=-Wl,--no-warn-rwx-segments");
    }

    credentials(&out);
    required("WIFI_SSID");
    required("WIFI_PASSWORD");
    // Optional: one more board whose images this build takes; see main.rs.
    println!("cargo:rerun-if-env-changed=EXAMPLE_ALSO_ACCEPT");

    version(&["../esp"]);
}
