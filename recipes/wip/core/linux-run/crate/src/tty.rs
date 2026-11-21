//! Terminal requests (plans/linux-compat.md, ioctl). Linux's `termios` and relibc's differ in
//! layout (Linux has c_line and 19 control characters; relibc has 32 and no c_line), in the bit of
//! several flags and in the index of every control character, so each field is converted.

use crate::{errno::*, mem::user_ok};

/// relibc's struct termios (src/header/termios/mod.rs, target_os = "redox").
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct Native {
    iflag: u32,
    oflag: u32,
    cflag: u32,
    lflag: u32,
    cc: [u8; 32],
}

unsafe extern "C" {
    fn tcgetattr(fd: i32, t: *mut Native) -> i32;
    fn tcsetattr(fd: i32, act: i32, t: *const Native) -> i32;
}

// (Linux bit, relibc bit) per field, from Linux's asm-generic/termbits.h and relibc's
// src/header/termios/constants.rs and src/header/termios/redox.rs.
const IFLAG: [(u32, u32); 11] = [
    (0o1, 0o1),         // IGNBRK
    (0o2, 0o2),         // BRKINT
    (0o4, 0o4),         // IGNPAR
    (0o10, 0o10),       // PARMRK
    (0o20, 0o20),       // INPCK
    (0o40, 0o40),       // ISTRIP
    (0o100, 0o100),     // INLCR
    (0o200, 0o200),     // IGNCR
    (0o400, 0o400),     // ICRNL
    (0o2000, 0o1000),   // IXON
    (0o10000, 0o2000),  // IXOFF
];
const IXANY: (u32, u32) = (0o4000, 0o4000);
const OFLAG: [(u32, u32); 8] = [
    (0o1, 0o1),     // OPOST
    (0o2, 0o4),     // OLCUC
    (0o4, 0o2),     // ONLCR
    (0o10, 0o10),   // OCRNL
    (0o20, 0o20),   // ONOCR
    (0o40, 0o40),   // ONLRET
    (0o100, 0o100), // OFILL
    (0o200, 0o200), // OFDEL
];
const CFLAG: [(u32, u32); 6] = [
    (0o100, 0o2000),    // CSTOPB
    (0o200, 0o4000),    // CREAD
    (0o400, 0o10000),   // PARENB
    (0o1000, 0o20000),  // PARODD
    (0o2000, 0o40000),  // HUPCL
    (0o4000, 0o100000), // CLOCAL
];
const LFLAG: [(u32, u32); 9] = [
    (0o1, 0x80),          // ISIG
    (0o2, 0x100),         // ICANON
    (0o10, 0o10),         // ECHO
    (0o20, 0x2),          // ECHOE
    (0o40, 0x4),          // ECHOK
    (0o100, 0x10),        // ECHONL
    (0o200, 0x8000_0000), // NOFLSH
    (0o400, 0x40_0000),   // TOSTOP
    (0o100000, 0x400),    // IEXTEN
];
// Linux index -> relibc index: VINTR VQUIT VERASE VKILL VEOF VTIME VMIN VSWTC VSTART VSTOP VSUSP
// VEOL VREPRINT VDISCARD VWERASE VLNEXT VEOL2.
const CC: [usize; 17] = [8, 9, 3, 5, 0, 17, 16, 7, 12, 13, 10, 1, 6, 15, 4, 14, 2];

fn map(v: u32, table: &[(u32, u32)], to_linux: bool) -> u32 {
    table.iter().fold(0, |out, &(l, n)| {
        let (from, to) = if to_linux { (n, l) } else { (l, n) };
        if v & from != 0 { out | to } else { out }
    })
}

/// Speeds B0 to B38400 share their numbers (0 to 15); relibc numbers the higher ones 16 up,
/// Linux 0o10001 up.
fn speed(v: u32, to_linux: bool) -> u32 {
    if to_linux {
        let s = v & 0o37;
        if s >= 0o20 { 0o10001 + (s - 0o20) } else { s }
    } else {
        let s = v & 0o10017;
        if s >= 0o10001 { 0o20 + (s - 0o10001) } else { s & 0o17 }
    }
}

const CSIZE: (u32, u32) = (0o60, 0o1400);

fn to_linux(n: &Native) -> [u8; 36] {
    let iflag = map(n.iflag, &IFLAG, true) | map(n.iflag, &[IXANY], true);
    let oflag = map(n.oflag, &OFLAG, true);
    let csize = ((n.cflag & CSIZE.1) >> 8) << 4;
    let cflag = map(n.cflag, &CFLAG, true) | speed(n.cflag, true) | csize;
    let lflag = map(n.lflag, &LFLAG, true);
    let mut out = [0u8; 36];
    out[0..4].copy_from_slice(&iflag.to_ne_bytes());
    out[4..8].copy_from_slice(&oflag.to_ne_bytes());
    out[8..12].copy_from_slice(&cflag.to_ne_bytes());
    out[12..16].copy_from_slice(&lflag.to_ne_bytes());
    // out[16] is c_line, 0 (N_TTY).
    for (l, &i) in CC.iter().enumerate() {
        out[17 + l] = n.cc[i];
    }
    out
}

fn from_linux(l: &[u8; 36], old: &Native) -> Native {
    let w = |i: usize| u32::from_ne_bytes(l[i..i + 4].try_into().unwrap());
    let (iflag, oflag, cflag, lflag) = (w(0), w(4), w(8), w(12));
    let mut n = *old;
    n.iflag = map(iflag, &IFLAG, false) | map(iflag, &[IXANY], false);
    n.oflag = map(oflag, &OFLAG, false);
    n.cflag = map(cflag, &CFLAG, false) | speed(cflag, false) | ((cflag & CSIZE.0) >> 4) << 8;
    n.lflag = map(lflag, &LFLAG, false);
    for (li, &i) in CC.iter().enumerate() {
        n.cc[i] = l[17 + li];
    }
    n
}

pub fn ioctl(fd: u64, req: u64, arg: u64) -> i64 {
    const TCGETS: u64 = 0x5401;
    const TCSETS: u64 = 0x5402;
    const TCSETSW: u64 = 0x5403;
    const TCSETSF: u64 = 0x5404;
    const TIOCGPGRP: u64 = 0x540f;
    const TIOCSPGRP: u64 = 0x5410;
    const TIOCGWINSZ: u64 = 0x5413;
    const TIOCSWINSZ: u64 = 0x5414;
    const FIONBIO: u64 = 0x5421;
    const FIONCLEX: u64 = 0x5450;
    const FIOCLEX: u64 = 0x5451;
    let fd = fd as i32;
    match req {
        TCGETS => {
            if !user_ok(arg, 36) {
                return -EFAULT;
            }
            let mut n = Native::default();
            if unsafe { tcgetattr(fd, &mut n) } != 0 {
                return -ENOTTY;
            }
            unsafe { *(arg as *mut [u8; 36]) = to_linux(&n) };
            0
        }
        TCSETS | TCSETSW | TCSETSF => {
            if !user_ok(arg, 36) {
                return -EFAULT;
            }
            let mut old = Native::default();
            if unsafe { tcgetattr(fd, &mut old) } != 0 {
                return -ENOTTY;
            }
            let n = from_linux(unsafe { &*(arg as *const [u8; 36]) }, &old);
            if unsafe { tcsetattr(fd, (req - TCSETS) as i32, &n) } != 0 {
                return -last_errno();
            }
            0
        }
        TIOCGWINSZ | TIOCSWINSZ => {
            if !user_ok(arg, 8) {
                return -EFAULT;
            }
            let native = if req == TIOCGWINSZ { libc::TIOCGWINSZ } else { libc::TIOCSWINSZ };
            let mut ws = unsafe { *(arg as *const [u16; 4]) };
            if unsafe { libc::ioctl(fd, native, ws.as_mut_ptr()) } != 0 {
                return -last_errno();
            }
            unsafe { *(arg as *mut [u16; 4]) = ws };
            0
        }
        TIOCGPGRP => {
            if !user_ok(arg, 4) {
                return -EFAULT;
            }
            let g = unsafe { libc::tcgetpgrp(fd) };
            if g < 0 {
                return -last_errno();
            }
            unsafe { *(arg as *mut i32) = g };
            0
        }
        TIOCSPGRP => {
            if !user_ok(arg, 4) {
                return -EFAULT;
            }
            if unsafe { libc::tcsetpgrp(fd, *(arg as *const i32)) } != 0 {
                return -last_errno();
            }
            0
        }
        FIONBIO => {
            if !user_ok(arg, 4) {
                return -EFAULT;
            }
            let on = unsafe { *(arg as *const i32) } != 0;
            let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if fl < 0 {
                return -last_errno();
            }
            let fl = if on { fl | libc::O_NONBLOCK } else { fl & !libc::O_NONBLOCK };
            if unsafe { libc::fcntl(fd, libc::F_SETFL, fl) } < 0 { -last_errno() } else { 0 }
        }
        FIOCLEX | FIONCLEX => {
            let v = if req == FIOCLEX { libc::FD_CLOEXEC } else { 0 };
            if unsafe { libc::fcntl(fd, libc::F_SETFD, v) } < 0 { -last_errno() } else { 0 }
        }
        _ => {
            crate::syscall::log_once(&format!("ioctl request {req:#x} is not answered (ENOTTY)"));
            -ENOTTY
        }
    }
}
