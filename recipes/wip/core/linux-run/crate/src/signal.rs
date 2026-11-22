//! Linux signals. The runtime keeps the Linux actions and masks itself and owns delivery
//! (plans/linux-compat.md, Signals). Native signals mirror the Linux actions: a signal the
//! program handles has a native recorder, one it ignores is ignored natively, and one left at its
//! default keeps the native default, so the process manager stops, ignores or ends the process
//! as Linux would, with the same status for the parent.
//!
//! The kernel enters the runtime's own signal entry (src/entry.S): a native signal that arrives
//! while Linux code runs, spinning or not, brings the thread into the runtime, where relibc runs
//! the recorder on the native stack and the runtime then builds the Linux frame; one that arrives
//! in the runtime interrupts a blocking call (EINTR, restarted or not as SA_RESTART says) and is
//! delivered when the call returns. Signals sent inside the process (kill to itself, tgkill)
//! are queued here and the target thread is woken with a native kick signal. A fault in Linux
//! code enters the runtime's exception entry and becomes SIGSEGV, SIGBUS, SIGFPE, SIGILL or
//! SIGTRAP with Linux's siginfo and context, or ends the process by that signal.

use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

use crate::{
    errno::*,
    mem::user_ok,
    thread::{Frame, Thread},
};

#[derive(Clone, Copy, Default)]
struct Action {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SIGKILL: u64 = 9;
const SIGSTOP: u64 = 19;
const SI_USER: i32 = 0;
const SI_TKILL: i32 = -6;
const UNBLOCKABLE: u64 = 1 << (SIGKILL - 1) | 1 << (SIGSTOP - 1);

static ACTIONS: Mutex<[Action; 65]> = Mutex::new(
    [Action {
        handler: 0,
        flags: 0,
        restorer: 0,
        mask: 0,
    }; 65],
);
/// Sent to the process with kill: (signal bits, sender pid of the last one).
static PROCESS_PENDING: AtomicU64 = AtomicU64::new(0);
/// Sender of the last pending signal of each number, for siginfo.
static SENDER: [AtomicI32; 65] = [const { AtomicI32::new(0) }; 65];
/// The native real-time signal that wakes a thread for a signal queued here.
const KICK: i32 = 64;

fn bit(sig: u64) -> u64 {
    1 << (sig - 1)
}

extern "C" fn recorder(sig: i32, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    if (1..=64).contains(&sig) {
        let from = if info.is_null() { 0 } else { unsafe { (*info).si_pid() } };
        SENDER[sig as usize].store(from, Ordering::Relaxed);
        PROCESS_PENDING.fetch_or(bit(sig as u64), Ordering::SeqCst);
    }
}

extern "C" fn kicked(_: i32) {}

/// Signals with a native counterpart: 1 to 31, less the two that cannot be caught.
fn native(sig: u64) -> bool {
    (1..=31).contains(&sig) && sig != SIGKILL && sig != SIGSTOP
}

/// Sets the native action that mirrors a Linux one.
fn mirror(sig: u64, handler: u64) {
    if !native(sig) {
        return;
    }
    unsafe {
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = match handler {
            SIG_DFL => libc::SIG_DFL,
            SIG_IGN => libc::SIG_IGN,
            _ => {
                sa.sa_flags = libc::SA_SIGINFO;
                recorder as *const () as usize
            }
        };
        libc::sigfillset(&mut sa.sa_mask);
        libc::sigaction(sig as i32, &sa, core::ptr::null_mut());
    }
}

/// At start: signals the parent left ignored stay ignored (as across Linux's execve), the native
/// mask the launcher inherited becomes the Linux thread's mask, and native signals are then
/// unblocked, since the runtime's entry takes them. Returns the Linux mask.
///
/// SIGPIPE is the exception: Rust's start-up code ignores it natively before `main`, so its
/// state is lost there. `sigpipe_ignored` says what it was: an `execve` passes
/// `--sigpipe-ignored` when the program ignored it; otherwise it is at its default.
pub fn init(sigpipe_ignored: bool) -> u64 {
    const SIGPIPE: u64 = 13;
    let mut actions = ACTIONS.lock().unwrap();
    for sig in 1..=31u64 {
        if !native(sig) {
            continue;
        }
        let mut old: libc::sigaction = unsafe { core::mem::zeroed() };
        unsafe { libc::sigaction(sig as i32, core::ptr::null(), &mut old) };
        let ignored = if sig == SIGPIPE { sigpipe_ignored } else { old.sa_sigaction == libc::SIG_IGN };
        if ignored {
            actions[sig as usize].handler = SIG_IGN;
            mirror(sig, SIG_IGN);
        } else {
            mirror(sig, SIG_DFL);
        }
    }
    drop(actions);
    unsafe {
        let mut sa: libc::sigaction = core::mem::zeroed();
        sa.sa_sigaction = kicked as *const () as usize;
        libc::sigaction(KICK, &sa, core::ptr::null_mut());
        let mut old: libc::sigset_t = core::mem::zeroed();
        let mut none: libc::sigset_t = core::mem::zeroed();
        libc::sigemptyset(&mut none);
        libc::sigprocmask(libc::SIG_SETMASK, &none, &mut old);
        (1..=31u64).filter(|&s| libc::sigismember(&old, s as i32) == 1).fold(0, |m, s| m | bit(s)) & !UNBLOCKABLE
    }
}

/// Before execve: the Linux mask becomes the native one, which the next linux-run reads back.
pub fn mask_for_exec(mask: u64) {
    unsafe {
        let mut set: libc::sigset_t = core::mem::zeroed();
        libc::sigemptyset(&mut set);
        for sig in 1..=31u64 {
            if mask & bit(sig) != 0 {
                libc::sigaddset(&mut set, sig as i32);
            }
        }
        libc::sigprocmask(libc::SIG_SETMASK, &set, core::ptr::null_mut());
    }
}

/// After a failed execve: native signals open again.
pub fn unmask_after_exec() {
    mask_for_exec(0);
}

/// The Linux signal actions, kept while a vfork child runs in this process (src/spawn.rs).
pub struct SavedActions([Action; 65]);

pub fn save_actions() -> SavedActions {
    SavedActions(*ACTIONS.lock().unwrap())
}

/// Puts saved actions back, with their native mirrors.
pub fn restore_actions(saved: SavedActions) {
    let mut actions = ACTIONS.lock().unwrap();
    for sig in 1..=64u64 {
        if actions[sig as usize].handler != saved.0[sig as usize].handler {
            mirror(sig, saved.0[sig as usize].handler);
        }
    }
    *actions = saved.0;
}

pub fn rt_sigaction(sig: u64, act: u64, oact: u64, size: u64) -> i64 {
    if size != 8 || sig == 0 || sig > 64 {
        return -EINVAL;
    }
    if act != 0 && (sig == SIGKILL || sig == SIGSTOP) {
        return -EINVAL;
    }
    if (act != 0 && !user_ok(act, 32)) || (oact != 0 && !user_ok(oact, 32)) {
        return -EFAULT;
    }
    let mut actions = ACTIONS.lock().unwrap();
    let old = actions[sig as usize];
    if act != 0 {
        let a = unsafe { core::slice::from_raw_parts(act as *const u64, 4) };
        actions[sig as usize] = Action {
            handler: a[0],
            flags: a[1],
            restorer: a[2],
            mask: a[3] & !UNBLOCKABLE,
        };
        mirror(sig, a[0]);
    }
    if oact != 0 {
        let o = unsafe { core::slice::from_raw_parts_mut(oact as *mut u64, 4) };
        o.copy_from_slice(&[old.handler, old.flags, old.restorer, old.mask]);
    }
    0
}

pub fn rt_sigprocmask(thread: &Thread, how: u64, set: u64, oset: u64, size: u64) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    if (set != 0 && !user_ok(set, 8)) || (oset != 0 && !user_ok(oset, 8)) {
        return -EFAULT;
    }
    let old = thread.mask.load(Ordering::Relaxed);
    if set != 0 {
        let s = unsafe { *(set as *const u64) };
        let new = match how {
            0 => old | s,
            1 => old & !s,
            2 => s,
            _ => return -EINVAL,
        };
        thread.mask.store(new & !UNBLOCKABLE, Ordering::Relaxed);
    }
    if oset != 0 {
        unsafe { *(oset as *mut u64) = old };
    }
    0
}

fn on_altstack(thread: &Thread, sp: u64) -> bool {
    let (base, flags, size) = *thread.altstack.lock().unwrap();
    flags & 2 == 0 && sp > base && sp <= base + size
}

pub fn sigaltstack(thread: &Thread, frame: &Frame, ss: u64, oss: u64) -> i64 {
    const SS_ONSTACK: i32 = 1;
    const SS_DISABLE: i32 = 2;
    const MINSIGSTKSZ: u64 = 2048;
    if (ss != 0 && !user_ok(ss, 24)) || (oss != 0 && !user_ok(oss, 24)) {
        return -EFAULT;
    }
    let on = on_altstack(thread, frame.rsp);
    let old = *thread.altstack.lock().unwrap();
    if ss != 0 {
        if on {
            return -EPERM;
        }
        let (sp, flags, size) = unsafe {
            (
                *(ss as *const u64),
                *((ss + 8) as *const i32),
                *((ss + 16) as *const u64),
            )
        };
        if flags & !(SS_DISABLE | (1 << 31)) != 0 {
            return -EINVAL;
        }
        let new = if flags & SS_DISABLE != 0 {
            (0, SS_DISABLE, 0)
        } else if size < MINSIGSTKSZ {
            return -ENOMEM;
        } else {
            (sp, 0, size)
        };
        *thread.altstack.lock().unwrap() = new;
    }
    if oss != 0 {
        unsafe {
            *(oss as *mut u64) = old.0;
            *((oss + 8) as *mut i32) = if on { SS_ONSTACK } else { old.1 };
            *((oss + 12) as *mut i32) = 0;
            *((oss + 16) as *mut u64) = old.2;
        }
    }
    0
}

/// The Linux action's handler for a signal: SIG_DFL, SIG_IGN or an address.
pub fn handler_of(sig: u64) -> u64 {
    ACTIONS.lock().unwrap()[sig as usize].handler
}

/// kill aimed at this process, for a signal the program handles or one with no native number.
pub fn send_process(sig: u64) {
    SENDER[sig as usize].store(crate::syscall::pid(), Ordering::Relaxed);
    PROCESS_PENDING.fetch_or(bit(sig), Ordering::SeqCst);
    kick_others(None);
}

/// tkill/tgkill: queued for the thread, which is woken if it is not the caller.
pub fn send_thread(thread: &Thread, sig: u64, caller: &Thread) {
    thread.pending.fetch_or(bit(sig), Ordering::SeqCst);
    if !core::ptr::eq(thread, caller) {
        kick(thread);
    }
}

fn kick(thread: &Thread) {
    unsafe { libc::pthread_kill(thread.native as libc::pthread_t, KICK) };
}

/// Wakes every thread but `except`, so that one with the signal unblocked takes it.
fn kick_others(except: Option<&Thread>) {
    let threads: Vec<_> = crate::thread::THREADS.lock().unwrap().iter().map(|(_, t)| std::sync::Arc::clone(t)).collect();
    let me = unsafe { libc::pthread_self() };
    for t in threads {
        if except.is_some_and(|e| core::ptr::eq(e, &*t)) || unsafe { libc::pthread_equal(t.native as libc::pthread_t, me) } != 0 {
            continue;
        }
        kick(&t);
    }
}

/// Whether a call that returned EINTR is restarted: yes unless a handler without SA_RESTART is
/// about to run (a signal that is ignored, defaulted or blocked does not interrupt on Linux).
pub fn restart_after_eintr(thread: &Thread) -> bool {
    let mask = thread.mask.load(Ordering::Relaxed);
    let ready = (thread.pending.load(Ordering::SeqCst) | PROCESS_PENDING.load(Ordering::SeqCst)) & !mask;
    if ready == 0 {
        return true;
    }
    let sig = ready.trailing_zeros() as u64 + 1;
    let a = ACTIONS.lock().unwrap()[sig as usize];
    match a.handler {
        SIG_DFL | SIG_IGN => true,
        _ => a.flags & SA_RESTART != 0,
    }
}

/// A signal is due for this thread (pending and not blocked).
pub fn pending_for(thread: &Thread) -> bool {
    (thread.pending.load(Ordering::SeqCst) | PROCESS_PENDING.load(Ordering::SeqCst))
        & !thread.mask.load(Ordering::Relaxed)
        != 0
}

pub fn rt_sigpending(thread: &Thread, set: u64, size: u64) -> i64 {
    if size > 8 {
        return -EINVAL;
    }
    if !user_ok(set, size) {
        return -EFAULT;
    }
    let p = (thread.pending.load(Ordering::SeqCst) | PROCESS_PENDING.load(Ordering::SeqCst))
        & thread.mask.load(Ordering::Relaxed);
    unsafe { core::ptr::copy_nonoverlapping(p.to_ne_bytes().as_ptr(), set as *mut u8, size as usize) };
    0
}

/// rt_sigsuspend: waits with `mask` until a handler is due; the handler's frame restores the
/// old mask.
pub fn sigsuspend(thread: &Thread, set: u64, size: u64) -> i64 {
    if size != 8 {
        return -EINVAL;
    }
    if !user_ok(set, 8) {
        return -EFAULT;
    }
    let new = unsafe { *(set as *const u64) } & !UNBLOCKABLE;
    let old = thread.mask.swap(new, Ordering::SeqCst);
    *thread.suspended_mask.lock().unwrap() = Some(old);
    unsafe {
        let mut all: libc::sigset_t = core::mem::zeroed();
        let mut none: libc::sigset_t = core::mem::zeroed();
        libc::sigfillset(&mut all);
        libc::sigemptyset(&mut none);
        loop {
            // Native signals are held while checking, then released atomically by the wait, so
            // one that arrives in between still ends it.
            libc::sigprocmask(libc::SIG_SETMASK, &all, core::ptr::null_mut());
            let ready = (thread.pending.load(Ordering::SeqCst) | PROCESS_PENDING.load(Ordering::SeqCst)) & !new;
            if ready != 0 {
                libc::sigprocmask(libc::SIG_SETMASK, &none, core::ptr::null_mut());
                break;
            }
            crate::ffi::sigsuspend(&none);
            libc::sigprocmask(libc::SIG_SETMASK, &none, core::ptr::null_mut());
        }
    }
    -EINTR
}

pub fn with_mask(thread: &Thread, set: u64, wait: impl FnOnce() -> i64) -> i64 {
    if set == 0 {
        return wait();
    }
    let new = unsafe { *(set as *const u64) } & !UNBLOCKABLE;
    let old = thread.mask.swap(new, Ordering::SeqCst);
    let r = wait();
    if r == -EINTR && pending_for(thread) {
        *thread.suspended_mask.lock().unwrap() = Some(old);
    } else {
        thread.mask.store(old, Ordering::SeqCst);
    }
    r
}

/// Bytes of a signal frame: rt_sigframe, the XSAVE image and its trailing magic, alignment.
pub fn frame_size() -> u64 {
    (8 + UC_SIZE + SIGINFO_SIZE + crate::cpu::xsave_size() as u64 + 4 + 64 + 16)
        .next_multiple_of(16)
        .max(2048)
}

const UC_SIZE: u64 = 304;
const SIGINFO_SIZE: u64 = 128;
const UC_FP_XSTATE: u64 = 1;
const UC_SIGCONTEXT_SS: u64 = 2;
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;

/// Takes one deliverable signal for this thread, if any: (signal, si_code).
fn take(thread: &Thread) -> Option<(u64, i32)> {
    let mask = thread.mask.load(Ordering::Relaxed);
    let own = thread.pending.load(Ordering::SeqCst) & !mask;
    if own != 0 {
        let sig = own.trailing_zeros() as u64 + 1;
        thread.pending.fetch_and(!bit(sig), Ordering::SeqCst);
        return Some((sig, SI_TKILL));
    }
    let proc = PROCESS_PENDING.load(Ordering::SeqCst) & !mask;
    if proc != 0 {
        let sig = proc.trailing_zeros() as u64 + 1;
        if PROCESS_PENDING.fetch_and(!bit(sig), Ordering::SeqCst) & bit(sig) != 0 {
            return Some((sig, SI_USER));
        }
    }
    None
}

/// Default actions: ignore, stop, or end the process by the signal itself. The native action is
/// the default (it mirrors the Linux one, or is reset here), so the process manager stops or ends
/// the process and the parent sees the signal as the cause.
pub fn default_action(sig: u64) {
    const IGNORED: [u64; 4] = [
        17, /* CHLD */
        18, /* CONT */
        23, /* URG */
        28, /* WINCH */
    ];
    if IGNORED.contains(&sig) {
        return;
    }
    if sig > 31 {
        // Real-time signals end the process; there is no native number to end it with.
        unsafe { libc::_exit(128 + sig as i32) }
    }
    unsafe {
        libc::signal(sig as i32, libc::SIG_DFL);
        libc::kill(libc::getpid(), sig as i32);
    }
    if (20..=22).contains(&sig) || sig == SIGSTOP {
        return;
    }
    // The end arrives asynchronously from the process manager.
    for _ in 0..200 {
        unsafe { libc::usleep(10_000) };
    }
    unsafe { libc::_exit(128 + sig as i32) }
}

/// Runs at the end of every crossing: puts a handler frame on the Linux stack for one pending
/// signal, if the mask allows one.
pub fn deliver(thread: &Thread, frame: &mut Frame) {
    loop {
        let Some((sig, code)) = take(thread) else {
            *thread.suspended_mask.lock().unwrap() = None;
            return;
        };
        let action = ACTIONS.lock().unwrap()[sig as usize];
        match action.handler {
            SIG_IGN => continue,
            SIG_DFL => {
                default_action(sig);
                continue;
            }
            _ => {}
        }
        let from = SENDER[sig as usize].load(Ordering::Relaxed);
        enter_handler(thread, frame, sig, &action, Info { code, pid: from, fault: None });
        return;
    }
}

struct Info {
    code: i32,
    pid: i32,
    /// (address, error code, trap number) of a fault.
    fault: Option<(u64, u64, u64)>,
}

fn enter_handler(thread: &Thread, frame: &mut Frame, sig: u64, action: &Action, info: Info) {
    if action.flags & SA_RESTORER == 0 {
        // x86-64 Linux requires a restorer; without one the return has nowhere to go.
        default_action(11);
        return;
    }
    if push_frame(thread, frame, sig, action, &info).is_err() {
        default_action(11);
        return;
    }
    let mut mask = thread.mask.load(Ordering::Relaxed) | action.mask;
    if action.flags & SA_NODEFER == 0 {
        mask |= bit(sig);
    }
    thread.mask.store(mask & !UNBLOCKABLE, Ordering::Relaxed);
    if action.flags & SA_RESETHAND != 0 {
        ACTIONS.lock().unwrap()[sig as usize] = Action::default();
        mirror(sig, SIG_DFL);
    }
}

/// Entered from src/entry.S when a native signal interrupted Linux code: relibc has run the
/// recorder; deliver what is now due.
pub extern "C" fn lr_interrupt(frame: *mut Frame, data: usize) {
    let frame = unsafe { &mut *frame };
    deliver(crate::thread::current(data), frame);
}

/// Entered from src/entry.S when Linux code faulted: the exception becomes a Linux signal.
pub extern "C" fn lr_fault(frame: *mut Frame, data: usize) {
    let frame = unsafe { &mut *frame };
    let thread = crate::thread::current(data);
    let ctl = crate::thread::sigcontrol(data);
    let (packed, addr) = unsafe { (*((ctl + 296) as *const u64), *((ctl + 304) as *const u64)) };
    // Delivery and further exceptions are allowed again (INHIBIT_DELIVERY | HANDLING_EXCEPTION).
    unsafe { (*((ctl + 272) as *const AtomicU64)).fetch_and(!3, Ordering::SeqCst) };
    let (kind, err) = (packed >> 32, packed & 0xffff_ffff);
    // Linux's x86 traps: si_code values from its siginfo.h.
    let (sig, code, si_addr) = match kind {
        0 => (8, 1 /* FPE_INTDIV */, frame.rip),
        1 | 3 => (5, 1 /* TRAP_BRKPT */, frame.rip),
        4 => (11, 0x80 /* SI_KERNEL */, 0),
        5 => (11, 0x80, 0),
        6 => (4, 2 /* ILL_ILLOPN */, frame.rip),
        7 => (8, 0x80, frame.rip),
        13 => (11, 0x80, 0),
        14 => (11, if err & 1 == 0 { 1 /* SEGV_MAPERR */ } else { 2 /* SEGV_ACCERR */ }, addr),
        16 | 19 => (8, 0x80, frame.rip),
        17 => (7, 1 /* BUS_ADRALN */, addr),
        _ => (11, 0x80, 0),
    };
    let action = ACTIONS.lock().unwrap()[sig as usize];
    let blocked = thread.mask.load(Ordering::Relaxed) & bit(sig) != 0;
    if action.handler == SIG_DFL || action.handler == SIG_IGN || blocked {
        // Linux forces the default action for a fault it cannot deliver.
        default_action(sig);
        unsafe { libc::_exit(128 + sig as i32) }
    }
    let trapno = kind;
    enter_handler(thread, frame, sig, &action, Info { code, pid: 0, fault: Some((si_addr, err, trapno)) });
}

fn push_frame(
    thread: &Thread,
    frame: &mut Frame,
    sig: u64,
    action: &Action,
    info: &Info,
) -> Result<(), ()> {
    let old_mask = thread
        .suspended_mask
        .lock()
        .unwrap()
        .take()
        .unwrap_or_else(|| thread.mask.load(Ordering::Relaxed));
    let xsize = crate::cpu::xsave_size() as u64;
    let (alt_sp, alt_flags, alt_size) = *thread.altstack.lock().unwrap();
    let on = on_altstack(thread, frame.rsp);
    let mut sp = if action.flags & SA_ONSTACK != 0 && alt_flags & 2 == 0 && !on {
        alt_sp + alt_size
    } else {
        frame.rsp - 128 // the red zone
    };
    sp -= xsize + 4;
    sp &= !63;
    let fp = sp;
    sp -= UC_SIZE + SIGINFO_SIZE;
    sp = (sp & !15) - 8;
    let base = sp;
    if !crate::mem::user_ok(base, fp + xsize + 4 - base) {
        return Err(());
    }
    let uc = base + 8;
    let siginfo = uc + UC_SIZE;
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, (fp - base) as usize);
        // The XSAVE image with Linux's software-reserved words.
        let xs = thread.xsave();
        core::ptr::copy_nonoverlapping(xs.as_ptr(), fp as *mut u8, xsize as usize);
        let sw = (fp + 464) as *mut u32;
        sw.write(FP_XSTATE_MAGIC1);
        sw.add(1).write(xsize as u32 + 4);
        ((fp + 472) as *mut u64).write(crate::cpu::xcr0());
        sw.add(4).write(xsize as u32);
        ((fp + xsize) as *mut u32).write(FP_XSTATE_MAGIC2);

        (base as *mut u64).write(action.restorer);
        let u = uc as *mut u64;
        u.write(UC_FP_XSTATE | UC_SIGCONTEXT_SS);
        u.add(1).write(0);
        // uc_stack
        u.add(2).write(alt_sp);
        (u.add(3) as *mut i32).write(if alt_flags & 2 != 0 {
            2
        } else if on {
            1
        } else {
            0
        });
        u.add(4).write(alt_size);
        // uc_mcontext: r8..r15 rdi rsi rbp rbx rdx rax rcx rsp rip eflags csgsfs err trapno
        // oldmask cr2 fpstate reserved[8]
        let f = *frame;
        let regs = [
            f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx,
            f.rdx, f.rax, f.rcx, f.rsp, f.rip, f.rflags,
        ];
        let m = u.add(5);
        for (i, r) in regs.iter().enumerate() {
            m.add(i).write(*r);
        }
        m.add(18).write(0x33 | 0x2b << 48);
        if let Some((addr, err, trapno)) = info.fault {
            m.add(19).write(err);
            m.add(20).write(trapno);
            m.add(22).write(addr);
        }
        m.add(21).write(old_mask);
        m.add(23).write(fp);
        // uc_sigmask
        u.add(37).write(old_mask);

        let si = siginfo as *mut i32;
        si.write(sig as i32);
        si.add(2).write(info.code);
        if let Some((addr, _, _)) = info.fault {
            (si.add(4) as *mut u64).write(addr);
        } else {
            si.add(4).write(info.pid);
            si.add(5).write(libc::getuid() as i32);
        }
    }
    frame.rsp = base;
    frame.rip = action.handler;
    frame.rdi = sig;
    frame.rsi = siginfo;
    frame.rdx = uc;
    frame.rax = 0;
    frame.rflags &= !(1 << 10 | 1 << 8); // DF, TF
    Ok(())
}

/// rt_sigreturn: the handler returned into its restorer, so RSP points at the ucontext.
pub fn sigreturn(thread: &Thread, frame: &mut Frame) -> Result<(), ()> {
    let uc = frame.rsp;
    if !user_ok(uc, UC_SIZE) {
        return Err(());
    }
    unsafe {
        let u = uc as *const u64;
        let m = u.add(5);
        let r = |i: usize| m.add(i).read();
        frame.r8 = r(0);
        frame.r9 = r(1);
        frame.r10 = r(2);
        frame.r11 = r(3);
        frame.r12 = r(4);
        frame.r13 = r(5);
        frame.r14 = r(6);
        frame.r15 = r(7);
        frame.rdi = r(8);
        frame.rsi = r(9);
        frame.rbp = r(10);
        frame.rbx = r(11);
        frame.rdx = r(12);
        frame.rax = r(13);
        frame.rcx = r(14);
        frame.rsp = r(15);
        frame.rip = r(16);
        // Arithmetic, direction, trap and alignment flags only, as Linux's FIX_EFLAGS.
        const USER_FLAGS: u64 = 0x0004_0DD5;
        frame.rflags = (frame.rflags & !USER_FLAGS) | (r(17) & USER_FLAGS);
        let fp = r(23);
        let xsize = crate::cpu::xsave_size() as u64;
        if fp != 0 && user_ok(fp, xsize) {
            let xs = thread.xsave();
            core::ptr::copy_nonoverlapping(fp as *const u8, xs.as_mut_ptr(), xsize as usize);
            sanitize_xsave(xs);
        }
        thread
            .mask
            .store(u.add(37).read() & !UNBLOCKABLE, Ordering::Relaxed);
        // uc_stack restores the alternate stack.
        let (sp, flags, size) = (
            u.add(2).read(),
            (u.add(3) as *const i32).read(),
            u.add(4).read(),
        );
        if !on_altstack(thread, frame.rsp) {
            *thread.altstack.lock().unwrap() = if flags & 2 != 0 {
                (0, 2, 0)
            } else {
                (sp, 0, size)
            };
        }
    }
    Ok(())
}

/// Makes an image the program wrote safe for XRSTOR, which faults on reserved bits.
fn sanitize_xsave(xs: &mut [u8]) {
    let mxcsr = u32::from_le_bytes(xs[24..28].try_into().unwrap()) & 0xffbf;
    xs[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    let bv = u64::from_le_bytes(xs[512..520].try_into().unwrap()) & crate::cpu::xcr0();
    xs[512..520].copy_from_slice(&bv.to_le_bytes());
    xs[520..576].fill(0);
}
