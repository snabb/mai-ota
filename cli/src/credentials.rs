// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors
// Copyright (c) 2026 mai-coap contributors

//! EDHOC credentials for a run, in the files the `mai-coap` CLI and aiocoap use.
//!
//! Adapted from `cli/src/credentials.rs` in mai-coap (copyright mai-coap
//! contributors, MIT OR Apache-2.0), trimmed to loading.

use std::{fs, os::unix::fs::MetadataExt, path::Path};

use mai_coap::{
    crypto::SoftwareCrypto,
    edhoc::{Identity, TrustedPeer},
    security::PeerId,
};
use zeroize::Zeroizing;

use crate::Failure;

/// The device: the one trusted peer of a run. A host name never selects a credential.
pub const DEVICE: PeerId = PeerId(1);

/// The RFC 9052 COSE_Key `{1: 2, -1: 1, -4: d}` (EC2, P-256, 32-byte `d`) up to `d`, as
/// aiocoap-keygen and `mai-coap --generate-identity` write it.
const COSE_KEY_HEADER: [u8; 8] = [0xa3, 0x01, 0x02, 0x20, 0x01, 0x23, 0x58, 0x20];

/// The files of a run, read again for each session: a session consumes them.
pub struct Files<'a> {
    pub identity: &'a Path,
    pub key: &'a Path,
    pub peer: &'a Path,
}

fn read(path: &Path) -> Result<Vec<u8>, Failure> {
    fs::read(path).map_err(|e| Failure::local(format!("{}: {e}", path.display())))
}

fn read_private_key(path: &Path) -> Result<Zeroizing<[u8; 32]>, Failure> {
    let context = |e: &dyn std::fmt::Display| Failure::local(format!("{}: {e}", path.display()));
    // The mode checked is the open file's, so the path cannot be swapped in between.
    let mut opened = fs::File::open(path).map_err(|e| context(&e))?;
    let metadata = opened.metadata().map_err(|e| context(&e))?;
    if metadata.mode() & 0o077 != 0 {
        return Err(context(&"private key is accessible by other users"));
    }
    let mut file = Zeroizing::new(Vec::new());
    std::io::Read::read_to_end(&mut opened, &mut file).map_err(|e| context(&e))?;
    file.strip_prefix(&COSE_KEY_HEADER)
        .and_then(|d| <[u8; 32]>::try_from(d).ok())
        .map(Zeroizing::new)
        .ok_or_else(|| context(&"not a P-256 COSE_Key of the form {1: 2, -1: 1, -4: d}"))
}

impl Files<'_> {
    /// This client's identity and the device's credential.
    pub fn load(
        &self,
        crypto: &mut SoftwareCrypto<rand::rngs::ThreadRng>,
    ) -> Result<(Identity, TrustedPeer), Failure> {
        let private = read_private_key(self.key)?;
        let identity = Identity::new(&read(self.identity)?, *private, crypto).map_err(|e| {
            Failure::local(format!(
                "{}: not a CCS matching the private key: {e}",
                self.identity.display()
            ))
        })?;
        let peer = TrustedPeer::new(DEVICE, &read(self.peer)?).map_err(|e| {
            Failure::local(format!("{}: not a valid CCS: {e}", self.peer.display()))
        })?;
        Ok((identity, peer))
    }
}
