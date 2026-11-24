use std::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use wayland_backend::client::Backend;
use zeroize::{Zeroize, Zeroizing};

use super::{
    ClipboardWaylandError, MAX_CLIPBOARD_BYTES, MAX_IO_TIMEOUT, MAX_PENDING_SENDS, MAX_URI_FILES,
};
use crate::time::Instant;

pub(super) const MAX_POLL_FDS: usize = 2 + 1 + MAX_PENDING_SENDS;
const READ_CHUNK_BYTES: usize = 16 * 1024;
const MAX_DRAIN_WAKE_ITERATIONS: usize = 16;

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn create_pipe() -> Result<[RawFd; 2], io::Error> {
    let mut fds = [-1 as RawFd; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    assert!(fds[0] >= 0 && fds[1] >= 0);
    Ok(fds)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_cloexec(fd: RawFd) -> Result<(), io::Error> {
    assert!(fd >= 0);
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn get_fd_flags(fd: RawFd) -> Result<libc::c_int, io::Error> {
    assert!(fd >= 0);
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(flags)
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> Result<(), io::Error> {
    assert!(fd >= 0);
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
fn own_fd(raw: RawFd) -> OwnedFd {
    assert!(raw >= 0);
    unsafe { OwnedFd::from_raw_fd(raw) }
}

pub(super) fn set_fd_nonblock(fd: RawFd) -> Result<(), io::Error> {
    let flags = get_fd_flags(fd)?;
    set_fd_flags(fd, flags | libc::O_NONBLOCK)
}

pub(super) fn make_nonblocking_pipe() -> Result<(OwnedFd, OwnedFd), ClipboardWaylandError> {
    let fds = create_pipe().map_err(|e| ClipboardWaylandError::PipeFailed(e.to_string()))?;
    let read_fd = own_fd(fds[0]);
    let write_fd = own_fd(fds[1]);
    set_fd_cloexec(read_fd.as_raw_fd())
        .and_then(|()| set_fd_cloexec(write_fd.as_raw_fd()))
        .and_then(|()| set_fd_nonblock(read_fd.as_raw_fd()))
        .and_then(|()| set_fd_nonblock(write_fd.as_raw_fd()))
        .map_err(|e| ClipboardWaylandError::PipeFailed(e.to_string()))?;
    Ok((read_fd, write_fd))
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn write_fd_nonblocking(fd: RawFd, buf: &[u8]) -> Result<usize, io::Error> {
    assert!(fd >= 0 && !buf.is_empty());
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn read_fd_nonblocking(fd: RawFd, buf: &mut [u8]) -> Result<usize, io::Error> {
    assert!(fd >= 0 && !buf.is_empty());
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn poll_fds(pfds: &mut [libc::pollfd], timeout_ms: i32) -> Result<i32, io::Error> {
    assert!(!pfds.is_empty() && pfds.len() <= MAX_POLL_FDS);
    for pfd in pfds.iter() {
        assert!(pfd.fd >= 0);
    }
    let rc = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc)
    }
}

#[allow(unsafe_code, clippy::undocumented_unsafe_blocks)]
pub(super) fn foreign_display_backend(display: NonNull<c_void>) -> Backend {
    assert!(display.as_ptr().addr() != 0);
    unsafe { Backend::from_foreign_display(display.as_ptr().cast()) }
}

pub(super) struct WakePipe {
    read_fd: OwnedFd,
}

pub(super) struct WakeHandle {
    write_fd: Arc<OwnedFd>,
}

impl Clone for WakeHandle {
    fn clone(&self) -> Self {
        Self {
            write_fd: self.write_fd.clone(),
        }
    }
}

impl WakeHandle {
    pub(super) fn wake(&self) {
        let byte = [1u8; 1];
        write_fd_nonblocking(self.write_fd.as_raw_fd(), &byte).ok();
    }
}

impl WakePipe {
    pub(super) fn new() -> Result<(Self, WakeHandle), ClipboardWaylandError> {
        let (read_fd, write_fd) = make_nonblocking_pipe()?;
        let handle = WakeHandle {
            write_fd: Arc::new(write_fd),
        };
        Ok((Self { read_fd }, handle))
    }

    pub(super) fn fd(&self) -> RawFd {
        self.read_fd.as_raw_fd()
    }

    pub(super) fn drain(&self) {
        let mut buf = [0u8; 64];
        for _ in 0..MAX_DRAIN_WAKE_ITERATIONS {
            match read_fd_nonblocking(self.read_fd.as_raw_fd(), &mut buf) {
                Ok(0) => break,
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }
}

pub(super) enum SendBytes {
    Shared(Arc<[u8]>),
    Secret(Zeroizing<Vec<u8>>),
}

impl SendBytes {
    pub(super) fn as_slice(&self) -> &[u8] {
        match self {
            SendBytes::Shared(bytes) => bytes,
            SendBytes::Secret(bytes) => bytes,
        }
    }
}

pub(super) struct PendingSend {
    pub(super) fd: OwnedFd,
    pub(super) data: SendBytes,
    pub(super) offset: usize,
    pub(super) deadline: Instant,
}

impl PendingSend {
    pub(super) fn start(fd: OwnedFd, data: SendBytes, now: Instant) -> Option<Self> {
        if set_fd_nonblock(fd.as_raw_fd()).is_err() {
            return None;
        }
        let mut pending = Self {
            fd,
            data,
            offset: 0,
            deadline: now + MAX_IO_TIMEOUT,
        };
        if pending.try_write() {
            None
        } else {
            Some(pending)
        }
    }

    pub(super) fn try_write(&mut self) -> bool {
        let slice = self.data.as_slice();
        while self.offset < slice.len() {
            match write_fd_nonblocking(self.fd.as_raw_fd(), &slice[self.offset..]) {
                Ok(0) => return true,
                Ok(n) => self.offset += n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return false,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return true,
            }
        }
        true
    }
}

pub(super) struct PendingRead {
    pub(super) fd: OwnedFd,
    pub(super) buf: Zeroizing<Vec<u8>>,
    pub(super) max_bytes: usize,
    pub(super) deadline: Instant,
    pub(super) reply: mpsc::Sender<Result<Zeroizing<Vec<u8>>, ClipboardWaylandError>>,
}

fn append_zeroizing(buf: &mut Zeroizing<Vec<u8>>, src: &[u8]) {
    let needed = buf.len().saturating_add(src.len());
    if needed > buf.capacity() {
        let cap = needed
            .max(buf.capacity().saturating_mul(2))
            .max(READ_CHUNK_BYTES);
        let mut next = Zeroizing::new(Vec::with_capacity(cap));
        next.extend_from_slice(buf);
        next.extend_from_slice(src);
        let old = std::mem::replace(buf, next);
        drop(old);
    } else {
        buf.extend_from_slice(src);
    }
}

impl PendingRead {
    pub(super) fn new(
        fd: OwnedFd,
        max_bytes: usize,
        now: Instant,
        reply: mpsc::Sender<Result<Zeroizing<Vec<u8>>, ClipboardWaylandError>>,
    ) -> Self {
        Self {
            fd,
            buf: Zeroizing::new(Vec::new()),
            max_bytes: max_bytes.min(MAX_CLIPBOARD_BYTES),
            deadline: now + MAX_IO_TIMEOUT,
            reply,
        }
    }

    pub(super) fn step(&mut self) -> Option<Result<Zeroizing<Vec<u8>>, ClipboardWaylandError>> {
        let mut chunk = Zeroizing::new([0u8; READ_CHUNK_BYTES]);
        loop {
            match read_fd_nonblocking(self.fd.as_raw_fd(), &mut *chunk) {
                Ok(0) => {
                    let out = std::mem::replace(&mut self.buf, Zeroizing::new(Vec::new()));
                    return Some(Ok(out));
                }
                Ok(n) => {
                    if self.buf.len().saturating_add(n) > self.max_bytes {
                        self.buf.zeroize();
                        self.buf.clear();
                        return Some(Err(ClipboardWaylandError::PayloadTooLarge));
                    }
                    append_zeroizing(&mut self.buf, &chunk[..n]);
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return None,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    self.buf.zeroize();
                    self.buf.clear();
                    return Some(Err(ClipboardWaylandError::PipeFailed(err.to_string())));
                }
            }
        }
    }
}

pub(super) fn poll_timeout_ms(deadlines: impl Iterator<Item = Instant>, now: Instant) -> i32 {
    let mut soonest: Option<Duration> = None;
    for deadline in deadlines {
        let rem = deadline.saturating_duration_since(now);
        soonest = Some(match soonest {
            Some(cur) => cur.min(rem),
            None => rem,
        });
    }
    match soonest {
        None => -1,
        Some(rem) => rem.as_millis().min(i32::MAX as u128) as i32,
    }
}

pub(super) fn format_uri_list(files: &[PathBuf]) -> Result<Vec<u8>, ClipboardWaylandError> {
    if files.len() > MAX_URI_FILES {
        return Err(ClipboardWaylandError::TooManyUris);
    }
    let mut out = Vec::new();
    for path in files {
        if let Some(uri) = path_to_file_uri(path) {
            out.extend_from_slice(uri.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
    }
    Ok(out)
}

fn path_to_file_uri(path: &Path) -> Option<String> {
    let raw = path.to_str()?;
    if !raw.starts_with('/') {
        return None;
    }
    let mut out = String::with_capacity(7 + raw.len());
    out.push_str("file://");
    for &b in raw.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~');
        if keep {
            out.push(b as char);
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    Some(out)
}

pub(super) fn parse_uri_list(bytes: &[u8]) -> Result<Vec<PathBuf>, ClipboardWaylandError> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("file://") else {
            continue;
        };
        let path_part = if rest.starts_with('/') {
            rest
        } else if let Some(after_host) = rest.strip_prefix("localhost/") {
            &rest[9..9 + 1 + after_host.len()]
        } else {
            continue;
        };
        if let Some(decoded) = percent_decode(path_part) {
            if out.len() >= MAX_URI_FILES {
                return Err(ClipboardWaylandError::TooManyUris);
            }
            out.push(PathBuf::from(decoded));
        }
    }
    Ok(out)
}

fn percent_decode(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hi = hex_val(bytes[i + 1])?;
            let lo = hex_val(bytes[i + 2])?;
            out.push((hi << 4) | lo);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}
