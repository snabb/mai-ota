// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! Uploads over CoAP with [mai-coap](https://github.com/snabb/mai-coap): `PUT`
//! with Block1, the body the raw application image.
//!
//! There is no manifest: the security layer authenticates every block, Block1
//! numbers them, and the final block says it is final, so a body that arrives
//! complete arrived intact. The final 2.04 means the image is written, checked
//! and activated. Route a [`Resource`] at the upload path for `GET` and `PUT`,
//! and allow it only to authenticated requests: wrap the endpoint's policy in
//! [`Proving`], which also confirms a trial.
//!
//! | Reply to `PUT` | When |
//! |---|---|
//! | 4.13 | the image is larger than the slot |
//! | 4.00 | not an image for this board, or not the size Size1 gave |
//! | 5.03 | the running image is on trial |
//! | 5.00 | the flash failed or read back wrong |
//!
//! `GET` answers 2.05 with a CBOR map (content format 60): `0` the board, `1`
//! [`Ota::version`], `2` whether the image is still on trial. Being
//! authenticated, the `GET` also confirms a trial.

use mai_coap::Error;
use mai_coap::block::BodyKey;
use mai_coap::codec::{Code, MessageView, OptionNumber};
use mai_coap::endpoint::{Application, Disposition, Handle, Request, ResponseWriter};
use mai_coap::security::{Policy, SecurityState};
use mai_coap::server::work::upload::{AsyncUploadSink, UploadWorker};
use mai_coap::server::work::{Handler, WorkQueue};
use mai_coap::time::{Clock, Instant};

use crate::{Ota, Refusal, Slot, Upload, Uploader};

/// CBOR's content format number.
const CBOR: u64 = 60;
/// Longest [`Ota::version`] a `GET` can report.
pub const VERSION_MAX: usize = 64;
/// Longest board name: what an ESP-IDF app descriptor's project name holds.
const BOARD_MAX: usize = 32;
/// The `GET` body at its longest: the map head and key, the board with a
/// two-byte head, a key, the version with a two-byte head, the trial pair.
const ABOUT_MAX: usize = 2 + 2 + BOARD_MAX + 1 + 2 + VERSION_MAX + 2;

/// An upload with no block for this long is abandoned.
///
/// Longer than a client retransmits one request (RFC 7252's MAX_TRANSMIT_WAIT,
/// 93 s), so a block that got through only on its last retransmission, or was
/// sent again after that, still finds the upload open.
pub const UPLOAD_TIMEOUT_MS: u64 = 120_000;

/// A policy that, on a trial boot, takes any request it allows over OSCORE as
/// proof the new image works; see [`prove`](crate::prove).
pub struct Proving<P: 'static>(pub &'static P);

impl<P: Policy> Policy for Proving<P> {
    fn allow(&self, state: SecurityState, request: MessageView<'_>) -> bool {
        let allowed = self.0.allow(state, request);
        if allowed && matches!(state, SecurityState::Protected { .. }) {
            crate::prove();
        }
        allowed
    }
}

/// Queue between the endpoint and the upload worker, for datagrams of up to `N`
/// bytes: one block in flight.
pub type Queue<const N: usize> = WorkQueue<N, 2, N>;

/// The upload path: `PUT` goes through the [`Queue`] to the worker [`run`]
/// drives, `GET` reports the running image.
pub struct Resource<'q, const N: usize> {
    upload: Handler<'q, N, 2, N>,
    board: &'static str,
    version: &'static str,
}

impl<'q, const N: usize> Resource<'q, N> {
    /// The resource for `ota`, whose uploads go through `queue`.
    ///
    /// # Panics
    ///
    /// When [`Ota::version`] is longer than [`VERSION_MAX`], or the board name
    /// longer than 32 bytes.
    pub fn new<S>(queue: &'q Queue<N>, ota: &Ota<S>) -> Self {
        Self::with(queue, ota.board, ota.version)
    }

    /// The resource for `board` running `version`, whose uploads go through
    /// `queue`: for a firmware whose [`worker`] feeds an uploader of its own.
    ///
    /// # Panics
    ///
    /// As [`new`](Self::new).
    pub fn with(queue: &'q Queue<N>, board: &'static str, version: &'static str) -> Self {
        assert!(version.len() <= VERSION_MAX, "version too long");
        assert!(board.len() <= BOARD_MAX, "board name too long");
        Self {
            upload: queue
                .handler(UPLOAD_TIMEOUT_MS)
                .expect("a nonzero upload timeout"),
            board,
            version,
        }
    }
}

/// The `GET` body: `{0: board, 1: version, 2: trial}`. `None` when `out` is too
/// short.
fn about(board: &str, version: &str, trial: bool, out: &mut [u8]) -> Option<usize> {
    let mut cbor = Cbor { out, at: 0 };
    // A map of three pairs; the keys and the booleans are one byte each.
    cbor.put(&[0xa3, 0x00])?;
    cbor.text(board)?;
    cbor.put(&[0x01])?;
    cbor.text(version)?;
    cbor.put(&[0x02, if trial { 0xf5 } else { 0xf4 }])?;
    Some(cbor.at)
}

/// Just enough of a CBOR encoder for [`about`].
struct Cbor<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl Cbor<'_> {
    fn put(&mut self, bytes: &[u8]) -> Option<()> {
        self.out
            .get_mut(self.at..self.at + bytes.len())?
            .copy_from_slice(bytes);
        self.at += bytes.len();
        Some(())
    }

    fn text(&mut self, s: &str) -> Option<()> {
        match s.len() {
            len @ 0..24 => self.put(&[0x60 | len as u8])?,
            len @ 24..256 => self.put(&[0x78, len as u8])?,
            _ => return None,
        }
        self.put(s.as_bytes())
    }
}

impl<const N: usize> Application<N> for Resource<'_, N> {
    fn handle(
        &mut self,
        request: Request<'_>,
        response: &mut ResponseWriter<'_, N>,
    ) -> Result<Disposition, Code> {
        if request.message.code() != Code::GET {
            return self.upload.handle(request, response);
        }
        let mut body = [0u8; ABOUT_MAX];
        let len = about(self.board, self.version, crate::on_trial(), &mut body)
            .ok_or(Code::INTERNAL_ERROR)?;
        response.set_code(Code::CONTENT);
        response.uint_option(OptionNumber::CONTENT_FORMAT, CBOR)?;
        response.set_payload(&body[..len])?;
        Ok(Disposition::Complete)
    }

    // The rest forwards to the upload handler, as mai-coap requires of a wrapper.
    fn recognizes(&self, number: OptionNumber) -> bool {
        self.upload.recognizes(number)
    }
    fn poll(&mut self, now: Instant, cx: &mut core::task::Context<'_>) {
        self.upload.poll(now, cx)
    }
    fn deadline(&self) -> Option<Instant> {
        self.upload.deadline()
    }
    fn uses_context(&self, generation: u64) -> bool {
        self.upload.uses_context(generation)
    }
    fn abort_context(&mut self, generation: u64) {
        self.upload.abort_context(generation)
    }
}

/// Run the upload worker and [`supervise`](crate::supervise) beside `server`,
/// the endpoint's own loop, until `server` returns.
pub async fn run<S: Slot, C: Clock, R, const N: usize, const COMMANDS: usize>(
    ota: Ota<S>,
    queue: &Queue<N>,
    handle: Handle<'_, N, COMMANDS>,
    clock: &C,
    server: impl Future<Output = R>,
) -> R {
    let confirm = ota.slot.confirm();
    let upload = Upload::new(ota.slot, ota.board, ota.events);
    let worker = worker(queue, handle, clock, upload);
    let supervise = crate::supervise(ota.boot, confirm, ota.events);
    match embassy_futures::select::select3(server, worker, supervise).await {
        embassy_futures::select::Either3::First(r) => r,
    }
}

/// Feed the `PUT`s a [`Resource`] queues into `uploader`, for as long as the
/// firmware runs.
///
/// The parts [`run`] puts together, for a firmware that owns them itself: it runs
/// [`supervise`](crate::supervise) from boot, whether or not the network is up,
/// and gives `uploader` the slot only while no other transport holds it. A
/// restarted endpoint takes a new `handle`; the queue and the worker stay.
pub async fn worker<U: Uploader, C: Clock, const N: usize, const COMMANDS: usize>(
    queue: &Queue<N>,
    handle: Handle<'_, N, COMMANDS>,
    clock: &C,
    uploader: U,
) -> ! {
    let capacity = uploader.capacity() as usize;
    let mut worker = UploadWorker::new(queue, Sink(uploader), capacity, UPLOAD_TIMEOUT_MS)
        .expect("a nonzero upload timeout");
    match worker.run(handle, clock).await {}
}

/// An [`Uploader`] as mai-coap's upload sink.
struct Sink<U>(U);

fn reply(refusal: Refusal) -> Error {
    match refusal {
        Refusal::OnTrial | Refusal::Unavailable => Error::Response(Code::UNAVAILABLE),
        Refusal::NotBegun => Error::Malformed,
        Refusal::TooLarge(_) => Error::Response(Code::TOO_LARGE),
        Refusal::Image(_) => Error::Response(Code::BAD_REQUEST),
        Refusal::OutOfOrder => Error::Malformed,
        Refusal::Length => Error::Response(Code::BAD_REQUEST),
        Refusal::Flash => Error::Response(Code::INTERNAL_ERROR),
    }
}

impl<U: Uploader> AsyncUploadSink for Sink<U> {
    async fn begin(&mut self, _key: &BodyKey, size: Option<u64>) -> Result<(), Error> {
        self.0.begin(size).map_err(reply)
    }

    async fn write(&mut self, offset: usize, bytes: &[u8]) -> Result<(), Error> {
        self.0.write(offset, bytes).await.map_err(reply)
    }

    async fn commit(&mut self) -> Result<(), Error> {
        self.0.commit().await.map_err(reply)
    }

    fn abort(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn about_encodes_a_three_pair_map() {
        let mut out = [0u8; 32];
        let len = about("pico", "v1", true, &mut out).unwrap();
        assert_eq!(
            &out[..len],
            b"\xa3\x00\x64pico\x01\x62v1\x02\xf5".as_slice()
        );
    }

    #[test]
    fn about_uses_a_length_byte_from_24_bytes() {
        let mut out = [0u8; 64];
        let version = "abcdefghijklmnopqrstuvwxyz";
        let len = about("b", version, false, &mut out).unwrap();
        assert_eq!(&out[5..7], &[0x78, 26]);
        assert_eq!(&out[len - 2..len], &[0x02, 0xf4]);
    }

    #[test]
    fn about_fits_the_longest_board_and_version() {
        let mut out = [0u8; ABOUT_MAX];
        let board = core::str::from_utf8(&[b'b'; BOARD_MAX]).unwrap();
        let version = core::str::from_utf8(&[b'v'; VERSION_MAX]).unwrap();
        assert!(about(board, version, true, &mut out).is_some());
        assert!(about(board, version, true, &mut out[..ABOUT_MAX - 1]).is_none());
    }
}
