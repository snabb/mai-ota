// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

// What the examples' build scripts share, through `include!`.

use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn required(name: &str) -> String {
    println!("cargo:rerun-if-env-changed={name}");
    env::var(name).unwrap_or_else(|_| panic!("set {name}; see the examples' README"))
}

fn copy(from: PathBuf, out: &Path, name: &str) {
    println!("cargo:rerun-if-changed={}", from.display());
    fs::copy(&from, out.join(name)).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
}

/// `MAI_OTA_DEVICE`'s `.ccs` and `.cosekey`, and `MAI_OTA_CLIENT`, into `out`.
fn credentials(out: &Path) {
    let device = PathBuf::from(required("MAI_OTA_DEVICE"));
    copy(device.with_extension("ccs"), out, "device.ccs");
    copy(device.with_extension("cosekey"), out, "device.cosekey");
    copy(required("MAI_OTA_CLIENT").into(), out, "client.ccs");
}

/// `EXAMPLE_VERSION`: the package version and the build time.
///
/// The build time makes two builds report different versions, which is how the
/// CLI tells a new image from a rollback. `sources` rerun the script on any source
/// change, not only for its inputs, so that a changed image gets a new time.
fn version(sources: &[&str]) {
    for source in sources
        .iter()
        .chain(&["../common/src", "../../src", "Cargo.lock"])
    {
        println!("cargo:rerun-if-changed={source}");
    }
    let built = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    println!(
        "cargo:rustc-env=EXAMPLE_VERSION={} built {built}",
        env::var("CARGO_PKG_VERSION").unwrap()
    );
}
