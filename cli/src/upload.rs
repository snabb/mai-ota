// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! `GET` and `PUT` on the device's upload path.

use std::time::{Duration, Instant};

use async_io::Timer;
use mai_coap::{
    Error,
    block::Block,
    codec::{Code, OptionNumber},
};

use crate::Failure;
use crate::session::Device;

/// Sends of one block before giving up. Each send is a CON request that the
/// endpoint retransmits for up to about 93 s, within the device's 120 s upload
/// timeout, so a retry finds the upload still open.
const BLOCK_TRIES: u32 = 6;
/// Fresh starts after the device lost the upload (4.08) before giving up.
const RESTARTS: u32 = 3;
/// application/octet-stream.
const OCTET_STREAM: u64 = 42;
/// CBOR, what `GET` answers in.
const CBOR: u64 = 60;

/// What `GET` on the upload path reports.
#[derive(Debug, PartialEq, Eq)]
pub struct About {
    pub board: String,
    pub version: String,
    pub trial: bool,
}

/// CoAP's dotted form of a response code, such as 4.13.
pub fn dotted(code: Code) -> String {
    format!("{}.{:02}", code.0 >> 5, code.0 & 31)
}

/// The `GET` body: `{0: board, 1: version, 2: trial}`; unknown keys are skipped.
pub fn decode(body: &[u8]) -> Result<About, minicbor::decode::Error> {
    let mut d = minicbor::Decoder::new(body);
    let (mut board, mut version, mut trial) = (None, None, None);
    let pairs = d
        .map()?
        .ok_or_else(|| minicbor::decode::Error::message("indefinite map"))?;
    for _ in 0..pairs {
        match d.u64()? {
            0 => board = Some(d.str()?.to_owned()),
            1 => version = Some(d.str()?.to_owned()),
            2 => trial = Some(d.bool()?),
            _ => d.skip()?,
        }
    }
    match (board, version, trial) {
        (Some(board), Some(version), Some(trial)) => Ok(About {
            board,
            version,
            trial,
        }),
        _ => Err(minicbor::decode::Error::message("a key is missing")),
    }
}

/// `GET` the upload path, which also confirms an image on trial.
/// `None` from a device that takes uploads but does not report its version: it
/// answers `GET` with 4.04 or 4.05.
pub async fn about(device: Device<'_>, path: &str) -> Result<Option<About>, Failure> {
    let reply = device.request(Code::GET, path)?.send()?.await?;
    let message = reply.message()?;
    if matches!(message.code(), Code::NOT_FOUND | Code::METHOD_NOT_ALLOWED) {
        return Ok(None);
    }
    if message.code() != Code::CONTENT {
        return Err(Failure::refused(format!(
            "GET {path}: {}",
            dotted(message.code())
        )));
    }
    if message
        .option(OptionNumber::CONTENT_FORMAT)
        .and_then(|o| o.uint().ok())
        != Some(CBOR)
    {
        return Err(Failure::refused(format!("GET {path}: not CBOR")));
    }
    decode(message.payload())
        .map(Some)
        .map_err(|e| Failure::refused(format!("GET {path}: unexpected body: {e}")))
}

/// How an upload ended, short of a failure.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    /// The device answered the last block with 2.04: written, checked, activated.
    Activated,
    /// The last block's answer was 4.01: the device no longer knows this session,
    /// which a reboot into the new image after a reply that was lost explains. It
    /// may run the new image, on trial, or still the old one.
    Unanswered,
}

/// `PUT` `image` to `path` one Block1 block per request, starting with blocks
/// of `16 << szx` bytes.
///
/// Each block is a request of its own rather than one blockwise transfer, so a
/// block whose retransmissions run out (a Thread child that loses its parent for
/// a minute) is sent again instead of losing the body. That is safe: the device
/// recognises a block it already wrote. All blocks of an attempt carry one
/// Request-Tag, which with the OSCORE context keys the body on the device; a
/// device that dropped the upload answers 4.08, and the upload starts over.
///
/// The last block goes without EDHOC recovery. After a lost final reply the
/// device may already run the new image, on trial; a fresh session sending the
/// block again would be its first authenticated request, and so confirm it,
/// whatever `--no-confirm` asked.
pub async fn send(device: Device<'_>, path: &str, image: &[u8], szx: u8) -> Result<Sent, Failure> {
    let mut szx = szx;
    let mut tag: [u8; 4] = rand::random();
    let (mut offset, mut tries, mut restarts, mut reported) = (0, 0, 0, 0);
    let started = Instant::now();
    while offset < image.len() {
        let size = 16 << szx;
        let chunk = &image[offset..image.len().min(offset + size)];
        let more = offset + chunk.len() < image.len();
        let block = (((offset / size) as u64) << 4) | (u64::from(more) << 3) | u64::from(szx);
        let mut request = device
            .recovery(more)
            .request(Code::PUT, path)?
            .uint_option(OptionNumber::CONTENT_FORMAT, OCTET_STREAM)?
            .uint_option(OptionNumber::BLOCK1, block)?
            .option(OptionNumber::REQUEST_TAG, &tag)?;
        if offset == 0 {
            request = request.uint_option(OptionNumber::SIZE1, image.len() as u64)?;
        }
        let reply = match request.payload(chunk)?.send()?.await {
            Ok(reply) => reply,
            Err(Error::Timeout) => {
                tries += 1;
                if tries >= BLOCK_TRIES {
                    // The last block may have arrived and its reply been lost.
                    if !more {
                        eprintln!("the last block went unanswered {tries} times");
                        return Ok(Sent::Unanswered);
                    }
                    return Err(Failure::network(format!(
                        "block at {offset}: no answer after {tries} tries"
                    )));
                }
                eprintln!("block at {offset}: no answer; sending it again");
                continue;
            }
            // Without recovery, a rebooted device's bare 4.01 to the last block arrives
            // as an error rather than as a reply.
            Err(Error::UnprotectedResponse(Code::UNAUTHORIZED)) if !more => {
                eprintln!(
                    "the device no longer knows this session at the last block: it may \
                     have rebooted after a reply that was lost"
                );
                return Ok(Sent::Unanswered);
            }
            // The host lost its route for a moment; the device still holds the upload.
            Err(e @ (Error::LocalSendFailed | Error::LocalSendTimeout)) => {
                tries += 1;
                if tries >= BLOCK_TRIES {
                    return Err(Failure::network(format!(
                        "block at {offset}: {e} after {tries} tries"
                    )));
                }
                eprintln!("block at {offset}: {e}; sending it again");
                sleep(Duration::from_secs(2)).await;
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        tries = 0;
        let message = reply.message()?;
        match message.code() {
            Code::INCOMPLETE => {
                restarts += 1;
                if restarts > RESTARTS {
                    return Err(Failure::network(format!(
                        "the device lost the upload {restarts} times; giving up"
                    )));
                }
                eprintln!("the device lost the upload at {offset}; starting over");
                tag = rand::random();
                offset = 0;
                reported = 0;
                continue;
            }
            Code::CONTINUE if more => {}
            Code::CHANGED if !more => {}
            code => {
                return Err(Failure::refused(format!(
                    "upload refused at {offset}: {} (see the device's log for why)",
                    dotted(code)
                )));
            }
        }
        // The device may ask for smaller blocks; the next ones follow its size. An
        // answer at this size must name the block that was sent.
        if let Some(echo) = message.option(OptionNumber::BLOCK1) {
            let Some(answered) = echo.uint().ok().and_then(|v| Block::decode(v).ok()) else {
                return Err(Failure::refused(format!(
                    "block at {offset}: the device's Block1 option is malformed"
                )));
            };
            if answered.szx > szx
                || (answered.szx == szx && answered.number as usize != offset / size)
            {
                return Err(Failure::refused(format!(
                    "block at {offset}: the device acknowledged block {} of {} bytes",
                    answered.number,
                    16 << answered.szx
                )));
            }
            szx = answered.szx;
        }
        offset += chunk.len();
        if offset - reported >= 64 * 1024 || offset == image.len() {
            reported = offset;
            eprintln!("  {offset} / {}", image.len());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "uploaded in {elapsed:.0} s ({:.1} KiB/s)",
        image.len() as f64 / elapsed / 1024.0
    );
    Ok(Sent::Activated)
}

/// Sleep, between the polls of a session.
pub async fn sleep(duration: Duration) {
    Timer::after(duration).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_the_device_sends() {
        let body = b"\xa3\x00\x64pico\x01\x62v1\x02\xf5";
        assert_eq!(
            decode(body).unwrap(),
            About {
                board: "pico".into(),
                version: "v1".into(),
                trial: true
            }
        );
    }

    #[test]
    fn skips_unknown_keys_and_needs_the_known_ones() {
        let body = b"\xa4\x00\x61b\x09\x01\x01\x61v\x02\xf4";
        assert!(!decode(body).unwrap().trial);
        assert!(decode(b"\xa1\x00\x61b").is_err());
    }

    #[test]
    fn codes_print_dotted() {
        assert_eq!(dotted(Code::TOO_LARGE), "4.13");
        assert_eq!(dotted(Code::CHANGED), "2.04");
    }
}
