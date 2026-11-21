//! Probes every route from a process to raw hardware that the security roadmap's S13 closes, as
//! the calling process: physical memory, DMA memory, address translation, port I/O through the
//! memory scheme and through the thread handle, and the schemes that reach devices (`irq`,
//! `serio`, `kernel.acpi`, `pci`, `acpi`), opened directly and through a namespace made with
//! mkns. Run it as root in a login shell.
//!
//! Each line reads `lockdown-probe: pass <check>: ...` when the route is closed to the caller,
//! and `lockdown-probe: FAIL <check>: ...` when it is open; an open route is used once, so the
//! line shows what it reached. Ordinary memory must keep working. The exit status is 0 only when
//! every line passes. `scripts/vm-test.py lockdown` runs it on a booted image.

use std::io;
use std::process::ExitCode;
use std::ptr;

use libredox::error::Error;
use libredox::{call, errno, flag, Fd};
use syscall::{CallFlags, ProcSchemeVerb};

const PAGE: usize = 4096;
/// A physical address inside the first megabyte, present on every PC.
const PHYS_ADDR: u64 = 0x1000;
/// Device schemes a process without the hardware right must not find, with a resource each.
const HIDDEN: &[(&str, &str, i32)] = &[
    ("irq", "/scheme/irq", flag::O_DIRECTORY),
    ("serio", "/scheme/serio/0", flag::O_RDONLY),
    ("kernel.acpi", "/scheme/kernel.acpi/msr", flag::O_RDONLY),
    ("pci", "/scheme/pci/access", flag::O_RDWR),
    ("acpi", "/scheme/acpi/tables", flag::O_DIRECTORY),
];

extern "C" {
    fn redox_cur_thrfd_v0() -> usize;
}

#[derive(Default)]
struct Report {
    passed: usize,
    total: usize,
}

impl Report {
    fn check(&mut self, name: &str, closed: bool, detail: String) {
        self.total += 1;
        if closed {
            self.passed += 1;
        }
        let verdict = if closed { "pass" } else { "FAIL" };
        println!("lockdown-probe: {verdict} {name}: {detail}");
    }

    /// An open that must fail: `closed` when it does.
    fn refused(
        &mut self,
        name: &str,
        result: libredox::error::Result<Fd>,
        used: impl Fn(&Fd) -> String,
    ) {
        match result {
            Err(err) => self.check(name, true, format!("refused: {}", text(err))),
            Ok(fd) => {
                let detail = format!("opened; {}", used(&fd));
                self.check(name, false, detail)
            }
        }
    }
}

fn text(err: Error) -> String {
    io::Error::from(err).to_string()
}

fn open(path: &str, flags: i32) -> libredox::error::Result<Fd> {
    Fd::open(path, flags | flag::O_CLOEXEC, 0)
}

/// Maps one page of physical memory through `fd` and shows its first bytes.
fn read_physical(fd: &Fd) -> String {
    let mapped = unsafe {
        call::mmap(call::MmapArgs {
            addr: ptr::null_mut(),
            length: PAGE,
            prot: flag::PROT_READ,
            flags: flag::MAP_SHARED,
            fd: fd.raw(),
            offset: PHYS_ADDR,
        })
    };
    match mapped {
        Err(err) => format!("mapping physical {PHYS_ADDR:#x} failed: {}", text(err)),
        Ok(base) => {
            let bytes = unsafe { std::slice::from_raw_parts(base as *const u8, 16) };
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let _ = unsafe { call::munmap(base, PAGE) };
            format!("physical {PHYS_ADDR:#x} reads {hex}")
        }
    }
}

/// Reads I/O port 0x80, the POST diagnostic port, which only works once I/O privilege is raised.
#[cfg(target_arch = "x86_64")]
fn read_port() -> String {
    let value: u8;
    unsafe {
        core::arch::asm!("in al, dx", out("al") value, in("dx") 0x80_u16, options(nomem, nostack, preserves_flags));
    }
    format!("port 0x80 reads {value:#04x}")
}

#[cfg(not(target_arch = "x86_64"))]
fn read_port() -> String {
    "no I/O ports on this architecture".to_owned()
}

/// Raises the calling thread's I/O privilege through a `port-io` handle, then uses it.
fn use_port_io(fd: &Fd) -> String {
    match fd.call_wo(&[], CallFlags::empty(), &[]) {
        Err(err) => format!("but raising I/O privilege failed: {}", text(err)),
        Ok(_) => read_port(),
    }
}

/// The route every driver used before S13: the Iopl verb on the caller's own thread handle.
fn iopl_verb(report: &mut Report) {
    let thread = match call::dup(unsafe { redox_cur_thrfd_v0() }, b"open_via_dup") {
        Err(err) => {
            return report.check(
                "iopl-verb",
                true,
                format!("no kernel thread handle: {}", text(err)),
            );
        }
        Ok(fd) => Fd::new(fd),
    };
    match thread.call_wo(&[], CallFlags::empty(), &[ProcSchemeVerb::Iopl as u64]) {
        Err(err) => report.check("iopl-verb", true, format!("refused: {}", text(err))),
        Ok(_) => report.check(
            "iopl-verb",
            false,
            format!("I/O privilege raised; {}", read_port()),
        ),
    }
}

/// Opens each hidden scheme's resource; only "no such device", the namespace's answer, passes.
fn hidden_schemes(report: &mut Report) {
    for &(name, path, flags) in HIDDEN {
        let check = format!("scheme-{name}");
        match open(path, flags) {
            Err(err) if err.errno() == errno::ENODEV => {
                report.check(&check, true, format!("not in the namespace: {}", text(err)))
            }
            Err(err) => report.check(
                &check,
                false,
                format!("in the namespace, refused by {path}: {}", text(err)),
            ),
            Ok(_) => report.check(&check, false, format!("opened {path}")),
        }
    }
}

/// Builds a namespace naming the hidden schemes and `memory`, then tries them through it.
fn mkns_escape(report: &mut Report) {
    let mut names: Vec<&str> = HIDDEN.iter().map(|&(name, _, _)| name).collect();
    names.push("memory");
    let slices: Vec<ioslice::IoSlice> = names
        .iter()
        .map(|name| ioslice::IoSlice::new(name.as_bytes()))
        .collect();
    let ns = match call::mkns(&slices) {
        Err(err) => {
            return report.check("mkns-escape", true, format!("mkns refused: {}", text(err)))
        }
        Ok(ns) => Fd::new(ns),
    };
    let mut reached = Vec::new();
    for &(name, _, _) in HIDDEN {
        if call::openat(
            ns.raw(),
            format!("/scheme/{name}"),
            flag::O_DIRECTORY | flag::O_CLOEXEC,
            0,
        )
        .is_ok()
        {
            reached.push(name.to_owned());
        }
    }
    if let Ok(memory) = call::openat(
        ns.raw(),
        "/scheme/memory",
        flag::O_DIRECTORY | flag::O_CLOEXEC,
        0,
    ) {
        let memory = Fd::new(memory);
        if memory
            .openat("physical", flag::O_RDONLY | flag::O_CLOEXEC, 0)
            .is_ok()
        {
            reached.push("memory/physical".to_owned());
        }
    }
    if reached.is_empty() {
        report.check(
            "mkns-escape",
            true,
            "the new namespace reaches no hidden scheme or physical memory".to_owned(),
        );
    } else {
        report.check(
            "mkns-escape",
            false,
            format!("the new namespace reaches {}", reached.join(", ")),
        );
    }
}

/// The control: ordinary memory from the same `memory` scheme still works.
fn ordinary_memory(report: &mut Report) {
    let result = open("/scheme/memory/zeroed", flag::O_RDWR).and_then(|fd| unsafe {
        call::mmap(call::MmapArgs {
            addr: ptr::null_mut(),
            length: PAGE,
            prot: flag::PROT_READ | flag::PROT_WRITE,
            flags: flag::MAP_PRIVATE,
            fd: fd.raw(),
            offset: 0,
        })
    });
    match result {
        Err(err) => report.check("ordinary-memory", false, format!("refused: {}", text(err))),
        Ok(base) => {
            let byte = base as *mut u8;
            let value = unsafe {
                byte.write_volatile(42);
                byte.read_volatile()
            };
            let _ = unsafe { call::munmap(base, PAGE) };
            report.check(
                "ordinary-memory",
                value == 42,
                format!("mapped a zeroed page and read back {value}"),
            )
        }
    }
}

fn main() -> ExitCode {
    let mut report = Report::default();
    match call::geteuid() {
        Ok(euid) => println!("lockdown-probe: running with effective uid {euid}"),
        Err(err) => println!("lockdown-probe: effective uid unknown: {}", text(err)),
    }

    report.refused(
        "physmem-root-shell",
        open("/scheme/memory/physical", flag::O_RDONLY),
        read_physical,
    );
    report.refused(
        "physmem-uncached",
        open("/scheme/memory/physical@uc", flag::O_RDONLY),
        read_physical,
    );
    report.refused(
        "dma-memory",
        open("/scheme/memory/zeroed@wb?phys_contiguous", flag::O_RDWR),
        |_| "physically contiguous memory for DMA".to_owned(),
    );
    report.refused(
        "translation",
        open("/scheme/memory/translation", flag::O_RDWR),
        |_| "virtual-to-physical translation".to_owned(),
    );
    match open("/scheme/memory/scheme-root", flag::O_RDONLY) {
        Err(err) => report.check(
            "memory-root",
            true,
            format!("no memory root: {}", text(err)),
        ),
        Ok(root) => report.refused(
            "memory-root",
            root.openat("physical", flag::O_RDONLY | flag::O_CLOEXEC, 0),
            read_physical,
        ),
    }
    report.refused(
        "port-io",
        open("/scheme/memory/port-io", flag::O_RDONLY),
        use_port_io,
    );
    iopl_verb(&mut report);
    hidden_schemes(&mut report);
    mkns_escape(&mut report);
    ordinary_memory(&mut report);

    let locked = report.passed == report.total;
    println!(
        "lockdown-probe: {} of {} checks pass; {}",
        report.passed,
        report.total,
        if locked {
            "locked down"
        } else {
            "raw hardware reachable"
        }
    );
    if locked {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
