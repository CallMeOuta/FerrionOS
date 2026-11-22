use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{LINUX_TOP, errno::*};

pub const PAGE: u64 = 4096;
/// relibc's MAP_FIXED_NOREPLACE (src/header/sys_mman/redox.rs); the libc crate lacks it.
const MAP_FIXED_NOREPLACE: i32 = 0x000C;
/// Lowest address the runtime picks by itself, as Linux's default mmap_min_addr.
const LOW: u64 = 0x10000;
const PROT_READ: u64 = 1;
const PROT_WRITE: u64 = 2;

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Private,
    Shared,
    FileView,
}

#[derive(Clone, Copy)]
struct Region {
    end: u64,
    prot: u64,
    kind: Kind,
}

struct Mem {
    regions: BTreeMap<u64, Region>,
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
    fn set(&mut self, start: u64, end: u64, what: Option<(u64, Kind)>) {
        let overlapping: Vec<(u64, Region)> = self
            .regions
            .range(..end)
            .filter(|(_, r)| r.end > start)
            .map(|(s, r)| (*s, *r))
            .collect();
        for (s, r) in overlapping {
            self.regions.remove(&s);
            if s < start {
                self.regions.insert(s, Region { end: start, ..r });
            }
            if r.end > end {
                self.regions.insert(end, r);
            }
        }
        if let Some((prot, kind)) = what {
            self.regions.insert(start, Region { end, prot, kind });
        }
    }
    fn at(&self, addr: u64) -> Option<Region> {
        self.regions
            .range(..=addr)
            .next_back()
            .filter(|(_, r)| r.end > addr)
            .map(|(_, r)| *r)
    }
    fn pieces(&self, start: u64, end: u64) -> Option<Vec<(u64, u64, Region)>> {
        let mut out = Vec::new();
        let mut at = start;
        while at < end {
            let r = self.at(at)?;
            let to = r.end.min(end);
            out.push((at, to, r));
            at = to;
        }
        Some(out)
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

pub fn note(start: u64, end: u64, prot: u64) {
    MEM.lock().unwrap().set(start, end, Some((prot, Kind::Private)));
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
    if fd >= 0 && !shared {
        return map_private_file(addr, len, prot, flags & !MAP_POPULATE, fd, off);
    }
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
            let kind = if shared { Kind::Shared } else { Kind::Private };
            MEM.lock().unwrap().set(a, a + len, Some((prot, kind)));
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

fn read_into(fd: i32, at: u64, len: u64, off: u64) -> Result<(), i64> {
    let mut done = 0u64;
    while done < len {
        let n = unsafe {
            libc::pread(
                fd,
                (at + done) as *mut libc::c_void,
                (len - done) as usize,
                (off + done) as libc::off_t,
            )
        };
        if n < 0 {
            let e = last_errno();
            if e == EINTR {
                continue;
            }
            return Err(e);
        }
        if n == 0 {
            break;
        }
        done += n as u64;
    }
    Ok(())
}

fn map_private_file(addr: u64, len: u64, prot: u64, flags: u64, fd: i32, off: u64) -> i64 {
    const MAP_ANONYMOUS: u64 = 0x20;
    let mut st: libc::stat = unsafe { core::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 {
        return -last_errno();
    }
    if st.st_mode & libc::S_IFMT != libc::S_IFREG {
        return -ENODEV;
    }
    let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if fl < 0 || fl & libc::O_ACCMODE == libc::O_WRONLY {
        return -EACCES;
    }
    let at = mmap(addr, len, PROT_READ | PROT_WRITE, flags | MAP_ANONYMOUS, -1, 0);
    if at < 0 {
        return at;
    }
    let at = at as u64;
    let size = st.st_size.max(0) as u64;
    let view = size.saturating_sub(off).next_multiple_of(PAGE).min(len);
    if prot & PROT_WRITE == 0
        && view != 0
        && mmap_native(at, view, native_prot(prot), libc::MAP_SHARED | libc::MAP_FIXED, fd, off).is_ok()
    {
        MEM.lock().unwrap().set(at, at + view, Some((prot, Kind::FileView)));
        if view < len {
            let r = mprotect(at + view, len - view, prot);
            if r < 0 {
                munmap(at, len);
                return r;
            }
        }
        return at as i64;
    }
    if let Err(e) = read_into(fd, at, len, off) {
        munmap(at, len);
        return -e;
    }
    if prot != PROT_READ | PROT_WRITE {
        let r = mprotect(at, len, prot);
        if r < 0 {
            munmap(at, len);
            return r;
        }
    }
    at as i64
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

fn make_private(mem: &mut Mem, start: u64, end: u64, prot: u64) -> Result<(), i64> {
    let len = (end - start) as usize;
    let mut bytes = vec![0u8; len];
    if prot & PROT_READ == 0 {
        if unsafe { libc::mprotect(start as *mut _, len, libc::PROT_READ) } != 0 {
            return Err(last_errno());
        }
    }
    unsafe { core::ptr::copy_nonoverlapping(start as *const u8, bytes.as_mut_ptr(), len) };
    mmap_native(
        start,
        len as u64,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
        -1,
        0,
    )?;
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), start as *mut u8, len) };
    if unsafe { libc::mprotect(start as *mut _, len, native_prot(prot)) } != 0 {
        return Err(last_errno());
    }
    mem.set(start, end, Some((prot, Kind::Private)));
    Ok(())
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
    let mut mem = MEM.lock().unwrap();
    if prot & PROT_WRITE != 0 {
        if let Some(pieces) = mem.pieces(addr, addr + len) {
            for (s, e, r) in pieces {
                if r.kind == Kind::FileView {
                    if let Err(err) = make_private(&mut mem, s, e, r.prot) {
                        return -err;
                    }
                }
            }
        }
    }
    if unsafe { libc::mprotect(addr as *mut _, len as usize, native_prot(prot)) } != 0 {
        return -last_errno();
    }
    let pieces = mem.pieces(addr, addr + len).unwrap_or_else(|| vec![(addr, addr + len, Region { end: addr + len, prot, kind: Kind::Private })]);
    for (s, e, r) in pieces {
        mem.set(s, e, Some((prot, r.kind)));
    }
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
    let mut page = addr;
    while page < addr + len {
        let region = MEM.lock().unwrap().at(page);
        let Some(region) = region else { return -ENOMEM };
        if region.kind == Kind::Private {
            let r = mmap_native(
                page,
                PAGE,
                native_prot(region.prot),
                libc::MAP_PRIVATE | libc::MAP_ANON | libc::MAP_FIXED,
                -1,
                0,
            );
            if let Err(e) = r {
                return -e;
            }
        }
        page += PAGE;
    }
    0
}

pub fn mremap(old: u64, old_len: u64, new_len: u64, flags: u64, new_addr: u64) -> i64 {
    const MAYMOVE: u64 = 1;
    const FIXED: u64 = 2;
    if old % PAGE != 0 || flags & !(MAYMOVE | FIXED) != 0 || (flags & FIXED != 0 && flags & MAYMOVE == 0) {
        return -EINVAL;
    }
    let round = |l: u64| l.checked_add(PAGE - 1).map(|l| l & !(PAGE - 1));
    let (Some(old_len), Some(new_len)) = (round(old_len), round(new_len)) else { return -EINVAL };
    if new_len == 0 || old_len == 0 || !user_ok(old, old_len) {
        return -EINVAL;
    }
    let pieces = MEM.lock().unwrap().pieces(old, old + old_len);
    let Some(pieces) = pieces else { return -EFAULT };
    let shared = pieces.iter().any(|(_, _, r)| r.kind == Kind::Shared);
    let kind = if shared { Kind::Shared } else { Kind::Private };
    let share = if shared { libc::MAP_SHARED } else { libc::MAP_PRIVATE };
    let prot = pieces[0].2.prot;
    if flags & FIXED == 0 {
        if new_len <= old_len {
            if new_len < old_len {
                let r = munmap(old + new_len, old_len - new_len);
                if r < 0 {
                    return r;
                }
            }
            return old as i64;
        }
        if user_ok(old, new_len) && map_at(old + old_len, new_len - old_len, prot, shared, -1, 0).is_ok() {
            MEM.lock().unwrap().set(old + old_len, old + new_len, Some((prot, kind)));
            return old as i64;
        }
        if flags & MAYMOVE == 0 {
            return -ENOMEM;
        }
    } else if new_addr % PAGE != 0 || !user_ok(new_addr, new_len) || (new_addr < old + old_len && old < new_addr + new_len) {
        return -EINVAL;
    }
    let to = if flags & FIXED != 0 {
        mmap_native(new_addr, new_len, libc::PROT_READ | libc::PROT_WRITE, share | libc::MAP_ANON | libc::MAP_FIXED, -1, 0)
    } else {
        map_anywhere(0, new_len, PROT_READ | PROT_WRITE, shared, -1, 0)
    };
    let to = match to {
        Ok(a) => a,
        Err(e) => return -e,
    };
    let copy = old_len.min(new_len) as usize;
    let unreadable = pieces.iter().any(|(_, _, r)| r.prot & PROT_READ == 0);
    if unreadable {
        unsafe { libc::mprotect(old as *mut _, old_len as usize, libc::PROT_READ) };
    }
    unsafe { core::ptr::copy_nonoverlapping(old as *const u8, to as *mut u8, copy) };
    unsafe { libc::munmap(old as *mut _, old_len as usize) };
    let mut mem = MEM.lock().unwrap();
    mem.set(old, old + old_len, None);
    mem.set(to, to + new_len, Some((prot, kind)));
    drop(mem);
    if prot != PROT_READ | PROT_WRITE {
        unsafe { libc::mprotect(to as *mut _, new_len as usize, native_prot(prot)) };
    }
    to as i64
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
        m.set(from, top, Some((3, Kind::Private)));
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
