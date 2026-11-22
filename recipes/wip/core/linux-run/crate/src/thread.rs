//! Linux threads: the per-thread crossing page, the kernel registration, entering Linux code and
//! `clone`.

use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::{LINUX_TOP, NATIVE_END, errno::*, mem::user_ok};

/// The registers of a Linux thread as the entry stub pushed them (src/entry.S).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Frame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
}

const FRAME_SIZE: usize = core::mem::size_of::<Frame>();
const PAGE: usize = 4096;
const NATIVE_STACK: usize = 256 * 1024;

// Offsets in the data page (src/entry.S).
const D_NATIVE_TOP: usize = 8;
const D_LINUX_FS: usize = 16;
const D_NATIVE_FS: usize = 24;
const D_HANDLER: usize = 32;
const D_THREAD: usize = 48;
const D_SIGCONTROL: usize = 56;
const D_NATIVE_SIGENTRY: usize = 64;
const D_NATIVE_EXCPENTRY: usize = 72;
const D_INTERRUPT: usize = 80;
const D_FAULT: usize = 88;
const D_XSAVE: usize = PAGE;

unsafe extern "C" {
    static lr_stub_start: u8;
    static lr_stub_exit: u8;
    static lr_stub_end: u8;
    static lr_sig_entry: u8;
    static lr_excp_entry: u8;
    fn redox_cur_thrfd_v0() -> usize;
    fn redox_dup_v1(fd: usize, buf: *const u8, len: usize) -> usize;
    fn redox_write_v1(fd: usize, buf: *const u8, len: usize) -> usize;
    fn redox_close_v1(fd: usize) -> usize;
}
core::arch::global_asm!(include_str!("entry.S"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskState {
    Normal,
    Temporary { saved: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltStack {
    Disabled,
    Enabled { sp: u64, size: u64 },
}

impl AltStack {
    pub fn contains(self, sp: u64) -> bool {
        match self {
            Self::Disabled => false,
            Self::Enabled { sp: base, size } => sp > base && sp <= base.saturating_add(size),
        }
    }
}

/// Runtime state of one Linux thread. THREADS holds one reference and the thread's data page
/// another (taken with `Arc::into_raw`), which `exit_thread` gives back.
pub struct Thread {
    /// Linux thread id; a forked child's thread takes the child's process id.
    pub tid: AtomicI32,
    /// Code page of this thread's stub copy; the data page follows it.
    pub area: usize,
    /// Linux signal mask.
    pub mask: AtomicU64,
    /// Signals sent to this thread with tgkill or tkill.
    pub pending: AtomicU64,
    pub altstack: Mutex<AltStack>,
    pub clear_tid: AtomicU64,
    pub robust_list: AtomicU64,
    pub mask_state: Mutex<MaskState>,
    /// The native thread, for waking it with the runtime's kick signal.
    pub native: usize,
}

impl Thread {
    pub fn tid(&self) -> i32 {
        self.tid.load(Ordering::Relaxed)
    }
    pub fn data(&self) -> usize {
        self.area + PAGE
    }
    pub fn linux_fs(&self) -> u64 {
        unsafe { *((self.data() + D_LINUX_FS) as *const u64) }
    }
    pub fn set_linux_fs(&self, fs: u64) {
        unsafe { *((self.data() + D_LINUX_FS) as *mut u64) = fs }
    }
    /// The saved vector state: the XSAVE image of the Linux registers at the last crossing.
    pub fn xsave(&self) -> &'static mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                (self.data() + D_XSAVE) as *mut u8,
                crate::cpu::xsave_size(),
            )
        }
    }
}

/// Threads by Linux thread id, for tgkill.
pub static THREADS: Mutex<Vec<Arc<Thread>>> = Mutex::new(Vec::new());
const THREAD_START_LIMIT: Duration = Duration::from_secs(30);
static NEXT_TID: AtomicU64 = AtomicU64::new(0);

/// Linux thread ids: the process id for the first thread, then a counter. They name threads of
/// this process only; LC1 has no session-wide table (plans/linux-compat.md, Processes and
/// threads).
pub fn next_tid() -> i32 {
    let pid = crate::syscall::pid() as u64;
    let n = NEXT_TID.fetch_add(1, Ordering::Relaxed);
    ((pid + n) & 0x3fff_ffff) as i32
}

fn map_native(len: usize, prot: i32) -> usize {
    let p = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            len,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        crate::fatal(&format!(
            "could not map runtime memory: {}",
            std::io::Error::last_os_error()
        ));
    }
    if (p as usize) < LINUX_TOP {
        crate::fatal(&format!(
            "runtime memory landed at {p:p}, below the native window"
        ));
    }
    p as usize
}

/// Makes the calling native thread a Linux thread: copies the stub, gives it a native stack and
/// registers the hook for this thread only. The native FS base is the caller's.
pub fn new_thread(tid: i32, mask: u64) -> &'static Thread {
    let stub_len =
        unsafe { (&lr_stub_end as *const u8 as usize) - (&lr_stub_start as *const u8 as usize) };
    assert!(stub_len <= PAGE);
    let xpages = crate::cpu::xsave_size().div_ceil(PAGE);
    let area = map_native((2 + xpages) * PAGE, libc::PROT_READ | libc::PROT_WRITE);
    unsafe {
        core::ptr::copy_nonoverlapping(&lr_stub_start as *const u8, area as *mut u8, stub_len);
        if libc::mprotect(area as *mut _, PAGE, libc::PROT_READ | libc::PROT_EXEC) != 0 {
            crate::fatal("could not make the crossing page executable");
        }
    }
    let stack = map_native(NATIVE_STACK, libc::PROT_READ | libc::PROT_WRITE);
    let thread = Arc::new(Thread {
        tid: AtomicI32::new(tid),
        area,
        mask: AtomicU64::new(mask),
        pending: AtomicU64::new(0),
        altstack: Mutex::new(AltStack::Disabled),
        clear_tid: AtomicU64::new(0),
        robust_list: AtomicU64::new(0),
        mask_state: Mutex::new(MaskState::Normal),
        native: unsafe { libc::pthread_self() } as usize,
    });
    let data = area + PAGE;
    unsafe {
        let w = |off: usize, v: u64| *((data + off) as *mut u64) = v;
        w(D_NATIVE_TOP, (stack + NATIVE_STACK) as u64);
        w(D_NATIVE_FS, crate::cpu::rdfsbase());
        w(D_HANDLER, crate::syscall::lr_dispatch as *const () as u64);
        w(D_THREAD, Arc::into_raw(Arc::clone(&thread)) as u64);
        w(D_INTERRUPT, crate::signal::lr_interrupt as *const () as u64);
        w(D_FAULT, crate::signal::lr_fault as *const () as u64);
        // XRSTOR loads MXCSR even for an image in its initial state: mask every exception, as
        // Linux starts a program.
        *((data + D_XSAVE + 24) as *mut u32) = 0x1f80;
    }
    register(area);
    THREADS.lock().unwrap().push(thread);
    current(data)
}

/// Registers this thread's foreign-call hook: its stub copy as the entry, the native window as
/// the range. Fails when the kernel has no hook.
pub fn register(area: usize) {
    let words: [usize; 3] = [area, LINUX_TOP, NATIVE_END];
    let ok = unsafe {
        let fd = redox_dup_v1(redox_cur_thrfd_v0(), b"foreign-call".as_ptr(), 12);
        if fd > usize::MAX - 4096 {
            false
        } else {
            let r = redox_write_v1(fd, words.as_ptr() as *const u8, 24);
            redox_close_v1(fd);
            r == 24
        }
    };
    if !ok {
        crate::fatal(
            "the kernel has no foreign-call hook for this thread (needs ferrion-foreign-call.patch)",
        );
    }
    register_sighandler(area);
}

/// relibc's SetSighandlerData (redox_syscall's data.rs).
#[repr(C)]
struct SetSighandlerData {
    user_handler: usize,
    excp_handler: usize,
    thread_control_addr: usize,
    proc_control_addr: usize,
}

// relibc's `redox_rt::signal::current_setsighandler_struct`, a Rust function whose mangled name
// build.rs finds in the libc.a this binary links (relibc exports no C name for it).
include!(concat!(env!("OUT_DIR"), "/sighandler_symbol.rs"));

/// Makes the runtime's entries this thread's signal and exception entries, keeping relibc's
/// control blocks and its entries for signals and faults that arrive in native code.
fn register_sighandler(area: usize) {
    let mut data = unsafe { relibc_setsighandler_struct() };
    let base = area + PAGE;
    let entry = |sym: &u8| area + (sym as *const u8 as usize - unsafe { &lr_stub_start } as *const u8 as usize);
    unsafe {
        let w = |off: usize, v: usize| *((base + off) as *mut usize) = v;
        w(D_SIGCONTROL, data.thread_control_addr);
        w(D_NATIVE_SIGENTRY, data.user_handler);
        w(D_NATIVE_EXCPENTRY, data.excp_handler);
        data.user_handler = entry(&lr_sig_entry);
        data.excp_handler = entry(&lr_excp_entry);
    }
    let ok = unsafe {
        let fd = redox_dup_v1(redox_cur_thrfd_v0(), b"sighandler".as_ptr(), 10);
        if fd > usize::MAX - 4096 {
            false
        } else {
            let r = redox_write_v1(fd, (&data as *const SetSighandlerData).cast(), size_of::<SetSighandlerData>());
            redox_close_v1(fd);
            r == size_of::<SetSighandlerData>()
        }
    };
    if !ok {
        crate::fatal("could not register the runtime's signal entry for this thread");
    }
}

/// The Sigcontrol of the thread whose data page is `data`.
pub fn sigcontrol(data: usize) -> usize {
    unsafe { *((data + D_SIGCONTROL) as *const usize) }
}

/// Sets the address space's mmap-min-addr, through the current thread's handle.
pub fn set_mmap_min(addr: usize) -> Result<(), i64> {
    unsafe {
        let fd = crate::errno::raw(redox_dup_v1(
            redox_cur_thrfd_v0(),
            b"mmap-min-addr".as_ptr(),
            13,
        ))?;
        let r = crate::errno::raw(redox_write_v1(
            fd,
            (&addr as *const usize).cast(),
            size_of::<usize>(),
        ));
        redox_close_v1(fd);
        r.map(|_| ())
    }
}

/// Starts or resumes Linux code on this thread with `frame`, never returning.
pub fn enter(thread: &Thread, frame: &Frame) -> ! {
    let data = thread.data();
    unsafe {
        let top = *((data + D_NATIVE_TOP) as *const u64) as usize;
        let at = (top - FRAME_SIZE) as *mut Frame;
        at.write(*frame);
        let exit = thread.area
            + ((&lr_stub_exit as *const u8 as usize) - (&lr_stub_start as *const u8 as usize));
        core::arch::asm!("mov rsp, {0}", "jmp {1}", in(reg) at, in(reg) exit, options(noreturn));
    }
}

/// The thread whose data page is `data`. The reference is valid until the thread exits, and only
/// that thread calls this.
pub fn current(data: usize) -> &'static Thread {
    unsafe { &*(*((data + D_THREAD) as *const u64) as *const Thread) }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StartState {
    Starting,
    Registered,
    Acked,
}

struct ChildStart {
    frame: Frame,
    fs: u64,
    tid: i32,
    mask: u64,
    clear_tid: u64,
    xsave: Vec<u8>,
    started: Arc<(Mutex<StartState>, Condvar)>,
}

extern "C" fn child_start(arg: *mut libc::c_void) -> *mut libc::c_void {
    let start = unsafe { Box::from_raw(arg as *mut ChildStart) };
    let thread = new_thread(start.tid, start.mask);
    thread.set_linux_fs(start.fs);
    thread.clear_tid.store(start.clear_tid, Ordering::Relaxed);
    thread.xsave().copy_from_slice(&start.xsave);
    {
        let (lock, cvar) = &*start.started;
        let mut state = lock.lock().unwrap();
        *state = StartState::Registered;
        cvar.notify_one();
        let deadline = Instant::now() + THREAD_START_LIMIT;
        while *state != StartState::Acked {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                crate::fatal("parent did not acknowledge cloned thread in time");
            };
            let (guard, timeout) = cvar.wait_timeout(state, remaining).unwrap();
            state = guard;
            if timeout.timed_out() && *state != StartState::Acked {
                crate::fatal("parent did not acknowledge cloned thread in time");
            }
        }
    }
    let frame = start.frame;
    drop(start);
    enter(thread, &frame)
}

const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_THREAD: u64 = 0x10000;
const CLONE_SETTLS: u64 = 0x80000;
const CLONE_PARENT_SETTID: u64 = 0x100000;
const CLONE_CHILD_CLEARTID: u64 = 0x200000;
const CLONE_CHILD_SETTID: u64 = 0x1000000;

/// clone(flags, stack, parent_tid, child_tid, tls).
pub fn clone(thread: &Thread, frame: &mut Frame) -> i64 {
    let (flags, stack, ptid, ctid, tls) = (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8);
    clone_with(thread, frame, flags, stack, ptid, ctid, tls)
}

/// clone3(args, size): the same as clone with the arguments in a structure, the new stack given
/// by its base and size.
pub fn clone3(thread: &Thread, frame: &mut Frame, args: u64, size: u64) -> i64 {
    const CLONE_PIDFD: u64 = 0x1000;
    const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;
    if !(64..=4096).contains(&size) {
        return -EINVAL;
    }
    if !user_ok(args, size) {
        return -EFAULT;
    }
    // flags, pidfd, child_tid, parent_tid, exit_signal, stack, stack_size, tls, set_tid,
    // set_tid_size, cgroup.
    let w = |i: u64| if 8 * i < size { unsafe { *((args + 8 * i) as *const u64) } } else { 0 };
    let (flags, child_tid, parent_tid, exit_signal) = (w(0), w(2), w(3), w(4));
    let (stack, stack_size, tls, set_tid_size) = (w(5), w(6), w(7), w(9));
    if flags & (CLONE_PIDFD | CLONE_INTO_CGROUP) != 0 || set_tid_size != 0 || exit_signal > 64 || flags & 0xff != 0 {
        return -EINVAL;
    }
    if (stack == 0) != (stack_size == 0) {
        return -EINVAL;
    }
    let top = if stack == 0 { 0 } else { stack + stack_size };
    clone_with(thread, frame, flags | exit_signal, top, parent_tid, child_tid, tls)
}

pub fn clone_with(thread: &Thread, frame: &mut Frame, flags: u64, stack: u64, ptid: u64, ctid: u64, tls: u64) -> i64 {
    if flags & CLONE_THREAD == 0 {
        // A new process. With CLONE_VM and CLONE_VFORK (vfork, posix_spawn) the child runs on
        // the parent's memory until it execs or exits (src/spawn.rs). Without CLONE_VM it is a
        // fork; CLONE_VM alone, or a second vfork child while one runs, gets a copy of the
        // address space and the parent waits until the child execs or exits.
        if flags & (CLONE_VM | CLONE_VFORK) == CLONE_VM | CLONE_VFORK {
            if flags & CLONE_PARENT_SETTID != 0 && !user_ok(ptid, 4) {
                return -EFAULT;
            }
            let settid = if flags & CLONE_PARENT_SETTID != 0 { ptid } else { 0 };
            if crate::spawn::start(thread, frame, stack, settid) {
                return 0;
            }
        }
        let hold = flags & (CLONE_VM | CLONE_VFORK) != 0;
        return fork(thread, frame, flags, stack, ptid, ctid, hold);
    }
    if flags & CLONE_VM == 0 {
        return -EINVAL;
    }
    if (flags & CLONE_PARENT_SETTID != 0 && !user_ok(ptid, 4))
        || (flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID) != 0 && !user_ok(ctid, 4))
    {
        return -EFAULT;
    }
    let tid = next_tid();
    if flags & CLONE_PARENT_SETTID != 0 {
        unsafe { *(ptid as *mut i32) = tid }
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        unsafe { *(ctid as *mut i32) = tid }
    }
    let mut child = *frame;
    child.rax = 0;
    if stack != 0 {
        child.rsp = stack;
    }
    let started = Arc::new((Mutex::new(StartState::Starting), Condvar::new()));
    let start = Box::new(ChildStart {
        frame: child,
        fs: if flags & CLONE_SETTLS != 0 {
            tls
        } else {
            thread.linux_fs()
        },
        tid,
        mask: thread.mask.load(Ordering::Relaxed),
        clear_tid: if flags & CLONE_CHILD_CLEARTID != 0 {
            ctid
        } else {
            0
        },
        xsave: thread.xsave().to_vec(),
        started: Arc::clone(&started),
    });
    let mut handle: libc::pthread_t = unsafe { core::mem::zeroed() };
    let arg = Box::into_raw(start) as *mut libc::c_void;
    let r = unsafe { libc::pthread_create(&mut handle, core::ptr::null(), child_start, arg) };
    if r != 0 {
        drop(unsafe { Box::from_raw(arg as *mut ChildStart) });
        return -linux_errno(r);
    }
    unsafe { libc::pthread_detach(handle) };
    let deadline = Instant::now() + THREAD_START_LIMIT;
    let (lock, cvar) = &*started;
    let mut state = lock.lock().unwrap();
    while *state == StartState::Starting {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            crate::fatal("cloned thread did not register in time");
        };
        let (guard, timeout) = cvar.wait_timeout(state, remaining).unwrap();
        state = guard;
        if timeout.timed_out() && *state == StartState::Starting {
            crate::fatal("cloned thread did not register in time");
        }
    }
    assert!(find(tid).is_some());
    *state = StartState::Acked;
    cvar.notify_one();
    drop(state);
    tid as i64
}

/// A new process: relibc's fork copies the address space, the runtime included; the child's
/// thread is a new kernel context, so it registers again, and it is the child's only thread.
/// With `hold`, the parent returns only once the child has exec'd or exited: the child keeps the
/// write end of a close-on-exec pipe, whose end the parent waits for.
fn fork(thread: &Thread, frame: &mut Frame, flags: u64, stack: u64, ptid: u64, ctid: u64, hold: bool) -> i64 {
    if (flags & CLONE_PARENT_SETTID != 0 && !user_ok(ptid, 4))
        || (flags & (CLONE_CHILD_SETTID | CLONE_CHILD_CLEARTID) != 0 && !user_ok(ctid, 4))
    {
        return -EFAULT;
    }
    let mut gate = [-1i32; 2];
    if hold && unsafe { libc::pipe2(gate.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return -last_errno();
    }
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        let e = -last_errno();
        if hold {
            unsafe {
                libc::close(gate[0]);
                libc::close(gate[1]);
            }
        }
        return e;
    }
    if pid == 0 {
        crate::syscall::refresh_pid();
        crate::session::forked_child();
        if set_mmap_min(LINUX_TOP).is_err() {
            crate::fatal("could not set mmap-min-addr in forked child");
        }
        register(thread.area);
        let tid = crate::syscall::pid();
        thread.tid.store(tid, Ordering::Relaxed);
        let mut threads = THREADS.lock().unwrap();
        threads.retain(|t| core::ptr::eq(&**t, thread));
        assert_eq!(threads.len(), 1);
        drop(threads);
        if flags & CLONE_CHILD_SETTID != 0 {
            unsafe { *(ctid as *mut i32) = tid }
        }
        thread.clear_tid.store(if flags & CLONE_CHILD_CLEARTID != 0 { ctid } else { 0 }, Ordering::Relaxed);
        if hold {
            unsafe { libc::close(gate[0]) };
        }
        if stack != 0 {
            frame.rsp = stack;
        }
        return 0;
    }
    if flags & CLONE_PARENT_SETTID != 0 {
        unsafe { *(ptid as *mut i32) = pid }
    }
    if hold {
        unsafe {
            libc::close(gate[1]);
            let mut b = 0u8;
            while libc::read(gate[0], (&mut b as *mut u8).cast(), 1) < 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
            {}
            libc::close(gate[0]);
        }
    }
    pid as i64
}

/// The exit call: ends this thread only.
pub fn exit_thread(thread: &Thread, status: i32) -> ! {
    let empty = {
        let mut threads = THREADS.lock().unwrap();
        threads.retain(|t| t.tid() != thread.tid());
        threads.is_empty()
    };
    if empty {
        crate::syscall::exit_group(status);
    }
    let ctid = thread.clear_tid.load(Ordering::Relaxed);
    if ctid != 0 && user_ok(ctid, 4) {
        unsafe {
            *(ctid as *mut i32) = 0;
            crate::syscall::futex_wake(ctid as *mut u32, 1);
        }
    }
    // The data page's reference; the crossing page and native stack stay mapped, since this
    // call still runs on that stack.
    unsafe { drop(Arc::from_raw(thread as *const Thread)) };
    unsafe { libc::pthread_exit(core::ptr::null_mut()) }
}

pub fn find(tid: i32) -> Option<Arc<Thread>> {
    THREADS
        .lock()
        .unwrap()
        .iter()
        .find(|t| t.tid() == tid)
        .cloned()
}
