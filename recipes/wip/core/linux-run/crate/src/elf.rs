//! Loading a Linux ELF64 x86-64 program, and the dynamic linker it names.

use crate::{LINUX_TOP, errno::*, mem};

pub struct Loaded {
    pub entry: u64,
    pub phdr: u64,
    pub phent: u64,
    pub phnum: u64,
    /// End of the highest segment, where the break starts.
    pub end: u64,
    /// Where the image was placed: its link address subtracted (0 for ET_EXEC).
    pub base: u64,
}

fn u16_at(b: &[u8], o: usize) -> u64 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u64
}
fn u32_at(b: &[u8], o: usize) -> u64 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as u64
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

const PT_LOAD: u64 = 1;
const PT_INTERP: u64 = 3;
const PT_PHDR: u64 = 6;

/// Checks that `file` is a program linux-run starts. Returns (errno, reason) otherwise.
pub fn check(file: &[u8]) -> Result<(), (i64, String)> {
    let no = |why: &str| Err((ENOEXEC, why.to_string()));
    if file.len() < 64 || &file[..4] != b"\x7fELF" {
        if file.starts_with(b"#!") {
            return no("a script: linux-run starts its interpreter, which execve finds itself");
        }
        return no("not an ELF file");
    }
    if file[4] != 2 {
        return no("not a 64-bit ELF file; the Linux layer runs no 32-bit programs");
    }
    if file[5] != 1 || u16_at(file, 18) != 62 {
        return no("not a little-endian x86-64 ELF file");
    }
    let ty = u16_at(file, 16);
    if ty != 2 && ty != 3 {
        return no("neither an executable nor a position-independent executable");
    }
    let (phoff, phent, phnum) = (u64_at(file, 32), u16_at(file, 54), u16_at(file, 56));
    if phent != 56
        || phnum == 0
        || phoff
            .checked_add(phent * phnum)
            .is_none_or(|e| e > file.len() as u64)
    {
        return no("bad program headers");
    }
    let mut loads = 0;
    for i in 0..phnum {
        let ph = &file[(phoff + i * 56) as usize..][..56];
        let ptype = u32_at(ph, 0);
        if ptype == PT_INTERP {
            if interp(file).is_none() {
                return no("a bad interpreter name");
            }
            continue;
        }
        if ptype != PT_LOAD {
            continue;
        }
        loads += 1;
        let (off, vaddr, filesz, memsz) = (
            u64_at(ph, 8),
            u64_at(ph, 16),
            u64_at(ph, 32),
            u64_at(ph, 40),
        );
        if filesz > memsz
            || off
                .checked_add(filesz)
                .is_none_or(|e| e > file.len() as u64)
            || vaddr
                .checked_add(memsz)
                .is_none_or(|e| e > LINUX_TOP as u64)
            || off % mem::PAGE != vaddr % mem::PAGE
        {
            return no("a segment lies outside the file or the Linux address space");
        }
    }
    if loads == 0 {
        return no("no loadable segment");
    }
    Ok(())
}

/// The dynamic linker a program names (PT_INTERP), an absolute path, if it names one.
pub fn interp(file: &[u8]) -> Option<Vec<u8>> {
    let (phoff, phnum) = (u64_at(file, 32), u16_at(file, 56));
    for i in 0..phnum {
        let ph = &file[(phoff + i * 56) as usize..][..56];
        if u32_at(ph, 0) != PT_INTERP {
            continue;
        }
        let (off, size) = (u64_at(ph, 8) as usize, u64_at(ph, 32) as usize);
        let name = file.get(off..off.checked_add(size)?)?;
        let name = name.strip_suffix(b"\0")?;
        return (name.starts_with(b"/") && !name.contains(&0)).then(|| name.to_vec());
    }
    None
}

pub fn load(file: &[u8]) -> Result<Loaded, (i64, String)> {
    check(file)?;
    let ty = u16_at(file, 16);
    let (phoff, phnum) = (u64_at(file, 32), u16_at(file, 56));
    let phs: Vec<&[u8]> = (0..phnum)
        .map(|i| &file[(phoff + i * 56) as usize..][..56])
        .collect();
    let loads: Vec<&&[u8]> = phs.iter().filter(|ph| u32_at(ph, 0) == PT_LOAD).collect();
    let lo = loads
        .iter()
        .map(|ph| u64_at(ph, 16) & !(mem::PAGE - 1))
        .min()
        .unwrap();
    let hi = loads
        .iter()
        .map(|ph| (u64_at(ph, 16) + u64_at(ph, 40) + mem::PAGE - 1) & !(mem::PAGE - 1))
        .max()
        .unwrap();
    let span = hi - lo;

    // ET_EXEC loads where it was linked; ET_DYN at a random base. Writable first, for copying.
    let at = if ty == 2 {
        mem::mmap(
            lo,
            span,
            3,
            0x22 | 0x100000, /* PRIVATE|ANONYMOUS|FIXED_NOREPLACE */
            -1,
            0,
        )
    } else {
        mem::mmap(0, span, 3, 0x22 /* PRIVATE|ANONYMOUS */, -1, 0)
    };
    if at < 0 {
        return Err((-at, "no room for the program's segments".into()));
    }
    let base = at as u64 - lo;

    for ph in &loads {
        let (off, vaddr, filesz) = (u64_at(ph, 8), u64_at(ph, 16), u64_at(ph, 32));
        unsafe {
            core::ptr::copy_nonoverlapping(
                file.as_ptr().add(off as usize),
                (base + vaddr) as *mut u8,
                filesz as usize,
            );
        }
    }
    // Page protections: the union of the segments on each page, PROT_NONE in gaps.
    let mut page = lo;
    while page < hi {
        let mut prot = 0;
        for ph in &loads {
            let (vaddr, memsz, flags) = (u64_at(ph, 16), u64_at(ph, 40), u32_at(ph, 4));
            if vaddr < page + mem::PAGE && vaddr + memsz > page {
                // ELF PF_X=1, PF_W=2, PF_R=4 to Linux PROT_READ=1, PROT_WRITE=2, PROT_EXEC=4.
                prot |= (flags & 4 != 0) as u64
                    | ((flags & 2 != 0) as u64) << 1
                    | ((flags & 1 != 0) as u64) << 2;
            }
        }
        let r = mem::mprotect(base + page, mem::PAGE, prot);
        if r < 0 {
            return Err((-r, "could not protect a segment".into()));
        }
        page += mem::PAGE;
    }

    let phdr = phs
        .iter()
        .find(|ph| u32_at(ph, 0) == PT_PHDR)
        .map(|ph| u64_at(ph, 16))
        .or_else(|| {
            loads
                .iter()
                .find(|ph| u64_at(ph, 8) == 0)
                .map(|ph| u64_at(ph, 16) + phoff)
        })
        .unwrap_or(0);
    Ok(Loaded {
        entry: base + u64_at(file, 24),
        phdr: if phdr == 0 { 0 } else { base + phdr },
        phent: 56,
        phnum,
        end: base + hi,
        base,
    })
}
