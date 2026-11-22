use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::Range;

use range_tree::RangeTree;
use syscall::PAGE_SIZE;
use syscall::error::{EBADFD, EINVAL, EIO, EMFILE, ENOMEM, Error, Result};
use syscall::flag::MapFlags;

use crate::{Disk, Node, Transaction, TreePtr};

pub const MAX_MAPPED_BYTES: u64 = 1 << 36;
pub const MAX_MAPPED_RANGES: usize = 4096;
pub const MAX_NODE_HANDLES: usize = 1 << 20;

pub type Time = (u64, u32);

pub trait PageMemory: Sized {
    fn allocate(len: usize) -> Result<Self>;

    fn grow(&mut self, len: usize) -> Result<()>;

    fn bytes(&mut self) -> &mut [u8];
}

#[must_use]
#[derive(Debug)]
pub struct CacheUpdate {
    node: u32,
    zero: Range<u64>,
    store_at: u64,
    store_len: usize,
}

impl CacheUpdate {
    fn none(node: u32) -> Self {
        Self {
            node,
            zero: 0..0,
            store_at: 0,
            store_len: 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Mapped {
    rc: usize,
    dirty: bool,
}

impl range_tree::Value for Mapped {
    type K = u64;

    fn try_merge_forward(self, other: &Self) -> core::result::Result<Self, Self> {
        if self.rc == other.rc && self.dirty == other.dirty {
            Ok(self)
        } else {
            Err(self)
        }
    }

    fn try_merge_backwards(self, other: &Self) -> core::result::Result<Self, Self> {
        self.try_merge_forward(other)
    }

    fn split(
        self,
        prev_range: Option<Range<u64>>,
        _range: Range<u64>,
        next_range: Option<Range<u64>>,
    ) -> (Option<Self>, Self, Option<Self>) {
        (prev_range.map(|_| self), self, next_range.map(|_| self))
    }
}

struct CachedNode<M> {
    memory: Option<M>,
    ranges: RangeTree<Mapped>,
    handles: usize,
}

impl<M: PageMemory> CachedNode<M> {
    fn new() -> Self {
        Self {
            memory: None,
            ranges: RangeTree::new(),
            handles: 0,
        }
    }

    fn in_use(&self) -> bool {
        self.handles > 0 || self.ranges.iter().any(|(_, mapped)| mapped.rc > 0)
    }

    fn each_cached(&mut self, span: Range<u64>, mut apply: impl FnMut(Range<u64>, &mut [u8])) {
        if span.start >= span.end {
            return;
        }
        let Some(memory) = self.memory.as_mut() else {
            return;
        };
        let bytes = memory.bytes();
        for (range, _) in self.ranges.conflicts(span.clone()) {
            assert!(range.end <= bytes.len() as u64);
            let from = range.start.max(span.start);
            let to = range.end.min(span.end);
            if from < to {
                apply(from..to, &mut bytes[from as usize..to as usize]);
            }
        }
    }

    fn store(&mut self, offset: u64, data: &[u8]) {
        let end = offset + data.len() as u64;
        self.each_cached(offset..end, |range, target| {
            let start = (range.start - offset) as usize;
            target.copy_from_slice(&data[start..start + target.len()]);
        });
    }

    fn zero(&mut self, span: Range<u64>) {
        self.each_cached(span, |_, target| target.fill(0));
    }

    fn flush<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        ranges: &[Range<u64>],
        mtime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        if ranges.is_empty() {
            return Ok(());
        }
        let size = tx.read_tree(node_ptr)?.data().size();
        let Some(memory) = self.memory.as_mut() else {
            return Err(Error::new(EIO));
        };
        let bytes = memory.bytes();
        for range in ranges {
            assert!(range.end <= bytes.len() as u64);
            let stop = range.end.min(size);
            if range.start < stop {
                tx.write_node(
                    node_ptr,
                    range.start,
                    &bytes[range.start as usize..stop as usize],
                    mtime.0,
                    mtime.1,
                )?;
            }
        }
        Ok(())
    }

    fn dirty_ranges(&self) -> Vec<Range<u64>> {
        self.ranges
            .iter()
            .filter(|(_, mapped)| mapped.dirty)
            .map(|(range, _)| range)
            .collect()
    }
}

pub struct PageCache<M: PageMemory> {
    nodes: BTreeMap<u32, CachedNode<M>>,
}

impl<M: PageMemory> Default for PageCache<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: PageMemory> PageCache<M> {
    pub fn new() -> Self {
        Self {
            nodes: BTreeMap::new(),
        }
    }

    pub fn cached_nodes(&self) -> usize {
        self.nodes.len()
    }

    pub fn open<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        let id = node_ptr.id();
        let in_use = match self.nodes.get(&id) {
            Some(cached) if cached.handles >= MAX_NODE_HANDLES => return Err(Error::new(EMFILE)),
            Some(cached) => cached.in_use(),
            None => false,
        };
        if !in_use {
            tx.on_open_node(node_ptr)?;
        }
        self.nodes.entry(id).or_insert_with(CachedNode::new).handles += 1;
        Ok(())
    }

    pub fn close<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        let cached = self
            .nodes
            .get_mut(&node_ptr.id())
            .ok_or(Error::new(EBADFD))?;
        cached.handles = cached.handles.checked_sub(1).ok_or(Error::new(EBADFD))?;
        self.release_unused(node_ptr, tx)
    }

    fn release_unused<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        if self
            .nodes
            .get(&node_ptr.id())
            .is_some_and(|cached| !cached.in_use())
        {
            self.nodes.remove(&node_ptr.id());
            tx.on_close_node(node_ptr)?;
        }
        Ok(())
    }

    pub fn map<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        flags: MapFlags,
        len: usize,
        offset: u64,
        atime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<usize> {
        let page = PAGE_SIZE as u64;
        if len == 0 || offset % page != 0 {
            return Err(Error::new(EINVAL));
        }
        let aligned = (len as u64)
            .checked_next_multiple_of(page)
            .ok_or(Error::new(ENOMEM))?;
        let end = offset
            .checked_add(aligned)
            .filter(|end| *end <= MAX_MAPPED_BYTES)
            .ok_or(Error::new(ENOMEM))?;
        let end_bytes = usize::try_from(end).map_err(|_| Error::new(ENOMEM))?;
        let cached = self
            .nodes
            .get_mut(&node_ptr.id())
            .filter(|cached| cached.handles > 0)
            .ok_or(Error::new(EBADFD))?;
        if cached.ranges.len() >= MAX_MAPPED_RANGES {
            return Err(Error::new(ENOMEM));
        }
        if cached.memory.is_none() {
            cached.memory = Some(M::allocate(end_bytes)?);
        }
        let Some(memory) = cached.memory.as_mut() else {
            return Err(Error::new(EIO));
        };
        if memory.bytes().len() < end_bytes {
            memory.grow(end_bytes)?;
        }
        let bytes = memory.bytes();
        assert!(bytes.len() >= end_bytes);

        let mut holes = Vec::new();
        let mut cursor = offset;
        for (range, _) in cached.ranges.conflicts(offset..end) {
            if range.start > cursor {
                holes.push(cursor..range.start);
            }
            cursor = cursor.max(range.end);
        }
        if cursor < end {
            holes.push(cursor..end);
        }
        for hole in holes {
            let target = &mut bytes[hole.start as usize..hole.end as usize];
            let count = tx.read_node(node_ptr, hole.start, target, atime.0, atime.1)?;
            target[count..].fill(0);
        }
        let address = bytes.as_mut_ptr() as usize + offset as usize;

        let dirty = flags.contains(MapFlags::MAP_SHARED) && flags.contains(MapFlags::PROT_WRITE);
        for (range, existing) in cached.ranges.remove_and_unused(offset..end) {
            let mapped = match existing {
                Some(mapped) => Mapped {
                    rc: mapped.rc + 1,
                    dirty: mapped.dirty || dirty,
                },
                None => Mapped { rc: 1, dirty },
            };
            if cached
                .ranges
                .insert(range.start, range.end - range.start, mapped)
                .is_err()
            {
                return Err(Error::new(EIO));
            }
        }
        Ok(address)
    }

    pub fn unmap<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        offset: u64,
        len: usize,
        mtime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        let page = PAGE_SIZE as u64;
        if len == 0 || offset % page != 0 {
            return Err(Error::new(EINVAL));
        }
        let aligned = (len as u64)
            .checked_next_multiple_of(page)
            .ok_or(Error::new(EINVAL))?;
        let end = offset.checked_add(aligned).ok_or(Error::new(EINVAL))?;
        let cached = self
            .nodes
            .get_mut(&node_ptr.id())
            .ok_or(Error::new(EBADFD))?;
        let mut mapped_before = true;
        let mut dirty = Vec::new();
        for (range, mut mapped) in cached.ranges.remove(offset..end) {
            match mapped.rc.checked_sub(1) {
                Some(rc) => mapped.rc = rc,
                None => mapped_before = false,
            }
            if mapped.dirty {
                dirty.push(range.clone());
            }
            if cached
                .ranges
                .insert(range.start, range.end - range.start, mapped)
                .is_err()
            {
                return Err(Error::new(EIO));
            }
        }
        cached.flush(node_ptr, &dirty, mtime, tx)?;
        self.release_unused(node_ptr, tx)?;
        if mapped_before {
            Ok(())
        } else {
            Err(Error::new(EINVAL))
        }
    }

    pub fn read<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        offset: u64,
        buf: &mut [u8],
        atime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<usize> {
        let count = tx.read_node(node_ptr, offset, buf, atime.0, atime.1)?;
        if count == 0 {
            return Ok(0);
        }
        let end = offset + count as u64;
        let Some(cached) = self.nodes.get_mut(&node_ptr.id()) else {
            return Ok(count);
        };
        let Some(memory) = cached.memory.as_mut() else {
            return Ok(count);
        };
        let bytes = memory.bytes();
        for (range, mapped) in cached.ranges.conflicts(offset..end) {
            if !mapped.dirty {
                continue;
            }
            assert!(range.end <= bytes.len() as u64);
            let from = range.start.max(offset);
            let to = range.end.min(end);
            if from < to {
                buf[(from - offset) as usize..(to - offset) as usize]
                    .copy_from_slice(&bytes[from as usize..to as usize]);
            }
        }
        Ok(count)
    }

    pub fn apply(&mut self, update: CacheUpdate, data: &[u8]) {
        assert!(update.store_len <= data.len());
        let Some(cached) = self.nodes.get_mut(&update.node) else {
            return;
        };
        cached.zero(update.zero);
        cached.store(update.store_at, &data[..update.store_len]);
    }

    pub fn write<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        offset: u64,
        data: &[u8],
        mtime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<(usize, CacheUpdate)> {
        let node = node_ptr.id();
        let cached = self
            .nodes
            .get(&node)
            .is_some_and(|cached| cached.memory.is_some());
        if !cached {
            let count = tx.write_node(node_ptr, offset, data, mtime.0, mtime.1)?;
            return Ok((count, CacheUpdate::none(node)));
        }
        let size = tx.read_tree(node_ptr)?.data().size();
        let count = tx.write_node(node_ptr, offset, data, mtime.0, mtime.1)?;
        assert!(count <= data.len());
        let update = CacheUpdate {
            node,
            zero: size..offset,
            store_at: offset,
            store_len: count,
        };
        Ok((count, update))
    }

    pub fn truncate<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        len: u64,
        mtime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<CacheUpdate> {
        let node = node_ptr.id();
        let cached = self
            .nodes
            .get(&node)
            .is_some_and(|cached| cached.memory.is_some());
        let size = if cached {
            Some(tx.read_tree(node_ptr)?.data().size())
        } else {
            None
        };
        tx.truncate_node(node_ptr, len, mtime.0, mtime.1)?;
        let Some(size) = size else {
            return Ok(CacheUpdate::none(node));
        };
        let resized = CacheUpdate {
            node,
            zero: size.min(len)..size.max(len),
            store_at: 0,
            store_len: 0,
        };
        Ok(resized)
    }

    pub fn sync<D: Disk>(
        &mut self,
        node_ptr: TreePtr<Node>,
        mtime: Time,
        tx: &mut Transaction<D>,
    ) -> Result<()> {
        let Some(cached) = self.nodes.get_mut(&node_ptr.id()) else {
            return Ok(());
        };
        let dirty = cached.dirty_ranges();
        cached.flush(node_ptr, &dirty, mtime, tx)
    }
}
