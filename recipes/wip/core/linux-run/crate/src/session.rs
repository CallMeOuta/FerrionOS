//! The launcher asks linux-sessiond (the `linux-session` scheme) for two file systems: the
//! Debian root, served read-only and shared by all sessions, and an empty one in memory, its own.
//! It forks a namespace from its own with the native schemes of `SCHEMES` that it holds,
//! registers the root there as `linux-root` and the memory file system as `linux-mem`, enters it,
//! and closes its handle to the old namespace. The session names no `file`: no native file
//! system at all, and nothing the caller could not name; neither the launcher nor anything it
//! starts holds a descriptor of one, past the standard descriptors the caller passed. The
//! launcher then writes the session's `/etc` files, the facts the runtime reads in place of
//! `/scheme/sys`, and a copy of this runtime, which `execve` starts again, into the memory file
//! system.
//!
//! The runtime maps every absolute Linux path onto the root, except a few that go to the memory
//! file system or the native device schemes (`linux_path`), and maps them back for `getcwd` and
//! `/proc/self/exe` (`linux_view`).

use std::ffi::CString;
use std::sync::{LazyLock, OnceLock};

use crate::errno::*;

/// The root's scheme in a session.
pub const ROOT: &str = "/scheme/linux-root";
/// The memory file system's scheme in a session.
pub const MEM: &str = "/scheme/linux-mem";
/// The runtime's copy in the session, which `execve` starts.
pub const RUNTIME: &str = "/scheme/linux-mem/runtime/linux-run";
/// The handle on linux-sessiond that keeps the session's file systems alive: every process of
/// the session inherits it, and the servers end when the last one closes it. Linux programs can
/// neither close nor replace it (src/files.rs, `reserved`).
pub const SESSION_FD: i32 = 1023;
pub const LEADER_FD: i32 = 1022;
const JOIN: u64 = 1;
pub const SET_LOCK: u64 = 2;
pub const GET_LOCK: u64 = 3;
pub const RELEASE_LOCKS: u64 = 4;
pub const INET_LISTEN: &str = "/scheme/linux-mem/inet/tcp";
pub const INET_PEER: &str = "/scheme/linux-mem/inet/peer";

/// Native schemes a session names, each only if the caller's namespace has it: memory, pipes,
/// events and time, randomness, the null and zero devices, terminals, Unix sockets, and the
/// descriptor objects of fdobjd.
const SCHEMES: &[&str] = &[
    "memory", "pipe", "event", "time", "rand", "null", "zero", "pty", "uds_stream", "uds_dgram",
    "eventfd", "timerfd",
];

const NETWORK_SCHEMES: &[&str] = &["tcp", "udp"];

/// Linux paths answered from the memory file system or a native scheme, longest first within a
/// prefix. A path equal to the Linux name or below it is mapped.
const MOUNTS: &[(&str, &str)] = &[
    ("/tmp", "/scheme/linux-mem/tmp"),
    ("/var/tmp", "/scheme/linux-mem/var-tmp"),
    ("/dev/shm", "/scheme/linux-mem/dev-shm"),
    ("/run", "/scheme/linux-mem/run"),
    ("/home", "/scheme/linux-mem/home"),
    ("/etc/passwd", "/scheme/linux-mem/etc/passwd"),
    ("/etc/group", "/scheme/linux-mem/etc/group"),
    ("/etc/hostname", "/scheme/linux-mem/etc/hostname"),
    ("/etc/hosts", "/scheme/linux-mem/etc/hosts"),
    ("/etc/nsswitch.conf", "/scheme/linux-mem/etc/nsswitch.conf"),
    ("/etc/resolv.conf", "/scheme/linux-mem/etc/resolv.conf"),
    ("/dev/null", "/scheme/null"),
    ("/dev/zero", "/scheme/zero"),
    ("/dev/random", "/scheme/rand"),
    ("/dev/urandom", "/scheme/rand"),
];

/// Facts the launcher writes for the runtimes of the session.
pub struct Facts {
    pub cpus: u64,
    pub memory: u64,
    /// Free memory when the session started; sysinfo reports it.
    pub memory_free: u64,
    pub hostname: Vec<u8>,
    pub network: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Native,
    Launcher,
    Inherited,
}

static MODE: OnceLock<Mode> = OnceLock::new();

/// The launcher's facts, read once from the memory file system.
pub static FACTS: LazyLock<Facts> = LazyLock::new(|| {
    let text = std::fs::read(format!("{MEM}/run/ferrion-facts")).unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.trim().parse::<u64>().ok())
            .unwrap_or(0)
    };
    let hostname = std::fs::read(format!("{MEM}/etc/hostname"))
        .map(|h| h.trim_ascii().to_vec())
        .unwrap_or_else(|_| b"localhost".to_vec());
    Facts {
        cpus: value("cpus ").max(1),
        memory: value("memory "),
        memory_free: value("memory-free "),
        hostname,
        network: value("network ") == 1,
    }
});

pub fn init_mode(native: bool) -> Mode {
    let mode = if native {
        Mode::Native
    } else if inherited() {
        Mode::Inherited
    } else {
        Mode::Launcher
    };
    assert!(MODE.set(mode).is_ok());
    mode
}

pub fn active() -> bool {
    matches!(MODE.get().copied().expect("session mode not set"), Mode::Launcher | Mode::Inherited)
}

fn inherited() -> bool {
    let p = CString::new(RUNTIME).unwrap();
    unsafe { libc::access(p.as_ptr(), libc::X_OK) == 0 }
}

/// Normalizes an absolute path without looking at the file system: `.` and empty components go,
/// `..` removes the one before it.
fn normalize(p: &[u8]) -> Vec<u8> {
    let mut parts: Vec<&[u8]> = Vec::new();
    for c in p.split(|&b| b == b'/') {
        match c {
            b"" | b"." => {}
            b".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    let mut out = Vec::with_capacity(p.len());
    for c in parts {
        out.push(b'/');
        out.extend_from_slice(c);
    }
    if out.is_empty() {
        out.push(b'/');
    }
    out
}

fn map(n: &[u8]) -> Vec<u8> {
    for (linux, native) in MOUNTS {
        let l = linux.as_bytes();
        if n.starts_with(l) && (n.len() == l.len() || n[l.len()] == b'/') {
            let mut out = native.as_bytes().to_vec();
            out.extend_from_slice(&n[l.len()..]);
            return out;
        }
    }
    let mut out = ROOT.as_bytes().to_vec();
    if n != b"/" {
        out.extend_from_slice(n);
    }
    out
}

fn mem_link(native: &[u8]) -> Option<Vec<u8>> {
    if !native.starts_with(MEM.as_bytes()) {
        return None;
    }
    let p = CString::new(native).ok()?;
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::readlink(p.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

fn resolve(n: Vec<u8>, follow: bool) -> Result<Vec<u8>, i64> {
    let mut n = n;
    let mut links = 0;
    'walk: loop {
        let parts: Vec<Vec<u8>> = n.split(|&b| b == b'/').filter(|c| !c.is_empty()).map(<[u8]>::to_vec).collect();
        let mut done: Vec<u8> = Vec::new();
        for (i, c) in parts.iter().enumerate() {
            let mut candidate = done.clone();
            candidate.push(b'/');
            candidate.extend_from_slice(c);
            let last = i + 1 == parts.len();
            if !last || follow {
                if let Some(target) = mem_link(&map(&candidate)) {
                    links += 1;
                    if links > 40 {
                        return Err(ELOOP);
                    }
                    let mut next = if target.starts_with(b"/") {
                        target
                    } else {
                        let mut d = done.clone();
                        d.push(b'/');
                        d.extend_from_slice(&target);
                        d
                    };
                    for rest in &parts[i + 1..] {
                        next.push(b'/');
                        next.extend_from_slice(rest);
                    }
                    n = normalize(&next);
                    continue 'walk;
                }
            }
            done = candidate;
        }
        return Ok(if done.is_empty() { b"/".to_vec() } else { done });
    }
}

pub fn linux_path(p: CString) -> CString {
    linux_path_at(p, false)
}

pub fn linux_path_at(p: CString, follow: bool) -> CString {
    let b = p.as_bytes();
    if !b.starts_with(b"/") || !active() {
        return p;
    }
    let n = normalize(b);
    let n = if touches_mem(&n) {
        resolve(n.clone(), follow).unwrap_or(n)
    } else {
        n
    };
    CString::new(map(&n)).unwrap_or(p)
}

fn touches_mem(n: &[u8]) -> bool {
    MOUNTS.iter().any(|(linux, native)| {
        let l = linux.as_bytes();
        native.starts_with(MEM) && n.starts_with(l) && (n.len() == l.len() || n[l.len()] == b'/')
    })
}

/// A native path as the Linux program sees it: the inverse of `linux_path` for the memory file
/// system, and the root's own scheme name dropped.
pub fn linux_view(native: &[u8]) -> Vec<u8> {
    if !active() {
        return native.to_vec();
    }
    for (linux, n) in MOUNTS {
        let nb = n.as_bytes();
        if nb.starts_with(MEM.as_bytes())
            && native.starts_with(nb)
            && (native.len() == nb.len() || native[nb.len()] == b'/')
        {
            let mut out = linux.as_bytes().to_vec();
            out.extend_from_slice(&native[nb.len()..]);
            return out;
        }
    }
    for root in [ROOT.as_bytes(), &b"linux-root:"[..]] {
        if let Some(rest) = native.strip_prefix(root) {
            if rest.is_empty() {
                return b"/".to_vec();
            }
            if rest.starts_with(b"/") {
                return rest.to_vec();
            }
        }
    }
    native.to_vec()
}

unsafe extern "C" {
    fn redox_openat_v1(fd: usize, path: *const u8, len: usize, flags: u32, fcntl: u32) -> usize;
    fn redox_mkns_v1(names: *const libc::iovec, n: usize, flags: u32) -> usize;
    fn redox_register_scheme_to_ns_v0(ns: usize, name: *const u8, len: usize, cap: usize) -> usize;
    fn redox_setns_v0(fd: usize) -> usize;
    fn redox_close_v1(fd: usize) -> usize;
}

fn native_err(what: &str, e: i64) -> String {
    format!("{what}: {}", std::io::Error::from_raw_os_error(e as i32))
}

fn write_file(path: &str, data: &[u8], mode: u32) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|e| format!("cannot write {path}: {e}"))?;
    std::io::Write::write_all(&mut f, data).map_err(|e| format!("cannot write {path}: {e}"))
}

fn mkdir(path: &str, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .mode(mode)
        .create(path)
        .and_then(|_| std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)))
        .map_err(|e| format!("cannot create {path}: {e}"))
}

/// The caller's user or group name, as the native account files give it.
fn name_of(id: u32, group: bool) -> Option<String> {
    unsafe {
        let p = if group {
            let g = libc::getgrgid(id);
            if g.is_null() { return None } else { (*g).gr_name }
        } else {
            let u = libc::getpwuid(id);
            if u.is_null() { return None } else { (*u).pw_name }
        };
        let s = std::ffi::CStr::from_ptr(p).to_str().ok()?;
        (!s.is_empty() && !s.contains(':')).then(|| s.to_string())
    }
}

fn native_resolver() -> Option<String> {
    if let Ok(text) = std::fs::read_to_string("/etc/resolv.conf") {
        let kept: String = text
            .lines()
            .filter(|l| ["nameserver ", "search ", "domain ", "options "].iter().any(|k| l.starts_with(k)))
            .map(|l| format!("{l}\n"))
            .collect();
        if !kept.is_empty() {
            return Some(kept);
        }
    }
    let dns = std::fs::read_to_string("/etc/net/dns").ok()?;
    let dns = dns.trim();
    (!dns.is_empty() && dns.parse::<std::net::IpAddr>().is_ok()).then(|| format!("nameserver {dns}\n"))
}

fn native_hosts() -> String {
    std::fs::read_to_string("/etc/hosts")
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.split_whitespace().skip(1).any(|n| n == "localhost"))
        .filter(|l| l.split_whitespace().next().is_some_and(|a| a.parse::<std::net::IpAddr>().is_ok()))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// Starts a session for this process (the launcher). Afterwards this process names only the
/// session's schemes, its working directory is `/` of the root, and it holds no descriptor past
/// 2 other than the session handle.
pub fn start(network: bool) -> Result<(), String> {
    assert_eq!(MODE.get().copied(), Some(Mode::Launcher));
    // What the session needs from the caller's view, read before leaving it.
    let runtime = std::fs::read(crate::syscall::LINUX_RUN)
        .map_err(|e| format!("cannot read {}: {e}", crate::syscall::LINUX_RUN))?;
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let user = name_of(uid, false).unwrap_or_else(|| format!("user{uid}"));
    let group = name_of(gid, true).unwrap_or_else(|| user.clone());
    let hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_graphic()))
        .unwrap_or_else(|| "localhost".to_string());
    let cpus = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) }.max(1) as u64;
    let resolver = if network { native_resolver() } else { None };
    let hosts = if network { native_hosts() } else { String::new() };
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(0) as u64;
    let memory = unsafe { libc::sysconf(crate::ffi::SC_PHYS_PAGES) }.max(0) as u64 * page;
    let memory_free = unsafe { libc::sysconf(crate::ffi::SC_AVPHYS_PAGES) }.max(0) as u64 * page;

    // The session's two file systems, and the handle that keeps them alive.
    let service = CString::new("/scheme/linux-session/new").unwrap();
    let handle = unsafe { libc::open(service.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if handle < 0 {
        return Err(native_err(
            "cannot reach linux-sessiond at /scheme/linux-session",
            last_errno(),
        ));
    }
    if unsafe { libc::dup2(handle, SESSION_FD) } != SESSION_FD {
        return Err(native_err("cannot keep the session handle", last_errno()));
    }
    unsafe { libc::close(handle) };
    let open_fs = |what: &str| -> Result<usize, String> {
        raw(unsafe {
            redox_openat_v1(
                SESSION_FD as usize,
                what.as_ptr(),
                what.len(),
                (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u32,
                0,
            )
        })
        .map_err(|e| native_err(&format!("linux-sessiond gave no {what} file system"), e))
    };
    let root = open_fs("root")?;
    let mem = open_fs("memory")?;

    // The namespace: the caller's own schemes that a session names, then the two file systems.
    let granted: &[&str] = if network { NETWORK_SCHEMES } else { &[] };
    let iov: Vec<libc::iovec> = SCHEMES
        .iter()
        .chain(granted)
        .map(|s| libc::iovec { iov_base: s.as_ptr() as *mut _, iov_len: s.len() })
        .collect();
    let ns = raw(unsafe { redox_mkns_v1(iov.as_ptr(), iov.len(), 0) })
        .map_err(|e| native_err("cannot make the session's namespace", e))?;
    for (name, cap) in [("linux-root", root), ("linux-mem", mem)] {
        raw(unsafe { redox_register_scheme_to_ns_v0(ns, name.as_ptr(), name.len(), cap) })
            .map_err(|e| native_err(&format!("cannot name {name} in the session"), e))?;
    }
    let old = unsafe { redox_setns_v0(ns) };
    if old != usize::MAX {
        unsafe { redox_close_v1(old) };
    }
    // A session starts as a login does, with every signal at its default action: the caller's
    // shell may ignore some (SIGPIPE, SIGINT for a background job), which Linux programs would
    // otherwise inherit across the session's boundary.
    for sig in 1..32 {
        if sig != libc::SIGKILL && sig != libc::SIGSTOP {
            unsafe { libc::signal(sig, libc::SIG_DFL) };
        }
    }
    // relibc's working directory is a descriptor: open it again, in the root.
    if unsafe { libc::chdir(c"/scheme/linux-root".as_ptr()) } != 0 {
        return Err(native_err("cannot enter the root", last_errno()));
    }

    // The memory file system: the mount points, /etc, the facts and the runtime.
    for (dir, mode) in [
        ("tmp", 0o1777),
        ("var-tmp", 0o1777),
        ("dev-shm", 0o1777),
        ("run", 0o755),
        ("etc", 0o755),
        ("home", 0o755),
        ("runtime", 0o755),
        ("inet", 0o755),
        ("inet/tcp", 0o1777),
        ("inet/peer", 0o1777),
    ] {
        mkdir(&format!("{MEM}/{dir}"), mode)?;
    }
    mkdir(&format!("{MEM}/home/{user}"), 0o700)?;
    let mut passwd = String::from("root:x:0:0:root:/root:/bin/sh\n");
    let mut groups = String::from("root:x:0:\n");
    if uid != 0 {
        passwd += &format!("{user}:x:{uid}:{gid}:{user}:/home/{user}:/bin/sh\n");
    }
    if gid != 0 {
        groups += &format!("{group}:x:{gid}:\n");
    }
    passwd += "nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin\n";
    groups += "nogroup:x:65534:\n";
    write_file(&format!("{MEM}/etc/passwd"), passwd.as_bytes(), 0o644)?;
    write_file(&format!("{MEM}/etc/group"), groups.as_bytes(), 0o644)?;
    write_file(&format!("{MEM}/etc/hostname"), format!("{hostname}\n").as_bytes(), 0o644)?;
    write_file(
        &format!("{MEM}/etc/hosts"),
        format!("127.0.0.1\tlocalhost\n127.0.1.1\t{hostname}\n::1\tlocalhost\n{hosts}").as_bytes(),
        0o644,
    )?;
    write_file(
        &format!("{MEM}/etc/nsswitch.conf"),
        b"passwd: files\ngroup: files\nshadow: files\nhosts: files dns\n",
        0o644,
    )?;
    if let Some(resolver) = resolver {
        write_file(&format!("{MEM}/etc/resolv.conf"), resolver.as_bytes(), 0o644)?;
    }
    write_file(
        &format!("{MEM}/run/ferrion-facts"),
        format!("cpus {cpus}\nmemory {memory}\nmemory-free {memory_free}\nnetwork {}\n", network as u8).as_bytes(),
        0o444,
    )?;
    write_file(RUNTIME, &runtime, 0o555)?;
    std::fs::set_permissions(
        format!("{MEM}/runtime"),
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o555),
    )
    .map_err(|e| format!("cannot close the runtime's directory: {e}"))?;

    // Nothing of the caller's past the standard descriptors goes into the session.
    for fd in 3..SESSION_FD {
        unsafe { libc::close(fd) };
    }
    let leader = raw(unsafe {
        redox_openat_v1(SESSION_FD as usize, b"leader".as_ptr(), 6, libc::O_RDONLY as u32, 0)
    })
    .map_err(|e| native_err("linux-sessiond gave no leader handle", e))?;
    if unsafe { libc::dup2(leader as i32, LEADER_FD) } != LEADER_FD {
        return Err(native_err("cannot keep the leader handle", last_errno()));
    }
    unsafe { libc::close(leader as i32) };
    join().map_err(|e| native_err("cannot join the session", e))
}

pub fn session_call(metadata: &[u64], payload: &mut [u64; 4]) -> Result<usize, i64> {
    unsafe extern "C" {
        fn redox_sys_call_v0(fd: usize, payload: *mut u8, len: usize, flags: usize, metadata: *const u64, n: usize) -> usize;
    }
    const READ_WRITE: usize = (1 << 9) | (1 << 10);
    raw(unsafe {
        redox_sys_call_v0(SESSION_FD as usize, payload.as_mut_ptr().cast(), 32, READ_WRITE, metadata.as_ptr(), metadata.len())
    })
}

fn join() -> Result<(), i64> {
    session_call(&[JOIN], &mut [0; 4]).map(|_| ())
}

pub fn forked_child() {
    if !active() {
        return;
    }
    unsafe { libc::close(LEADER_FD) };
    if join().is_err() {
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
    }
}
