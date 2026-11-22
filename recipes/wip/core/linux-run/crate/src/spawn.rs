//! `vfork` and `clone(CLONE_VM|CLONE_VFORK)` (plans/linux-compat.md, Processes and threads): the
//! child shares the parent's memory until it execs or exits, and the parent waits. glibc's
//! `posix_spawn` depends on that: the child writes the error of a failed `execve` into the
//! parent's memory, and the parent reads it there.
//!
//! The runtime runs the child in the parent's own process, on the calling thread: the child's
//! Linux code works on the parent's memory because it is that memory. What Linux gives the child
//! of its own is kept aside and put back when the child ends: the parent's registers, vector
//! state, thread pointer and signal mask, the Linux signal actions, the working directory, the
//! umask, and each descriptor number the child changes (src/files.rs reports them here before the
//! change). `setsid` and `setpgid` are deferred to the native child. The native child process is
//! made only when the child execs: a native fork of this process, in the state the child left
//! it, that then execs; its pid is what the parent's `clone` or `vfork` returns. A failed exec
//! returns its error to the child, which goes on as on Linux. A child that exits instead becomes
//! a native child that exits with its status.
//!
//! Other threads of the parent go on running meanwhile and see the child's descriptor changes
//! until the child ends; on Linux the child has a table of its own. Only one vfork child runs in a
//! process at a time; another one meanwhile is a fork whose parent waits, as before LC3.

use core::sync::atomic::Ordering;
use std::sync::Mutex;

use crate::{
    errno::*,
    signal,
    syscall::{Exec, run_exec},
    thread::{Frame, Thread},
};

/// Copies of the descriptors a vfork child changes are kept at and above this number, which
/// Linux programs cannot name (src/files.rs, `reserved`).
pub const SAVED_FDS: i32 = 960;

struct Vfork {
    /// The native thread that runs the child.
    native: usize,
    parent: Frame,
    xsave: Vec<u8>,
    fs: u64,
    mask: u64,
    actions: Option<signal::SavedActions>,
    /// (descriptor, its copy, close-on-exec), or a copy of -1 for a number that was free.
    fds: Vec<(i32, i32, bool)>,
    cwd: Option<Vec<u8>>,
    umask: libc::mode_t,
    setsid: bool,
    setpgid: Option<(i32, i32)>,
    /// CLONE_PARENT_SETTID's address, written with the child's pid.
    parent_tid: u64,
}

static STATE: Mutex<Option<Vfork>> = Mutex::new(None);

fn me() -> usize {
    unsafe { libc::pthread_self() as usize }
}

/// Whether the calling thread runs a vfork child.
pub fn active() -> bool {
    STATE.lock().unwrap().as_ref().is_some_and(|v| v.native == me())
}

/// Starts a vfork child on this thread. Returns false when another one runs.
pub fn start(thread: &Thread, frame: &mut Frame, stack: u64, parent_tid: u64) -> bool {
    let mut state = STATE.lock().unwrap();
    if state.is_some() {
        return false;
    }
    let umask = unsafe { libc::umask(0o022) };
    unsafe { libc::umask(umask) };
    *state = Some(Vfork {
        native: me(),
        parent: *frame,
        xsave: thread.xsave().to_vec(),
        fs: thread.linux_fs(),
        mask: thread.mask.load(Ordering::Relaxed),
        actions: Some(signal::save_actions()),
        fds: Vec::new(),
        cwd: None,
        umask,
        setsid: false,
        setpgid: None,
        parent_tid,
    });
    if stack != 0 {
        frame.rsp = stack;
    }
    true
}

/// Before the child changes descriptor `fd` (closes, replaces or flags it): keeps a copy.
pub fn before_fd_change(fd: i32) -> bool {
    let mut state = STATE.lock().unwrap();
    let Some(v) = state.as_mut().filter(|v| v.native == me()) else { return true };
    if fd < 0 || fd >= SAVED_FDS || v.fds.iter().any(|&(f, _, _)| f == fd) {
        return true;
    }
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        v.fds.push((fd, -1, false));
        return true;
    }
    let copy = crate::files::dup_at_least(fd, SAVED_FDS, crate::files::RUNTIME_FDS_END, true);
    if copy < 0 {
        return false;
    }
    v.fds.push((fd, copy as i32, flags & libc::FD_CLOEXEC != 0));
    true
}

/// After the child got the new descriptor `fd`: the number was free in the parent.
pub fn fd_created(fd: i32) {
    let mut state = STATE.lock().unwrap();
    let Some(v) = state.as_mut().filter(|v| v.native == me()) else { return };
    if fd >= 0 && !v.fds.iter().any(|&(f, _, _)| f == fd) {
        v.fds.push((fd, -1, false));
    }
}

/// Before the child changes the working directory: keeps the parent's.
pub fn before_chdir() {
    let mut state = STATE.lock().unwrap();
    let Some(v) = state.as_mut().filter(|v| v.native == me()) else { return };
    if v.cwd.is_none() {
        let mut buf = vec![0u8; 4096];
        if !unsafe { libc::getcwd(buf.as_mut_ptr().cast(), buf.len()) }.is_null() {
            let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            buf.truncate(n);
            v.cwd = Some(buf);
        }
    }
}

/// setsid in the child: done by the native child, before it execs.
pub fn setsid() -> i64 {
    if let Some(v) = STATE.lock().unwrap().as_mut() {
        v.setsid = true;
    }
    // The child's pid is its session id; the pid is not known until it execs.
    crate::syscall::pid() as i64
}

/// setpgid in the child, which only names itself: done by the native child.
pub fn setpgid(pid: i32, pgid: i32) -> i64 {
    if pid != 0 && pid != crate::syscall::pid() {
        return -ESRCH;
    }
    if let Some(v) = STATE.lock().unwrap().as_mut() {
        v.setpgid = Some((0, pgid));
    }
    0
}

/// Ends the child: the parent's state comes back and its call returns `pid`.
fn finish(thread: &Thread, frame: &mut Frame, pid: i32) -> i64 {
    let mut v = STATE.lock().unwrap().take().expect("a vfork child runs");
    for &(fd, copy, cloexec) in v.fds.iter().rev() {
        unsafe {
            if copy < 0 {
                libc::close(fd);
            } else {
                libc::dup2(copy, fd);
                libc::fcntl(fd, libc::F_SETFD, if cloexec { libc::FD_CLOEXEC } else { 0 });
                libc::close(copy);
            }
        }
        crate::files::forget_dir(fd);
    }
    if let Some(cwd) = v.cwd.take() {
        if let Ok(cwd) = std::ffi::CString::new(cwd) {
            unsafe { libc::chdir(cwd.as_ptr()) };
        }
    }
    unsafe { libc::umask(v.umask) };
    if let Some(actions) = v.actions.take() {
        signal::restore_actions(actions);
    }
    thread.xsave().copy_from_slice(&v.xsave);
    thread.set_linux_fs(v.fs);
    thread.mask.store(v.mask, Ordering::Relaxed);
    *frame = v.parent;
    if v.parent_tid != 0 && crate::mem::user_ok(v.parent_tid, 4) {
        unsafe { *(v.parent_tid as *mut i32) = pid };
    }
    pid as i64
}

/// What the native child does first: the deferred session and group changes.
fn native_child_setup() {
    let (setsid, setpgid) = STATE
        .lock()
        .unwrap()
        .as_ref()
        .map_or((false, None), |v| (v.setsid, v.setpgid));
    unsafe {
        if setsid {
            libc::setsid();
        }
        if let Some((pid, pgid)) = setpgid {
            libc::setpgid(pid, pgid);
        }
    }
}

/// execve in the child: a native fork execs the program; the parent resumes with its pid once
/// that exec has happened. A failed exec returns its error to the child.
pub fn exec(thread: &Thread, frame: &mut Frame, exec: &Exec) -> i64 {
    let mut report = [-1i32; 2];
    if unsafe { libc::pipe2(report.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return -last_errno();
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = last_errno();
        unsafe {
            libc::close(report[0]);
            libc::close(report[1]);
        }
        return -e;
    }
    if pid == 0 {
        unsafe { libc::close(report[0]) };
        crate::session::forked_child();
        native_child_setup();
        let err = run_exec(exec, thread.mask.load(Ordering::Relaxed)) as i32;
        unsafe {
            libc::write(report[1], (&err as *const i32).cast(), 4);
            libc::_exit(127);
        }
    }
    unsafe { libc::close(report[1]) };
    let mut err = 0i32;
    let n = loop {
        let n = unsafe { libc::read(report[0], (&mut err as *mut i32).cast(), 4) };
        if n < 0 && last_errno() == EINTR {
            continue;
        }
        break n;
    };
    unsafe { libc::close(report[0]) };
    if n == 4 {
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0) };
        return -(err as i64);
    }
    finish(thread, frame, pid)
}

/// exit or exit_group in the child: a native child exits with `status`; the parent resumes.
pub fn exit(thread: &Thread, frame: &mut Frame, status: i32) -> i64 {
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe { libc::_exit(status) }
    }
    if pid < 0 {
        // No process for the child's status: the parent sees the fork fail.
        let e = last_errno();
        finish(thread, frame, -1);
        return -e;
    }
    finish(thread, frame, pid)
}
