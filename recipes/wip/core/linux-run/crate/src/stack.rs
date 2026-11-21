//! The stack a Linux program starts with: argc, argv, the environment and the auxiliary vector.

use crate::{elf::Loaded, mem};

const STACK_SIZE: u64 = 8 << 20;

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_FLAGS: u64 = 8;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_PLATFORM: u64 = 15;
const AT_HWCAP: u64 = 16;
const AT_CLKTCK: u64 = 17;
const AT_SECURE: u64 = 23;
const AT_RANDOM: u64 = 25;
const AT_HWCAP2: u64 = 26;
const AT_EXECFN: u64 = 31;
const AT_MINSIGSTKSZ: u64 = 51;

/// Linux's HWCAP2_FSGSBASE: user code may use the FS and GS base instructions.
const HWCAP2_FSGSBASE: u64 = 2;

/// Maps an 8 MiB stack at a random place below the runtime's window and fills it. Returns the
/// stack pointer the program starts with. No vDSO is offered (no AT_SYSINFO_EHDR): the C
/// libraries and Go then make their clock calls as system calls, which the runtime answers.
pub fn build(
    loaded: &Loaded,
    execfn: &str,
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
) -> Result<u64, i64> {
    let base = mem::map_anywhere(0, STACK_SIZE, 3, false, -1, 0)?;
    mem::note(base, base + STACK_SIZE, 3);
    let top = base + STACK_SIZE;
    let mut sp = top;
    let mut put = |bytes: &[u8]| -> u64 {
        sp -= bytes.len() as u64 + 1;
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), sp as *mut u8, bytes.len());
            *((sp + bytes.len() as u64) as *mut u8) = 0;
        }
        sp
    };
    let execfn_at = put(execfn.as_bytes());
    let platform_at = put(b"x86_64");
    let argv_at: Vec<u64> = argv.iter().map(|a| put(a)).collect();
    let envp_at: Vec<u64> = envp.iter().map(|e| put(e)).collect();
    let mut random = [0u8; 16];
    crate::syscall::fill_random(&mut random);
    sp -= 16;
    let random_at = sp;
    unsafe { core::ptr::copy_nonoverlapping(random.as_ptr(), random_at as *mut u8, 16) };

    let (uid, euid, gid, egid) = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    let auxv: [(u64, u64); 20] = [
        (AT_PHDR, loaded.phdr),
        (AT_PHENT, loaded.phent),
        (AT_PHNUM, loaded.phnum),
        (AT_PAGESZ, mem::PAGE),
        (AT_BASE, 0),
        (AT_FLAGS, 0),
        (AT_ENTRY, loaded.entry),
        (AT_UID, uid as u64),
        (AT_EUID, euid as u64),
        (AT_GID, gid as u64),
        (AT_EGID, egid as u64),
        (AT_SECURE, 0),
        (AT_RANDOM, random_at),
        (AT_HWCAP, crate::cpu::hwcap()),
        (AT_HWCAP2, HWCAP2_FSGSBASE),
        (AT_CLKTCK, 100),
        (AT_PLATFORM, platform_at),
        (AT_EXECFN, execfn_at),
        (AT_MINSIGSTKSZ, crate::signal::frame_size()),
        (AT_NULL, 0),
    ];
    let words = 1 + argv_at.len() + 1 + envp_at.len() + 1 + 2 * auxv.len();
    sp &= !15;
    if words % 2 == 1 {
        sp -= 8;
    }
    sp -= 8 * words as u64;
    let mut w = sp as *mut u64;
    let mut push = |v: u64| unsafe {
        w.write(v);
        w = w.add(1);
    };
    push(argv_at.len() as u64);
    argv_at.iter().for_each(|&a| push(a));
    push(0);
    envp_at.iter().for_each(|&e| push(e));
    push(0);
    for (k, v) in auxv {
        push(k);
        push(v);
    }
    Ok(sp)
}
