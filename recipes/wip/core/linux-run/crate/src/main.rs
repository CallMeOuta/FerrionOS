//! linux-run: starts a static Linux x86-64 program on FerrionOS (plans/linux-compat.md, LC1).
//!
//!     linux-run [--argv0 NAME] [--] PROGRAM [ARGUMENT...]
//!
//! The same binary is the runtime: it stays mapped in the process, above the Linux program, and
//! answers the program's system calls, which the kernel hands back to it through the
//! foreign-call hook (kernel patch ferrion-foreign-call.patch). The program runs in the caller's
//! namespace with the caller's descriptors and rights; linux-run adds none.
//!
//! Exit status: the program's, or 126 when it cannot be started (not a static x86-64 Linux
//! program, no FSGSBASE or XSAVE, no hook in the kernel) and 127 when it cannot be read.

mod cpu;
mod elf;
mod errno;
mod mem;
mod signal;
mod stack;
mod syscall;
mod thread;

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::Ordering;

/// Linux code lives below this address; the runtime, relibc and their memory above it.
pub const LINUX_TOP: usize = 0x7000_0000_0000;
/// End of user memory on x86_64 (the kernel's USER_END_OFFSET).
pub const NATIVE_END: usize = 1 << 47;

pub fn fatal(why: &str) -> ! {
    eprintln!("linux-run: {why}");
    unsafe { libc::_exit(126) }
}

fn usage() -> ! {
    eprintln!("usage: linux-run [--argv0 NAME] [--] PROGRAM [ARGUMENT...]");
    unsafe { libc::_exit(2) }
}

fn main() {
    let mut args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|a| a.as_bytes().to_vec())
        .collect();
    let mut argv0 = None;
    loop {
        match args.first().map(|a| a.as_slice()) {
            Some(b"--argv0") if args.len() >= 2 => {
                argv0 = Some(args[1].clone());
                args.drain(..2);
            }
            Some(b"--") => {
                args.remove(0);
                break;
            }
            Some(a) if a.starts_with(b"-") => usage(),
            Some(_) => break,
            None => usage(),
        }
    }
    if args.is_empty() {
        usage();
    }
    let program = args.remove(0);
    let shown = String::from_utf8_lossy(&program).into_owned();
    syscall::TRACE.store(
        std::env::var_os("LINUX_RUN_TRACE").is_some(),
        Ordering::Relaxed,
    );

    // Mappings whose address the kernel picks (relibc's heap, thread stacks, the crossing pages)
    // start at the native window; Linux mappings are always placed with MAP_FIXED_NOREPLACE,
    // which this floor does not limit.
    if let Err(e) = thread::set_mmap_min(LINUX_TOP) {
        fatal(&format!(
            "cannot raise mmap-min-addr to the native window: errno {e}"
        ));
    }
    if let Err(why) = cpu::check() {
        fatal(&format!("cannot run Linux programs on this machine: {why}"));
    }
    // The runtime's code, heap, stack and thread block must all lie in the native window, or a
    // Linux mapping could land on them.
    let heap = Box::new(0u8);
    let on_stack = 0u8;
    for (what, at) in [
        ("code", main as *const () as usize),
        ("heap", &*heap as *const u8 as usize),
        ("stack", &on_stack as *const u8 as usize),
        ("thread block", cpu::rdfsbase() as usize),
    ] {
        if !(LINUX_TOP..NATIVE_END).contains(&at) {
            fatal(&format!(
                "the runtime's {what} lies at {at:#x}, below the native window"
            ));
        }
    }
    drop(heap);

    let file = match std::fs::read(std::ffi::OsStr::from_bytes(&program)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("linux-run: {shown}: {e}");
            unsafe { libc::_exit(127) }
        }
    };
    if let Err((_, why)) = elf::check(&file) {
        eprintln!("linux-run: {shown}: cannot execute (ENOEXEC): {why}");
        unsafe { libc::_exit(126) }
    }
    let exe = std::fs::canonicalize(std::ffi::OsStr::from_bytes(&program))
        .map(|p| p.as_os_str().as_bytes().to_vec())
        .unwrap_or_else(|_| program.clone());
    syscall::EXE.set(CString::new(exe).unwrap()).ok();

    syscall::refresh_pid();
    signal::block_native();
    let main_thread = thread::new_thread(thread::next_tid(), 0);

    let loaded = match elf::load(&file) {
        Ok(l) => l,
        Err((_, why)) => fatal(&format!("{shown}: {why}")),
    };
    drop(file);
    mem::init_brk(loaded.end);

    let mut argv = vec![argv0.unwrap_or_else(|| program.clone())];
    argv.extend(args);
    let envp: Vec<Vec<u8>> = std::env::vars_os()
        .filter(|(k, _)| k != "LD_LIBRARY_PATH" && k != "LD_PRELOAD" && k != "LINUX_RUN_TRACE")
        .map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes()].concat())
        .collect();
    let sp = match stack::build(&loaded, &shown, &argv, &envp) {
        Ok(sp) => sp,
        Err(_) => fatal("no room for the program's stack"),
    };
    let frame = thread::Frame {
        rip: loaded.entry,
        rsp: sp,
        rflags: 0x202,
        ..Default::default()
    };
    thread::enter(main_thread, &frame)
}
