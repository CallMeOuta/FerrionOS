//! The Linux system calls of LC1 (plans/linux-compat.md, "The calls, in the order programs need
//! them"), plus clock_gettime because no vDSO is offered, and the few the crossing probe needs.
//! Every other number answers ENOSYS; with LINUX_RUN_TRACE set in the environment the runtime
//! names each call it refuses on standard error.

use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;

use crate::{
    errno::*,
    mem::{self, user_ok},
    signal,
    thread::{self, Frame, Thread},
};

static PID: AtomicI32 = AtomicI32::new(0);
/// Linux's AT_FDCWD as a call argument.
const AT_FDCWD_U: u64 = -100i64 as u64;
pub static TRACE: AtomicBool = AtomicBool::new(false);
/// The Linux program's path, for /proc/self/exe.
pub static EXE: OnceLock<CString> = OnceLock::new();
/// This binary, for execve.
pub const LINUX_RUN: &str = "/usr/bin/linux-run";

pub fn pid() -> i32 {
    PID.load(Ordering::Relaxed)
}
pub fn refresh_pid() {
    PID.store(unsafe { libc::getpid() }, Ordering::Relaxed);
}

pub fn fill_random(buf: &mut [u8]) {
    let ok = std::fs::File::open("/scheme/rand").and_then(|mut f| f.read_exact(buf));
    if ok.is_err() {
        crate::fatal("could not read /scheme/rand");
    }
}

pub fn exit_group(status: i32) -> ! {
    unsafe { libc::_exit(status) }
}

pub unsafe fn futex_wake(addr: *mut u32, n: u32) -> usize {
    unsafe extern "C" {
        fn redox_futex_wake_v0(addr: *mut u32, num: u32) -> usize;
    }
    unsafe { redox_futex_wake_v0(addr, n) }
}

/// Called by the entry stub on the thread's native stack with the native FS base loaded.
pub extern "C" fn lr_dispatch(frame: *mut Frame, data: usize) {
    let frame = unsafe { &mut *frame };
    let thread = thread::current(data);
    let number = frame.rax;
    if let Some(r) = call(thread, frame) {
        if r == -EINTR && number != 34 && number != 130 && signal::restart_after_eintr(thread) {
            // Restarted as Linux does: back over the two-byte `syscall` with the call number,
            // after any handler that runs first.
            frame.rax = number;
            frame.rip -= 2;
        } else {
            frame.rax = r as u64;
        }
    }
    signal::deliver(thread, frame);
}

/// Says once on standard error, with LINUX_RUN_TRACE set, that something is not answered.
pub fn log_once(what: &str) {
    static SEEN: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    if TRACE.load(Ordering::Relaxed) {
        let mut seen = SEEN.lock().unwrap();
        if !seen.iter().any(|s| s == what) {
            eprintln!("linux-run: {what}");
            seen.push(what.to_string());
        }
    }
}

/// Reads a NUL-terminated Linux string.
pub fn path(p: u64) -> Result<CString, i64> {
    if !user_ok(p, 1) {
        return Err(EFAULT);
    }
    let mut n = 0u64;
    while n < 4096 {
        if !user_ok(p + n, 1) {
            return Err(EFAULT);
        }
        if unsafe { *((p + n) as *const u8) } == 0 {
            let bytes = unsafe { core::slice::from_raw_parts(p as *const u8, n as usize) };
            return CString::new(bytes).map_err(|_| EINVAL);
        }
        n += 1;
    }
    Err(ENAMETOOLONG)
}

fn ret(r: isize) -> i64 {
    if r < 0 { -last_errno() } else { r as i64 }
}

/// Linux open flags to relibc's.
fn open_flags(f: u64) -> Result<i32, i64> {
    let mut o = match f & 3 {
        0 => libc::O_RDONLY,
        1 => libc::O_WRONLY,
        2 => libc::O_RDWR,
        _ => return Err(EINVAL),
    };
    let table: [(u64, i32); 9] = [
        (0x40, libc::O_CREAT),
        (0x80, libc::O_EXCL),
        (0x100, libc::O_NOCTTY),
        (0x200, libc::O_TRUNC),
        (0x400, libc::O_APPEND),
        (0x800, libc::O_NONBLOCK),
        (0x10000, libc::O_DIRECTORY),
        (0x20000, libc::O_NOFOLLOW),
        (0x80000, libc::O_CLOEXEC),
    ];
    for (l, n) in table {
        if f & l != 0 {
            o |= n;
        }
    }
    if f & 0x1000 != 0 {
        o |= libc::O_SYNC;
    }
    if f & 0x200000 != 0 {
        o |= libc::O_PATH;
    }
    Ok(o)
}

/// relibc status flags to Linux's, for F_GETFL.
fn linux_status_flags(n: i32) -> u64 {
    let mut f = match n & libc::O_ACCMODE {
        x if x == libc::O_WRONLY => 1,
        x if x == libc::O_RDWR => 2,
        _ => 0,
    };
    if n & libc::O_APPEND != 0 {
        f |= 0x400
    }
    if n & libc::O_NONBLOCK != 0 {
        f |= 0x800
    }
    if n & libc::O_SYNC != 0 {
        f |= 0x101000
    }
    f | 0x8000 // O_LARGEFILE, as Linux reports for 64-bit programs
}

fn write_stat(st: &libc::stat, out: u64) -> i64 {
    if !user_ok(out, 144) {
        return -EFAULT;
    }
    let w: [u64; 18] = [
        st.st_dev as u64,
        st.st_ino as u64,
        st.st_nlink as u64,
        (st.st_mode as u64) | (st.st_uid as u64) << 32,
        st.st_gid as u64,
        st.st_rdev as u64,
        st.st_size as u64,
        st.st_blksize as u64,
        st.st_blocks as u64,
        st.st_atime as u64,
        st.st_atime_nsec as u64,
        st.st_mtime as u64,
        st.st_mtime_nsec as u64,
        st.st_ctime as u64,
        st.st_ctime_nsec as u64,
        0,
        0,
        0,
    ];
    unsafe { core::ptr::copy_nonoverlapping(w.as_ptr(), out as *mut u64, 18) };
    0
}

/// Linux clock ids to relibc's.
fn clock(id: u64) -> Result<libc::clockid_t, i64> {
    match id {
        0 | 5 => Ok(libc::CLOCK_REALTIME),
        1 | 4 | 6 | 7 => Ok(libc::CLOCK_MONOTONIC),
        2 => Ok(libc::CLOCK_PROCESS_CPUTIME_ID),
        _ => Err(EINVAL),
    }
}

fn futex(frame: &Frame) -> i64 {
    unsafe extern "C" {
        fn redox_futex_wait_v0(addr: *mut u32, val: u32, deadline: *const libc::timespec) -> usize;
    }
    const WAIT: u64 = 0;
    const WAKE: u64 = 1;
    const WAIT_BITSET: u64 = 9;
    const WAKE_BITSET: u64 = 10;
    let (addr, op, val, timeout) = (frame.rdi, frame.rsi & 0x7f, frame.rdx, frame.r10);
    if !user_ok(addr, 4) || addr % 4 != 0 {
        return -EFAULT;
    }
    match op {
        WAIT | WAIT_BITSET => {
            let deadline = if timeout == 0 {
                None
            } else {
                if !user_ok(timeout, 16) {
                    return -EFAULT;
                }
                let (sec, nsec) =
                    unsafe { (*(timeout as *const i64), *((timeout + 8) as *const i64)) };
                if !(0..1_000_000_000).contains(&nsec) || sec < 0 {
                    return -EINVAL;
                }
                let mut now = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                // FUTEX_WAIT counts a relative time; WAIT_BITSET an absolute one on the
                // monotonic clock (or the real-time clock with FUTEX_CLOCK_REALTIME).
                if op == WAIT {
                    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
                    let ns = now.tv_nsec as i64 + nsec;
                    Some(libc::timespec {
                        tv_sec: now.tv_sec + sec + ns / 1_000_000_000,
                        tv_nsec: (ns % 1_000_000_000) as _,
                    })
                } else if frame.rsi & 256 != 0 {
                    // Absolute real time: turn it into monotonic.
                    let mut real = libc::timespec {
                        tv_sec: 0,
                        tv_nsec: 0,
                    };
                    unsafe {
                        libc::clock_gettime(libc::CLOCK_REALTIME, &mut real);
                        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now);
                    }
                    let delta = (sec - real.tv_sec) * 1_000_000_000 + nsec - real.tv_nsec as i64;
                    let ns = now.tv_sec * 1_000_000_000 + now.tv_nsec as i64 + delta.max(0);
                    Some(libc::timespec {
                        tv_sec: ns / 1_000_000_000,
                        tv_nsec: (ns % 1_000_000_000) as _,
                    })
                } else {
                    Some(libc::timespec {
                        tv_sec: sec,
                        tv_nsec: nsec as _,
                    })
                }
            };
            let r = unsafe {
                redox_futex_wait_v0(
                    addr as *mut u32,
                    val as u32,
                    deadline.as_ref().map_or(core::ptr::null(), |d| d),
                )
            };
            match raw(r) {
                Ok(_) => 0,
                Err(e) => -e,
            }
        }
        WAKE | WAKE_BITSET => match raw(unsafe { futex_wake(addr as *mut u32, val as u32) }) {
            Ok(n) => n as i64,
            Err(e) => -e,
        },
        _ => -ENOSYS,
    }
}

fn execve(thread: &Thread, frame: &Frame) -> i64 {
    let file = match path(frame.rdi) {
        Ok(p) => crate::files::exe_alias(&p).unwrap_or(p),
        Err(e) => return -e,
    };
    let list = |p: u64| -> Result<Vec<CString>, i64> {
        let mut out = Vec::new();
        if p == 0 {
            return Ok(out);
        }
        for i in 0..4096u64 {
            if !user_ok(p + 8 * i, 8) {
                return Err(EFAULT);
            }
            let s = unsafe { *((p + 8 * i) as *const u64) };
            if s == 0 {
                return Ok(out);
            }
            out.push(path(s)?);
        }
        Err(E2BIG)
    };
    let (argv, envp) = match (list(frame.rsi), list(frame.rdx)) {
        (Ok(a), Ok(e)) => (a, e),
        (Err(e), _) | (_, Err(e)) => return -e,
    };
    // Errors Linux's execve would return come back from here, not from the next linux-run.
    let bytes = match std::fs::read(std::ffi::OsStr::from_bytes(file.as_bytes())) {
        Ok(b) => b,
        Err(e) => return -linux_errno(e.raw_os_error().unwrap_or(libc::EIO)),
    };
    if let Err((e, _)) = crate::elf::check(&bytes) {
        return -e;
    }
    drop(bytes);
    let argv0 = argv.first().cloned().unwrap_or_else(|| file.clone());
    let mut args: Vec<CString> = vec![
        CString::new(LINUX_RUN).unwrap(),
        CString::new("--argv0").unwrap(),
        argv0,
        CString::new("--").unwrap(),
        file,
    ];
    args.extend(argv.into_iter().skip(1));
    let envp: Vec<CString> = envp
        .into_iter()
        .filter(|e| {
            !e.as_bytes().starts_with(b"LD_LIBRARY_PATH=")
                && !e.as_bytes().starts_with(b"LD_PRELOAD=")
        })
        .collect();
    let mut a: Vec<*const libc::c_char> = args.iter().map(|s| s.as_ptr()).collect();
    a.push(core::ptr::null());
    let mut e: Vec<*const libc::c_char> = envp.iter().map(|s| s.as_ptr()).collect();
    e.push(core::ptr::null());
    // The Linux mask crosses execve as the native one (src/signal.rs, init).
    signal::mask_for_exec(thread.mask.load(Ordering::Relaxed));
    unsafe { libc::execve(a[0], a.as_ptr(), e.as_ptr()) };
    let err = last_errno();
    signal::unmask_after_exec();
    -err
}

fn uname(buf: u64) -> i64 {
    if !user_ok(buf, 6 * 65) {
        return -EFAULT;
    }
    let mut native: libc::utsname = unsafe { core::mem::zeroed() };
    unsafe { libc::uname(&mut native) };
    let node: Vec<u8> = native
        .nodename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    let fields: [&[u8]; 6] = [
        b"Linux",
        &node,
        b"6.6.0-ferrion",
        b"#1 FerrionOS",
        b"x86_64",
        b"(none)",
    ];
    unsafe {
        core::ptr::write_bytes(buf as *mut u8, 0, 6 * 65);
        for (i, f) in fields.iter().enumerate() {
            let n = f.len().min(64);
            core::ptr::copy_nonoverlapping(f.as_ptr(), (buf + 65 * i as u64) as *mut u8, n);
        }
    }
    0
}

fn call(thread: &Thread, f: &mut Frame) -> Option<i64> {
    let (a, b, c, d) = (f.rdi, f.rsi, f.rdx, f.r10);
    let r = match f.rax {
        0 => {
            if !user_ok(b, c) && c != 0 {
                return Some(-EFAULT);
            }
            ret(unsafe { libc::read(a as i32, b as *mut _, c as usize) })
        }
        1 => {
            if !user_ok(b, c) && c != 0 {
                return Some(-EFAULT);
            }
            ret(unsafe { libc::write(a as i32, b as *const _, c as usize) })
        }
        3 => crate::files::close(a),
        5 => {
            let mut st: libc::stat = unsafe { core::mem::zeroed() };
            if unsafe { libc::fstat(a as i32, &mut st) } != 0 {
                return Some(-last_errno());
            }
            write_stat(&st, b)
        }
        8 => crate::files::lseek(a, b, c),
        9 => mem::mmap(a, b, c, d, f.r8 as i64, f.r9),
        10 => mem::mprotect(a, b, c),
        11 => mem::munmap(a, b),
        12 => mem::brk(a),
        13 => signal::rt_sigaction(a, b, c, d),
        14 => signal::rt_sigprocmask(thread, a, b, c, d),
        15 => {
            if signal::sigreturn(thread, f).is_err() {
                signal::default_action(11);
            }
            return None;
        }
        16 => crate::tty::ioctl(a, b, c),
        20 => {
            if c > 1024 {
                return Some(-EINVAL);
            }
            if c != 0 && !user_ok(b, 16 * c) {
                return Some(-EFAULT);
            }
            for i in 0..c {
                let (p, n) = unsafe {
                    (
                        *((b + 16 * i) as *const u64),
                        *((b + 16 * i + 8) as *const u64),
                    )
                };
                if n != 0 && !user_ok(p, n) {
                    return Some(-EFAULT);
                }
            }
            ret(unsafe { libc::writev(a as i32, b as *const libc::iovec, c as i32) })
        }
        24 => ret(unsafe { libc::sched_yield() } as isize),
        28 => mem::madvise(a, b, c),
        35 => {
            if !user_ok(a, 16) || (b != 0 && !user_ok(b, 16)) {
                return Some(-EFAULT);
            }
            ret(
                unsafe { libc::nanosleep(a as *const libc::timespec, b as *mut libc::timespec) }
                    as isize,
            )
        }
        39 => pid() as i64,
        56 => thread::clone(thread, f),
        57 | 58 => {
            // fork, and vfork, which is fork-like here: the parent waits for exec or exit.
            let rdi = f.rdi;
            f.rdi = if f.rax == 58 { 0x4000 | 0x100 | 17 } else { 17 };
            f.rsi = 0;
            let r = thread::clone(thread, f);
            f.rdi = rdi;
            r
        }
        59 => execve(thread, f),
        60 => thread::exit_thread(thread, a as i32),
        62 => kill(a as i64, b),
        63 => uname(a),
        72 => fcntl(a, b, c),
        102 => unsafe { libc::getuid() as i64 },
        104 => unsafe { libc::getgid() as i64 },
        107 => unsafe { libc::geteuid() as i64 },
        108 => unsafe { libc::getegid() as i64 },
        110 => unsafe { libc::getppid() as i64 },
        131 => signal::sigaltstack(thread, f, a, b),
        158 => arch_prctl(thread, a, b),
        186 => thread.tid() as i64,
        200 => tgkill(thread, pid() as i64, a as i64, b),
        202 => futex(f),
        204 => {
            if b < 8 {
                return Some(-EINVAL);
            }
            if !user_ok(c, b.min(128)) {
                return Some(-EFAULT);
            }
            let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.clamp(1, 64) as u32;
            unsafe {
                core::ptr::write_bytes(c as *mut u8, 0, b.min(128) as usize);
                *(c as *mut u64) = if n == 64 { !0 } else { (1u64 << n) - 1 };
            }
            8
        }
        218 => {
            thread.clear_tid.store(a, Ordering::Relaxed);
            thread.tid() as i64
        }
        228 | 229 => {
            let id = match clock(a) {
                Ok(id) => id,
                Err(e) => return Some(-e),
            };
            if b == 0 {
                return Some(if f.rax == 229 { -EINVAL } else { -EFAULT });
            }
            if !user_ok(b, 16) {
                return Some(-EFAULT);
            }
            let mut ts = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            let r = unsafe {
                if f.rax == 228 {
                    libc::clock_gettime(id, &mut ts)
                } else {
                    libc::clock_getres(id, &mut ts)
                }
            };
            if r != 0 {
                return Some(-last_errno());
            }
            unsafe { *(b as *mut [i64; 2]) = [ts.tv_sec, ts.tv_nsec as i64] };
            0
        }
        231 => exit_group(a as i32),
        234 => tgkill(thread, a as i64, b as i64, c),
        2 | 257 => {
            let (a, b, c, d) = if f.rax == 2 { (AT_FDCWD_U, a, b, c) } else { (a, b, c, d) };
            let p = match path(b) {
                Ok(p) => crate::files::exe_alias(&p).unwrap_or(p),
                Err(e) => return Some(-e),
            };
            let flags = match open_flags(c) {
                Ok(o) => o,
                Err(e) => return Some(-e),
            };
            if c & 3 == 0 {
                if let Some(r) = crate::files::etc_view(&p, c & 0x80000 != 0) {
                    return Some(r);
                }
            }
            ret(unsafe { libc::openat(a as i32, p.as_ptr(), flags, d as libc::c_uint) } as isize)
        }
        4 | 6 | 262 => {
            let (a, b, c, d) = match f.rax {
                4 => (AT_FDCWD_U, a, b, 0),
                6 => (AT_FDCWD_U, a, b, 0x100),
                _ => (a, b, c, d),
            };
            const AT_EMPTY_PATH: u64 = 0x1000;
            const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
            let p = match path(b) {
                Ok(p) => p,
                Err(e) => return Some(-e),
            };
            let p = crate::files::exe_alias(&p).unwrap_or(p);
            let mut st: libc::stat = unsafe { core::mem::zeroed() };
            let r = if p.as_bytes().is_empty() && d & AT_EMPTY_PATH != 0 {
                unsafe { libc::fstat(a as i32, &mut st) }
            } else {
                let fl = if d & AT_SYMLINK_NOFOLLOW != 0 {
                    libc::AT_SYMLINK_NOFOLLOW
                } else {
                    0
                };
                unsafe { libc::fstatat(a as i32, p.as_ptr(), &mut st, fl) }
            };
            if r != 0 {
                return Some(-last_errno());
            }
            write_stat(&st, c)
        }
        267 => readlinkat(a, b, c, d),
        273 => {
            if b != 24 {
                return Some(-EINVAL);
            }
            thread.robust_list.store(a, Ordering::Relaxed);
            0
        }
        302 => prlimit(a, b, c, d),
        318 => {
            if b != 0 && !user_ok(a, b) {
                return Some(-EFAULT);
            }
            let n = b.min(1 << 25) as usize;
            fill_random(unsafe { core::slice::from_raw_parts_mut(a as *mut u8, n) });
            n as i64
        }
        7 => crate::files::poll(a, b, c as i64),
        17 => crate::files::pread(a, b, c, d, false),
        18 => crate::files::pread(a, b, c, d, true),
        19 => crate::files::readv(a, b, c),
        21 => crate::files::faccessat(AT_FDCWD_U, a, b, 0),
        22 => crate::files::pipe2(a, 0),
        32 => ret(unsafe { libc::dup(a as i32) } as isize),
        33 => crate::files::dup2(a, b),
        34 => {
            let mut none: libc::sigset_t = unsafe { core::mem::zeroed() };
            unsafe { libc::sigemptyset(&mut none) };
            while !signal::pending_for(thread) {
                unsafe { crate::ffi::sigsuspend(&none) };
            }
            -EINTR
        }
        37 => unsafe { libc::alarm(a as u32) as i64 },
        40 => crate::files::sendfile(a, b, c, d),
        61 => crate::files::wait4(a, b, c, d),
        76 => crate::files::truncate(a, b),
        77 => ret(unsafe { libc::ftruncate(a as i32, b as libc::off_t) } as isize),
        79 => crate::files::getcwd(a, b),
        80 => crate::files::chdir(a),
        81 => crate::files::fchdir(a),
        82 => crate::files::renameat(AT_FDCWD_U, a, AT_FDCWD_U, b, 0),
        83 => crate::files::mkdirat(AT_FDCWD_U, a, b),
        84 => crate::files::unlinkat(AT_FDCWD_U, a, 0x200),
        86 => crate::files::linkat(AT_FDCWD_U, a, AT_FDCWD_U, b, 0),
        87 => crate::files::unlinkat(AT_FDCWD_U, a, 0),
        88 => crate::files::symlinkat(a, AT_FDCWD_U, b),
        89 => readlinkat(AT_FDCWD_U, a, b, c),
        90 => crate::files::fchmodat(AT_FDCWD_U, a, b),
        91 => ret(unsafe { libc::fchmod(a as i32, b as libc::mode_t) } as isize),
        92 => crate::files::fchownat(AT_FDCWD_U, a, b, c, 0),
        93 => ret(unsafe { libc::fchown(a as i32, b as u32, c as u32) } as isize),
        94 => crate::files::fchownat(AT_FDCWD_U, a, b, c, 0x100),
        95 => unsafe { libc::umask(a as libc::mode_t) as i64 },
        96 => {
            if a != 0 {
                if !user_ok(a, 16) {
                    return Some(-EFAULT);
                }
                let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) };
                unsafe { *(a as *mut [i64; 2]) = [ts.tv_sec, ts.tv_nsec as i64 / 1000] };
            }
            if b != 0 && user_ok(b, 8) {
                unsafe { *(b as *mut [i32; 2]) = [0, 0] };
            }
            0
        }
        97 => prlimit(0, a, 0, b),
        160 => prlimit(0, a, b, 0),
        98 => {
            if !user_ok(b, 144) {
                return Some(-EFAULT);
            }
            unsafe { core::ptr::write_bytes(b as *mut u8, 0, 144) };
            0
        }
        100 => {
            if a != 0 {
                if !user_ok(a, 32) {
                    return Some(-EFAULT);
                }
                unsafe { *(a as *mut [i64; 4]) = [0; 4] };
            }
            let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
            ts.tv_sec * 100 + ts.tv_nsec as i64 / 10_000_000
        }
        105 => ret(unsafe { libc::setuid(a as u32) } as isize),
        106 => ret(unsafe { libc::setgid(a as u32) } as isize),
        109 => ret(unsafe { libc::setpgid(a as i32, b as i32) } as isize),
        111 => ret(unsafe { libc::getpgrp() } as isize),
        112 => ret(unsafe { libc::setsid() } as isize),
        113 => ret(unsafe { libc::setreuid(a as u32, b as u32) } as isize),
        114 => ret(unsafe { libc::setregid(a as u32, b as u32) } as isize),
        115 => crate::files::getgroups(a, b),
        116 => -EPERM,
        117 | 119 => {
            // setresuid/setresgid: -1 keeps an id; the process manager's rule decides the rest.
            let keep = |v: u64| v as u32 == u32::MAX;
            let r = unsafe {
                if f.rax == 117 {
                    libc::setreuid(if keep(a) { u32::MAX } else { a as u32 }, if keep(b) { u32::MAX } else { b as u32 })
                } else {
                    libc::setregid(if keep(a) { u32::MAX } else { a as u32 }, if keep(b) { u32::MAX } else { b as u32 })
                }
            };
            ret(r as isize)
        }
        118 => unsafe { crate::files::getres([libc::getuid(), libc::geteuid(), libc::geteuid()], [a, b, c]) },
        120 => unsafe { crate::files::getres([libc::getgid(), libc::getegid(), libc::getegid()], [a, b, c]) },
        121 => ret(unsafe { libc::getpgid(a as i32) } as isize),
        124 => ret(unsafe { crate::ffi::getsid(a as i32) } as isize),
        127 => signal::rt_sigpending(thread, a, b),
        130 => signal::sigsuspend(thread, a, b),
        157 => crate::files::prctl(a, b),
        201 => {
            let t = unsafe { libc::time(core::ptr::null_mut()) } as i64;
            if a != 0 {
                if !user_ok(a, 8) {
                    return Some(-EFAULT);
                }
                unsafe { *(a as *mut i64) = t };
            }
            t
        }
        217 => crate::files::getdents64(a, b, c),
        230 => {
            // clock_nanosleep(clock, flags, request, remain); TIMER_ABSTIME is 1.
            let id = match clock(a) {
                Ok(id) => id,
                Err(e) => return Some(-e),
            };
            if !user_ok(c, 16) || (d != 0 && !user_ok(d, 16)) {
                return Some(-EFAULT);
            }
            // relibc has no clock_nanosleep: an absolute time becomes the interval from now.
            let mut req = unsafe { *(c as *const libc::timespec) };
            if b & 1 != 0 {
                let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
                unsafe { libc::clock_gettime(id, &mut now) };
                let ns = (req.tv_sec - now.tv_sec) * 1_000_000_000 + (req.tv_nsec - now.tv_nsec) as i64;
                let ns = ns.max(0);
                req = libc::timespec { tv_sec: ns / 1_000_000_000, tv_nsec: (ns % 1_000_000_000) as _ };
            }
            let rem = if b & 1 == 0 { d } else { 0 };
            ret(unsafe { libc::nanosleep(&req, rem as *mut libc::timespec) } as isize)
        }
        258 => crate::files::mkdirat(a, b, c),
        260 => crate::files::fchownat(a, b, c, d, f.r8),
        263 => crate::files::unlinkat(a, b, c),
        264 => crate::files::renameat(a, b, c, d, 0),
        316 => crate::files::renameat(a, b, c, d, f.r8),
        265 => crate::files::linkat(a, b, c, d, f.r8),
        266 => crate::files::symlinkat(a, b, c),
        268 => crate::files::fchmodat(a, b, c),
        269 => crate::files::faccessat(a, b, c, 0),
        439 => crate::files::faccessat(a, b, c, d),
        280 => crate::files::utimensat(a, b, c, d),
        292 => crate::files::dup3(a, b, c),
        293 => crate::files::pipe2(a, b),
        436 => {
            // close_range(first, last, flags): CLOSE_RANGE_CLOEXEC is 4.
            if a > b {
                return Some(-EINVAL);
            }
            for fd in a..=b.min(4095) {
                if c & 4 != 0 {
                    unsafe { libc::fcntl(fd as i32, libc::F_SETFD, libc::FD_CLOEXEC) };
                } else {
                    crate::files::close(fd);
                }
            }
            0
        }
        // rseq: glibc carries on without it. clone3: glibc falls back to clone.
        334 | 435 => -ENOSYS,
        n => {
            if TRACE.load(Ordering::Relaxed) {
                eprintln!("linux-run: system call {n} is not answered (ENOSYS)");
            }
            -ENOSYS
        }
    };
    Some(r)
}

fn fcntl(fd: u64, cmd: u64, arg: u64) -> i64 {
    let fd = fd as i32;
    match cmd {
        0 => ret(unsafe { libc::fcntl(fd, libc::F_DUPFD, arg as i32) } as isize),
        1030 => ret(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, arg as i32) } as isize),
        1 => {
            let r = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if r < 0 {
                -last_errno()
            } else {
                (r & libc::FD_CLOEXEC != 0) as i64
            }
        }
        2 => ret(unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFD,
                if arg & 1 != 0 { libc::FD_CLOEXEC } else { 0 },
            )
        } as isize),
        3 => {
            let r = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if r < 0 {
                -last_errno()
            } else {
                linux_status_flags(r) as i64
            }
        }
        4 => {
            let mut n = 0;
            if arg & 0x400 != 0 {
                n |= libc::O_APPEND
            }
            if arg & 0x800 != 0 {
                n |= libc::O_NONBLOCK
            }
            ret(unsafe { libc::fcntl(fd, libc::F_SETFL, n) } as isize)
        }
        _ => -EINVAL,
    }
}

fn arch_prctl(thread: &Thread, code: u64, addr: u64) -> i64 {
    const SET_GS: u64 = 0x1001;
    const SET_FS: u64 = 0x1002;
    const GET_FS: u64 = 0x1003;
    const GET_GS: u64 = 0x1004;
    match code {
        SET_FS | SET_GS => {
            if addr >= crate::LINUX_TOP as u64 {
                return -EPERM;
            }
            if code == SET_FS {
                thread.set_linux_fs(addr)
            } else {
                crate::cpu::wrgsbase(addr)
            }
            0
        }
        GET_FS | GET_GS => {
            if !user_ok(addr, 8) {
                return -EFAULT;
            }
            let v = if code == GET_FS {
                thread.linux_fs()
            } else {
                crate::cpu::rdgsbase()
            };
            unsafe { *(addr as *mut u64) = v };
            0
        }
        _ => -EINVAL,
    }
}

fn kill(target: i64, sig: u64) -> i64 {
    if sig > 64 {
        return -EINVAL;
    }
    if target == pid() as i64 && sig != 0 {
        // To itself: a handled signal (or one without a native number) is queued here and
        // delivered before kill returns; otherwise the native default or ignore applies.
        let h = signal::handler_of(sig);
        if sig > 31 || (h != 0 && h != 1) {
            signal::send_process(sig);
            return 0;
        }
    }
    if sig > 31 {
        return -EINVAL;
    }
    ret(unsafe { libc::kill(target as i32, sig as i32) } as isize)
}

fn tgkill(caller: &Thread, tgid: i64, tid: i64, sig: u64) -> i64 {
    if sig > 64 || tid <= 0 {
        return -EINVAL;
    }
    if tgid != pid() as i64 {
        if tgid > 0 && tid == tgid && sig <= 31 {
            // Another process's main thread: the process-directed native signal.
            return ret(unsafe { libc::kill(tgid as i32, sig as i32) } as isize);
        }
        return -ESRCH;
    }
    match thread::find(tid as i32) {
        Some(t) => {
            if sig != 0 {
                signal::send_thread(&t, sig, caller);
            }
            0
        }
        None => -ESRCH,
    }
}

pub fn readlinkat(dirfd: u64, p: u64, buf: u64, size: u64) -> i64 {
    let p = match path(p) {
        Ok(p) => p,
        Err(e) => return -e,
    };
    if size == 0 || size > i32::MAX as u64 {
        return -EINVAL;
    }
    if !user_ok(buf, size) {
        return -EFAULT;
    }
    if let Some(exe) = crate::files::exe_alias(&p) {
        let exe = exe.as_bytes();
        let n = exe.len().min(size as usize);
        unsafe { core::ptr::copy_nonoverlapping(exe.as_ptr(), buf as *mut u8, n) };
        return n as i64;
    }
    ret(unsafe {
        libc::readlinkat(
            dirfd as i32,
            p.as_ptr(),
            buf as *mut libc::c_char,
            size as usize,
        )
    })
}

fn prlimit(target: u64, res: u64, new: u64, old: u64) -> i64 {
    if target != 0 && target != pid() as u64 {
        return -EPERM;
    }
    if res > 15 {
        return -EINVAL;
    }
    if (new != 0 && !user_ok(new, 16)) || (old != 0 && !user_ok(old, 16)) {
        return -EFAULT;
    }
    let mut cur = libc::rlimit {
        rlim_cur: libc::RLIM_INFINITY,
        rlim_max: libc::RLIM_INFINITY,
    };
    // relibc numbers RLIMIT_CPU to RLIMIT_MEMLOCK (0 to 8) as Linux does; it knows no others,
    // which Linux programs see as unlimited.
    if res <= 8 && unsafe { libc::getrlimit(res as i32, &mut cur) } != 0 {
        return -last_errno();
    }
    if old != 0 {
        unsafe { *(old as *mut [u64; 2]) = [cur.rlim_cur, cur.rlim_max] };
    }
    if new != 0 {
        let [c, m] = unsafe { *(new as *const [u64; 2]) };
        if c > m {
            return -EINVAL;
        }
        if res > 8 {
            return 0;
        }
        let lim = libc::rlimit {
            rlim_cur: c,
            rlim_max: m,
        };
        if unsafe { libc::setrlimit(res as i32, &lim) } != 0 {
            return -last_errno();
        }
    }
    0
}
