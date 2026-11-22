//! The same binary is the runtime: it stays mapped in the process, above the Linux program, and
//! answers the program's system calls, which the kernel hands back to it through the
//! foreign-call hook (kernel patch ferrion-foreign-call.patch). A program that names a dynamic
//! linker (PT_INTERP) gets it loaded beside it.
//!
//! Exit status: the program's, or 126 when it cannot be started (not an x86-64 Linux program, no
//! FSGSBASE or XSAVE, no hook in the kernel, no session) and 127 when it cannot be read.

mod cpu;
mod elf;
mod errno;
mod files;
mod locks;
mod mem;
mod net;
mod session;
mod signal;
mod spawn;
mod stack;
mod syscall;
mod thread;
mod tty;
mod wait;

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::Ordering;

/// Linux code lives below this address; the runtime, relibc and their memory above it.
pub const LINUX_TOP: usize = 0x7000_0000_0000;
/// End of user memory on x86_64 (the kernel's USER_END_OFFSET).
pub const NATIVE_END: usize = 1 << 47;

// relibc functions and values the libc crate does not declare for this target.
pub mod ffi {
    unsafe extern "C" {
        pub fn sigsuspend(mask: *const libc::sigset_t) -> i32;
        pub fn getsid(pid: libc::pid_t) -> libc::pid_t;
        pub fn fdatasync(fd: libc::c_int) -> libc::c_int;
        pub fn sync();
    }
    /// relibc's src/header/unistd/sysconf/constants.rs.
    pub const SC_PHYS_PAGES: libc::c_int = 85;
    pub const SC_AVPHYS_PAGES: libc::c_int = 86;
}

pub fn fatal(why: &str) -> ! {
    eprintln!("linux-run: {why}");
    unsafe { libc::_exit(126) }
}

fn usage() -> ! {
    eprintln!("usage: linux-run [--native | --network] [--argv0 NAME] [--] PROGRAM [ARGUMENT...]");
    unsafe { libc::_exit(2) }
}

/// Reads a program file named by a Linux path.
fn read_linux(path: &[u8]) -> std::io::Result<Vec<u8>> {
    let native = session::linux_path_at(CString::new(path).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?, true);
    std::fs::read(std::ffi::OsStr::from_bytes(native.as_bytes()))
}

fn main() {
    let mut args: Vec<Vec<u8>> = std::env::args_os()
        .skip(1)
        .map(|a| a.as_bytes().to_vec())
        .collect();
    let mut argv0 = None;
    let mut native = false;
    let mut network = false;
    let mut sigpipe_ignored = false;
    loop {
        match args.first().map(|a| a.as_slice()) {
            Some(b"--argv0") if args.len() >= 2 => {
                argv0 = Some(args[1].clone());
                args.drain(..2);
            }
            Some(b"--sigpipe-ignored") => {
                sigpipe_ignored = true;
                args.remove(0);
            }
            Some(b"--native") => {
                native = true;
                args.remove(0);
            }
            Some(b"--network") => {
                network = true;
                args.remove(0);
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
    if args.is_empty() || (native && network) {
        usage();
    }
    let mode = session::init_mode(native);
    let program = args.remove(0);
    let shown = String::from_utf8_lossy(&program).into_owned();
    let trace = std::env::var_os("LINUX_RUN_TRACE");
    syscall::TRACE_ALL.store(trace.as_deref() == Some("all".as_ref()), Ordering::Relaxed);
    syscall::TRACE.store(trace.is_some(), Ordering::Relaxed);

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

    if mode == session::Mode::Launcher {
        if let Err(why) = session::start(network) {
            fatal(&format!("cannot start a Linux session: {why}"));
        }
    }

    let file = match read_linux(&program) {
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
    let native = session::linux_path_at(CString::new(program.clone()).unwrap(), true);
    let exe = std::fs::canonicalize(std::ffi::OsStr::from_bytes(native.as_bytes()))
        .map(|p| session::linux_view(p.as_os_str().as_bytes()))
        .unwrap_or_else(|_| program.clone());
    syscall::EXE.set(CString::new(exe).unwrap()).ok();

    syscall::refresh_pid();
    let mask = signal::init(sigpipe_ignored);
    let main_thread = thread::new_thread(thread::next_tid(), mask);

    let loaded = match elf::load(&file) {
        Ok(l) => l,
        Err((_, why)) => fatal(&format!("{shown}: {why}")),
    };
    // A dynamic program starts in its dynamic linker, loaded at a random place of its own, which
    // finds the program through AT_PHDR and AT_ENTRY and maps the libraries itself.
    let (entry, interp_base) = match elf::interp(&file) {
        None => (loaded.entry, 0),
        Some(name) => {
            let shown_interp = String::from_utf8_lossy(&name).into_owned();
            let ld = match read_linux(&name) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("linux-run: {shown}: its dynamic linker {shown_interp}: {e}");
                    unsafe { libc::_exit(127) }
                }
            };
            if elf::check(&ld).is_err() || elf::interp(&ld).is_some() || u16::from_le_bytes([ld[16], ld[17]]) != 3 {
                fatal(&format!("{shown}: {shown_interp} is not a dynamic linker"));
            }
            match elf::load(&ld) {
                Ok(l) => (l.entry, l.base),
                Err((_, why)) => fatal(&format!("{shown_interp}: {why}")),
            }
        }
    };
    drop(file);
    mem::init_brk(loaded.end);

    let mut argv = vec![argv0.unwrap_or_else(|| program.clone())];
    argv.extend(args);
    let envp = environment(mode == session::Mode::Launcher);
    let sp = match stack::build(&loaded, interp_base, &shown, &argv, &envp) {
        Ok(sp) => sp,
        Err(_) => fatal("no room for the program's stack"),
    };
    let frame = thread::Frame {
        rip: entry,
        rsp: sp,
        rflags: 0x202,
        ..Default::default()
    };
    thread::enter(main_thread, &frame)
}

/// The program's environment: the caller's, without the variables that would point the dynamic
/// linker at another library tree. The launcher of a session also leaves out the display (X11 is
/// never offered: plans/linux-compat.md, "X11 client libraries in the Linux root") and gives the
/// session's home, shell and search path; a runtime that `execve` started keeps what the program
/// passed.
fn environment(launcher: bool) -> Vec<Vec<u8>> {
    const DROPPED: &[&str] = &["LD_LIBRARY_PATH", "LD_PRELOAD", "LINUX_RUN_TRACE"];
    const SESSION_DROPPED: &[&str] = &["DISPLAY", "XAUTHORITY", "WAYLAND_DISPLAY", "HOME", "SHELL", "PATH"];
    let mut envp: Vec<Vec<u8>> = std::env::vars_os()
        .filter(|(k, _)| {
            let k = k.as_bytes();
            !DROPPED.iter().any(|d| d.as_bytes() == k)
                && !(launcher && SESSION_DROPPED.iter().any(|d| d.as_bytes() == k))
        })
        .map(|(k, v)| [k.as_bytes(), b"=", v.as_bytes()].concat())
        .collect();
    if launcher {
        let uid = unsafe { libc::getuid() }.to_string();
        let home = std::fs::read(format!("{}/etc/passwd", session::MEM))
            .ok()
            .and_then(|p| {
                String::from_utf8_lossy(&p).lines().find_map(|l| {
                    let f: Vec<&str> = l.split(':').collect();
                    (f.len() == 7 && f[2] == uid).then(|| f[5].to_string())
                })
            })
            .unwrap_or_else(|| "/".to_string());
        envp.push(format!("HOME={home}").into_bytes());
        envp.push(b"SHELL=/bin/sh".to_vec());
        envp.push(b"PATH=/usr/local/bin:/usr/bin:/bin".to_vec());
    }
    envp
}
