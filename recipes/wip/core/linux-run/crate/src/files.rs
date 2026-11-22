//! The file, directory, descriptor, identity and process calls a shell and the core tools need
//! (plans/linux-compat.md, LC2 and LC3): each is relibc's call with Linux's flags, layouts and
//! error numbers.

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Mutex;

use crate::{errno::*, mem::user_ok, syscall::cstr};

const AT_FDCWD: i64 = -100;
const L_AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const L_AT_REMOVEDIR: u64 = 0x200;
const L_AT_EACCESS: u64 = 0x200;
const L_AT_SYMLINK_FOLLOW: u64 = 0x400;
const L_AT_EMPTY_PATH: u64 = 0x1000;

/// Descriptors from this number up are the runtime's: the copies a vfork child's changes keep
/// (src/spawn.rs) and the session handle (src/session.rs). Linux calls cannot name, close or
/// replace them, and RLIMIT_NOFILE ends below them.
pub const RESERVED: i32 = crate::spawn::SAVED_FDS;

pub fn reserved(fd: i32) -> bool {
    fd >= RESERVED
}

fn ret(r: isize) -> i64 {
    if r < 0 { -last_errno() } else { r as i64 }
}

/// A descriptor a call created for the Linux program: one in the runtime's range is given back
/// (EMFILE), as when Linux's table is full; a vfork child's is noted (src/spawn.rs).
pub fn new_fd(r: i64) -> i64 {
    if r < 0 {
        return r;
    }
    if reserved(r as i32) {
        unsafe { libc::close(r as i32) };
        return -EMFILE;
    }
    crate::spawn::fd_created(r as i32);
    r
}

pub fn dirfd(fd: u64) -> i32 {
    if fd as i64 as i32 as i64 == AT_FDCWD { libc::AT_FDCWD } else { fd as i32 }
}

/// The native path for a Linux path: /proc/self/exe answered, then the session's mounts.
pub fn native_path(raw: CString, follow: bool) -> CString {
    let p = exe_alias(&raw).unwrap_or(raw);
    crate::session::linux_path_at(p, follow)
}

/// Opening /dev/stdin, /dev/stdout, /dev/stderr, /dev/fd/N or /proc/self/fd/N gives a new
/// descriptor of the open file N, as Linux's reopen of it does for pipes and terminals.
pub fn open_own_fd(p: &CString, cloexec: bool) -> Option<i64> {
    let b = p.as_bytes();
    let n: i32 = match b {
        b"/dev/stdin" => 0,
        b"/dev/stdout" => 1,
        b"/dev/stderr" => 2,
        _ => {
            let rest = b.strip_prefix(b"/dev/fd/").or_else(|| b.strip_prefix(b"/proc/self/fd/"))?;
            std::str::from_utf8(rest).ok()?.parse().ok()?
        }
    };
    if reserved(n) {
        return Some(-ENOENT);
    }
    let how = if cloexec { libc::F_DUPFD_CLOEXEC } else { libc::F_DUPFD };
    let r = unsafe { libc::fcntl(n, how, 0) };
    Some(if r < 0 { -ENOENT } else { new_fd(r as i64) })
}

pub const RUNTIME_FDS_END: i32 = 4096;

pub fn dup_at_least(fd: i32, min: i32, end: i32, cloexec: bool) -> i64 {
    if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return -last_errno();
    }
    for n in min..end {
        if unsafe { libc::fcntl(n, libc::F_GETFD) } >= 0 {
            continue;
        }
        if unsafe { libc::dup2(fd, n) } != n {
            return -last_errno();
        }
        unsafe { libc::fcntl(n, libc::F_SETFD, if cloexec { libc::FD_CLOEXEC } else { 0 }) };
        return n as i64;
    }
    -EMFILE
}

macro_rules! p {
    ($e:expr) => {
        p!($e, false)
    };
    ($e:expr, $follow:expr) => {
        match cstr($e) {
            Ok(p) => crate::session::linux_path_at(p, $follow),
            Err(e) => return -e,
        }
    };
}

/// Directory streams behind getdents64, by descriptor. relibc reads directories through `DIR`,
/// so each descriptor gets a stream on a duplicate of itself, with the entry that did not fit
/// the caller's buffer kept for the next call.
struct DirStream {
    dir: usize,
    held: Option<(u64, u8, Vec<u8>)>,
    off: i64,
}
static DIRS: Mutex<Option<HashMap<i32, DirStream>>> = Mutex::new(None);

/// close and lseek forget or rewind a descriptor's stream.
pub fn forget_dir(fd: i32) {
    if let Some(s) = DIRS.lock().unwrap().get_or_insert_with(HashMap::new).remove(&fd) {
        unsafe { libc::closedir(s.dir as *mut libc::DIR) };
    }
}

pub fn getdents64(fd: u64, buf: u64, len: u64) -> i64 {
    let fd = fd as i32;
    if !user_ok(buf, len) {
        return -EFAULT;
    }
    let mut dirs = DIRS.lock().unwrap();
    let dirs = dirs.get_or_insert_with(HashMap::new);
    if !dirs.contains_key(&fd) {
        let dup = dup_at_least(fd, RESERVED, RUNTIME_FDS_END, true);
        if dup < 0 {
            return dup;
        }
        let dup = dup as i32;
        let dir = unsafe { libc::fdopendir(dup) };
        if dir.is_null() {
            let e = last_errno();
            unsafe { libc::close(dup) };
            return -e;
        }
        dirs.insert(fd, DirStream { dir: dir as usize, held: None, off: 0 });
    }
    let s = dirs.get_mut(&fd).unwrap();
    let mut used = 0u64;
    loop {
        let entry = match s.held.take() {
            Some(e) => e,
            None => {
                unsafe { *libc::__errno_location() = 0 };
                let d = unsafe { libc::readdir(s.dir as *mut libc::DIR) };
                if d.is_null() {
                    let e = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if e != 0 && used == 0 {
                        return -linux_errno(e);
                    }
                    break;
                }
                let d = unsafe { &*d };
                let name: Vec<u8> = d.d_name.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
                (d.d_ino as u64, d.d_type, name)
            }
        };
        let reclen = (19 + entry.2.len() as u64 + 1).next_multiple_of(8);
        if used + reclen > len {
            if used == 0 {
                s.held = Some(entry);
                return -EINVAL;
            }
            s.held = Some(entry);
            break;
        }
        s.off += 1;
        let at = buf + used;
        unsafe {
            core::ptr::write_bytes(at as *mut u8, 0, reclen as usize);
            *(at as *mut u64) = entry.0;
            *((at + 8) as *mut i64) = s.off;
            *((at + 16) as *mut u16) = reclen as u16;
            *((at + 18) as *mut u8) = entry.1;
            core::ptr::copy_nonoverlapping(entry.2.as_ptr(), (at + 19) as *mut u8, entry.2.len());
        }
        used += reclen;
    }
    used as i64
}

pub fn lseek(fd: u64, off: u64, whence: u64) -> i64 {
    if whence == 0 && off == 0 {
        if let Some(s) = DIRS.lock().unwrap().get_or_insert_with(HashMap::new).get_mut(&(fd as i32)) {
            unsafe { libc::rewinddir(s.dir as *mut libc::DIR) };
            s.held = None;
            s.off = 0;
            return 0;
        }
    }
    ret(unsafe { libc::lseek(fd as i32, off as libc::off_t, whence as i32) } as isize)
}

fn forget(fd: i32) {
    forget_dir(fd);
    if !crate::spawn::active() {
        crate::locks::before_close(fd);
        crate::net::closed(fd);
        crate::wait::closed(fd);
    }
}

pub fn duplicated(old: i32, new: i32) {
    if !crate::spawn::active() {
        crate::net::duplicated(old, new);
        crate::wait::duplicated(old, new);
    }
}

pub fn close(fd: u64) -> i64 {
    if reserved(fd as i32) {
        return -EBADF;
    }
    if !crate::spawn::before_fd_change(fd as i32) {
        return -EMFILE;
    }
    forget(fd as i32);
    ret(unsafe { libc::close(fd as i32) } as isize)
}

pub fn pipe2(fds: u64, flags: u64) -> i64 {
    if flags & !(0x80000 | 0x800 | 0x4000) != 0 {
        return -EINVAL;
    }
    if !user_ok(fds, 8) {
        return -EFAULT;
    }
    let mut n = 0;
    if flags & 0x80000 != 0 {
        n |= libc::O_CLOEXEC;
    }
    if flags & 0x800 != 0 {
        n |= libc::O_NONBLOCK;
    }
    let mut p = [0i32; 2];
    if unsafe { libc::pipe2(p.as_mut_ptr(), n) } != 0 {
        return -last_errno();
    }
    if reserved(p[0]) || reserved(p[1]) {
        unsafe {
            libc::close(p[0]);
            libc::close(p[1]);
        }
        return -EMFILE;
    }
    crate::spawn::fd_created(p[0]);
    crate::spawn::fd_created(p[1]);
    unsafe { *(fds as *mut [i32; 2]) = p };
    0
}

pub fn dup(old: u64) -> i64 {
    if reserved(old as i32) {
        return -EBADF;
    }
    let r = new_fd(ret(unsafe { libc::fcntl(old as i32, libc::F_DUPFD, 0) } as isize));
    if r >= 0 {
        duplicated(old as i32, r as i32);
    }
    r
}

pub fn dup3(old: u64, new: u64, flags: u64) -> i64 {
    if flags & !0x80000 != 0 || old == new {
        return -EINVAL;
    }
    if reserved(old as i32) || reserved(new as i32) {
        return -EBADF;
    }
    if !crate::spawn::before_fd_change(new as i32) {
        return -EMFILE;
    }
    forget(new as i32);
    let r = unsafe { libc::dup2(old as i32, new as i32) };
    if r < 0 {
        return -last_errno();
    }
    duplicated(old as i32, r);
    if flags != 0 {
        unsafe { libc::fcntl(r, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    r as i64
}

pub fn dup2(old: u64, new: u64) -> i64 {
    if reserved(old as i32) || reserved(new as i32) {
        return -EBADF;
    }
    if old == new {
        return if unsafe { libc::fcntl(old as i32, libc::F_GETFD) } < 0 { -last_errno() } else { new as i64 };
    }
    if !crate::spawn::before_fd_change(new as i32) {
        return -EMFILE;
    }
    forget(new as i32);
    let r = ret(unsafe { libc::dup2(old as i32, new as i32) } as isize);
    if r >= 0 {
        duplicated(old as i32, r as i32);
    }
    r
}

pub fn faccessat(fd: u64, p: u64, mode: u64, flags: u64) -> i64 {
    let p = p!(p, flags & L_AT_SYMLINK_NOFOLLOW == 0);
    let mut f = 0;
    if flags & L_AT_EACCESS != 0 {
        f |= libc::AT_EACCESS;
    }
    if flags & L_AT_SYMLINK_NOFOLLOW != 0 {
        f |= libc::AT_SYMLINK_NOFOLLOW;
    }
    ret(unsafe { libc::faccessat(dirfd(fd), p.as_ptr(), mode as i32, f) } as isize)
}

pub fn mkdirat(fd: u64, p: u64, mode: u64) -> i64 {
    let p = p!(p);
    ret(unsafe { libc::mkdirat(dirfd(fd), p.as_ptr(), mode as libc::mode_t) } as isize)
}

pub fn unlinkat(fd: u64, p: u64, flags: u64) -> i64 {
    if flags & !L_AT_REMOVEDIR != 0 {
        return -EINVAL;
    }
    let p = p!(p);
    let f = if flags & L_AT_REMOVEDIR != 0 { libc::AT_REMOVEDIR } else { 0 };
    ret(unsafe { libc::unlinkat(dirfd(fd), p.as_ptr(), f) } as isize)
}

/// renameat2(old dir, old path, new dir, new path, flags). RENAME_NOREPLACE refuses an existing
/// target; RENAME_EXCHANGE and RENAME_WHITEOUT are not offered (EINVAL, which `mv` handles).
pub fn renameat(ofd: u64, op: u64, nfd: u64, np: u64, flags: u64) -> i64 {
    const RENAME_NOREPLACE: u64 = 1;
    if flags & !RENAME_NOREPLACE != 0 {
        return -EINVAL;
    }
    let (o, n) = (p!(op), p!(np));
    if flags & RENAME_NOREPLACE != 0 {
        // Not atomic: another process could create the target in between.
        let mut st: libc::stat = unsafe { core::mem::zeroed() };
        if unsafe { libc::fstatat(dirfd(nfd), n.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) } == 0 {
            return -EEXIST;
        }
    }
    ret(unsafe { libc::renameat(dirfd(ofd), o.as_ptr(), dirfd(nfd), n.as_ptr()) } as isize)
}

pub fn linkat(ofd: u64, op: u64, nfd: u64, np: u64, flags: u64) -> i64 {
    let (o, n) = (p!(op), p!(np));
    let f = if flags & L_AT_SYMLINK_FOLLOW != 0 { libc::AT_SYMLINK_FOLLOW } else { 0 };
    ret(unsafe { libc::linkat(dirfd(ofd), o.as_ptr(), dirfd(nfd), n.as_ptr(), f) } as isize)
}

pub fn symlinkat(target: u64, fd: u64, p: u64) -> i64 {
    // The link holds the target as the program wrote it.
    let t = match cstr(target) {
        Ok(t) => t,
        Err(e) => return -e,
    };
    let p = p!(p);
    ret(unsafe { libc::symlinkat(t.as_ptr(), dirfd(fd), p.as_ptr()) } as isize)
}

pub fn fchmodat(fd: u64, p: u64, mode: u64) -> i64 {
    let p = p!(p, true);
    ret(unsafe { libc::fchmodat(dirfd(fd), p.as_ptr(), mode as libc::mode_t, 0) } as isize)
}

pub fn fchownat(fd: u64, p: u64, uid: u64, gid: u64, flags: u64) -> i64 {
    let p = p!(p, flags & L_AT_SYMLINK_NOFOLLOW == 0);
    if p.as_bytes().is_empty() && flags & L_AT_EMPTY_PATH != 0 {
        return ret(unsafe { libc::fchown(fd as i32, uid as u32, gid as u32) } as isize);
    }
    let f = if flags & L_AT_SYMLINK_NOFOLLOW != 0 { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
    ret(unsafe { libc::fchownat(dirfd(fd), p.as_ptr(), uid as u32, gid as u32, f) } as isize)
}

/// utimensat(fd, path, times, flags); a null path names the descriptor itself.
pub fn utimensat(fd: u64, p: u64, times: u64, flags: u64) -> i64 {
    if times != 0 && !user_ok(times, 32) {
        return -EFAULT;
    }
    let mut t = [libc::timespec { tv_sec: 0, tv_nsec: libc::UTIME_NOW }; 2];
    if times != 0 {
        let raw = unsafe { *(times as *const [i64; 4]) };
        for i in 0..2 {
            t[i].tv_sec = raw[2 * i];
            t[i].tv_nsec = match raw[2 * i + 1] {
                0x3fffffff => libc::UTIME_NOW,
                0x3ffffffe => libc::UTIME_OMIT,
                n => n as _,
            };
        }
    }
    if p == 0 {
        return ret(unsafe { libc::futimens(fd as i32, t.as_ptr()) } as isize);
    }
    let p = p!(p, flags & L_AT_SYMLINK_NOFOLLOW == 0);
    let f = if flags & L_AT_SYMLINK_NOFOLLOW != 0 { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
    ret(unsafe { libc::utimensat(dirfd(fd), p.as_ptr(), t.as_ptr(), f) } as isize)
}

/// getcwd: the working directory as the Linux program names it (src/session.rs, `linux_view`).
pub fn getcwd(buf: u64, size: u64) -> i64 {
    if !user_ok(buf, size) {
        return -EFAULT;
    }
    let mut native = vec![0u8; 4096];
    let r = unsafe { libc::getcwd(native.as_mut_ptr().cast(), native.len()) };
    if r.is_null() {
        return -last_errno();
    }
    let n = native.iter().position(|&b| b == 0).unwrap_or(0);
    let view = crate::session::linux_view(&native[..n]);
    if view.len() + 1 > size as usize {
        return -ERANGE;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(view.as_ptr(), buf as *mut u8, view.len());
        *((buf + view.len() as u64) as *mut u8) = 0;
    }
    (view.len() + 1) as i64
}

pub fn chdir(p: u64) -> i64 {
    let p = p!(p, true);
    crate::spawn::before_chdir();
    ret(unsafe { libc::chdir(p.as_ptr()) } as isize)
}

pub fn fchdir(fd: u64) -> i64 {
    if reserved(fd as i32) {
        return -EBADF;
    }
    crate::spawn::before_chdir();
    ret(unsafe { libc::fchdir(fd as i32) } as isize)
}

pub fn truncate(p: u64, len: u64) -> i64 {
    let p = p!(p, true);
    ret(unsafe { libc::truncate(p.as_ptr(), len as libc::off_t) } as isize)
}

/// sendfile(out, in, offset, count): copied through a buffer.
pub fn sendfile(out: u64, inp: u64, off: u64, count: u64) -> i64 {
    if off != 0 && !user_ok(off, 8) {
        return -EFAULT;
    }
    let mut buf = vec![0u8; count.min(1 << 16) as usize];
    let mut pos = if off != 0 { unsafe { *(off as *const i64) } } else { -1 };
    let mut done = 0u64;
    while done < count {
        let want = (count - done).min(buf.len() as u64) as usize;
        let n = unsafe {
            if pos >= 0 {
                libc::pread(inp as i32, buf.as_mut_ptr().cast(), want, pos as libc::off_t)
            } else {
                libc::read(inp as i32, buf.as_mut_ptr().cast(), want)
            }
        };
        if n < 0 {
            if done == 0 {
                return -last_errno();
            }
            break;
        }
        if n == 0 {
            break;
        }
        let mut w = 0;
        while w < n as usize {
            let m = unsafe { libc::write(out as i32, buf[w..].as_ptr().cast(), n as usize - w) };
            if m < 0 {
                if done == 0 && w == 0 {
                    return -last_errno();
                }
                if pos >= 0 {
                    unsafe { *(off as *mut i64) = pos + w as i64 };
                }
                return (done + w as u64) as i64;
            }
            w += m as usize;
        }
        done += n as u64;
        if pos >= 0 {
            pos += n as i64;
        }
    }
    if off != 0 {
        unsafe { *(off as *mut i64) = pos };
    }
    done as i64
}

pub fn pread(fd: u64, buf: u64, len: u64, off: u64, write: bool) -> i64 {
    if len != 0 && !user_ok(buf, len) {
        return -EFAULT;
    }
    ret(unsafe {
        if write {
            libc::pwrite(fd as i32, buf as *const _, len as usize, off as libc::off_t)
        } else {
            libc::pread(fd as i32, buf as *mut _, len as usize, off as libc::off_t)
        }
    })
}

pub fn readv(fd: u64, iov: u64, n: u64) -> i64 {
    if n > 1024 || (n != 0 && !user_ok(iov, 16 * n)) {
        return -EINVAL;
    }
    for i in 0..n {
        let (p, l) = unsafe { (*((iov + 16 * i) as *const u64), *((iov + 16 * i + 8) as *const u64)) };
        if l != 0 && !user_ok(p, l) {
            return -EFAULT;
        }
    }
    ret(unsafe { libc::readv(fd as i32, iov as *const libc::iovec, n as i32) })
}

/// wait4(pid, status, options, rusage). Status words are encoded as on Linux by relibc too.
pub fn wait4(pid: u64, status: u64, options: u64, rusage: u64) -> i64 {
    if (status != 0 && !user_ok(status, 4)) || (rusage != 0 && !user_ok(rusage, 144)) {
        return -EFAULT;
    }
    // Linux's WNOHANG 1, WUNTRACED 2, WCONTINUED 8 are relibc's; __WALL and __WCLONE are ignored.
    let mut st = 0;
    let r = unsafe { libc::waitpid(pid as i32, &mut st, (options & 0xb) as i32) };
    if r < 0 {
        return -last_errno();
    }
    if status != 0 && r > 0 {
        unsafe { *(status as *mut i32) = st };
    }
    if rusage != 0 {
        unsafe { core::ptr::write_bytes(rusage as *mut u8, 0, 144) };
    }
    r as i64
}

pub fn getgroups(size: u64, list: u64) -> i64 {
    if size > 65536 || (size != 0 && !user_ok(list, 4 * size)) {
        return -EINVAL;
    }
    ret(unsafe { libc::getgroups(size as i32, list as *mut libc::gid_t) } as isize)
}

pub fn getres(ids: [u32; 3], out: [u64; 3]) -> i64 {
    for o in out {
        if !user_ok(o, 4) {
            return -EFAULT;
        }
    }
    for (o, v) in out.iter().zip(ids) {
        unsafe { *(*o as *mut u32) = v };
    }
    0
}

/// prctl: names and flags recorded, the rest refused (plans/linux-compat.md, Processes and
/// threads).
pub fn prctl(op: u64, arg: u64) -> i64 {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NO_NEW_PRIVS: AtomicU64 = AtomicU64::new(0);
    static DUMPABLE: AtomicU64 = AtomicU64::new(1);
    static NAME: Mutex<[u8; 16]> = Mutex::new([0; 16]);
    match op {
        15 /* PR_SET_NAME */ => {
            if !user_ok(arg, 1) {
                return -EFAULT;
            }
            let mut n = [0u8; 16];
            for (i, b) in n.iter_mut().take(15).enumerate() {
                if !user_ok(arg + i as u64, 1) {
                    break;
                }
                *b = unsafe { *((arg + i as u64) as *const u8) };
                if *b == 0 {
                    break;
                }
            }
            *NAME.lock().unwrap() = n;
            0
        }
        16 /* PR_GET_NAME */ => {
            if !user_ok(arg, 16) {
                return -EFAULT;
            }
            unsafe { *(arg as *mut [u8; 16]) = *NAME.lock().unwrap() };
            0
        }
        38 /* PR_SET_NO_NEW_PRIVS */ => {
            if arg != 1 {
                return -EINVAL;
            }
            NO_NEW_PRIVS.store(1, Ordering::Relaxed);
            0
        }
        39 /* PR_GET_NO_NEW_PRIVS */ => NO_NEW_PRIVS.load(Ordering::Relaxed) as i64,
        4 /* PR_SET_DUMPABLE */ => {
            if arg > 1 {
                return -EINVAL;
            }
            DUMPABLE.store(arg, Ordering::Relaxed);
            0
        }
        3 /* PR_GET_DUMPABLE */ => DUMPABLE.load(Ordering::Relaxed) as i64,
        23 /* PR_CAPBSET_READ */ => {
            if arg > 63 {
                -EINVAL
            } else {
                0
            }
        }
        _ => {
            crate::syscall::log_once(&format!("prctl option {op} is not answered (EINVAL)"));
            -EINVAL
        }
    }
}

/// A path the Linux program names that the runtime answers itself: /proc/self/exe.
pub fn exe_alias(p: &CString) -> Option<CString> {
    let pid = crate::syscall::pid().to_string();
    let b = p.as_bytes();
    let own = b == b"/proc/self/exe" || b == format!("/proc/{pid}/exe").as_bytes();
    own.then(|| crate::syscall::EXE.get().cloned()).flatten()
}

/// Outside a session only (a session writes its own /etc): /etc/passwd and /etc/group in
/// Linux's format, for a read-only open. FerrionOS writes them with `;` and another field order
/// (redox_users), which Linux programs reject. The converted text is served from an unlinked
/// file of the caller's own in /tmp; the native files are only read.
pub fn etc_view(p: &CString, cloexec: bool) -> Option<i64> {
    if crate::session::active() {
        return None;
    }
    let passwd = match p.as_bytes() {
        b"/etc/passwd" => true,
        b"/etc/group" => false,
        _ => return None,
    };
    let text = std::fs::read_to_string(std::ffi::OsStr::new(p.to_str().ok()?)).ok()?;
    let mut out = String::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let f: Vec<&str> = line.split(';').collect();
        if passwd && f.len() == 6 {
            // user;uid;gid;name;home;shell -> user:x:uid:gid:name:home:shell
            out += &format!("{}:x:{}:{}:{}:{}:{}\n", f[0], f[1], f[2], f[3], f[4], f[5]);
        } else if !passwd && f.len() == 4 {
            // group;password;gid;users -> the same fields with `:`
            out += &format!("{}:{}:{}:{}\n", f[0], f[1], f[2], f[3]);
        }
    }
    static N: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let tmp = CString::new(format!("/tmp/.linux-run-etc-{}-{n}", crate::syscall::pid())).ok()?;
    let mut flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL;
    if cloexec {
        flags |= libc::O_CLOEXEC;
    }
    let fd = unsafe { libc::open(tmp.as_ptr(), flags, 0o600) };
    if fd < 0 {
        return Some(-last_errno());
    }
    unsafe {
        libc::unlink(tmp.as_ptr());
        libc::write(fd, out.as_ptr().cast(), out.len());
        libc::lseek(fd, 0, libc::SEEK_SET);
    }
    Some(new_fd(fd as i64))
}
