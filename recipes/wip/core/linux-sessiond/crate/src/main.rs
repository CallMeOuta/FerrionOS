//! linux-sessiond: the file systems of the Linux layer's sessions (plans/linux-compat.md, "The
//! session"), served under the scheme `linux-session`.
//!
//! A session is a handle on `new`. Through it its owner opens two file systems, which come back
//! as scheme handles that the launcher registers in the session's namespace (linux-run's
//! src/session.rs):
//!
//! - `root`: the Debian root of the linux-root package. One `redoxfs --read-only` process serves
//!   it to every session; it is started with this service and never written.
//! - `memory` or `memory/MIB`: a new, empty file system in memory (`redoxfs --memory`), at most
//!   `MEMORY_MIB` MiB (256 when not given; a caller may ask for less, never more). Its process is
//!   ended when the last handle on the session closes.
//!
//! Only the uid that made a session opens its file systems, and each uid has at most
//! `SESSIONS_PER_UID` sessions (`SESSIONS` in all), so that no user can tie up the machine's
//! memory through this service. The service never hands out its own namespace: each file system
//! process registers its scheme in a namespace of its own, made for it alone, from which this
//! service takes the scheme's handle and which it then drops.
//!
//! This service runs as root because only uid 0 may create a scheme (the kernel's
//! `src/scheme/mod.rs`); it starts without the hardware right.

use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use redox_scheme::scheme::{SchemeState, SchemeSync, register_sync_scheme};
use redox_scheme::{CallerCtx, OpenResult, RequestKind, Response, SignalBehavior, Socket};
use syscall::error::*;
use syscall::schemev2::NewFdFlags;

const REDOXFS: &str = "/usr/bin/redoxfs";
const ROOT_IMAGE: &str = "/usr/share/linux-root/root.redoxfs";
/// The largest memory file system a session gets, and the size it gets when it names none.
const MEMORY_MIB: u64 = 256;
const SESSIONS_PER_UID: usize = 4;
const SESSIONS: usize = 32;
/// What a redoxfs process needs in its own namespace: its image and program, memory, the process
/// manager's credentials capability, events, time, randomness and the right to create a scheme.
const SERVER_SCHEMES: &[&str] = &[
    "file", "memory", "proc", "event", "time", "rand", "null", "zero", "pipe", "log",
    "namespace:scheme-create",
];

unsafe extern "C" {
    fn redox_mkns_v1(names: *const libc::iovec, n: usize, flags: u32) -> usize;
    fn redox_setns_v0(fd: usize) -> usize;
    fn redox_get_ns_v0() -> usize;
    fn redox_dup_v1(fd: usize, buf: *const u8, len: usize) -> usize;
    fn redox_openat_v1(fd: usize, path: *const u8, len: usize, flags: u32, fcntl: u32) -> usize;
    fn redox_close_v1(fd: usize) -> usize;
}

fn raw(r: usize) -> Result<usize> {
    if r > usize::MAX - 4096 {
        Err(Error::new((r as isize).wrapping_neg() as i32))
    } else {
        Ok(r)
    }
}

/// Enters a namespace of its own, which loses the hardware right.
fn drop_hardware() -> Result<()> {
    // NsDup::ShrinkPermissions with LIST | INSERT | DELETE | SCHEME_CREATE, and no hardware bit.
    let mut buf = [0u8; 16];
    buf[..8].copy_from_slice(&1usize.to_ne_bytes());
    buf[8..].copy_from_slice(&0b1111usize.to_ne_bytes());
    let ns = raw(unsafe { redox_get_ns_v0() })?;
    let shrunk = raw(unsafe { redox_dup_v1(ns, buf.as_ptr(), buf.len()) })?;
    let old = unsafe { redox_setns_v0(shrunk) };
    if old != usize::MAX {
        unsafe { redox_close_v1(old) };
    }
    Ok(())
}

/// Starts redoxfs with `args` in a namespace made for it, registered there as `name`, and returns
/// the process and the handle of its scheme; the namespace is dropped. `name` is the one the
/// session registers the handle under too (`linux-root`, `linux-mem`), so that paths the file
/// system reports (fpath) name it in the session.
fn serve(name: &str, args: &[&str]) -> Result<(Child, usize)> {
    let iov: Vec<libc::iovec> = SERVER_SCHEMES
        .iter()
        .map(|s| libc::iovec { iov_base: s.as_ptr() as *mut _, iov_len: s.len() })
        .collect();
    let ns = raw(unsafe { redox_mkns_v1(iov.as_ptr(), iov.len(), 0) })?;
    let mut cmd = Command::new(REDOXFS);
    cmd.arg("--no-daemon")
        .args(args)
        .arg(&name)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    unsafe {
        cmd.pre_exec(move || {
            let old = redox_setns_v0(ns);
            if old != usize::MAX {
                redox_close_v1(old);
            }
            Ok(())
        });
    }
    let spawned = cmd.spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            unsafe { redox_close_v1(ns) };
            eprintln!("linux-sessiond: cannot start {REDOXFS}: {e}");
            return Err(Error::new(EIO));
        }
    };
    let path = format!("/scheme/{name}");
    let deadline = Instant::now() + Duration::from_secs(20);
    let cap = loop {
        let flags = (libc::O_DIRECTORY | libc::O_CLOEXEC) as u32;
        match raw(unsafe { redox_openat_v1(ns, path.as_ptr(), path.len(), flags, 0) }) {
            Ok(cap) => break Ok(cap),
            Err(_) if Instant::now() < deadline && matches!(child.try_wait(), Ok(None)) => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => break Err(e),
        }
    };
    unsafe { redox_close_v1(ns) };
    match cap {
        Ok(cap) => Ok((child, cap)),
        Err(e) => {
            eprintln!("linux-sessiond: {REDOXFS} {args:?} served nothing: {e}");
            let _ = child.kill();
            let _ = child.wait();
            Err(Error::new(EIO))
        }
    }
}

struct Lock {
    key: (u64, u64),
    whole_file: bool,
    pid: usize,
    start: u64,
    end: u64,
    write: bool,
}

struct Session {
    uid: u32,
    memory: Option<Child>,
    members: Vec<usize>,
    leader: Option<usize>,
    ended: bool,
    locks: Vec<Lock>,
}

const JOIN: u64 = 1;
const SET_LOCK: u64 = 2;
const GET_LOCK: u64 = 3;
const RELEASE_LOCKS: u64 = 4;
const READ_LOCK: u64 = 0;
const WRITE_LOCK: u64 = 1;
const UNLOCK: u64 = 2;
const MEMBERS: usize = 4096;
const LOCKS: usize = 4096;

fn alive(pid: usize) -> bool {
    let found = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    found || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

impl Session {
    fn conflict(&mut self, key: (u64, u64), whole_file: bool, pid: usize, start: u64, end: u64, write: bool) -> Option<usize> {
        let clashes = |l: &Lock| {
            l.key == key && l.whole_file == whole_file && l.pid != pid && l.start < end && start < l.end && (write || l.write)
        };
        let dead: Vec<usize> = self.locks.iter().filter(|l| clashes(l) && !alive(l.pid)).map(|l| l.pid).collect();
        self.locks.retain(|l| !dead.contains(&l.pid));
        self.locks.iter().position(clashes)
    }

    fn set_lock(&mut self, key: (u64, u64), whole_file: bool, pid: usize, start: u64, end: u64, kind: u64) -> Result<()> {
        if kind != UNLOCK && self.conflict(key, whole_file, pid, start, end, kind == WRITE_LOCK).is_some() {
            return Err(Error::new(EAGAIN));
        }
        let mut kept = Vec::with_capacity(self.locks.len() + 2);
        for l in self.locks.drain(..) {
            if l.key != key || l.whole_file != whole_file || l.pid != pid || l.end <= start || end <= l.start {
                kept.push(l);
                continue;
            }
            if l.start < start {
                kept.push(Lock { end: start, ..l });
                if l.end > end {
                    kept.push(Lock { start: end, ..l });
                }
            } else if l.end > end {
                kept.push(Lock { start: end, ..l });
            }
        }
        self.locks = kept;
        if kind != UNLOCK {
            if self.locks.len() >= LOCKS {
                return Err(Error::new(ENOLCK));
            }
            self.locks.push(Lock { key, whole_file, pid, start, end, write: kind == WRITE_LOCK });
        }
        Ok(())
    }

    fn end(&mut self) {
        self.ended = true;
        for pid in self.members.drain(..) {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        self.locks.clear();
    }
}

struct Sessions {
    root: usize,
    sessions: BTreeMap<usize, Session>,
    leaders: BTreeMap<usize, usize>,
    next: usize,
}

impl SchemeSync for Sessions {
    fn scheme_root(&mut self) -> Result<usize> {
        Ok(0)
    }

    fn openat(
        &mut self,
        dir: usize,
        path: &str,
        _flags: usize,
        _fcntl_flags: u32,
        ctx: &CallerCtx,
    ) -> Result<OpenResult> {
        if dir == 0 {
            if path != "new" {
                return Err(Error::new(ENOENT));
            }
            let mine = self.sessions.values().filter(|s| s.uid == ctx.uid).count();
            if mine >= SESSIONS_PER_UID || self.sessions.len() >= SESSIONS {
                return Err(Error::new(EMFILE));
            }
            self.next += 1;
            self.sessions.insert(
                self.next,
                Session { uid: ctx.uid, memory: None, members: Vec::new(), leader: None, ended: false, locks: Vec::new() },
            );
            return Ok(OpenResult::ThisScheme { number: self.next, flags: NewFdFlags::empty() });
        }
        let session = self.sessions.get_mut(&dir).ok_or(Error::new(EBADF))?;
        if session.uid != ctx.uid {
            return Err(Error::new(EACCES));
        }
        if path == "root" {
            let fd = raw(unsafe { redox_dup_v1(self.root, core::ptr::null(), 0) })?;
            return Ok(OpenResult::OtherScheme { fd });
        }
        if path == "leader" {
            if session.leader.is_some() || session.ended {
                return Err(Error::new(EEXIST));
            }
            self.next += 1;
            session.leader = Some(self.next);
            self.leaders.insert(self.next, dir);
            return Ok(OpenResult::ThisScheme { number: self.next, flags: NewFdFlags::empty() });
        }
        let mib = match path {
            "memory" => MEMORY_MIB,
            p => p
                .strip_prefix("memory/")
                .and_then(|m| m.parse::<u64>().ok())
                .filter(|m| (1..=MEMORY_MIB).contains(m))
                .ok_or(Error::new(ENOENT))?,
        };
        if session.memory.is_some() {
            return Err(Error::new(EEXIST));
        }
        let (child, fd) = serve("linux-mem", &["--memory", &mib.to_string()])?;
        session.memory = Some(child);
        Ok(OpenResult::OtherScheme { fd })
    }

    fn call(&mut self, id: usize, payload: &mut [u8], metadata: &[u64], ctx: &CallerCtx) -> Result<usize> {
        let session = self.sessions.get_mut(&id).ok_or(Error::new(EBADF))?;
        let pid = ctx.pid;
        match *metadata {
            [JOIN, ..] => {
                if session.ended {
                    return Err(Error::new(ESRCH));
                }
                if !session.members.contains(&pid) {
                    if session.members.len() >= MEMBERS {
                        session.members.retain(|&p| alive(p));
                    }
                    if session.members.len() >= MEMBERS {
                        return Err(Error::new(EMFILE));
                    }
                    session.members.push(pid);
                }
                Ok(0)
            }
            [op @ (SET_LOCK | GET_LOCK), a, b, ..] => {
                let words = payload.get(..32).ok_or(Error::new(EINVAL))?;
                let w: [u64; 4] = core::array::from_fn(|i| u64::from_ne_bytes(words[8 * i..8 * i + 8].try_into().unwrap()));
                let [start, len, kind, whole_file] = w;
                if kind > UNLOCK || (op == GET_LOCK && kind == UNLOCK) {
                    return Err(Error::new(EINVAL));
                }
                let end = if len == 0 { u64::MAX } else { start.checked_add(len).ok_or(Error::new(EOVERFLOW))? };
                let whole_file = whole_file != 0;
                if op == SET_LOCK {
                    return session.set_lock((a, b), whole_file, pid, start, end, kind).map(|_| 0);
                }
                let Some(i) = session.conflict((a, b), whole_file, pid, start, end, kind == WRITE_LOCK) else {
                    return Ok(0);
                };
                let l = &session.locks[i];
                let answer = [
                    if l.write { WRITE_LOCK } else { READ_LOCK },
                    l.start,
                    if l.end == u64::MAX { 0 } else { l.end - l.start },
                    l.pid as u64,
                ];
                let out = payload.get_mut(..32).ok_or(Error::new(EINVAL))?;
                for (chunk, v) in out.chunks_exact_mut(8).zip(answer) {
                    chunk.copy_from_slice(&v.to_ne_bytes());
                }
                Ok(32)
            }
            [RELEASE_LOCKS, a, b, ..] => {
                session.locks.retain(|l| l.key != (a, b) || l.whole_file || l.pid != pid);
                Ok(0)
            }
            _ => Err(Error::new(EINVAL)),
        }
    }

    fn on_close(&mut self, id: usize) {
        if let Some(dir) = self.leaders.remove(&id) {
            if let Some(session) = self.sessions.get_mut(&dir) {
                session.end();
            }
            return;
        }
        if let Some(mut session) = self.sessions.remove(&id) {
            if let Some(leader) = session.leader {
                self.leaders.remove(&leader);
            }
            session.end();
            if let Some(mut child) = session.memory {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn run() -> Result<()> {
    let notify: usize = std::env::var("INIT_NOTIFY")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or(Error::new(EINVAL))?;
    drop_hardware()?;
    let (_root_server, root) = serve("linux-root", &["--read-only", ROOT_IMAGE])?;
    let socket = Socket::create()?;
    let mut sessions = Sessions { root, sessions: BTreeMap::new(), leaders: BTreeMap::new(), next: 0 };
    let mut state = SchemeState::new();
    register_sync_scheme(&socket, "linux-session", &mut sessions)?;
    syscall::write(notify, &[0])?;
    unsafe { libc::close(notify as i32) };
    loop {
        let Some(request) = socket.next_request(SignalBehavior::Restart)? else {
            return Ok(());
        };
        match request.kind() {
            RequestKind::Call(call) => {
                let response = call.handle_sync(&mut sessions, &mut state);
                socket.write_response(response, SignalBehavior::Restart)?;
            }
            RequestKind::OnClose { id } => sessions.on_close(id),
            RequestKind::SendFd(request) => {
                socket.write_response(Response::err(EOPNOTSUPP, request), SignalBehavior::Restart)?;
            }
            _ => {}
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("linux-sessiond: {error}");
        std::process::exit(1);
    }
}
