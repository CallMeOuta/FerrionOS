use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{errno::*, mem::user_ok, signal, thread::Thread};

const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;
const READINESS: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x40 | 0x80 | 0x100 | 0x200;

enum Edge {
    Level,
    Edge { reported: u32 },
}

enum Registration {
    Armed { edge: Edge },
    Disarmed,
}

struct Interest {
    events: u32,
    data: u64,
    reg: Registration,
}

#[derive(Default)]
struct Epoll {
    list: BTreeMap<i32, Interest>,
}

static EPOLLS: Mutex<BTreeMap<i32, Arc<Mutex<Epoll>>>> = Mutex::new(BTreeMap::new());

fn epoll(fd: u64) -> Option<Arc<Mutex<Epoll>>> {
    EPOLLS.lock().unwrap().get(&(fd as i32)).cloned()
}

pub fn duplicated(old: i32, new: i32) {
    let mut e = EPOLLS.lock().unwrap();
    e.remove(&new);
    if let Some(ep) = e.get(&old).cloned() {
        e.insert(new, ep);
    }
}

fn unique_epolls() -> Vec<Arc<Mutex<Epoll>>> {
    let e = EPOLLS.lock().unwrap();
    let mut out: Vec<Arc<Mutex<Epoll>>> = Vec::new();
    for ep in e.values() {
        if !out.iter().any(|seen| Arc::ptr_eq(seen, ep)) {
            out.push(Arc::clone(ep));
        }
    }
    out
}

pub fn closed(fd: i32) {
    let epolls = {
        let mut e = EPOLLS.lock().unwrap();
        e.remove(&fd);
        let mut out: Vec<Arc<Mutex<Epoll>>> = Vec::new();
        for ep in e.values() {
            if !out.iter().any(|seen| Arc::ptr_eq(seen, ep)) {
                out.push(Arc::clone(ep));
            }
        }
        out
    };
    for ep in epolls {
        ep.lock().unwrap().list.remove(&fd);
    }
}

pub fn drained(fd: i32) {
    for ep in unique_epolls() {
        let mut ep = ep.lock().unwrap();
        if let Some(i) = ep.list.get_mut(&fd) {
            if let Registration::Armed { edge: Edge::Edge { reported } } = &mut i.reg {
                *reported = 0;
            }
        }
    }
}

fn ret(r: isize) -> i64 {
    if r < 0 { -last_errno() } else { r as i64 }
}

pub fn epoll_create(flags: u64) -> i64 {
    const EPOLL_CLOEXEC: u64 = 0x80000;
    if flags & !EPOLL_CLOEXEC != 0 {
        return -EINVAL;
    }
    let mut o = libc::O_RDWR;
    if flags & EPOLL_CLOEXEC != 0 {
        o |= libc::O_CLOEXEC;
    }
    let fd = crate::files::new_fd(ret(unsafe { libc::open(c"/scheme/event".as_ptr(), o) } as isize));
    if fd >= 0 {
        EPOLLS.lock().unwrap().insert(fd as i32, Arc::new(Mutex::new(Epoll::default())));
    }
    fd
}

pub fn epoll_ctl(epfd: u64, op: u64, fd: u64, event: u64) -> i64 {
    const ADD: u64 = 1;
    const DEL: u64 = 2;
    const MOD: u64 = 3;
    let Some(ep) = epoll(epfd) else {
        return if unsafe { libc::fcntl(epfd as i32, libc::F_GETFD) } < 0 { -EBADF } else { -EINVAL };
    };
    let fd = fd as i32;
    if crate::files::reserved(fd) || unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
        return -EBADF;
    }
    if fd == epfd as i32 || epoll(fd as u64).is_some() {
        return -EINVAL;
    }
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } == 0 && matches!(st.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFDIR) {
        return -EPERM;
    }
    let (events, data) = if op == DEL {
        (0, 0)
    } else {
        if !user_ok(event, 12) {
            return -EFAULT;
        }
        unsafe { (*(event as *const u32), core::ptr::read_unaligned((event + 4) as *const u64)) }
    };
    let mut ep = ep.lock().unwrap();
    let r = match op {
        ADD if ep.list.contains_key(&fd) => -EEXIST,
        ADD | MOD => {
            if op == MOD && !ep.list.contains_key(&fd) {
                return -ENOENT;
            }
            let edge = if events & EPOLLET != 0 { Edge::Edge { reported: 0 } } else { Edge::Level };
            ep.list.insert(fd, Interest { events, data, reg: Registration::Armed { edge } });
            0
        }
        DEL => {
            if ep.list.remove(&fd).is_none() { -ENOENT } else { 0 }
        }
        _ => -EINVAL,
    };
    r
}

fn poll_events(events: u32) -> i16 {
    let mut e = (events & READINESS & !(EPOLLERR | EPOLLHUP)) as i16;
    if events & EPOLLRDHUP != 0 {
        e |= libc::POLLHUP;
    }
    e
}

fn slice_ms(deadline: Option<std::time::Instant>, slice: i32) -> i32 {
    match deadline {
        None => slice,
        Some(d) => d.saturating_duration_since(std::time::Instant::now()).as_millis().min(slice as u128) as i32,
    }
}

pub fn epoll_wait(thread: &Thread, epfd: u64, out: u64, max: u64, timeout: i64, mask: u64) -> i64 {
    let Some(ep) = epoll(epfd) else {
        return if unsafe { libc::fcntl(epfd as i32, libc::F_GETFD) } < 0 { -EBADF } else { -EINVAL };
    };
    if max == 0 || max > 1 << 20 {
        return -EINVAL;
    }
    if !user_ok(out, 12 * max) {
        return -EFAULT;
    }
    let deadline = (timeout >= 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout as u64));
    signal::with_mask(thread, mask, || loop {
        let mut fds: Vec<libc::pollfd> = {
            let ep = ep.lock().unwrap();
            ep.list
                .iter()
                .filter(|(_, i)| matches!(i.reg, Registration::Armed { .. }))
                .map(|(&fd, i)| {
                    let mut events = poll_events(i.events);
                    if crate::net::connecting(fd) {
                        events &= !libc::POLLIN;
                    }
                    libc::pollfd { fd, events, revents: 0 }
                })
                .collect()
        };
        let wait = slice_ms(deadline, 100);
        let n = if fds.is_empty() {
            let ts = libc::timespec { tv_sec: 0, tv_nsec: wait as libc::c_long * 1_000_000 };
            unsafe { libc::nanosleep(&ts, core::ptr::null_mut()) }
        } else {
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, wait) }
        };
        if n < 0 {
            return -last_errno();
        }
        let mut got = 0u64;
        {
            let mut ep = ep.lock().unwrap();
            for p in &fds {
                if got == max {
                    break;
                }
                let Some(i) = ep.list.get_mut(&p.fd) else { continue };
                if p.revents & libc::POLLNVAL != 0 {
                    ep.list.remove(&p.fd);
                    continue;
                }
                let revents = crate::net::settle(p.fd, p.events, p.revents);
                let mut ready = revents as u32 & READINESS;
                if p.revents & libc::POLLHUP != 0 && i.events & EPOLLRDHUP != 0 {
                    ready |= EPOLLRDHUP;
                }
                let ready = ready & (i.events | EPOLLERR | EPOLLHUP);
                let report = match &mut i.reg {
                    Registration::Disarmed => continue,
                    Registration::Armed { edge } => match edge {
                        Edge::Level => ready,
                        Edge::Edge { reported } => {
                            let r = ready & !*reported;
                            *reported = ready;
                            r
                        }
                    },
                };
                if report == 0 {
                    continue;
                }
                if i.events & EPOLLONESHOT != 0 {
                    i.reg = Registration::Disarmed;
                }
                let at = out + 12 * got;
                unsafe {
                    *(at as *mut u32) = report;
                    core::ptr::write_unaligned((at + 4) as *mut u64, i.data);
                }
                got += 1;
            }
        }
        if got > 0 {
            return got as i64;
        }
        if signal::pending_for(thread) {
            return -EINTR;
        }
        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            return 0;
        }
    })
}

pub fn poll(thread: &Thread, fds: u64, n: u64, timeout: i64, mask: u64) -> i64 {
    if n > 4096 {
        return -EINVAL;
    }
    if n != 0 && !user_ok(fds, 8 * n) {
        return -EFAULT;
    }
    signal::with_mask(thread, mask, || {
        let user = unsafe { core::slice::from_raw_parts_mut(fds as *mut libc::pollfd, n as usize) };
        if !user.iter().any(|p| crate::net::connecting(p.fd)) {
            return ret(unsafe { libc::poll(fds as *mut libc::pollfd, n as libc::nfds_t, timeout.clamp(-1, i32::MAX as i64) as i32) } as isize);
        }
        let deadline = (timeout >= 0).then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout as u64));
        loop {
            let mut copy: Vec<libc::pollfd> = user
                .iter()
                .map(|p| {
                    let events = if crate::net::connecting(p.fd) { p.events & !libc::POLLIN } else { p.events };
                    libc::pollfd { fd: p.fd, events, revents: 0 }
                })
                .collect();
            let r = unsafe { libc::poll(copy.as_mut_ptr(), copy.len() as libc::nfds_t, slice_ms(deadline, 50)) };
            if r < 0 {
                return -last_errno();
            }
            let mut count = 0;
            for (u, c) in user.iter_mut().zip(&copy) {
                u.revents = crate::net::settle(c.fd, u.events, c.revents);
                if u.revents != 0 {
                    count += 1;
                }
            }
            if count > 0 {
                return count;
            }
            if signal::pending_for(thread) {
                return -EINTR;
            }
            if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                return 0;
            }
        }
    })
}

pub fn timespec_ms(ts: u64) -> Result<i64, i64> {
    if ts == 0 {
        return Ok(-1);
    }
    if !user_ok(ts, 16) {
        return Err(EFAULT);
    }
    let (s, ns) = unsafe { (*(ts as *const i64), *((ts + 8) as *const i64)) };
    if s < 0 || !(0..1_000_000_000).contains(&ns) {
        return Err(EINVAL);
    }
    Ok(s.saturating_mul(1000).saturating_add((ns + 999_999) / 1_000_000))
}

pub fn sigmask_arg(set: u64, size: u64) -> Result<u64, i64> {
    if set == 0 {
        return Ok(0);
    }
    if size != 8 {
        return Err(EINVAL);
    }
    if !user_ok(set, 8) {
        return Err(EFAULT);
    }
    Ok(set)
}

pub fn select(thread: &Thread, n: u64, r: u64, w: u64, e: u64, timeout_ms: i64, mask: u64) -> i64 {
    if n > 1024 {
        return -EINVAL;
    }
    let words = n.div_ceil(64);
    for set in [r, w, e] {
        if set != 0 && words != 0 && !user_ok(set, 8 * words) {
            return -EFAULT;
        }
    }
    let bit = |set: u64, fd: u64| set != 0 && unsafe { *((set + 8 * (fd / 64)) as *const u64) } & (1 << (fd % 64)) != 0;
    let mut fds = Vec::new();
    for fd in 0..n {
        let mut ev = 0i16;
        if bit(r, fd) {
            ev |= libc::POLLIN;
        }
        if bit(w, fd) {
            ev |= libc::POLLOUT;
        }
        if bit(e, fd) {
            ev |= libc::POLLPRI;
        }
        if ev != 0 {
            fds.push(libc::pollfd { fd: fd as i32, events: ev, revents: 0 });
        }
    }
    let got = signal::with_mask(thread, mask, || {
        ret(unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms.clamp(-1, i32::MAX as i64) as i32) } as isize)
    });
    if got < 0 {
        return got;
    }
    if fds.iter().any(|p| p.revents & libc::POLLNVAL != 0) {
        return -EBADF;
    }
    for set in [r, w, e] {
        if set != 0 {
            unsafe { core::ptr::write_bytes(set as *mut u8, 0, 8 * words as usize) };
        }
    }
    let mut count = 0;
    let mut mark = |set: u64, fd: i32| {
        if set != 0 {
            unsafe { *((set + 8 * (fd as u64 / 64)) as *mut u64) |= 1 << (fd as u64 % 64) };
            count += 1;
        }
    };
    for p in &fds {
        if p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && p.events & libc::POLLIN != 0 {
            mark(r, p.fd);
        }
        if p.revents & (libc::POLLOUT | libc::POLLERR) != 0 && p.events & libc::POLLOUT != 0 {
            mark(w, p.fd);
        }
        if p.revents & libc::POLLPRI != 0 && p.events & libc::POLLPRI != 0 {
            mark(e, p.fd);
        }
    }
    count
}
