// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 mai-ota contributors

//! `mai-ota`: uploads firmware to a mai-ota device over CoAP and OSCORE, then
//! checks that the device came back running it.

mod credentials;
mod image;
mod session;
mod upload;

use std::{
    cell::Cell,
    net::{IpAddr, SocketAddr, ToSocketAddrs},
    path::PathBuf,
    process::ExitCode,
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use credentials::Files;

/// Exit status: the device refused, or came back running the old image.
const REFUSED: u8 = 1;
/// Exit status: bad arguments, files or local tools.
const LOCAL: u8 = 2;
/// Exit status: the device did not answer.
const NETWORK: u8 = 3;

/// How long a device has to come back after an upload.
const COME_BACK: Duration = Duration::from_secs(300);
/// Between tries while it reboots.
const POLL: Duration = Duration::from_secs(5);

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// This client's EDHOC credential (CCS), as the device has it.
    #[arg(long, global = true, value_name = "CCS")]
    identity: Option<PathBuf>,
    /// This client's private key: a COSE_Key as aiocoap-keygen or
    /// `mai-coap --generate-identity` writes it, readable by its owner only.
    #[arg(long, global = true, value_name = "COSEKEY")]
    key: Option<PathBuf>,
    /// The device's EDHOC credential (CCS).
    #[arg(long, global = true, value_name = "CCS")]
    peer: Option<PathBuf>,
    /// The device's upload path.
    #[arg(long, global = true, default_value = "/fw")]
    path: String,
    /// The device's CoAP port.
    #[arg(long, global = true, default_value_t = 5683)]
    port: u16,
    /// Library logs on stderr.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the board, version and trial state the device reports. On an image
    /// that is on trial, this request confirms it.
    Info {
        /// Host name, IPv6 or IPv4 address; `%zone` for a link-local one.
        host: String,
    },
    /// Install an image: send it, wait for the device to reboot into it, and
    /// read its version, which confirms the trial.
    Upload {
        /// Host name, IPv6 or IPv4 address; `%zone` for a link-local one.
        host: String,
        /// The firmware ELF.
        elf: PathBuf,
        /// The ESP32 chip, for `espflash save-image`; by default the one the ELF's
        /// esp-hal metadata names, else esp32c6 for RISC-V and esp32s3 for Xtensa.
        #[arg(long)]
        chip: Option<String>,
        /// Block size in bytes; the device may ask for smaller.
        #[arg(long, default_value_t = 1024, value_parser = block_size)]
        block_size: u16,
        /// Stop after the upload and send nothing more, so the trial times out
        /// and the device rolls back; for testing the rollback.
        #[arg(long)]
        no_confirm: bool,
    },
}

/// A CoAP block size: a power of two from 16 to 1024.
fn block_size(s: &str) -> Result<u16, String> {
    match s.parse::<u16>() {
        Ok(n) if n.is_power_of_two() && (16..=1024).contains(&n) => Ok(n),
        _ => Err("a power of two from 16 to 1024".into()),
    }
}

/// A failure and the exit status it gives.
#[derive(Debug)]
pub struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    pub fn local(message: impl Into<String>) -> Self {
        Self {
            code: LOCAL,
            message: message.into(),
        }
    }
    pub fn network(message: impl Into<String>) -> Self {
        Self {
            code: NETWORK,
            message: message.into(),
        }
    }
    pub fn refused(message: impl Into<String>) -> Self {
        Self {
            code: REFUSED,
            message: message.into(),
        }
    }
}

impl From<mai_coap::Error> for Failure {
    fn from(error: mai_coap::Error) -> Self {
        Self::network(error.to_string())
    }
}

/// Library logs on stderr for `-v`.
struct StderrLogger;
impl log::Log for StderrLogger {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        eprintln!("{}: {}", record.level(), record.args());
    }
    fn flush(&self) {}
}
static LOGGER: StderrLogger = StderrLogger;

/// Every address `host` names. A rebooted Wi-Fi device can come back without
/// the address its name pointed at, so all of them are tried, and resolved again
/// on every round.
fn addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, Failure> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    // A link-local IPv6 address with its zone, which IpAddr does not parse.
    if let Ok(addr) = format!("[{host}]:{port}").parse::<SocketAddr>() {
        return Ok(vec![addr]);
    }
    let found: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| Failure::network(format!("{host}: {e}")))?
        .collect();
    if found.is_empty() {
        return Err(Failure::network(format!("{host}: no address")));
    }
    Ok(found)
}

fn print(about: &upload::About) {
    println!(
        "board {}, version {}{}",
        about.board,
        about.version,
        if about.trial { ", on trial" } else { "" }
    );
}

fn print_or_unknown(about: &Option<upload::About>) {
    match about {
        Some(about) => print(about),
        None => println!("version not reported (no GET on the upload path)"),
    }
}

fn info(files: &Files<'_>, host: &str, args: &Args) -> Result<u8, Failure> {
    let mut last = None;
    for remote in addresses(host, args.port)? {
        match session::run(files, remote, async |device| {
            upload::about(device, &args.path).await
        }) {
            Ok(about) => {
                print_or_unknown(&about);
                return Ok(0);
            }
            Err(failure) if failure.code == NETWORK => last = Some(failure),
            Err(failure) => return Err(failure),
        }
    }
    Err(last.unwrap_or_else(|| Failure::network(format!("{host}: no answer"))))
}

fn install(
    files: &Files<'_>,
    host: &str,
    image: &[u8],
    szx: u8,
    confirm: bool,
    args: &Args,
) -> Result<u8, Failure> {
    // The upload goes to the first address that answers the GET, so one that does
    // not route (an IPv6 address without a route, say) falls through to the next.
    // A failure once the upload has started ends it there.
    let mut before = None;
    let mut last = None;
    for remote in addresses(host, args.port)? {
        let sending = Cell::new(false);
        match session::run(files, remote, async |device| {
            let before = upload::about(device, &args.path).await?;
            eprint!("running: ");
            print_or_unknown(&before);
            eprintln!("uploading {} bytes to {remote}", image.len());
            sending.set(true);
            let sent = upload::send(device, &args.path, image, szx).await?;
            Ok((before, sent))
        }) {
            Ok(about) => {
                before = Some((remote, about));
                break;
            }
            Err(failure) if failure.code == NETWORK && !sending.get() => {
                eprintln!("{remote}: {}", failure.message);
                last = Some(failure);
            }
            Err(failure) => return Err(failure),
        }
    }
    let Some((uploaded_to, (before, sent))) = before else {
        return Err(last.unwrap_or_else(|| Failure::network(format!("{host}: no answer"))));
    };
    if !confirm {
        if sent == upload::Sent::Unanswered {
            return Err(Failure::network(
                "not confirming, and not knowing whether the device took the image: it \
                 runs the old one, or the new one until its trial times out",
            ));
        }
        eprintln!("not confirming: the device rolls back when its trial times out");
        return Ok(0);
    }
    // The device reboots a couple of seconds after the last reply. The first GET
    // the new image answers confirms it; the ones after show when it is written.
    // The address that took the upload is tried first: the others may not route.
    let deadline = Instant::now() + COME_BACK;
    std::thread::sleep(POLL);
    while Instant::now() < deadline {
        let mut candidates = vec![uploaded_to];
        candidates.extend(
            addresses(host, args.port)
                .unwrap_or_default()
                .into_iter()
                .filter(|a| *a != uploaded_to),
        );
        for remote in candidates {
            let after = session::run(files, remote, async |device| {
                let mut after = upload::about(device, &args.path).await?;
                while after.as_ref().is_some_and(|after| after.trial) && Instant::now() < deadline {
                    upload::sleep(Duration::from_secs(1)).await;
                    after = upload::about(device, &args.path).await?;
                }
                Ok(after)
            });
            let Ok(after) = after else { continue };
            print_or_unknown(&after);
            // The image uploaded reports its version, so one that does not is the old one.
            let Some(after) = after else {
                return Err(Failure::refused(
                    "the device answered without reporting a version: it rolled back to \
                     the image it ran before",
                ));
            };
            if before
                .as_ref()
                .is_some_and(|before| before.version == after.version)
            {
                return Err(Failure::refused(
                    "the old version answered: the device rolled back, or the new \
                     image reports the same version",
                ));
            }
            if after.trial {
                return Err(Failure::refused(
                    "the new image is still on trial: it never confirmed",
                ));
            }
            eprintln!("confirmed");
            return Ok(0);
        }
        std::thread::sleep(POLL);
    }
    Err(Failure::network(format!(
        "{host}: no answer for {} s after the upload",
        COME_BACK.as_secs()
    )))
}

fn run(args: &Args) -> Result<u8, Failure> {
    if args.verbose > 0 && log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(if args.verbose > 1 {
            log::LevelFilter::Trace
        } else {
            log::LevelFilter::Debug
        });
    }
    let (Some(identity), Some(key), Some(peer)) = (&args.identity, &args.key, &args.peer) else {
        return Err(Failure::local(
            "--identity, --key and --peer are all needed: a device takes firmware only over OSCORE",
        ));
    };
    let files = Files {
        identity,
        key,
        peer,
    };
    match &args.command {
        Command::Info { host } => info(&files, host, args),
        Command::Upload {
            host,
            elf,
            chip,
            block_size,
            no_confirm,
        } => {
            let image = image::from_elf(elf, chip.as_deref())?;
            // 16 << szx bytes.
            let szx = (block_size.trailing_zeros() - 4) as u8;
            install(&files, host, &image, szx, !no_confirm, args)
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(failure) => {
            eprintln!("mai-ota: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}
