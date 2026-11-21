//! Linux signals. The runtime keeps the Linux actions and masks itself; every native signal stays
//! blocked in a Linux process, because relibc's signal entry writes through FS, which belongs to
//! the Linux C library while Linux code runs. Signals reach a thread when it crosses into the
//! runtime: those sent with kill to this process or with tgkill to the thread, and those another
//! process sent, which wait natively until a crossing takes them. A handler gets Linux's frame
//! (siginfo, ucontext with every register, the XSAVE image) and rt_sigreturn resumes from it.
//!
//! What LC1 does not do (plans/linux-compat.md, Signals): a signal reaches a thread only at its
//! next system call, so a thread spinning in Linux code without calls never sees it, and a
//! thread blocked in a call is not interrupted; a fault in Linux code ends the process instead of
//! becoming SIGSEGV with a frame.

use core::sync::atomic::{AtomicU64, Ordering};
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

fn bit(sig: u64) -> u64 {
    1 << (sig - 1)
}

/// Blocks every native signal on the calling thread; threads created later inherit the mask.
pub fn block_native() {
    unsafe {
        let mut all: libc::sigset_t = core::mem::zeroed();
        libc::sigfillset(&mut all);
        libc::sigprocmask(libc::SIG_SETMASK, &all, core::ptr::null_mut());
    }
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

/// kill and tkill/tgkill aimed at this process.
pub fn send_process(sig: u64) {
    PROCESS_PENDING.fetch_or(bit(sig), Ordering::SeqCst);
}
pub fn send_thread(thread: &Thread, sig: u64) {
    thread.pending.fetch_or(bit(sig), Ordering::SeqCst);
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
    // Signals other processes sent wait natively, blocked.
    unsafe {
        let mut pending: libc::sigset_t = core::mem::zeroed();
        libc::sigpending(&mut pending);
        for sig in 1..32u64 {
            if mask & bit(sig) == 0 && libc::sigismember(&pending, sig as i32) == 1 {
                let mut one: libc::sigset_t = core::mem::zeroed();
                libc::sigaddset(&mut one, sig as i32);
                let zero = libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                };
                if libc::sigtimedwait(&one, core::ptr::null_mut(), &zero) == sig as i32 {
                    return Some((sig, SI_USER));
                }
            }
        }
    }
    None
}

/// Default actions: ignore, or end the process by the signal itself.
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
    if (20..=22).contains(&sig) || sig == SIGSTOP {
        // Job-control stops arrive with LC2's process groups.
        return;
    }
    if sig > 31 {
        // Real-time signals end the process; there is no native number to end it with.
        unsafe { libc::_exit(128 + sig as i32) }
    }
    unsafe {
        libc::signal(sig as i32, libc::SIG_DFL);
        let mut one: libc::sigset_t = core::mem::zeroed();
        libc::sigaddset(&mut one, sig as i32);
        libc::kill(libc::getpid(), sig as i32);
        libc::sigprocmask(libc::SIG_UNBLOCK, &one, core::ptr::null_mut());
        libc::_exit(128 + sig as i32);
    }
}

/// Runs at the end of every crossing: puts a handler frame on the Linux stack for one pending
/// signal, if the mask allows one.
pub fn deliver(thread: &Thread, frame: &mut Frame) {
    loop {
        let Some((sig, code)) = take(thread) else {
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
        if action.flags & SA_RESTORER == 0 {
            // x86-64 Linux requires a restorer; without one the return has nowhere to go.
            default_action(11);
            return;
        }
        if push_frame(thread, frame, sig, code, &action).is_err() {
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
        }
        return;
    }
}

fn push_frame(
    thread: &Thread,
    frame: &mut Frame,
    sig: u64,
    code: i32,
    action: &Action,
) -> Result<(), ()> {
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
    let info = uc + UC_SIZE;
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
        m.add(21).write(thread.mask.load(Ordering::Relaxed));
        m.add(23).write(fp);
        // uc_sigmask
        u.add(37).write(thread.mask.load(Ordering::Relaxed));

        let si = info as *mut i32;
        si.write(sig as i32);
        si.add(2).write(code);
        si.add(4).write(crate::syscall::pid());
        si.add(5).write(libc::getuid() as i32);
    }
    frame.rsp = base;
    frame.rip = action.handler;
    frame.rdi = sig;
    frame.rsi = info;
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
