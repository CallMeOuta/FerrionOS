//! Test tools for the sound path. Run as root in a login shell.
//!
//! `audio-probe ring-churn N` opens a playback handle under `/scheme/audiohw`, opens the shared
//! ring `audio_shm` on it, and closes both, N times. Each round prints
//! `audio-probe: round <n>: ok` or `audio-probe: round <n>: FAIL <step>: <error>`, and the last
//! line counts the rounds that opened the ring. The exit status is 0 only when every round did.
//! `scripts/vm-test.py audio-ring` runs it on a booted image.

use std::process::ExitCode;

use libredox::{flag, Fd};

fn round() -> Result<(), String> {
    let handle = Fd::open("/scheme/audiohw/ring", flag::O_RDWR | flag::O_CLOEXEC, 0)
        .map_err(|err| format!("open /scheme/audiohw/ring: {err}"))?;
    let ring = handle
        .openat("audio_shm", flag::O_RDWR | flag::O_CLOEXEC, 0)
        .map_err(|err| format!("openat audio_shm: {err}"))?;
    drop(ring);
    drop(handle);
    Ok(())
}

fn ring_churn(rounds: usize) -> ExitCode {
    let mut opened = 0;
    for n in 1..=rounds {
        match round() {
            Ok(()) => {
                opened += 1;
                println!("audio-probe: round {n}: ok");
            }
            Err(err) => println!("audio-probe: round {n}: FAIL {err}"),
        }
    }
    println!("audio-probe: ring-churn: {opened} of {rounds} rounds opened the ring");
    if opened == rounds {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, rounds] if command == "ring-churn" => match rounds.parse() {
            Ok(rounds) => ring_churn(rounds),
            Err(_) => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: audio-probe ring-churn N");
    ExitCode::from(2)
}
