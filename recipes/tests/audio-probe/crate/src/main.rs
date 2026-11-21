//! Test tools for the sound path. Run as root in a login shell.
//!
//! `audio-probe ring-churn N` opens a playback handle under `/scheme/audiohw`, opens the shared
//! ring `audio_shm` on it, and closes both, N times. Each round prints
//! `audio-probe: round <n>: ok` or `audio-probe: round <n>: FAIL <step>: <error>`, and the last
//! line counts the rounds that opened the ring. The exit status is 0 only when every round did.
//! `scripts/vm-test.py audio-ring` runs it on a booted image.
//! `tone HZ SECONDS` plays s16le on the legacy root; `stream ID HZ SECONDS` plays
//! f32le through a root-owned factory. `churn N` opens and closes factory streams.
//! `loopback HZ SECONDS` plays a tone into the loopback input of test images.
//! `record ID SESSION SECONDS` and `tap ID SESSION TARGET SECONDS` record through a factory and
//! print one `audio-probe: heard:` line: frames, nonzero frames, peak, zero crossings a second over
//! the nonzero span, and the amplitude at 440 and 880 Hz. `grant SECONDS LINE...` holds the
//! capture service's handle for SECONDS with each LINE granted (plans/audio.md AU3).

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

fn loopback(hz: f64, seconds: usize) -> Result<(), String> {
    let mut input = std::fs::OpenOptions::new()
        .write(true)
        .open("/scheme/audio/loopback")
        .map_err(|err| format!("open loopback: {err}"))?;
    let frames = seconds.checked_mul(44100).ok_or("duration overflow")?;
    let mut data = [0u8; 1024 * 8];
    for start in (0..frames).step_by(1024) {
        let count = 1024.min(frames - start);
        for (n, out) in data[..count * 8].chunks_exact_mut(8).enumerate() {
            let sample =
                (0.25 * (std::f64::consts::TAU * hz * (start + n) as f64 / 44100.0).sin()) as f32;
            out[..4].copy_from_slice(&sample.to_le_bytes());
            out[4..].copy_from_slice(&sample.to_le_bytes());
        }
        std::io::Write::write_all(&mut input, &data[..count * 8])
            .map_err(|err| format!("write loopback: {err}"))?;
    }
    println!("audio-probe: loopback complete");
    Ok(())
}

/// Amplitude of `hz` in the left channel, by a single-bin discrete Fourier transform.
fn level(samples: &[[f32; 2]], hz: f64) -> f64 {
    let step = std::f64::consts::TAU * hz / 44100.0;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, frame) in samples.iter().enumerate() {
        re += frame[0] as f64 * (step * n as f64).cos();
        im -= frame[0] as f64 * (step * n as f64).sin();
    }
    2.0 * (re * re + im * im).sqrt() / samples.len().max(1) as f64
}

fn hear(mut input: ferrion_audio::Record, seconds: usize) -> Result<(), String> {
    let frames = seconds.checked_mul(44100).ok_or("duration overflow")?;
    let mut samples = vec![[0.0f32; 2]; frames];
    input
        .read_frames(&mut samples)
        .map_err(|err| format!("read frames: {err}"))?;
    let nonzero: Vec<usize> = (0..frames).filter(|&n| samples[n] != [0.0, 0.0]).collect();
    let peak = samples
        .iter()
        .flat_map(|frame| frame.iter())
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    let (crossings, span) = match (nonzero.first(), nonzero.last()) {
        (Some(&first), Some(&last)) if last > first => {
            let mut sign = 0.0f32;
            let mut count = 0;
            for frame in &samples[first..=last] {
                if frame[0] != 0.0 {
                    if sign != 0.0 && frame[0].signum() != sign {
                        count += 1;
                    }
                    sign = frame[0].signum();
                }
            }
            (count, &samples[first..=last])
        }
        _ => (0, &samples[..0]),
    };
    let per_second = if span.is_empty() {
        0.0
    } else {
        crossings as f64 * 44100.0 / span.len() as f64
    };
    println!(
        "audio-probe: heard: frames={frames} nonzero={} peak={peak} crossings_per_second={per_second:.1} level_440={:.6} level_880={:.6}",
        nonzero.len(),
        level(span, 440.0),
        level(span, 880.0)
    );
    Ok(())
}

fn record(
    identity: &str,
    session: &str,
    target: Option<&str>,
    seconds: usize,
) -> Result<(), String> {
    let factory = ferrion_audio::open_factory(identity, session)
        .map_err(|err| format!("open factory: {err}"))?;
    let input = match target {
        Some(target) => ferrion_audio::Record::tap_from_factory(factory.as_fd(), target)
            .map_err(|err| format!("openat tap/{target}: {err}"))?,
        None => ferrion_audio::Record::from_factory(factory.as_fd())
            .map_err(|err| format!("openat record: {err}"))?,
    };
    hear(input, seconds)
}

fn holder(word: &str) -> Result<ferrion_audio::Holder<'_>, String> {
    match word.split_once(':') {
        Some(("program", id)) => Ok(ferrion_audio::Holder::Program(id)),
        Some(("session", id)) => Ok(ferrion_audio::Holder::Session(id)),
        _ => Err(format!("holder {word}: want program:ID or session:ID")),
    }
}

fn grant(seconds: u64, lines: &[String]) -> Result<(), String> {
    let mut grants = ferrion_audio::Grants::open().map_err(|err| format!("open grants: {err}"))?;
    for line in lines {
        let words: Vec<_> = line.split_whitespace().collect();
        match words.as_slice() {
            ["microphone", who] => grants.microphone(holder(who)?),
            ["tap", who, target] => grants.tap(holder(who)?, target),
            _ => {
                return Err(format!(
                    "grant {line}: want `microphone HOLDER` or `tap HOLDER PROGRAM`"
                ))
            }
        }
        .map_err(|err| format!("grant {line}: {err}"))?;
    }
    let json = grants.json().map_err(|err| format!("read grants: {err}"))?;
    println!("audio-probe: grants: {}", json.trim());
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    println!("audio-probe: grants released");
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
        [command, hz, seconds] if command == "loopback" => match (hz.parse(), seconds.parse()) {
            (Ok(hz), Ok(seconds)) => result(loopback(hz, seconds)),
            _ => usage(),
        },
        [command, identity, session, seconds] if command == "record" => match seconds.parse() {
            Ok(seconds) => result(record(identity, session, None, seconds)),
            Err(_) => usage(),
        },
        [command, identity, session, target, seconds] if command == "tap" => {
            match seconds.parse() {
                Ok(seconds) => result(record(identity, session, Some(target), seconds)),
                Err(_) => usage(),
            }
        }
        [command, seconds, lines @ ..] if command == "grant" && !lines.is_empty() => {
            match seconds.parse() {
                Ok(seconds) => result(grant(seconds, lines)),
                Err(_) => usage(),
            }
        }
        [command, rounds] if command == "ring-churn" => match rounds.parse() {
            Ok(rounds) => ring_churn(rounds),
            Err(_) => usage(),
        },
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: audio-probe ring-churn N | tone HZ SECONDS | stream ID HZ SECONDS | churn N | loopback HZ SECONDS | record ID SESSION SECONDS | tap ID SESSION TARGET SECONDS | grant SECONDS LINE..."
    );
    ExitCode::from(2)
}
