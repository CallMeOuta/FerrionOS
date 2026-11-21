//! Linux threads: the per-thread crossing page, the kernel registration, entering Linux code and
//! `clone`.

use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
const D_XSAVE: usize = PAGE;

unsafe extern "C" {
    static lr_stub_start: u8;
    static lr_stub_exit: u8;
    static lr_stub_end: u8;
    fn redox_cur_thrfd_v0() -> usize;
    fn redox_dup_v1(fd: usize, buf: *const u8, len: usize) -> usize;
    fn redox_write_v1(fd: usize, buf: *const u8, len: usize) -> usize;
    fn redox_close_v1(fd: usize) -> usize;
}
core::arch::global_asm!(include_str!("entry.S"));

/// Runtime state of one Linux thread. THREADS holds one reference and the thread's data page
/// another (taken with `Arc::into_raw`), which `exit_thread` gives back.
pub struct Thread {
    pub tid: i32,
    /// Code page of this thread's stub copy; the data page follows it.
    pub area: usize,
    /// Linux signal mask.
    pub mask: AtomicU64,
    /// Signals sent to this thread with tgkill or tkill.
    pub pending: AtomicU64,
    /// sigaltstack: (sp, flags, size).
    pub altstack: Mutex<(u64, i32, u64)>,
    pub clear_tid: AtomicU64,
    pub robust_list: AtomicU64,
}

impl Thread {
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
pub static THREADS: Mutex<Vec<(i32, Arc<Thread>)>> = Mutex::new(Vec::new());
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
        tid,
        area,
        mask: AtomicU64::new(mask),
        pending: AtomicU64::new(0),
        altstack: Mutex::new((0, 2 /* SS_DISABLE */, 0)),
        clear_tid: AtomicU64::new(0),
        robust_list: AtomicU64::new(0),
    });
    let data = area + PAGE;
    unsafe {
        let w = |off: usize, v: u64| *((data + off) as *mut u64) = v;
        w(D_NATIVE_TOP, (stack + NATIVE_STACK) as u64);
        w(D_NATIVE_FS, crate::cpu::rdfsbase());
        w(D_HANDLER, crate::syscall::lr_dispatch as *const () as u64);
        w(D_THREAD, Arc::into_raw(Arc::clone(&thread)) as u64);
        // XRSTOR loads MXCSR even for an image in its initial state: mask every exception, as
        // Linux starts a program.
        *((data + D_XSAVE + 24) as *mut u32) = 0x1f80;
    }
    register(area);
    THREADS.lock().unwrap().push((tid, thread));
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

struct ChildStart {
    frame: Frame,
    fs: u64,
    tid: i32,
    mask: u64,
    clear_tid: u64,
    xsave: Vec<u8>,
}

extern "C" fn child_start(arg: *mut libc::c_void) -> *mut libc::c_void {
    let start = unsafe { Box::from_raw(arg as *mut ChildStart) };
    let thread = new_thread(start.tid, start.mask);
    thread.set_linux_fs(start.fs);
    thread.clear_tid.store(start.clear_tid, Ordering::Relaxed);
    thread.xsave().copy_from_slice(&start.xsave);
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
pub fn clone(thread: &Thread, frame: &Frame) -> i64 {
    let (flags, stack, ptid, ctid, tls) = (frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8);
    if flags & CLONE_VM == 0 {
        return fork(thread, flags, ctid);
    }
    if flags & CLONE_THREAD == 0 || flags & CLONE_VFORK != 0 {
        // A process sharing memory (vfork, posix_spawn) is LC3's.
        return -ENOSYS;
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
    });
    let mut handle: libc::pthread_t = unsafe { core::mem::zeroed() };
    let arg = Box::into_raw(start) as *mut libc::c_void;
    let r = unsafe { libc::pthread_create(&mut handle, core::ptr::null(), child_start, arg) };
    if r != 0 {
        drop(unsafe { Box::from_raw(arg as *mut ChildStart) });
        return -linux_errno(r);
    }
    unsafe { libc::pthread_detach(handle) };
    tid as i64
}

/// A new process (fork, or clone with only a signal number): relibc's fork copies the address
/// space, the runtime included; the child's thread is a new kernel context, so it registers again.
fn fork(thread: &Thread, flags: u64, ctid: u64) -> i64 {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return -last_errno();
    }
    if pid == 0 {
        crate::syscall::refresh_pid();
        register(thread.area);
        let tid = crate::syscall::pid();
        if flags & CLONE_CHILD_SETTID != 0 && user_ok(ctid, 4) {
            unsafe { *(ctid as *mut i32) = tid }
        }
        if flags & CLONE_CHILD_CLEARTID != 0 {
            thread.clear_tid.store(ctid, Ordering::Relaxed);
        }
        return 0;
    }
    pid as i64
}

/// The exit call: ends this thread only.
pub fn exit_thread(thread: &Thread, status: i32) -> ! {
    THREADS
        .lock()
        .unwrap()
        .retain(|&(tid, _)| tid != thread.tid);
    if THREADS.lock().unwrap().is_empty() {
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
        .find(|(t, _)| *t == tid)
        .map(|(_, t)| Arc::clone(t))
}
