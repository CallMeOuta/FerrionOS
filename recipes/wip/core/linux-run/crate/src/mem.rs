//! Linux memory: every Linux mapping lies below LINUX_TOP, at an address the runtime picks at
//! random when the program leaves the choice to it, and the runtime keeps the protection of each
//! page range so that `madvise(MADV_DONTNEED)` can hand back zero pages.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{LINUX_TOP, errno::*};

pub const PAGE: u64 = 4096;
/// relibc's MAP_FIXED_NOREPLACE (src/header/sys_mman/redox.rs); the libc crate lacks it.
const MAP_FIXED_NOREPLACE: i32 = 0x000C;
/// Lowest address the runtime picks by itself, as Linux's default mmap_min_addr.
const LOW: u64 = 0x10000;

/// A Linux pointer to `len` bytes lies in Linux memory.
pub fn user_ok(p: u64, len: u64) -> bool {
    p != 0
        && p.checked_add(len)
            .is_some_and(|end| end <= LINUX_TOP as u64)
}

/// Linux PROT_* to relibc's.
pub fn native_prot(prot: u64) -> i32 {
    let mut p = 0;
    if prot & 1 != 0 {
        p |= libc::PROT_READ
    }
    if prot & 2 != 0 {
        p |= libc::PROT_WRITE
    }
    if prot & 4 != 0 {
        p |= libc::PROT_EXEC
    }
    p
}

pub fn random_u64() -> u64 {
    let mut b = [0u8; 8];
    crate::syscall::fill_random(&mut b);
    u64::from_ne_bytes(b)
}

struct Mem {
    /// start -> (end, Linux prot) for every Linux mapping.
    regions: BTreeMap<u64, (u64, u64)>,
    brk_base: u64,
    brk_cur: u64,
    brk_mapped: u64,
}

static MEM: Mutex<Mem> = Mutex::new(Mem {
    regions: BTreeMap::new(),
    brk_base: 0,
    brk_cur: 0,
    brk_mapped: 0,
});

impl Mem {
    /// Sets the protection of [start, end), or forgets it with None.
    fn set(&mut self, start: u64, end: u64, prot: Option<u64>) {
        let overlapping: Vec<(u64, (u64, u64))> = self
            .regions
            .range(..end)
            .filter(|(_, (e, _))| *e > start)
            .map(|(s, v)| (*s, *v))
            .collect();
        for (s, (e, p)) in overlapping {
            self.regions.remove(&s);
            if s < start {
                self.regions.insert(s, (start, p));
            }
            if e > end {
                self.regions.insert(end, (e, p));
            }
        }
        if let Some(p) = prot {
            self.regions.insert(start, (end, p));
        }
    }
    fn prot_at(&self, addr: u64) -> Option<u64> {
        self.regions
            .range(..=addr)
            .next_back()
            .filter(|(_, (e, _))| *e > addr)
            .map(|(_, (_, p))| *p)
    }
}

fn mmap_native(addr: u64, len: u64, prot: i32, flags: i32, fd: i32, off: u64) -> Result<u64, i64> {
    let p = unsafe {
        libc::mmap(
            addr as *mut _,
            len as usize,
            prot,
            flags,
            fd,
            off as libc::off_t,
        )
    };
    if p == libc::MAP_FAILED {
        Err(last_errno())
    } else {
        Ok(p as u64)
    }
}

/// Maps anonymous or file memory at exactly `addr` without replacing anything.
fn map_at(addr: u64, len: u64, prot: u64, shared: bool, fd: i32, off: u64) -> Result<u64, i64> {
    let kind = if fd < 0 { libc::MAP_ANON } else { 0 };
    let share = if shared {
        libc::MAP_SHARED
    } else {
        libc::MAP_PRIVATE
    };
    mmap_native(
        addr,
        len,
        native_prot(prot),
        share | kind | MAP_FIXED_NOREPLACE,
        fd,
        off,
    )
}

/// Picks a free place for `len` bytes below LINUX_TOP: the hint first, then random addresses.
pub fn map_anywhere(
    hint: u64,
    len: u64,
    prot: u64,
    shared: bool,
    fd: i32,
    off: u64,
) -> Result<u64, i64> {
    if hint >= LOW && user_ok(hint, len) && hint % PAGE == 0 {
        if let Ok(a) = map_at(hint, len, prot, shared, fd, off) {
            return Ok(a);
        }
    }
    // Random pages in [LOW, LINUX_TOP - len), 46 bits of address space less the runtime's window.
    let span = (LINUX_TOP as u64 - LOW).checked_sub(len).ok_or(ENOMEM)? / PAGE;
    for _ in 0..64 {
        let addr = LOW + (random_u64() % span) * PAGE;
        match map_at(addr, len, prot, shared, fd, off) {
            Ok(a) => return Ok(a),
            Err(e) if e == EEXIST => continue,
            Err(e) => return Err(e),
        }
    }
    Err(ENOMEM)
}

/// Records a mapping the loader made.
pub fn note(start: u64, end: u64, prot: u64) {
    MEM.lock().unwrap().set(start, end, Some(prot));
}

pub fn mmap(addr: u64, len: u64, prot: u64, flags: u64, fd: i64, off: u64) -> i64 {
    const MAP_SHARED: u64 = 1;
    const MAP_PRIVATE: u64 = 2;
    const MAP_FIXED: u64 = 0x10;
    const MAP_ANONYMOUS: u64 = 0x20;
    const MAP_32BIT: u64 = 0x40;
    const MAP_POPULATE: u64 = 0x8000;
    const MAP_FIXED_NOREPLACE_L: u64 = 0x100000;
    const MAP_HUGETLB: u64 = 0x40000;
    const MAP_DROPPABLE: u64 = 0x08;
    if len == 0 || off % PAGE != 0 {
        return -EINVAL;
    }
    let len = match len.checked_add(PAGE - 1) {
        Some(l) => l & !(PAGE - 1),
        None => return -ENOMEM,
    };
    let shared = match flags & 3 {
        MAP_SHARED => true,
        MAP_PRIVATE => false,
        _ => return -EINVAL,
    };
    if flags & (MAP_HUGETLB | MAP_DROPPABLE) != 0 {
        return if flags & MAP_HUGETLB != 0 {
            -ENOMEM
        } else {
            -EINVAL
        };
    }
    let fd = if flags & MAP_ANONYMOUS != 0 {
        -1
    } else {
        fd as i32
    };
    let result = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE_L) != 0 {
        if addr % PAGE != 0 {
            return -EINVAL;
        }
        if !user_ok(addr, len) {
            // The runtime's window and the kernel's half are not Linux's to map.
            return -ENOMEM;
        }
        if flags & MAP_FIXED_NOREPLACE_L != 0 {
            map_at(addr, len, prot, shared, fd, off)
        } else {
            let kind = if fd < 0 { libc::MAP_ANON } else { 0 };
            let share = if shared {
                libc::MAP_SHARED
            } else {
                libc::MAP_PRIVATE
            };
            mmap_native(
                addr,
                len,
                native_prot(prot),
                share | kind | libc::MAP_FIXED,
                fd,
                off,
            )
        }
    } else if flags & MAP_32BIT != 0 {
        map_anywhere(addr, len, prot, shared, fd, off).and_then(|a| {
            if a + len <= 1 << 31 {
                Ok(a)
            } else {
                unsafe { libc::munmap(a as *mut _, len as usize) };
                Err(ENOMEM)
            }
        })
    } else {
        map_anywhere(addr, len, prot, shared, fd, off)
    };
    match result {
        Ok(a) => {
            MEM.lock().unwrap().set(a, a + len, Some(prot));
            if flags & MAP_POPULATE != 0 && prot & 1 != 0 {
                for p in (a..a + len).step_by(PAGE as usize) {
                    unsafe { core::ptr::read_volatile(p as *const u8) };
                }
            }
            a as i64
        }
        Err(e) => -e,
    }
}

pub fn munmap(addr: u64, len: u64) -> i64 {
    if addr % PAGE != 0 || len == 0 {
        return -EINVAL;
    }
    let len = (len + PAGE - 1) & !(PAGE - 1);
    if !user_ok(addr, len) {
        return -EINVAL;
    }
    if unsafe { libc::munmap(addr as *mut _, len as usize) } != 0 {
        return -last_errno();
    }
    MEM.lock().unwrap().set(addr, addr + len, None);
    0
}

pub fn mprotect(addr: u64, len: u64, prot: u64) -> i64 {
    if addr % PAGE != 0 {
        return -EINVAL;
    }
    let len = (len + PAGE - 1) & !(PAGE - 1);
    if len == 0 {
        return 0;
    }
    if !user_ok(addr, len) {
        return -ENOMEM;
    }
    if unsafe { libc::mprotect(addr as *mut _, len as usize, native_prot(prot)) } != 0 {
        return -last_errno();
    }
    MEM.lock().unwrap().set(addr, addr + len, Some(prot));
    0
}

pub fn madvise(addr: u64, len: u64, advice: u64) -> i64 {
    const MADV_DONTNEED: u64 = 4;
    const MADV_FREE: u64 = 8;
    if addr % PAGE != 0 {
        return -EINVAL;
    }
    let len = (len + PAGE - 1) & !(PAGE - 1);
    if len != 0 && !user_ok(addr, len) {
        return -ENOMEM;
    }
    if advice != MADV_DONTNEED && advice != MADV_FREE {
        // Hints (huge pages, readahead, fork behavior): nothing to do.
        return 0;
    }
    // A fresh anonymous mapping over each page, with its protection, so that the next read sees
    // zeros (relibc's madvise does nothing).
    let mut page = addr;
    while page < addr + len {
        let prot = MEM.lock().unwrap().prot_at(page);
        let Some(prot) = prot else { return -ENOMEM };
        let r = mmap_native(
            page,
            PAGE,
            native_prot(prot),
            libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
            -1,
            0,
        );
        if let Err(e) = r {
            return -e;
        }
        page += PAGE;
    }
    0
}

/// Places the break a random number of pages above the program's end.
pub fn init_brk(image_end: u64) {
    let base = ((image_end + PAGE - 1) & !(PAGE - 1)) + (random_u64() % 8192) * PAGE;
    let mut m = MEM.lock().unwrap();
    m.brk_base = base;
    m.brk_cur = base;
    m.brk_mapped = base;
}

pub fn brk(want: u64) -> i64 {
    let mut m = MEM.lock().unwrap();
    if want < m.brk_base || !user_ok(want, 0) {
        return m.brk_cur as i64;
    }
    let top = (want + PAGE - 1) & !(PAGE - 1);
    if top > m.brk_mapped {
        if map_at(m.brk_mapped, top - m.brk_mapped, 3, false, -1, 0).is_err() {
            return m.brk_cur as i64;
        }
        let from = m.brk_mapped;
        m.set(from, top, Some(3));
        m.brk_mapped = top;
    } else if top < m.brk_mapped {
        unsafe { libc::munmap(top as *mut _, (m.brk_mapped - top) as usize) };
        let to = m.brk_mapped;
        m.set(top, to, None);
        m.brk_mapped = top;
    }
    m.brk_cur = want;
    want as i64
}
