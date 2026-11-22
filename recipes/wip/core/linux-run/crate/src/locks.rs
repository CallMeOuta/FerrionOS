use std::collections::BTreeSet;
use std::sync::Mutex;

use crate::{errno::*, mem::user_ok, session, signal, thread::Thread};

const ENOLCK: i64 = 37;
const F_RDLCK: u64 = 0;
const F_WRLCK: u64 = 1;
const F_UNLCK: u64 = 2;

static HELD: Mutex<(i32, BTreeSet<((u64, u64), bool)>)> = Mutex::new((0, BTreeSet::new()));

fn held() -> std::sync::MutexGuard<'static, (i32, BTreeSet<((u64, u64), bool)>)> {
    let mut h = HELD.lock().unwrap();
    let pid = crate::syscall::pid();
    if h.0 != pid {
        *h = (pid, BTreeSet::new());
    }
    h
}

fn key(fd: i32) -> Result<(u64, u64), i64> {
    unsafe extern "C" {
        fn redox_fpath_v1(fd: usize, buf: *mut u8, len: usize) -> usize;
    }
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return Err(last_errno());
    }
    let mut buf = [0u8; 4096];
    let n = raw(unsafe { redox_fpath_v1(fd as usize, buf.as_mut_ptr(), buf.len()) })?;
    let path = &buf[..n.min(buf.len())];
    let scheme = match path.strip_prefix(b"/scheme/") {
        Some(rest) => rest.split(|&b| b == b'/').next().unwrap_or_default(),
        None => path.split(|&b| b == b':').next().unwrap_or_default(),
    };
    let hash = scheme.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
    Ok((hash, st.st_ino as u64))
}

fn range(fd: i32, p: u64) -> Result<(u64, u64), i64> {
    let whence = unsafe { *((p + 2) as *const i16) };
    let (start, len) = unsafe { (*((p + 8) as *const i64), *((p + 16) as *const i64)) };
    let base = match whence {
        0 => 0,
        1 => {
            let r = unsafe { libc::lseek(fd, 0, libc::SEEK_CUR) };
            if r < 0 {
                return Err(last_errno());
            }
            r
        }
        2 => {
            let mut st: libc::stat = unsafe { core::mem::zeroed() };
            if unsafe { libc::fstat(fd, &mut st) } != 0 {
                return Err(last_errno());
            }
            st.st_size
        }
        _ => return Err(EINVAL),
    };
    let mut start = base.checked_add(start).ok_or(EINVAL)?;
    let mut len = len;
    if len < 0 {
        start = start.checked_add(len).ok_or(EINVAL)?;
        len = -len;
    }
    if start < 0 {
        return Err(EINVAL);
    }
    Ok((start as u64, len as u64))
}

fn set(thread: &Thread, k: (u64, u64), start: u64, len: u64, kind: u64, whole_file: bool, wait: bool) -> i64 {
    loop {
        match session::session_call(&[session::SET_LOCK, k.0, k.1], &mut [start, len, kind, whole_file as u64]) {
            Ok(_) => {
                if kind != F_UNLCK {
                    held().1.insert((k, whole_file));
                }
                return 0;
            }
            Err(EAGAIN) if wait => {
                if signal::pending_for(thread) {
                    return -EINTR;
                }
                let ts = libc::timespec { tv_sec: 0, tv_nsec: 10_000_000 };
                unsafe { libc::nanosleep(&ts, core::ptr::null_mut()) };
            }
            Err(e) => return -e,
        }
    }
}

pub fn fcntl(thread: &Thread, fd: i32, cmd: u64, p: u64) -> i64 {
    const F_GETLK: u64 = 5;
    const F_SETLKW: u64 = 7;
    const F_OFD_GETLK: u64 = 36;
    const F_OFD_SETLKW: u64 = 38;
    if !user_ok(p, 32) {
        return -EFAULT;
    }
    let kind = unsafe { *(p as *const i16) } as u64;
    if kind > F_UNLCK {
        return -EINVAL;
    }
    let (start, len) = match range(fd, p) {
        Ok(r) => r,
        Err(e) => return -e,
    };
    if !session::active() {
        return -ENOLCK;
    }
    let k = match key(fd) {
        Ok(k) => k,
        Err(e) => return -e,
    };
    if cmd == F_GETLK || cmd == F_OFD_GETLK {
        if kind == F_UNLCK {
            return -EINVAL;
        }
        let mut w = [start, len, kind, 0];
        return match session::session_call(&[session::GET_LOCK, k.0, k.1], &mut w) {
            Ok(32) => {
                unsafe {
                    *(p as *mut i16) = w[0] as i16;
                    *((p + 2) as *mut i16) = 0;
                    *((p + 8) as *mut i64) = w[1] as i64;
                    *((p + 16) as *mut i64) = w[2] as i64;
                    *((p + 24) as *mut i32) = if cmd == F_OFD_GETLK { -1 } else { w[3] as i32 };
                }
                0
            }
            Ok(_) => {
                unsafe { *(p as *mut i16) = F_UNLCK as i16 };
                0
            }
            Err(e) => -e,
        };
    }
    let mode = unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_ACCMODE;
    if (kind == F_RDLCK && mode == libc::O_WRONLY) || (kind == F_WRLCK && mode == libc::O_RDONLY) {
        return -EBADF;
    }
    set(thread, k, start, len, kind, false, cmd == F_SETLKW || cmd == F_OFD_SETLKW)
}

pub fn flock(thread: &Thread, fd: i32, op: u64) -> i64 {
    if crate::files::reserved(fd) || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return -EBADF;
    }
    let kind = match op & !4 {
        1 => F_RDLCK,
        2 => F_WRLCK,
        8 => F_UNLCK,
        _ => return -EINVAL,
    };
    if !session::active() {
        return -ENOLCK;
    }
    let k = match key(fd) {
        Ok(k) => k,
        Err(e) => return -e,
    };
    set(thread, k, 0, 0, kind, true, op & 4 == 0)
}

pub fn before_close(fd: i32) {
    if held().1.is_empty() || !session::active() {
        return;
    }
    let Ok(k) = key(fd) else { return };
    let mut h = held();
    if h.1.remove(&(k, false)) {
        let _ = session::session_call(&[session::RELEASE_LOCKS, k.0, k.1], &mut [0; 4]);
    }
    if h.1.remove(&(k, true)) {
        let _ = session::session_call(&[session::SET_LOCK, k.0, k.1], &mut [0, 0, F_UNLCK, 1]);
    }
}
