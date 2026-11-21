//! Test tools for the sound path. Run as root in a login shell.
//!
//! `audio-probe ring-churn N` opens a playback handle under `/scheme/audiohw`, opens the shared
//! ring `audio_shm` on it, and closes both, N times. Each round prints
//! `audio-probe: round <n>: ok` or `audio-probe: round <n>: FAIL <step>: <error>`, and the last
//! line counts the rounds that opened the ring. The exit status is 0 only when every round did.
//! `scripts/vm-test.py audio-ring` runs it on a booted image.
//! `tone HZ SECONDS` plays s16le on the legacy root; `stream ID HZ SECONDS` plays
//! f32le through a root-owned factory. `churn N` opens and closes factory streams.

use std::os::fd::AsFd;
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

fn stream(identity: &str) -> Result<ferrion_audio::Stream, String> {
    let factory = ferrion_audio::open_factory(identity, "audio-probe")
        .map_err(|err| format!("open factory: {err}"))?;
    ferrion_audio::Stream::from_factory(factory.as_fd())
        .map_err(|err| format!("openat stream: {err}"))
}

fn tone(identity: Option<&str>, hz: f64, seconds: usize) -> Result<(), String> {
    if !hz.is_finite() || hz <= 0.0 || hz >= 22050.0 || seconds == 0 {
        return Err("invalid frequency or duration".into());
    }
    if let Some(identity) = identity {
        let mut output = stream(identity)?;
        output
            .set_gain_db(0.0)
            .map_err(|err| format!("stream gain: {err}"))?;
        let frames = seconds.checked_mul(44100).ok_or("duration overflow")?;
        let mut data = [[0.0f32; 2]; 1024];
        for start in (0..frames).step_by(data.len()) {
            let count = data.len().min(frames - start);
            for (n, frame) in data[..count].iter_mut().enumerate() {
                let sample = (0.25
                    * (std::f64::consts::TAU * hz * (start + n) as f64 / 44100.0).sin())
                    as f32;
                *frame = [sample; 2];
            }
            output
                .write_frames(&data[..count])
                .map_err(|err| format!("write frames: {err}"))?;
        }
        println!("audio-probe: tone complete");
        return Ok(());
    }
    let output = Fd::open("/scheme/audio", flag::O_WRONLY | flag::O_CLOEXEC, 0)
        .map_err(|err| format!("open legacy audio: {err}"))?;
    let frames = seconds.checked_mul(44100).ok_or("duration overflow")?;
    let mut data = Vec::with_capacity(1024 * 8);
    for start in (0..frames).step_by(1024) {
        data.clear();
        for frame in start..(start + 1024).min(frames) {
            let sample = 0.25 * (std::f64::consts::TAU * hz * frame as f64 / 44100.0).sin();
            for _ in 0..2 {
                data.extend_from_slice(&((sample * 32767.0) as i16).to_le_bytes());
            }
        }
        let mut rest = data.as_slice();
        while !rest.is_empty() {
            let written = output
                .write(rest)
                .map_err(|err| format!("write audio: {err}"))?;
            if written == 0 {
                return Err("audio write made no progress".into());
            }
            rest = &rest[written..];
        }
    }
    println!("audio-probe: tone complete");
    Ok(())
}

fn churn(rounds: usize) -> Result<(), String> {
    for _ in 0..rounds {
        drop(stream("churn")?);
    }
    println!("audio-probe: churn: {rounds} closed");
    Ok(())
}

fn result(result: Result<(), String>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("audio-probe: FAIL {error}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command, hz, seconds] if command == "tone" => match (hz.parse(), seconds.parse()) {
            (Ok(hz), Ok(seconds)) => result(tone(None, hz, seconds)),
            _ => usage(),
        },
        [command, identity, hz, seconds] if command == "stream" => {
            match (hz.parse(), seconds.parse()) {
                (Ok(hz), Ok(seconds)) => result(tone(Some(identity), hz, seconds)),
                _ => usage(),
            }
        }
        [command, rounds] if command == "churn" => match rounds.parse() {
            Ok(rounds) => result(churn(rounds)),
            Err(_) => usage(),
        },
        [command, rounds] if command == "ring-churn" => match rounds.parse() {
            Ok(rounds) => ring_churn(rounds),
            Err(_) => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: audio-probe ring-churn N | tone HZ SECONDS | stream ID HZ SECONDS | churn N");
    ExitCode::from(2)
}
