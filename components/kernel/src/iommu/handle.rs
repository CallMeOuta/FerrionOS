use alloc::sync::Arc;
use core::num::NonZeroUsize;

use crate::{
    context::memory::{
        handle_notify_files, AddrSpace, AddrSpaceWrapper, Grant, PageSpan, UnmapVec,
    },
    memory::{get_page_info, Frame, RefKind, VirtualAddress, PAGE_SIZE},
    sync::CleanLockToken,
    syscall::{
        data::Map,
        error::{Error, Result, EACCES, EINVAL, ENOENT, EOPNOTSUPP},
        flag::MapFlags,
        usercopy::UserSliceRw,
    },
};

use super::{RequesterId, MAX_HANDLES, MAX_PIN_PAGES};

pub const ID_BASE: usize = 1 << 40;
pub const VERB_ADDRESS: u64 = 1;
pub const VERB_RELEASE: u64 = 2;
const RELEASE_PAYLOAD: usize = 16;

pub fn slot(id: usize) -> Option<usize> {
    id.checked_sub(ID_BASE).filter(|slot| *slot < MAX_HANDLES)
}

pub fn open(name: &str, privileged: bool, uid: u32) -> Result<usize> {
    if !privileged || uid != 0 {
        return Err(Error::new(EACCES));
    }
    let rid = RequesterId::parse(name).ok_or(Error::new(ENOENT))?;
    Ok(ID_BASE + super::open(rid)?)
}

pub fn fmap(
    slot: usize,
    addr_space: &Arc<AddrSpaceWrapper>,
    map: &Map,
    token: &mut CleanLockToken,
) -> Result<usize> {
    if map.flags.contains(MapFlags::MAP_SHARED) {
        return Err(Error::new(EOPNOTSUPP));
    }
    let memory_type = map.offset / PAGE_SIZE;
    if map.offset % PAGE_SIZE != 0 || memory_type > 3 {
        return Err(Error::new(EINVAL));
    }
    let span = PageSpan::validate_nonempty(VirtualAddress::new(map.address), map.size)
        .ok_or(Error::new(EINVAL))?;
    let count = NonZeroUsize::new(span.count).ok_or(Error::new(EINVAL))?;
    if count.get() > MAX_PIN_PAGES {
        return Err(Error::new(EINVAL));
    }
    let mem_policy = {
        let addrspace = addr_space.inner.read(token.token());
        addrspace.table.utable.allocator().0
    };
    let fixed = map.flags.contains(MapFlags::MAP_FIXED)
        || map.flags.contains(MapFlags::MAP_FIXED_NOREPLACE);

    super::reserve(slot)?;
    let mut pinned = None;
    let mut notify_files = UnmapVec::new();
    let mapped = {
        let mut lock_token = token.token();
        addr_space.acquire_write(lock_token.downgrade()).mmap(
            addr_space,
            (map.address != 0 || fixed).then_some(span.base),
            count,
            map.flags,
            Some(&mut notify_files),
            |dst_page, mut flags, mapper, flusher| {
                flags = match memory_type {
                    0 => flags,
                    1 => flags.uncacheable(true),
                    2 => flags.write_combining(true),
                    _ => flags.device_memory(true),
                };
                let grant = Grant::zeroed_phys_contiguous(
                    PageSpan::new(dst_page, count.get()),
                    flags,
                    mapper,
                    flusher,
                    mem_policy,
                )?;
                let (physical, _) = mapper
                    .translate(dst_page.start_address())
                    .expect("a physically contiguous grant is mapped when it is made");
                let base = Frame::containing(physical);
                for page in 0..count.get() {
                    get_page_info(base.next_by(page))
                        .expect("an allocated frame has a PageInfo")
                        .add_ref(RefKind::Shared)
                        .expect("a fresh frame takes a second reference");
                }
                pinned = Some(base);
                Ok(grant)
            },
        )
    };
    handle_notify_files(notify_files, token);
    let page = match mapped {
        Ok(page) => page,
        Err(err) => {
            super::unreserve(slot);
            return Err(err);
        }
    };
    let base = pinned.expect("a successful mmap ran the grant closure");
    match super::map(slot, base, count.get()) {
        Ok(_) => Ok(page.start_address().data()),
        Err(err) => {
            super::unpin(base, count.get());
            if let Ok(notify) = addr_space.munmap(PageSpan::new(page, count.get()), false, token) {
                handle_notify_files(notify, token);
            }
            Err(err)
        }
    }
}

pub fn call(
    slot: usize,
    payload: UserSliceRw,
    metadata: &[u64],
    token: &mut CleanLockToken,
) -> Result<usize> {
    match metadata.first().copied() {
        Some(VERB_ADDRESS) => {
            let virt = VirtualAddress::new(payload.read_usize()?);
            let physical = {
                let mut token = token.token();
                let addr = AddrSpace::current()?;
                let addr = addr.acquire_read(token.downgrade());
                let (physical, _) = addr
                    .table
                    .utable
                    .translate(virt)
                    .ok_or(Error::new(ENOENT))?;
                physical
            };
            let device = super::address(slot, physical.data() as u64)?;
            payload.write_usize(device as usize)?;
            Ok(0)
        }
        Some(VERB_RELEASE) => {
            if payload.len() != RELEASE_PAYLOAD {
                return Err(Error::new(EINVAL));
            }
            let mut words = [0u8; RELEASE_PAYLOAD];
            payload.copy_to_slice(&mut words)?;
            let address = u64::from_ne_bytes(words[..8].try_into().expect("eight bytes"));
            let length = u64::from_ne_bytes(words[8..].try_into().expect("eight bytes"));
            super::release(slot, address, length)?;
            Ok(0)
        }
        _ => Err(Error::new(EINVAL)),
    }
}

pub fn close(slot: usize) {
    super::close(slot);
}
