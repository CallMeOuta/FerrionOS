use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex};

use crate::{errno::*, mem::user_ok, session};

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_NONBLOCK: u64 = 0x800;
const SOCK_CLOEXEC: u64 = 0x80000;
const MSG_PEEK: u64 = 0x2;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_WAITALL: u64 = 0x100;
const MSG_NOSIGNAL: u64 = 0x4000;
const EADDRINUSE: i64 = 98;
const EADDRNOTAVAIL: i64 = 99;
const ENETUNREACH: i64 = 101;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;
const ECONNREFUSED: i64 = 111;
const EINPROGRESS: i64 = 115;
const ENOPROTOOPT: i64 = 92;
const EPROTONOSUPPORT: i64 = 93;
const LOOPBACK: u32 = 0x7f00_0001;
const EMSGSIZE: i64 = 90;

type Addr = (u32, u16);

#[derive(Default)]
struct Sock {
    local: Option<Addr>,
    peer: Option<Addr>,
    listening: bool,
    bound: Option<(i32, CString)>,
    options: BTreeMap<(u64, u64), Vec<u8>>,
}

static SOCKS: Mutex<BTreeMap<i32, Arc<Mutex<Sock>>>> = Mutex::new(BTreeMap::new());

fn sock(fd: u64) -> Option<Arc<Mutex<Sock>>> {
    SOCKS.lock().unwrap().get(&(fd as i32)).cloned()
}

fn not_a_socket(fd: u64) -> i64 {
    if unsafe { libc::fcntl(fd as i32, libc::F_GETFD) } < 0 { -EBADF } else { -ENOTSOCK }
}

macro_rules! sock_or_err {
    ($fd:expr) => {
        match sock($fd) {
            Some(s) => s,
            None => return not_a_socket($fd),
        }
    };
}

pub fn duplicated(old: i32, new: i32) {
    let mut socks = SOCKS.lock().unwrap();
    socks.remove(&new);
    if let Some(s) = socks.get(&old).cloned() {
        socks.insert(new, s);
    }
}

pub fn closed(fd: i32) {
    let mut socks = SOCKS.lock().unwrap();
    let Some(s) = socks.remove(&fd) else { return };
    if Arc::strong_count(&s) > 1 || crate::spawn::active() {
        return;
    }
    let s = s.lock().unwrap();
    if let Some((pid, path)) = &s.bound {
        if *pid == crate::syscall::pid() {
            unsafe { libc::unlink(path.as_ptr()) };
        }
    }
}

fn ret(r: isize) -> i64 {
    if r < 0 { -last_errno() } else { r as i64 }
}

fn read_addr(p: u64, len: u64) -> Result<(u64, Addr), i64> {
    if len < 2 || !user_ok(p, len.min(16)) {
        return Err(if len < 2 { EINVAL } else { EFAULT });
    }
    let family = unsafe { *(p as *const u16) } as u64;
    if family != AF_INET {
        return Ok((family, (0, 0)));
    }
    if len < 16 {
        return Err(EINVAL);
    }
    let port = u16::from_be(unsafe { *((p + 2) as *const u16) });
    let addr = u32::from_be(unsafe { *((p + 4) as *const u32) });
    Ok((family, (addr, port)))
}

fn write_addr(p: u64, len: u64, a: Addr) -> i64 {
    if p == 0 {
        return 0;
    }
    if !user_ok(len, 4) {
        return -EFAULT;
    }
    let room = unsafe { *(len as *const u32) } as u64;
    let mut raw = [0u8; 16];
    raw[..2].copy_from_slice(&(AF_INET as u16).to_ne_bytes());
    raw[2..4].copy_from_slice(&a.1.to_be_bytes());
    raw[4..8].copy_from_slice(&a.0.to_be_bytes());
    let n = room.min(16);
    if n != 0 && !user_ok(p, n) {
        return -EFAULT;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(raw.as_ptr(), p as *mut u8, n as usize);
        *(len as *mut u32) = 16;
    }
    0
}

fn unix_addr(path: &str) -> libc::sockaddr_un {
    let mut a: libc::sockaddr_un = unsafe { core::mem::zeroed() };
    a.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (d, s) in a.sun_path.iter_mut().zip(path.bytes()) {
        *d = s as libc::c_char;
    }
    a
}

fn unix_bind(fd: i32, path: &str) -> Result<(), i64> {
    let a = unix_addr(path);
    let r = unsafe {
        libc::bind(fd, &a as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_un>() as u32)
    };
    if r != 0 {
        return Err(match last_errno() {
            EEXIST => EADDRINUSE,
            e => e,
        });
    }
    Ok(())
}

fn exists(path: &str) -> bool {
    let p = CString::new(path).unwrap();
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    unsafe { libc::lstat(p.as_ptr(), &mut st) == 0 }
}

fn ephemeral() -> u16 {
    loop {
        let port = 32768 + (crate::mem::random_u64() % (60999 - 32768 + 1)) as u16;
        if !exists(&format!("{}/{port}", session::INET_LISTEN)) {
            return port;
        }
    }
}

pub fn socket(domain: u64, kind: u64, protocol: u64) -> i64 {
    if domain != AF_INET || !session::active() {
        return -EAFNOSUPPORT;
    }
    if kind & 0xf != SOCK_STREAM || kind & !(0xf | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return -EPROTONOSUPPORT;
    }
    if protocol != 0 && protocol != 6 {
        return -EPROTONOSUPPORT;
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    let fd = crate::files::new_fd(ret(fd as isize));
    if fd < 0 {
        return fd;
    }
    unsafe {
        if kind & SOCK_CLOEXEC != 0 {
            libc::fcntl(fd as i32, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        if kind & SOCK_NONBLOCK != 0 {
            libc::fcntl(fd as i32, libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    SOCKS.lock().unwrap().insert(fd as i32, Arc::new(Mutex::new(Sock::default())));
    fd
}

fn bind_port(fd: i32, s: &mut Sock, addr: u32, port: u16) -> i64 {
    if s.local.is_some() {
        return -EINVAL;
    }
    if port != 0 && port < 1024 && unsafe { libc::geteuid() } != 0 {
        return -EACCES;
    }
    for _ in 0..64 {
        let p = if port == 0 { ephemeral() } else { port };
        let path = format!("{}/{p}", session::INET_LISTEN);
        match unix_bind(fd, &path) {
            Ok(()) => {
                s.local = Some((addr, p));
                s.bound = Some((crate::syscall::pid(), CString::new(path).unwrap()));
                return 0;
            }
            Err(EADDRINUSE) if port == 0 => continue,
            Err(e) => return -e,
        }
    }
    -EADDRINUSE
}

pub fn bind(fd: u64, p: u64, len: u64) -> i64 {
    let s = sock_or_err!(fd);
    let (family, (addr, port)) = match read_addr(p, len) {
        Ok(a) => a,
        Err(e) => return -e,
    };
    if family != AF_INET {
        return -EAFNOSUPPORT;
    }
    if addr != 0 && addr >> 24 != 127 {
        return -EADDRNOTAVAIL;
    }
    let mut s = s.lock().unwrap();
    bind_port(fd as i32, &mut s, addr, port)
}

pub fn listen(fd: u64, _backlog: u64) -> i64 {
    let s = sock_or_err!(fd);
    let mut s = s.lock().unwrap();
    if s.peer.is_some() {
        return -EINVAL;
    }
    if s.local.is_none() {
        let r = bind_port(fd as i32, &mut s, 0, 0);
        if r < 0 {
            return r;
        }
    }
    s.listening = true;
    0
}

fn peer_port(fd: i32) -> u16 {
    let mut a: libc::sockaddr_un = unsafe { core::mem::zeroed() };
    let mut len = core::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    if unsafe { libc::getpeername(fd, &mut a as *mut _ as *mut libc::sockaddr, &mut len) } != 0 {
        return 0;
    }
    let path: Vec<u8> = a.sun_path.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or_default();
    std::str::from_utf8(name).ok().and_then(|n| n.parse().ok()).unwrap_or(0)
}

pub fn accept(fd: u64, addr: u64, len: u64, flags: u64) -> i64 {
    if flags & !(SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return -EINVAL;
    }
    let s = sock_or_err!(fd);
    let local = {
        let s = s.lock().unwrap();
        if !s.listening {
            return -EINVAL;
        }
        s.local.unwrap_or((0, 0))
    };
    let new = unsafe { libc::accept(fd as i32, core::ptr::null_mut(), core::ptr::null_mut()) };
    let new = crate::files::new_fd(ret(new as isize));
    if new < 0 {
        return new;
    }
    unsafe {
        if flags & SOCK_CLOEXEC != 0 {
            libc::fcntl(new as i32, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        if flags & SOCK_NONBLOCK != 0 {
            libc::fcntl(new as i32, libc::F_SETFL, libc::O_NONBLOCK);
        }
    }
    let local = (if local.0 == 0 { LOOPBACK } else { local.0 }, local.1);
    let peer = (LOOPBACK, peer_port(new as i32));
    SOCKS.lock().unwrap().insert(
        new as i32,
        Arc::new(Mutex::new(Sock { local: Some(local), peer: Some(peer), ..Default::default() })),
    );
    let r = write_addr(addr, len, peer);
    if r < 0 {
        return r;
    }
    new
}

pub fn connect(fd: u64, p: u64, len: u64) -> i64 {
    let s = sock_or_err!(fd);
    let (family, (addr, port)) = match read_addr(p, len) {
        Ok(a) => a,
        Err(e) => return -e,
    };
    if family != AF_INET {
        return -EAFNOSUPPORT;
    }
    {
        let s = s.lock().unwrap();
        if s.peer.is_some() {
            return -EISCONN;
        }
        if s.listening {
            return -EINVAL;
        }
    }
    if addr >> 24 != 127 && addr != 0 {
        return -ENETUNREACH;
    }
    let target = format!("{}/{port}", session::INET_LISTEN);
    if port == 0 || !exists(&target) {
        return -ECONNREFUSED;
    }
    let mut guard = s.lock().unwrap();
    if guard.local.is_none() {
        let mine = ephemeral();
        let name = format!("{}/{mine}", session::INET_PEER);
        if let Err(e) = unix_bind(fd as i32, &name) {
            return -e;
        }
        let c = CString::new(name).unwrap();
        unsafe { libc::unlink(c.as_ptr()) };
        guard.local = Some((LOOPBACK, mine));
    }
    drop(guard);
    let a = unix_addr(&target);
    let r = unsafe {
        libc::connect(fd as i32, &a as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_un>() as u32)
    };
    if r != 0 {
        return -match last_errno() {
            ENOENT | EBADF => ECONNREFUSED,
            EAGAIN => EINPROGRESS,
            e => e,
        };
    }
    s.lock().unwrap().peer = Some((if addr == 0 { LOOPBACK } else { addr }, port));
    0
}

pub fn name(fd: u64, addr: u64, len: u64, peer: bool) -> i64 {
    let s = sock_or_err!(fd);
    let s = s.lock().unwrap();
    let a = if peer {
        match s.peer {
            Some(a) => a,
            None => return -ENOTCONN,
        }
    } else {
        s.local.unwrap_or((0, 0))
    };
    if addr == 0 {
        return -EFAULT;
    }
    write_addr(addr, len, a)
}

fn would_block(fd: i32, events: i16) -> bool {
    let mut p = libc::pollfd { fd, events, revents: 0 };
    unsafe { libc::poll(&mut p, 1, 0) == 0 }
}

fn send_bytes(fd: i32, buf: &[u8], flags: u64) -> i64 {
    if flags & !(MSG_DONTWAIT | MSG_NOSIGNAL) != 0 {
        return -EOPNOTSUPP;
    }
    if flags & MSG_DONTWAIT != 0 && would_block(fd, libc::POLLOUT) {
        return -EAGAIN;
    }
    ret(unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) })
}

fn recv_bytes(fd: i32, buf: &mut [u8], flags: u64) -> i64 {
    if flags & !(MSG_DONTWAIT | MSG_WAITALL | MSG_PEEK) != 0 {
        return -EOPNOTSUPP;
    }
    if flags & MSG_PEEK != 0 {
        let native = (flags & (MSG_PEEK | MSG_DONTWAIT)) as i32;
        return ret(unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), native) });
    }
    if flags & MSG_DONTWAIT != 0 && would_block(fd, libc::POLLIN) {
        return -EAGAIN;
    }
    let mut got = 0usize;
    loop {
        let n = unsafe { libc::read(fd, buf[got..].as_mut_ptr().cast(), buf.len() - got) };
        if n < 0 {
            return if got > 0 { got as i64 } else { -last_errno() };
        }
        got += n as usize;
        if n == 0 || got == buf.len() || flags & MSG_WAITALL == 0 {
            return got as i64;
        }
    }
}

fn iovecs(iov: u64, n: u64) -> Result<Vec<(u64, u64)>, i64> {
    if n > 1024 {
        return Err(EMSGSIZE);
    }
    if n != 0 && !user_ok(iov, 16 * n) {
        return Err(EFAULT);
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        let (p, l) = unsafe { (*((iov + 16 * i) as *const u64), *((iov + 16 * i + 8) as *const u64)) };
        if l != 0 && !user_ok(p, l) {
            return Err(EFAULT);
        }
        out.push((p, l));
    }
    Ok(out)
}

pub fn sendto(fd: u64, buf: u64, len: u64, flags: u64, _dest: u64, _dest_len: u64) -> i64 {
    let _s = sock_or_err!(fd);
    if len != 0 && !user_ok(buf, len) {
        return -EFAULT;
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, len as usize) };
    send_bytes(fd as i32, bytes, flags)
}

pub fn recvfrom(fd: u64, buf: u64, len: u64, flags: u64, src: u64, src_len: u64) -> i64 {
    let s = sock_or_err!(fd);
    if len != 0 && !user_ok(buf, len) {
        return -EFAULT;
    }
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
    let n = recv_bytes(fd as i32, bytes, flags);
    if n >= 0 && src != 0 {
        let peer = s.lock().unwrap().peer;
        if let Some(peer) = peer {
            let r = write_addr(src, src_len, peer);
            if r < 0 {
                return r;
            }
        }
    }
    n
}

struct Msghdr {
    name: u64,
    iov: Vec<(u64, u64)>,
    control_len: u64,
}

fn msghdr(p: u64) -> Result<Msghdr, i64> {
    if !user_ok(p, 56) {
        return Err(EFAULT);
    }
    let w = unsafe { *(p as *const [u64; 7]) };
    Ok(Msghdr { name: w[0], iov: iovecs(w[2], w[3])?, control_len: w[5] })
}

fn sendmsg_one(fd: u64, p: u64, flags: u64) -> i64 {
    let m = match msghdr(p) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    if m.control_len != 0 {
        return -EOPNOTSUPP;
    }
    let mut all = Vec::new();
    for (b, l) in m.iov {
        all.extend_from_slice(unsafe { core::slice::from_raw_parts(b as *const u8, l as usize) });
    }
    send_bytes(fd as i32, &all, flags)
}

pub fn sendmsg(fd: u64, p: u64, flags: u64) -> i64 {
    let _s = sock_or_err!(fd);
    sendmsg_one(fd, p, flags)
}

pub fn recvmsg(fd: u64, p: u64, flags: u64) -> i64 {
    let s = sock_or_err!(fd);
    let m = match msghdr(p) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    let total: u64 = m.iov.iter().map(|&(_, l)| l).sum();
    let mut buf = vec![0u8; total.min(1 << 24) as usize];
    let n = recv_bytes(fd as i32, &mut buf, flags);
    if n < 0 {
        return n;
    }
    let mut at = 0usize;
    for (b, l) in m.iov {
        if at >= n as usize {
            break;
        }
        let k = (l as usize).min(n as usize - at);
        unsafe { core::ptr::copy_nonoverlapping(buf[at..].as_ptr(), b as *mut u8, k) };
        at += k;
    }
    unsafe {
        *((p + 40) as *mut u64) = 0;
        *((p + 48) as *mut i32) = 0;
    }
    if m.name != 0 {
        let peer = s.lock().unwrap().peer;
        if let Some(peer) = peer {
            let r = write_addr(m.name, p + 8, peer);
            if r < 0 {
                return r;
            }
        } else {
            unsafe { *((p + 8) as *mut u32) = 0 };
        }
    }
    n
}

pub fn sendmmsg(fd: u64, v: u64, n: u64, flags: u64) -> i64 {
    let _s = sock_or_err!(fd);
    let n = n.min(1024);
    if n != 0 && !user_ok(v, 64 * n) {
        return -EFAULT;
    }
    for i in 0..n {
        let r = sendmsg_one(fd, v + 64 * i, flags);
        if r < 0 {
            return if i == 0 { r } else { i as i64 };
        }
        unsafe { *((v + 64 * i + 56) as *mut u32) = r as u32 };
    }
    n as i64
}

pub fn shutdown(fd: u64, how: u64) -> i64 {
    let _s = sock_or_err!(fd);
    if how > 2 {
        return -EINVAL;
    }
    ret(unsafe { libc::shutdown(fd as i32, how as i32) } as isize)
}

fn known_option(level: u64, name: u64) -> bool {
    const SOL_SOCKET: u64 = 1;
    const IPPROTO_IP: u64 = 0;
    const IPPROTO_TCP: u64 = 6;
    match level {
        SOL_SOCKET => matches!(name, 2 | 6 | 7 | 8 | 9 | 10 | 12 | 13 | 15 | 20 | 21),
        IPPROTO_TCP => matches!(name, 1 | 4 | 5 | 6),
        IPPROTO_IP => matches!(name, 1 | 2),
        _ => false,
    }
}

pub fn setsockopt(fd: u64, level: u64, name: u64, val: u64, len: u64) -> i64 {
    let s = sock_or_err!(fd);
    if !known_option(level, name) {
        return -ENOPROTOOPT;
    }
    if len > 64 || (len != 0 && !user_ok(val, len)) {
        return -EFAULT;
    }
    if len < 4 {
        return -EINVAL;
    }
    let bytes = unsafe { core::slice::from_raw_parts(val as *const u8, len as usize) }.to_vec();
    s.lock().unwrap().options.insert((level, name), bytes);
    0
}

pub fn getsockopt(fd: u64, level: u64, name: u64, val: u64, len: u64) -> i64 {
    let s = sock_or_err!(fd);
    let s = s.lock().unwrap();
    let int = |v: i32| v.to_ne_bytes().to_vec();
    let value = match (level, name) {
        (1, 3) => int(SOCK_STREAM as i32),
        (1, 4) => int(0),
        (1, 30) => int(s.listening as i32),
        (1, 38) => int(6),
        (1, 39) => int(AF_INET as i32),
        _ if known_option(level, name) => s.options.get(&(level, name)).cloned().unwrap_or_else(|| match (level, name) {
            (1, 7) => int(16384),
            (1, 8) => int(131072),
            (1, 13) | (1, 20) | (1, 21) => vec![0; if name == 13 { 8 } else { 16 }],
            (0, 2) => int(64),
            _ => int(0),
        }),
        _ => return -ENOPROTOOPT,
    };
    if !user_ok(len, 4) {
        return -EFAULT;
    }
    let room = unsafe { *(len as *const u32) } as usize;
    let n = room.min(value.len());
    if n != 0 && !user_ok(val, n as u64) {
        return -EFAULT;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(value.as_ptr(), val as *mut u8, n);
        *(len as *mut u32) = n as u32;
    }
    0
}
