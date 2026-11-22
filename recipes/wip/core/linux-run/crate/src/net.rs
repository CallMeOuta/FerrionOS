use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex};

use crate::{errno::*, mem::user_ok, session};

const AF_INET: u64 = 2;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
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
const EDESTADDRREQ: i64 = 89;
const ETIMEDOUT: i64 = 110;
const CONNECT_LIMIT_MS: u128 = 75_000;

type Addr = (u32, u16);

#[derive(Clone, Copy)]
enum Via {
    Emulated { local: Addr },
    Native,
}

struct ListenPath {
    owner: i32,
    path: CString,
}

enum Stream {
    Fresh,
    Bound { local: Addr, path: ListenPath },
    PeerBound { local: Addr },
    Listening { local: Addr, path: ListenPath },
    Connecting { peer: Addr, via: Via },
    Connected { peer: Addr, via: Via },
    Failed { error: i64 },
}

enum DgramState {
    Unconnected { local: Option<Addr> },
    Connected { local: Option<Addr>, peer: Addr },
}

enum Kind {
    Stream(Stream),
    Dgram { native: bool, state: DgramState },
}

struct Sock {
    kind: Kind,
    options: BTreeMap<(u64, u64), Vec<u8>>,
}

enum Conn {
    Pending,
    Done,
    Failed(i64),
}

fn unlink_path(lp: &ListenPath) {
    if lp.owner == crate::syscall::pid() {
        unsafe { libc::unlink(lp.path.as_ptr()) };
    }
}

impl Sock {
    fn is_dgram(&self) -> bool {
        matches!(self.kind, Kind::Dgram { .. })
    }

    fn is_native(&self) -> bool {
        matches!(
            self.kind,
            Kind::Stream(
                Stream::Connecting { via: Via::Native, .. }
                    | Stream::Connected { via: Via::Native, .. }
            ) | Kind::Dgram { native: true, .. }
        )
    }

    fn is_connecting(&self) -> bool {
        matches!(self.kind, Kind::Stream(Stream::Connecting { .. }))
    }

    fn is_listening(&self) -> bool {
        matches!(self.kind, Kind::Stream(Stream::Listening { .. }))
    }

    fn peer(&self) -> Option<Addr> {
        match &self.kind {
            Kind::Stream(Stream::Connected { peer, .. } | Stream::Connecting { peer, .. }) => Some(*peer),
            Kind::Dgram { state: DgramState::Connected { peer, .. }, .. } => Some(*peer),
            _ => None,
        }
    }

    fn poll_connect(&mut self, fd: i32) -> Conn {
        let (peer, via) = match self.kind {
            Kind::Stream(Stream::Connecting { peer, via }) => (peer, via),
            _ => unreachable!("poll_connect requires Stream::Connecting"),
        };
        let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        let n = unsafe { libc::poll(&mut p, 1, 0) };
        match via {
            Via::Native => {
                if n > 0 && p.revents & libc::POLLOUT != 0 {
                    self.kind = Kind::Stream(Stream::Connected { peer, via });
                    return Conn::Done;
                }
                if native_name(fd, true).is_none_or(|a| a == (0, 0)) {
                    self.kind = Kind::Stream(Stream::Failed { error: ECONNREFUSED });
                    return Conn::Failed(ECONNREFUSED);
                }
                Conn::Pending
            }
            Via::Emulated { .. } => {
                if n > 0 && p.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                    self.kind = Kind::Stream(Stream::Failed { error: ECONNREFUSED });
                    return Conn::Failed(ECONNREFUSED);
                }
                if n > 0 && p.revents & libc::POLLOUT != 0 {
                    self.kind = Kind::Stream(Stream::Connected { peer, via });
                    return Conn::Done;
                }
                Conn::Pending
            }
        }
    }
}

fn granted() -> bool {
    session::FACTS.network
}

fn loopback(addr: u32) -> bool {
    addr == 0 || addr >> 24 == 127
}

fn sockaddr_in(a: Addr) -> libc::sockaddr_in {
    let mut s: libc::sockaddr_in = unsafe { core::mem::zeroed() };
    s.sin_family = libc::AF_INET as libc::sa_family_t;
    s.sin_port = a.1.to_be();
    s.sin_addr.s_addr = a.0.to_be();
    s
}

fn from_sockaddr_in(s: &libc::sockaddr_in) -> Addr {
    (u32::from_be(s.sin_addr.s_addr), u16::from_be(s.sin_port))
}

fn native_errno() -> i64 {
    match last_errno() {
        ENOENT | ENODEV | EACCES | EPERM => ENETUNREACH,
        e => e,
    }
}

fn native_socket(kind: i32) -> Result<i32, i64> {
    let n = unsafe { libc::socket(libc::AF_INET, kind, 0) };
    if n < 0 { Err(native_errno()) } else { Ok(n) }
}

fn keep_flags<T>(fd: i32, f: impl FnOnce() -> T) -> T {
    let (fd_flags, fl) = unsafe { (libc::fcntl(fd, libc::F_GETFD), libc::fcntl(fd, libc::F_GETFL)) };
    let r = f();
    unsafe {
        if fl >= 0 {
            libc::fcntl(fd, libc::F_SETFL, fl & libc::O_NONBLOCK);
        }
        if fd_flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, fd_flags & libc::FD_CLOEXEC);
        }
    }
    r
}

fn replace(fd: i32, n: i32) -> Result<(), i64> {
    let r = keep_flags(fd, || unsafe { libc::dup2(n, fd) });
    let e = last_errno();
    unsafe { libc::close(n) };
    if r != fd { Err(e) } else { Ok(()) }
}

fn native_name(fd: i32, peer: bool) -> Option<Addr> {
    let mut a: libc::sockaddr_in = unsafe { core::mem::zeroed() };
    let mut len = core::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let p = &mut a as *mut _ as *mut libc::sockaddr;
    let r = unsafe { if peer { libc::getpeername(fd, p, &mut len) } else { libc::getsockname(fd, p, &mut len) } };
    (r == 0).then(|| from_sockaddr_in(&a))
}

fn wait_connected(fd: i32, s: &Mutex<Sock>) -> i64 {
    let start = std::time::Instant::now();
    loop {
        let mut p = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        unsafe { libc::poll(&mut p, 1, 50) };
        let mut g = s.lock().unwrap();
        match g.kind {
            Kind::Stream(Stream::Connecting { .. }) => match g.poll_connect(fd) {
                Conn::Done => return 0,
                Conn::Failed(e) => {
                    g.kind = Kind::Stream(Stream::Fresh);
                    return -e;
                }
                Conn::Pending if start.elapsed().as_millis() >= CONNECT_LIMIT_MS => {
                    g.kind = Kind::Stream(Stream::Fresh);
                    return -ETIMEDOUT;
                }
                Conn::Pending => {}
            },
            Kind::Stream(Stream::Connected { .. }) => return 0,
            Kind::Stream(Stream::Failed { error }) => {
                g.kind = Kind::Stream(Stream::Fresh);
                return -error;
            }
            _ => return -ENOTCONN,
        }
    }
}

pub fn connecting(fd: i32) -> bool {
    sock(fd as u64).is_some_and(|s| s.lock().unwrap().is_connecting())
}

pub fn settle(fd: i32, events: i16, revents: i16) -> i16 {
    let Some(s) = sock(fd as u64) else { return revents };
    let mut s = s.lock().unwrap();
    if !s.is_connecting() {
        return revents;
    }
    match s.poll_connect(fd) {
        Conn::Pending => revents & !(libc::POLLIN | libc::POLLRDNORM),
        Conn::Done => revents | (events & libc::POLLOUT),
        Conn::Failed(_) => libc::POLLERR | libc::POLLHUP | (events & libc::POLLOUT),
    }
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
    if let Kind::Stream(Stream::Bound { path, .. } | Stream::Listening { path, .. }) = &s.kind {
        unlink_path(path);
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
    let dgram = kind & 0xf == SOCK_DGRAM;
    if !(dgram || kind & 0xf == SOCK_STREAM) || kind & !(0xf | SOCK_NONBLOCK | SOCK_CLOEXEC) != 0 {
        return -EPROTONOSUPPORT;
    }
    if protocol != 0 && protocol != if dgram { 17 } else { 6 } {
        return -EPROTONOSUPPORT;
    }
    let native = dgram && granted();
    let fd = if native {
        match native_socket(libc::SOCK_DGRAM) {
            Ok(n) => n as i64,
            Err(e) => return -e,
        }
    } else {
        let unix = if dgram { libc::SOCK_DGRAM } else { libc::SOCK_STREAM };
        ret(unsafe { libc::socket(libc::AF_UNIX, unix, 0) } as isize)
    };
    let fd = crate::files::new_fd(fd);
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
    let sock_kind = if dgram {
        Kind::Dgram { native, state: DgramState::Unconnected { local: None } }
    } else {
        Kind::Stream(Stream::Fresh)
    };
    SOCKS.lock().unwrap().insert(fd as i32, Arc::new(Mutex::new(Sock { kind: sock_kind, options: BTreeMap::new() })));
    fd
}

fn bind_port(fd: i32, stream: &mut Stream, addr: u32, port: u16) -> i64 {
    if !matches!(stream, Stream::Fresh) {
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
                *stream = Stream::Bound {
                    local: (addr, p),
                    path: ListenPath { owner: crate::syscall::pid(), path: CString::new(path).unwrap() },
                };
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
    if !loopback(addr) {
        return -EADDRNOTAVAIL;
    }
    let mut s = s.lock().unwrap();
    match &mut s.kind {
        Kind::Dgram { native, state } => bind_dgram(fd as i32, *native, state, addr, port),
        Kind::Stream(stream) => bind_port(fd as i32, stream, addr, port),
    }
}

fn bind_dgram(fd: i32, native: bool, state: &mut DgramState, addr: u32, port: u16) -> i64 {
    let local = match state {
        DgramState::Unconnected { local } | DgramState::Connected { local, .. } => local,
    };
    if local.is_some() {
        return -EINVAL;
    }
    if port != 0 && port < 1024 && unsafe { libc::geteuid() } != 0 {
        return -EACCES;
    }
    if !native {
        *local = Some((addr, if port == 0 { ephemeral() } else { port }));
        return 0;
    }
    if addr != 0 {
        return -EADDRNOTAVAIL;
    }
    let sa = sockaddr_in((0, port));
    let r = keep_flags(fd, || unsafe {
        libc::bind(fd, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32)
    });
    if r != 0 {
        return -native_errno();
    }
    *local = Some(native_name(fd, false).unwrap_or((0, port)));
    0
}

fn start_listening(fd: i32, s: &mut Sock) -> i64 {
    let stream = match &mut s.kind {
        Kind::Dgram { .. } => return -EOPNOTSUPP,
        Kind::Stream(stream) => stream,
    };
    match stream {
        Stream::Fresh => {
            let r = bind_port(fd, stream, 0, 0);
            if r < 0 {
                return r;
            }
        }
        Stream::Bound { .. } => {}
        Stream::Listening { .. } => return 0,
        Stream::Connecting { via: Via::Native, .. } | Stream::Connected { via: Via::Native, .. } => return -EOPNOTSUPP,
        Stream::PeerBound { .. } | Stream::Connecting { .. } | Stream::Connected { .. } | Stream::Failed { .. } => return -EINVAL,
    }
    let prev = core::mem::replace(stream, Stream::Fresh);
    match prev {
        Stream::Bound { local, path } => {
            *stream = Stream::Listening { local, path };
            0
        }
        other => {
            *stream = other;
            unreachable!("expected Bound stream before Listening");
        }
    }
}

pub fn listen(fd: u64, _backlog: u64) -> i64 {
    let s = sock_or_err!(fd);
    let mut s = s.lock().unwrap();
    start_listening(fd as i32, &mut s)
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
        match s.kind {
            Kind::Stream(Stream::Listening { local, .. }) => local,
            _ => return -EINVAL,
        }
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
        Arc::new(Mutex::new(Sock {
            kind: Kind::Stream(Stream::Connected { peer, via: Via::Emulated { local } }),
            options: BTreeMap::new(),
        })),
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
    if s.lock().unwrap().is_dgram() {
        return connect_dgram(fd as i32, &s, family, (addr, port));
    }
    if family != AF_INET {
        return -EAFNOSUPPORT;
    }
    {
        let mut g = s.lock().unwrap();
        match g.kind {
            Kind::Stream(Stream::Connecting { .. }) => {
                return match g.poll_connect(fd as i32) {
                    Conn::Pending => -EALREADY,
                    Conn::Done => -EISCONN,
                    Conn::Failed(e) => {
                        g.kind = Kind::Stream(Stream::Fresh);
                        -e
                    }
                };
            }
            Kind::Stream(Stream::Connected { .. }) => return -EISCONN,
            Kind::Stream(Stream::Listening { .. }) => return -EINVAL,
            Kind::Stream(Stream::Failed { .. }) => g.kind = Kind::Stream(Stream::Fresh),
            Kind::Stream(Stream::Fresh | Stream::Bound { .. } | Stream::PeerBound { .. }) => {}
            Kind::Dgram { .. } => unreachable!(),
        }
    }
    if !loopback(addr) {
        return connect_native(fd as i32, &s, (addr, port));
    }
    let target = format!("{}/{port}", session::INET_LISTEN);
    if port == 0 || !exists(&target) {
        return -ECONNREFUSED;
    }
    let local = {
        let mut guard = s.lock().unwrap();
        match guard.kind {
            Kind::Stream(Stream::Bound { local, .. } | Stream::PeerBound { local }) => local,
            Kind::Stream(Stream::Fresh) => {
                let mine = ephemeral();
                let name = format!("{}/{mine}", session::INET_PEER);
                if let Err(e) = unix_bind(fd as i32, &name) {
                    return -e;
                }
                let c = CString::new(name).unwrap();
                unsafe { libc::unlink(c.as_ptr()) };
                let local = (LOOPBACK, mine);
                guard.kind = Kind::Stream(Stream::PeerBound { local });
                local
            }
            _ => return -EINVAL,
        }
    };
    let peer = (if addr == 0 { LOOPBACK } else { addr }, port);
    let a = unix_addr(&target);
    let r = unsafe {
        libc::connect(fd as i32, &a as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_un>() as u32)
    };
    if r != 0 {
        let err = last_errno();
        if err == EAGAIN {
            let mut g = s.lock().unwrap();
            if let Kind::Stream(Stream::Bound { path, .. }) = &g.kind {
                unlink_path(path);
            }
            g.kind = Kind::Stream(Stream::Connecting { peer, via: Via::Emulated { local } });
            return -EINPROGRESS;
        }
        return -match err {
            ENOENT | EBADF => ECONNREFUSED,
            e => e,
        };
    }
    let mut g = s.lock().unwrap();
    if let Kind::Stream(Stream::Bound { path, .. }) = &g.kind {
        unlink_path(path);
    }
    g.kind = Kind::Stream(Stream::Connected { peer, via: Via::Emulated { local } });
    0
}

const EALREADY: i64 = 114;

fn connect_native(fd: i32, s: &Mutex<Sock>, to: Addr) -> i64 {
    if !granted() {
        return -ENETUNREACH;
    }
    let nonblock = unsafe { libc::fcntl(fd, libc::F_GETFL) } & libc::O_NONBLOCK != 0;
    let n = match native_socket(libc::SOCK_STREAM) {
        Ok(n) => n,
        Err(e) => return -e,
    };
    let sa = sockaddr_in(to);
    if unsafe { libc::connect(n, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32) } != 0 {
        let e = native_errno();
        unsafe { libc::close(n) };
        return -e;
    }
    if let Err(e) = replace(fd, n) {
        return -e;
    }
    {
        let mut g = s.lock().unwrap();
        if let Kind::Stream(Stream::Bound { path, .. }) = &g.kind {
            unlink_path(path);
        }
        g.kind = Kind::Stream(Stream::Connecting { peer: to, via: Via::Native });
        for name in [20u64, 21] {
            if let Some(v) = g.options.get(&(1, name)) {
                unsafe { libc::setsockopt(fd, libc::SOL_SOCKET, name as i32, v.as_ptr().cast(), v.len() as u32) };
            }
        }
    }
    if nonblock {
        return -EINPROGRESS;
    }
    wait_connected(fd, s)
}

fn connect_dgram(fd: i32, s: &Mutex<Sock>, family: u64, to: Addr) -> i64 {
    let mut g = s.lock().unwrap();
    let (native, state) = match &mut g.kind {
        Kind::Dgram { native, state } => (*native, state),
        _ => unreachable!(),
    };
    if family == 0 {
        if native {
            let mut sa: libc::sockaddr_in = unsafe { core::mem::zeroed() };
            sa.sin_family = libc::AF_UNSPEC as libc::sa_family_t;
            keep_flags(fd, || unsafe {
                libc::connect(fd, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32)
            });
        }
        let local = match *state {
            DgramState::Unconnected { local } | DgramState::Connected { local, .. } => local,
        };
        *state = DgramState::Unconnected { local };
        return 0;
    }
    if family != AF_INET {
        return -EAFNOSUPPORT;
    }
    if loopback(to.0) {
        return -ECONNREFUSED;
    }
    if !granted() || !native {
        return -ENETUNREACH;
    }
    let sa = sockaddr_in(to);
    let r = keep_flags(fd, || unsafe {
        libc::connect(fd, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32)
    });
    if r != 0 {
        return -native_errno();
    }
    let prev_local = match *state {
        DgramState::Unconnected { local } | DgramState::Connected { local, .. } => local,
    };
    let local = native_name(fd, false).or(prev_local);
    *state = DgramState::Connected { local, peer: to };
    0
}

pub fn name(fd: u64, addr: u64, len: u64, peer: bool) -> i64 {
    let s = sock_or_err!(fd);
    let mut s = s.lock().unwrap();
    let a = if peer {
        if s.is_connecting() {
            s.poll_connect(fd as i32);
        }
        match s.kind {
            Kind::Stream(Stream::Connected { peer, .. }) => peer,
            Kind::Dgram { state: DgramState::Connected { peer, .. }, .. } => peer,
            _ => return -ENOTCONN,
        }
    } else {
        match s.kind {
            Kind::Stream(Stream::Fresh | Stream::Failed { .. }) => (0, 0),
            Kind::Stream(Stream::Bound { local, .. } | Stream::PeerBound { local } | Stream::Listening { local, .. }) => local,
            Kind::Stream(
                Stream::Connecting { via: Via::Emulated { local }, .. }
                | Stream::Connected { via: Via::Emulated { local }, .. },
            ) => local,
            Kind::Stream(
                Stream::Connecting { via: Via::Native, .. }
                | Stream::Connected { via: Via::Native, .. },
            ) => native_name(fd as i32, false).unwrap_or((0, 0)),
            Kind::Dgram {
                state: DgramState::Unconnected { local } | DgramState::Connected { local, .. },
                ..
            } => local.unwrap_or((0, 0)),
        }
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

fn send_dgram(fd: i32, s: &Mutex<Sock>, buf: &[u8], flags: u64, dest: u64, dest_len: u64) -> i64 {
    if flags & !(MSG_DONTWAIT | MSG_NOSIGNAL) != 0 {
        return -EOPNOTSUPP;
    }
    let (native, peer) = {
        let g = s.lock().unwrap();
        (g.is_native(), g.peer())
    };
    let to = if dest == 0 {
        match peer {
            Some(_) => None,
            None => return -EDESTADDRREQ,
        }
    } else {
        match read_addr(dest, dest_len) {
            Ok((AF_INET, a)) => Some(a),
            Ok(_) => return -EAFNOSUPPORT,
            Err(e) => return -e,
        }
    };
    if let Some(a) = to {
        if loopback(a.0) {
            return -ECONNREFUSED;
        }
        if !granted() || !native {
            return -ENETUNREACH;
        }
    }
    if flags & MSG_DONTWAIT != 0 && would_block(fd, libc::POLLOUT) {
        return -EAGAIN;
    }
    match to {
        None => ret(unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) }),
        Some(a) => {
            let sa = sockaddr_in(a);
            let r = keep_flags(fd, || unsafe {
                libc::sendto(fd, buf.as_ptr().cast(), buf.len(), 0, &sa as *const _ as *const libc::sockaddr, core::mem::size_of::<libc::sockaddr_in>() as u32)
            });
            if r < 0 { -native_errno() } else { r as i64 }
        }
    }
}

fn recv_dgram(fd: i32, s: &Mutex<Sock>, buf: &mut [u8], flags: u64) -> Result<(i64, Option<Addr>), i64> {
    if flags & !(MSG_DONTWAIT | MSG_WAITALL | MSG_PEEK) != 0 || flags & MSG_PEEK != 0 {
        return Err(EOPNOTSUPP);
    }
    if flags & MSG_DONTWAIT != 0 && would_block(fd, libc::POLLIN) {
        return Err(EAGAIN);
    }
    let (native, peer) = {
        let g = s.lock().unwrap();
        (g.is_native(), g.peer())
    };
    if peer.is_some() || !native {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        return if n < 0 { Err(last_errno()) } else { Ok((n as i64, peer)) };
    }
    let mut sa: libc::sockaddr_in = unsafe { core::mem::zeroed() };
    let mut len = core::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let n = unsafe { libc::recvfrom(fd, buf.as_mut_ptr().cast(), buf.len(), 0, &mut sa as *mut _ as *mut libc::sockaddr, &mut len) };
    if n < 0 { Err(last_errno()) } else { Ok((n as i64, Some(from_sockaddr_in(&sa)))) }
}

pub fn sendto(fd: u64, buf: u64, len: u64, flags: u64, dest: u64, dest_len: u64) -> i64 {
    let s = sock_or_err!(fd);
    if len != 0 && !user_ok(buf, len) {
        return -EFAULT;
    }
    let bytes = unsafe { core::slice::from_raw_parts(buf as *const u8, len as usize) };
    if s.lock().unwrap().is_dgram() {
        return send_dgram(fd as i32, &s, bytes, flags, dest, dest_len);
    }
    send_bytes(fd as i32, bytes, flags)
}

pub fn recvfrom(fd: u64, buf: u64, len: u64, flags: u64, src: u64, src_len: u64) -> i64 {
    let s = sock_or_err!(fd);
    if len != 0 && !user_ok(buf, len) {
        return -EFAULT;
    }
    let bytes = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len as usize) };
    if s.lock().unwrap().is_dgram() {
        return match recv_dgram(fd as i32, &s, bytes, flags) {
            Ok((n, from)) => {
                if let (Some(a), true) = (from, src != 0) {
                    let r = write_addr(src, src_len, a);
                    if r < 0 {
                        return r;
                    }
                }
                n
            }
            Err(e) => -e,
        };
    }
    if flags & MSG_PEEK != 0 && s.lock().unwrap().is_native() {
        return -EOPNOTSUPP;
    }
    let n = recv_bytes(fd as i32, bytes, flags);
    if n >= 0 && src != 0 {
        let peer = s.lock().unwrap().peer();
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
    name_len: u64,
    iov: Vec<(u64, u64)>,
    control_len: u64,
}

fn msghdr(p: u64) -> Result<Msghdr, i64> {
    if !user_ok(p, 56) {
        return Err(EFAULT);
    }
    let w = unsafe { *(p as *const [u64; 7]) };
    Ok(Msghdr { name: w[0], name_len: w[1] & 0xffff_ffff, iov: iovecs(w[2], w[3])?, control_len: w[5] })
}

fn sendmsg_one(fd: u64, s: &Mutex<Sock>, p: u64, flags: u64) -> i64 {
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
    if s.lock().unwrap().is_dgram() {
        return send_dgram(fd as i32, s, &all, flags, m.name, m.name_len);
    }
    send_bytes(fd as i32, &all, flags)
}

pub fn sendmsg(fd: u64, p: u64, flags: u64) -> i64 {
    let s = sock_or_err!(fd);
    sendmsg_one(fd, &s, p, flags)
}

pub fn recvmsg(fd: u64, p: u64, flags: u64) -> i64 {
    let s = sock_or_err!(fd);
    let m = match msghdr(p) {
        Ok(m) => m,
        Err(e) => return -e,
    };
    let total: u64 = m.iov.iter().map(|&(_, l)| l).sum();
    let mut buf = vec![0u8; total.min(1 << 24) as usize];
    let dgram = s.lock().unwrap().is_dgram();
    let (n, from) = if dgram {
        match recv_dgram(fd as i32, &s, &mut buf, flags) {
            Ok(r) => r,
            Err(e) => return -e,
        }
    } else {
        if flags & MSG_PEEK != 0 && s.lock().unwrap().is_native() {
            return -EOPNOTSUPP;
        }
        (recv_bytes(fd as i32, &mut buf, flags), s.lock().unwrap().peer())
    };
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
        if let Some(peer) = from {
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
    let s = sock_or_err!(fd);
    let n = n.min(1024);
    if n != 0 && !user_ok(v, 64 * n) {
        return -EFAULT;
    }
    for i in 0..n {
        let r = sendmsg_one(fd, &s, v + 64 * i, flags);
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
        SOL_SOCKET => matches!(name, 2 | 7 | 8 | 9 | 20 | 21),
        IPPROTO_TCP => matches!(name, 1 | 4 | 5 | 6),
        IPPROTO_IP => matches!(name, 2 | 11),
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
    let mut s = s.lock().unwrap();
    if level == 1 && (name == 20 || name == 21) {
        if len < 16 {
            return -EINVAL;
        }
        if s.is_native() && unsafe { libc::setsockopt(fd as i32, libc::SOL_SOCKET, name as i32, bytes.as_ptr().cast(), len as u32) } != 0 {
            return -native_errno();
        }
    }
    s.options.insert((level, name), bytes);
    0
}

pub fn getsockopt(fd: u64, level: u64, name: u64, val: u64, len: u64) -> i64 {
    let s = sock_or_err!(fd);
    let mut s = s.lock().unwrap();
    let int = |v: i32| v.to_ne_bytes().to_vec();
    let value = match (level, name) {
        (1, 3) => int(if s.is_dgram() { SOCK_DGRAM } else { SOCK_STREAM } as i32),
        (1, 4) => {
            let e = match s.kind {
                Kind::Stream(Stream::Connecting { .. }) => match s.poll_connect(fd as i32) {
                    Conn::Failed(e) => {
                        s.kind = Kind::Stream(Stream::Fresh);
                        e
                    }
                    Conn::Pending | Conn::Done => 0,
                },
                Kind::Stream(Stream::Failed { error }) => {
                    s.kind = Kind::Stream(Stream::Fresh);
                    error
                }
                _ => 0,
            };
            int(e as i32)
        }
        (1, 30) => int(s.is_listening() as i32),
        (1, 38) => int(if s.is_dgram() { 17 } else { 6 }),
        (1, 39) => int(AF_INET as i32),
        _ if known_option(level, name) => s.options.get(&(level, name)).cloned().unwrap_or_else(|| match (level, name) {
            (1, 7) => int(16384),
            (1, 8) => int(131072),
            (1, 20) | (1, 21) => vec![0; 16],
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
