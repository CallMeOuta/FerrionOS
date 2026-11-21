//! Audio streams use interleaved stereo f32 little-endian samples at 44,100 Hz.
//! Stream, application and session gains are amplitude decibels: 0 is unity,
//! negative values attenuate, and negative infinity mutes. Legacy master volume
//! is an integer percentage with the server's cubic response.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd};

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: usize = 2;

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// An identity or session component is 1–64 ASCII letters, digits, '.', '_' or '-'.
pub fn valid_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

fn child(fd: BorrowedFd<'_>, name: &std::ffi::CStr, flags: libc::c_int) -> io::Result<File> {
    // The borrowed directory remains alive through openat; the returned FD is owned.
    let raw = unsafe { libc::openat(fd.as_raw_fd(), name.as_ptr(), flags | libc::O_CLOEXEC, 0) };
    if raw < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(raw) })
    }
}

fn write_number(mut file: File, value: impl std::fmt::Display) -> io::Result<()> {
    let mut bytes = [0u8; 64];
    let len = {
        let mut buffer = io::Cursor::new(bytes.as_mut_slice());
        write!(buffer, "{value}")?;
        buffer.position() as usize
    };
    file.write_all(&bytes[..len])
}

fn write_gain(mut file: File, db: f32) -> io::Result<()> {
    if db == f32::NEG_INFINITY {
        file.write_all(b"mute")
    } else if db.is_finite() && db <= 0.0 {
        write_number(file, db)
    } else {
        Err(invalid(
            "Gain must be finite and at most 0 dB, or negative infinity for mute",
        ))
    }
}

fn read_gain(mut file: File) -> io::Result<f32> {
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    if text.trim() == "mute" {
        return Ok(f32::NEG_INFINITY);
    }
    text.trim()
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v <= 0.0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid audio gain"))
}

/// A blocking stereo stream. Dropping it closes its stream handle.
pub struct Stream {
    file: File,
}

impl Stream {
    /// Open a stream associated with the caller's process and user.
    pub fn open() -> io::Result<Self> {
        Ok(Self {
            file: OpenOptions::new()
                .write(true)
                .open("/scheme/audio/stream")?,
        })
    }

    /// Open through a previously authorized factory FD. The server supplies the
    /// trusted identity and session; this method cannot override them.
    pub fn from_factory(factory: BorrowedFd<'_>) -> io::Result<Self> {
        Ok(Self {
            file: child(factory, c"stream", libc::O_WRONLY)?,
        })
    }

    /// Write complete [left, right] frames without heap allocation.
    /// Partial writes and interruptions are handled by write_all.
    pub fn write_frames(&mut self, frames: &[[f32; CHANNELS]]) -> io::Result<()> {
        #[cfg(target_endian = "little")]
        {
            // f32 and arrays have contiguous initialized representations; bytes
            // borrow the samples and cannot outlive them or mutate them.
            let bytes = unsafe {
                std::slice::from_raw_parts(
                    frames.as_ptr().cast::<u8>(),
                    std::mem::size_of_val(frames),
                )
            };
            self.file.write_all(bytes)
        }
        #[cfg(target_endian = "big")]
        {
            let mut bytes = [0u8; 2048];
            for chunk in frames.chunks(256) {
                for (frame, output) in chunk.iter().zip(bytes.chunks_exact_mut(8)) {
                    output[..4].copy_from_slice(&frame[0].to_le_bytes());
                    output[4..].copy_from_slice(&frame[1].to_le_bytes());
                }
                self.file.write_all(&bytes[..chunk.len() * 8])?;
            }
            Ok(())
        }
    }

    pub fn set_gain_db(&self, db: f32) -> io::Result<()> {
        write_gain(child(self.file.as_fd(), c"gain", libc::O_WRONLY)?, db)
    }

    pub fn gain_db(&self) -> io::Result<f32> {
        read_gain(child(self.file.as_fd(), c"gain", libc::O_RDONLY)?)
    }
}

/// Root opens a factory once and may pass its FD to a client.
pub fn open_factory(identity: &str, session: &str) -> io::Result<File> {
    if !valid_identity(identity) || !valid_identity(session) {
        return Err(invalid("Invalid audio identity or session"));
    }
    File::open(format!("/scheme/audio/factory/{identity}/{session}"))
}

/// Raw JSON from the audio service; callers choose their own JSON library.
pub fn status_json() -> io::Result<String> {
    fs::read_to_string("/scheme/audio/status")
}

/// Root-only application inventory as JSON.
pub fn apps_json() -> io::Result<String> {
    fs::read_to_string("/scheme/audio/apps")
}

pub fn volume() -> io::Result<u8> {
    fs::read_to_string("/scheme/audio/volume")?
        .trim()
        .parse::<u8>()
        .ok()
        .filter(|v| *v <= 100)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid audio volume"))
}

pub fn set_volume(percent: u8) -> io::Result<()> {
    if percent > 100 {
        return Err(invalid("Volume must be between 0 and 100"));
    }
    let file = OpenOptions::new()
        .write(true)
        .open("/scheme/audio/volume")?;
    write_number(file, percent)
}

#[derive(Clone, Copy)]
pub enum GainTarget<'a> {
    App(&'a str),
    Session(&'a str),
}

impl GainTarget<'_> {
    fn path(self) -> io::Result<String> {
        let (kind, identity) = match self {
            Self::App(id) => ("program", id),
            Self::Session(id) => ("session", id),
        };
        if !valid_identity(identity) {
            return Err(invalid("Invalid audio identity or session"));
        }
        Ok(format!("/scheme/audio/{kind}/{identity}/gain"))
    }

    pub fn gain_db(self) -> io::Result<f32> {
        read_gain(File::open(self.path()?)?)
    }

    pub fn set_gain_db(self, db: f32) -> io::Result<()> {
        write_gain(OpenOptions::new().write(true).open(self.path()?)?, db)
    }
}

/// Map an amplitude percentage to dB (50% is about -6.0206 dB).
pub fn percent_to_db(percent: u8) -> io::Result<f32> {
    match percent {
        0 => Ok(f32::NEG_INFINITY),
        1..=100 => Ok(20.0 * (f32::from(percent) / 100.0).log10()),
        _ => Err(invalid("Volume must be between 0 and 100")),
    }
}
