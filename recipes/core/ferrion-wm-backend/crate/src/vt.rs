// SPDX-License-Identifier: MIT
// Input consumer descriptor helpers follow base/drivers/inputd/src/lib.rs at
// 20f7573b4dc011ea5267189185edc0b2f12bab0f; its notice is in LICENSE.
use std::env;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::slice;

use libredox::flag::{O_CLOEXEC, O_NONBLOCK, O_RDWR};

const INPUT_CONSUMER_FD_ENV: &str = "FERRION_INPUT_CONSUMER_FD";
const INPUT_CONSUMER_FPATH_PREFIX: &str = "/scheme/display";
const MAX_VT: usize = 64;
const MAX_FPATH_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(crate) struct WireEvent {
    pub(crate) code: i64,
    pub(crate) a: i64,
    pub(crate) b: i64,
}

pub(crate) enum ConsumerEvent<'a> {
    Events(&'a [WireEvent]),
    Handoff,
}

pub(crate) struct Consumer {
    fd: OwnedFd,
    vt: usize,
}

impl Consumer {
    pub(crate) fn open_or_inherited() -> io::Result<(Self, bool)> {
        match env::var_os(INPUT_CONSUMER_FD_ENV) {
            Some(value) => {
                let text = value.to_str().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "FERRION_INPUT_CONSUMER_FD is not valid UTF-8",
                    )
                })?;
                let raw_fd: RawFd = text.parse().map_err(|_| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "FERRION_INPUT_CONSUMER_FD is not a valid descriptor",
                    )
                })?;
                if raw_fd <= 2 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "FERRION_INPUT_CONSUMER_FD must not be a standard stream",
                    ));
                }
                let consumer = Self::from_inherited_raw(raw_fd)?;
                Ok((consumer, true))
            }
            None => {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                    .open("/scheme/input/consumer")?;
                let fd: OwnedFd = file.into();
                let vt = parse_consumer_vt(fd.as_raw_fd())?;
                Ok((Self { fd, vt }, false))
            }
        }
    }

    fn from_inherited_raw(raw_fd: RawFd) -> io::Result<Self> {
        let fd_flags = unsafe { libc::fcntl(raw_fd, libc::F_GETFD) };
        if fd_flags < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, fd_flags | libc::FD_CLOEXEC) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let vt = parse_consumer_vt(fd.as_raw_fd())?;
        Ok(Self { fd, vt })
    }

    pub(crate) fn vt(&self) -> usize {
        self.vt
    }

    pub(crate) fn event_handle(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    pub(crate) fn open_display(&self) -> io::Result<File> {
        let display_fd = libredox::call::openat(
            self.fd.as_raw_fd() as usize,
            "display",
            O_CLOEXEC | O_NONBLOCK | O_RDWR,
            0,
        )?;
        Ok(unsafe { File::from_raw_fd(display_fd as RawFd) })
    }

    pub(crate) fn read_events<'a>(
        &self,
        events: &'a mut [WireEvent],
    ) -> io::Result<ConsumerEvent<'a>> {
        let byte_len = size_of_val(events);
        let buf = unsafe { slice::from_raw_parts_mut(events.as_mut_ptr().cast::<u8>(), byte_len) };
        match libredox::call::read(self.fd.as_raw_fd() as usize, buf) {
            Ok(bytes) => Ok(ConsumerEvent::Events(
                &events[..bytes / size_of::<WireEvent>()],
            )),
            Err(err) if err.errno() == libc::ESTALE => Ok(ConsumerEvent::Handoff),
            Err(err) => Err(err.into()),
        }
    }
}

fn parse_consumer_vt(raw_fd: RawFd) -> io::Result<usize> {
    let mut path = [0_u8; MAX_FPATH_BYTES];
    let length = libredox::call::fpath(raw_fd as usize, &mut path)?;
    let path = std::str::from_utf8(&path[..length]).map_err(io::Error::other)?;
    let rest = path.strip_prefix(INPUT_CONSUMER_FPATH_PREFIX).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "inputd consumer descriptor fpath lacks display prefix",
        )
    })?;
    let (scheme_tail, vt_text) = rest.split_once('/').ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "inputd consumer descriptor returned no VT separator",
        )
    })?;
    if (!scheme_tail.is_empty() && !scheme_tail.starts_with('.')) || vt_text.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inputd consumer descriptor fpath has unexpected shape",
        ));
    }
    let vt = vt_text.parse::<usize>().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "inputd consumer descriptor returned no VT number",
        )
    })?;
    if !(2..=MAX_VT).contains(&vt) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inputd consumer VT number is out of range",
        ));
    }
    Ok(vt)
}
