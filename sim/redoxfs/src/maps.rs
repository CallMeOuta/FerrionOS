use std::collections::{BTreeSet, VecDeque};

use redoxfs::{Disk, FileSystem, Node, PageCache, PageMemory, Transaction, TreePtr};
use syscall::error::{EINVAL, ENOMEM, Error, Result};
use syscall::flag::MapFlags;

use crate::disk::{Durability, SimDisk};
use crate::model::Time;
use crate::rng::{Rng, seed_entropy};

const PAGE: u64 = 4096;
const DISK_BYTES: usize = 16 << 20;
const NAMES: usize = 4;
const MAX_FILES: usize = 12;
const MAX_HANDLES: usize = 6;
const MAX_MAPS: usize = 6;
const MAX_FILE_BYTES: u64 = 24 * PAGE;
const MAX_IO_BYTES: u64 = 3 * PAGE;
const MAX_MAP_PAGES: u64 = 8;
const MEMORY_BYTES: usize = 64 * PAGE as usize;
const MAX_MAPPED_WRITES: u64 = 6;
const TRACE_STEPS: usize = 24;
const START: Time = (1_700_000_000, 0);
const GARBAGE: u8 = 0xa5;
const FAILED_COMMIT_ONE_IN: u64 = 8;
const MAX_FAILING_WRITE: u64 = 3;

struct HeapPages {
    bytes: Vec<u8>,
}

impl PageMemory for HeapPages {
    fn allocate(len: usize) -> Result<Self> {
        if len > MEMORY_BYTES {
            return Err(Error::new(ENOMEM));
        }
        let mut bytes = Vec::with_capacity(MEMORY_BYTES);
        bytes.resize(len, GARBAGE);
        Ok(HeapPages { bytes })
    }

    fn grow(&mut self, len: usize) -> Result<()> {
        if len > MEMORY_BYTES {
            return Err(Error::new(ENOMEM));
        }
        let before = self.bytes.as_ptr();
        self.bytes.resize(len, GARBAGE);
        assert_eq!(before, self.bytes.as_ptr());
        Ok(())
    }

    fn bytes(&mut self) -> &mut [u8] {
        &mut self.bytes
    }
}

struct File {
    node: u32,
    data: Vec<u8>,
    linked: bool,
}

struct Handle {
    file: usize,
    closing: bool,
}

struct Mapping {
    handle: usize,
    offset: u64,
    len: usize,
    writable: bool,
    address: usize,
}

pub struct MapStats {
    pub ops: u64,
    pub maps: u64,
    pub mapped_writes: u64,
    pub reads: u64,
    pub node_reuses: u64,
    pub failed_commits: u64,
}

struct Workload {
    fs: FileSystem<SimDisk>,
    cache: PageCache<HeapPages>,
    files: Vec<Option<File>>,
    names: [Option<usize>; NAMES],
    handles: Vec<Option<Handle>>,
    maps: Vec<Option<Mapping>>,
    freed_nodes: BTreeSet<u32>,
    trace: VecDeque<String>,
    now: Time,
    stats: MapStats,
}

fn errno(context: &str, err: Error) -> String {
    format!("{context}: {err}")
}

fn name(slot: usize) -> String {
    format!("m{slot}")
}

fn tx<T>(
    fs: &mut FileSystem<SimDisk>,
    work: impl FnOnce(&mut Transaction<SimDisk>) -> Result<T>,
) -> Result<T> {
    fs.tx(work)
}

fn node_size<D: Disk>(tx: &mut Transaction<D>, node: u32) -> Result<u64> {
    Ok(tx.read_tree(TreePtr::<Node>::new(node))?.data().size())
}

impl Workload {
    fn new() -> std::result::Result<Workload, String> {
        let disk = SimDisk::new(vec![0; DISK_BYTES], Durability::Ordered);
        let fs =
            FileSystem::create(disk, None, START.0, START.1).map_err(|err| errno("mkfs", err))?;
        Ok(Workload {
            fs,
            cache: PageCache::new(),
            files: (0..MAX_FILES).map(|_| None).collect(),
            names: [None; NAMES],
            handles: (0..MAX_HANDLES).map(|_| None).collect(),
            maps: (0..MAX_MAPS).map(|_| None).collect(),
            freed_nodes: BTreeSet::new(),
            trace: VecDeque::new(),
            now: START,
            stats: MapStats {
                ops: 0,
                maps: 0,
                mapped_writes: 0,
                reads: 0,
                node_reuses: 0,
                failed_commits: 0,
            },
        })
    }

    fn note(&mut self, text: String) {
        if self.trace.len() == TRACE_STEPS {
            self.trace.pop_front();
        }
        self.trace.push_back(text);
    }

    fn arm_commit_failure(&mut self, rng: &mut Rng) -> u64 {
        if rng.chance(1, FAILED_COMMIT_ONE_IN) {
            self.fs
                .disk
                .arm_write_error(rng.range(1, MAX_FAILING_WRITE));
        }
        self.fs.disk.write_errors
    }

    fn commit_failed<T>(
        &mut self,
        errors_before: u64,
        result: &Result<T>,
        what: &str,
    ) -> std::result::Result<bool, String> {
        self.fs.disk.disarm_write_error();
        let failed = self.fs.disk.write_errors != errors_before;
        match (result, failed) {
            (Err(_), true) => {
                self.stats.failed_commits += 1;
                Ok(true)
            }
            (Err(err), false) => Err(errno(what, err.clone())),
            (Ok(_), true) => Err(format!("{what} succeeded although a disk write failed")),
            (Ok(_), false) => Ok(false),
        }
    }

    fn file(&self, index: usize) -> &File {
        self.files[index].as_ref().expect("live file")
    }

    fn handle_file(&self, handle: usize) -> usize {
        self.handles[handle].as_ref().expect("open handle").file
    }

    fn handle_node(&self, handle: usize) -> TreePtr<Node> {
        TreePtr::new(self.file(self.handle_file(handle)).node)
    }

    fn free_slot<T>(slots: &[Option<T>]) -> Option<usize> {
        slots.iter().position(Option::is_none)
    }

    fn used_slots<T>(slots: &[Option<T>]) -> Vec<usize> {
        (0..slots.len())
            .filter(|&slot| slots[slot].is_some())
            .collect()
    }

    fn open_handles(&self) -> Vec<usize> {
        (0..self.handles.len())
            .filter(|&slot| {
                self.handles[slot]
                    .as_ref()
                    .is_some_and(|handle| !handle.closing)
            })
            .collect()
    }

    fn release_file(&mut self, index: usize) {
        let held = self
            .handles
            .iter()
            .flatten()
            .any(|handle| handle.file == index);
        let file = self.file(index);
        if !held && !file.linked {
            self.freed_nodes.insert(file.node);
            self.files[index] = None;
        }
    }

    fn create(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let free_names: Vec<usize> = (0..NAMES)
            .filter(|&slot| self.names[slot].is_none())
            .collect();
        let (Some(file_slot), Some(handle_slot)) =
            (Self::free_slot(&self.files), Self::free_slot(&self.handles))
        else {
            return Ok(());
        };
        if free_names.is_empty() {
            return Ok(());
        }
        let name_slot = *rng.pick(&free_names);
        let now = self.now;
        let cache = &mut self.cache;
        let node = tx(&mut self.fs, |tx| {
            let node = tx.create_node(
                TreePtr::<Node>::root(),
                &name(name_slot),
                Node::MODE_FILE | 0o644,
                now.0,
                now.1,
            )?;
            cache.open(node.ptr(), tx)?;
            Ok(node.id())
        })
        .map_err(|err| errno("create", err))?;
        if self.freed_nodes.remove(&node) {
            self.stats.node_reuses += 1;
        }
        self.note(format!(
            "create {} -> node {node}, handle {handle_slot}",
            name(name_slot)
        ));
        self.files[file_slot] = Some(File {
            node,
            data: Vec::new(),
            linked: true,
        });
        self.names[name_slot] = Some(file_slot);
        self.handles[handle_slot] = Some(Handle {
            file: file_slot,
            closing: false,
        });
        Ok(())
    }

    fn open(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let named: Vec<usize> = (0..NAMES)
            .filter(|&slot| self.names[slot].is_some())
            .collect();
        let Some(handle_slot) = Self::free_slot(&self.handles) else {
            return Ok(());
        };
        if named.is_empty() {
            return Ok(());
        }
        let name_slot = *rng.pick(&named);
        let file = self.names[name_slot].expect("named file");
        let node = TreePtr::<Node>::new(self.file(file).node);
        let truncate = rng.chance(1, 6);
        let now = self.now;
        if truncate {
            let errors = self.arm_commit_failure(rng);
            let cache = &mut self.cache;
            let result = tx(&mut self.fs, |tx| cache.truncate(node, 0, now, tx));
            if self.commit_failed(errors, &result, "open O_TRUNC")? {
                self.note(format!("open {} O_TRUNC: commit failed", name(name_slot)));
                return Ok(());
            }
            let resized = result.map_err(|err| errno("open O_TRUNC", err))?;
            self.cache.apply(resized, &[]);
            self.files[file].as_mut().expect("live file").data.clear();
        }
        let cache = &mut self.cache;
        tx(&mut self.fs, |tx| cache.open(node, tx)).map_err(|err| errno("open", err))?;
        self.note(format!(
            "open {}{} -> handle {handle_slot}",
            name(name_slot),
            if truncate { " O_TRUNC" } else { "" }
        ));
        self.handles[handle_slot] = Some(Handle {
            file,
            closing: false,
        });
        Ok(())
    }

    fn finish_close(&mut self, handle: usize) -> std::result::Result<(), String> {
        let node = self.handle_node(handle);
        let file = self.handle_file(handle);
        let cache = &mut self.cache;
        tx(&mut self.fs, |tx| cache.close(node, tx)).map_err(|err| errno("close", err))?;
        self.handles[handle] = None;
        self.release_file(file);
        Ok(())
    }

    fn close(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        self.note(format!("close handle {handle}"));
        if self
            .maps
            .iter()
            .flatten()
            .any(|mapping| mapping.handle == handle)
        {
            self.handles[handle].as_mut().expect("open handle").closing = true;
            return Ok(());
        }
        self.finish_close(handle)
    }

    fn unlink(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let named: Vec<usize> = (0..NAMES)
            .filter(|&slot| self.names[slot].is_some())
            .collect();
        if named.is_empty() {
            return Ok(());
        }
        let name_slot = *rng.pick(&named);
        tx(&mut self.fs, |tx| {
            tx.remove_node(TreePtr::<Node>::root(), &name(name_slot), Node::MODE_FILE)
                .map(|_| ())
        })
        .map_err(|err| errno("unlink", err))?;
        let file = self.names[name_slot].take().expect("named file");
        self.note(format!("unlink {}", name(name_slot)));
        self.files[file].as_mut().expect("live file").linked = false;
        self.release_file(file);
        Ok(())
    }

    fn write(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let file = self.handle_file(handle);
        let size = self.file(file).data.len() as u64;
        let len = rng.range(1, MAX_IO_BYTES);
        let highest = (size + 2 * PAGE).min(MAX_FILE_BYTES - len);
        let offset = rng.range(0, highest);
        let mut data = vec![0; len as usize];
        rng.fill(&mut data);
        let node = self.handle_node(handle);
        let now = self.now;
        let errors = self.arm_commit_failure(rng);
        let cache = &mut self.cache;
        let result = tx(&mut self.fs, |tx| cache.write(node, offset, &data, now, tx));
        if self.commit_failed(errors, &result, "write")? {
            self.note(format!(
                "write handle {handle} at {offset} len {len}: commit failed"
            ));
            return Ok(());
        }
        let (count, update) = result.map_err(|err| errno("write", err))?;
        self.cache.apply(update, &data);
        self.note(format!("write handle {handle} at {offset} len {len}"));
        if count != data.len() {
            return Err(format!("write returned {count} of {len} bytes"));
        }
        let model = &mut self.files[file].as_mut().expect("live file").data;
        let end = (offset + len) as usize;
        if model.len() < end {
            model.resize(end, 0);
        }
        model[offset as usize..end].copy_from_slice(&data);
        Ok(())
    }

    fn read(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let file = self.handle_file(handle);
        let size = self.file(file).data.len() as u64;
        let offset = rng.range(0, size + PAGE);
        let len = rng.range(1, MAX_IO_BYTES);
        let node = self.handle_node(handle);
        let now = self.now;
        let mut buf = vec![GARBAGE; len as usize];
        let cache = &mut self.cache;
        let count = tx(&mut self.fs, |tx| {
            cache.read(node, offset, &mut buf, now, tx)
        })
        .map_err(|err| errno("read", err))?;
        self.stats.reads += 1;
        self.note(format!("read handle {handle} at {offset} len {len}"));
        let data = &self.file(file).data;
        let start = (offset as usize).min(data.len());
        let expected = &data[start..(start + len as usize).min(data.len())];
        if count != expected.len() {
            return Err(format!(
                "read at {offset} len {len} returned {count} bytes, file holds {}",
                expected.len()
            ));
        }
        if let Some(index) = (0..count).find(|&index| buf[index] != expected[index]) {
            return Err(format!(
                "read at {offset}: byte {} is {:02x}, file holds {:02x}",
                offset + index as u64,
                buf[index],
                expected[index]
            ));
        }
        Ok(())
    }

    fn truncate(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let file = self.handle_file(handle);
        let size = self.file(file).data.len() as u64;
        let len = if rng.chance(1, 2) {
            rng.range(0, size)
        } else {
            rng.range(size, MAX_FILE_BYTES)
        };
        let node = self.handle_node(handle);
        let now = self.now;
        let errors = self.arm_commit_failure(rng);
        let cache = &mut self.cache;
        let result = tx(&mut self.fs, |tx| cache.truncate(node, len, now, tx));
        if self.commit_failed(errors, &result, "truncate")? {
            self.note(format!("truncate handle {handle} to {len}: commit failed"));
            return Ok(());
        }
        let resized = result.map_err(|err| errno("truncate", err))?;
        self.cache.apply(resized, &[]);
        self.note(format!("truncate handle {handle} to {len}"));
        self.files[file]
            .as_mut()
            .expect("live file")
            .data
            .resize(len as usize, 0);
        Ok(())
    }

    fn map(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        let Some(map_slot) = Self::free_slot(&self.maps) else {
            return Ok(());
        };
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let file = self.handle_file(handle);
        let size = self.file(file).data.len() as u64;
        let offset = rng.range(0, size.div_ceil(PAGE)) * PAGE;
        let len = rng.range(1, MAX_MAP_PAGES * PAGE) as usize;
        let writable = rng.chance(1, 2);
        let mut flags = MapFlags::MAP_SHARED | MapFlags::PROT_READ;
        if writable {
            flags |= MapFlags::PROT_WRITE;
        }
        let node = self.handle_node(handle);
        let now = self.now;
        let cache = &mut self.cache;
        let address = tx(&mut self.fs, |tx| {
            cache.map(node, flags, len, offset, now, tx)
        })
        .map_err(|err| errno("map", err))?;
        self.stats.maps += 1;
        self.note(format!(
            "map handle {handle} at {offset} len {len}{} -> mapping {map_slot}",
            if writable { " writable" } else { "" }
        ));
        self.maps[map_slot] = Some(Mapping {
            handle,
            offset,
            len,
            writable,
            address,
        });
        Ok(())
    }

    fn unmap(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let used = Self::used_slots(&self.maps);
        if used.is_empty() {
            return Ok(());
        }
        let map_slot = *rng.pick(&used);
        let mapping = self.maps[map_slot].take().expect("live mapping");
        let node = self.handle_node(mapping.handle);
        let now = self.now;
        let cache = &mut self.cache;
        tx(&mut self.fs, |tx| {
            cache.unmap(node, mapping.offset, mapping.len, now, tx)
        })
        .map_err(|err| errno("unmap", err))?;
        self.note(format!("unmap mapping {map_slot}"));
        let closing = self.handles[mapping.handle]
            .as_ref()
            .is_some_and(|handle| handle.closing);
        let mapped = self
            .maps
            .iter()
            .flatten()
            .any(|other| other.handle == mapping.handle);
        if closing && !mapped {
            self.finish_close(mapping.handle)?;
        }
        Ok(())
    }

    fn mapped_write(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let writable: Vec<usize> = (0..self.maps.len())
            .filter(|&slot| {
                self.maps[slot]
                    .as_ref()
                    .is_some_and(|mapping| mapping.writable)
            })
            .collect();
        if writable.is_empty() {
            return Ok(());
        }
        let map_slot = *rng.pick(&writable);
        let (handle, offset, len, address) = {
            let mapping = self.maps[map_slot].as_ref().expect("live mapping");
            (mapping.handle, mapping.offset, mapping.len, mapping.address)
        };
        let file = self.handle_file(handle);
        let size = self.file(file).data.len() as u64;
        let visible = (offset + len as u64).min(size);
        if visible <= offset {
            return Ok(());
        }
        let writes = rng.range(1, MAX_MAPPED_WRITES);
        for _ in 0..writes {
            let start = rng.range(offset, visible - 1);
            let run = rng.range(1, (visible - start).min(PAGE));
            let mut data = vec![0; run as usize];
            rng.fill(&mut data);
            let target = (address + (start - offset) as usize) as *mut u8;
            unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len()) };
            let model = &mut self.files[file].as_mut().expect("live file").data;
            model[start as usize..(start + run) as usize].copy_from_slice(&data);
        }
        self.stats.mapped_writes += 1;
        self.note(format!("mapped write mapping {map_slot}, {writes} runs"));
        Ok(())
    }

    fn fsync(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let node = self.handle_node(handle);
        let now = self.now;
        let cache = &mut self.cache;
        tx(&mut self.fs, |tx| cache.sync(node, now, tx)).map_err(|err| errno("fsync", err))?;
        self.note(format!("fsync handle {handle}"));
        Ok(())
    }

    fn malformed(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        let open = self.open_handles();
        if open.is_empty() {
            return Ok(());
        }
        let handle = *rng.pick(&open);
        let node = self.handle_node(handle);
        let unaligned = rng.range(1, PAGE - 1);
        let len = rng.range(1, MAX_MAP_PAGES * PAGE) as usize;
        let flags = MapFlags::MAP_SHARED | MapFlags::PROT_READ | MapFlags::PROT_WRITE;
        let now = self.now;
        let cache = &mut self.cache;
        let answers = tx(&mut self.fs, |tx| {
            Ok([
                cache.map(node, flags, 0, 0, now, tx).err(),
                cache.map(node, flags, len, unaligned, now, tx).err(),
                cache.unmap(node, 0, 0, now, tx).err(),
                cache.unmap(node, unaligned, len, now, tx).err(),
            ])
        })
        .map_err(|err| errno("malformed", err))?;
        self.note(format!("malformed requests on handle {handle}"));
        for (index, answer) in answers.iter().enumerate() {
            if answer.map(|err| err.errno) != Some(EINVAL) {
                return Err(format!(
                    "malformed request {index} answered {answer:?}, not EINVAL"
                ));
            }
        }
        Ok(())
    }

    fn step(&mut self, rng: &mut Rng) -> std::result::Result<(), String> {
        self.now = (self.now.0 + 1, 0);
        self.stats.ops += 1;
        match rng.below(100) {
            0..=9 => self.create(rng),
            10..=17 => self.open(rng),
            18..=27 => self.close(rng),
            28..=33 => self.unlink(rng),
            34..=47 => self.write(rng),
            48..=57 => self.read(rng),
            58..=63 => self.truncate(rng),
            64..=75 => self.map(rng),
            76..=85 => self.unmap(rng),
            86..=95 => self.mapped_write(rng),
            96..=97 => self.fsync(rng),
            _ => self.malformed(rng),
        }
    }

    fn check(&mut self) -> std::result::Result<(), String> {
        for slot in 0..self.maps.len() {
            let Some(mapping) = self.maps[slot].as_ref() else {
                continue;
            };
            let data = &self.file(self.handle_file(mapping.handle)).data;
            let start = mapping.offset as usize;
            let end = (start + mapping.len).min(data.len());
            for position in start..end.max(start) {
                let seen = unsafe { *((mapping.address + position - start) as *const u8) };
                if seen != data[position] {
                    return Err(format!(
                        "mapping {slot} at byte {position} shows {seen:02x}, file holds {:02x}",
                        data[position]
                    ));
                }
            }
        }
        let mut open_nodes = BTreeSet::new();
        for handle in self.handles.iter().flatten() {
            open_nodes.insert(self.file(handle.file).node);
        }
        for &node in &open_nodes {
            let file = self
                .files
                .iter()
                .flatten()
                .find(|file| file.node == node)
                .expect("file");
            let expected = file.data.len() as u64;
            let size =
                tx(&mut self.fs, |tx| node_size(tx, node)).map_err(|err| errno("stat", err))?;
            if size != expected {
                return Err(format!(
                    "node {node} has size {size}, file holds {expected}"
                ));
            }
        }
        if self.cache.cached_nodes() != open_nodes.len() {
            return Err(format!(
                "page cache holds {} nodes, {} are open",
                self.cache.cached_nodes(),
                open_nodes.len()
            ));
        }
        Ok(())
    }

    fn check_disk(&mut self) -> std::result::Result<(), String> {
        for slot in 0..self.maps.len() {
            if let Some(mapping) = self.maps[slot].take() {
                let node = self.handle_node(mapping.handle);
                let now = self.now;
                let cache = &mut self.cache;
                tx(&mut self.fs, |tx| {
                    cache.unmap(node, mapping.offset, mapping.len, now, tx)
                })
                .map_err(|err| errno("final unmap", err))?;
            }
        }
        for handle in 0..self.handles.len() {
            if self.handles[handle].is_some() {
                self.finish_close(handle)?;
            }
        }
        if self.cache.cached_nodes() != 0 {
            return Err(format!(
                "page cache holds {} nodes after every file closed",
                self.cache.cached_nodes()
            ));
        }
        for file in self.files.iter().flatten() {
            let mut buf = vec![GARBAGE; file.data.len() + 1];
            let node = TreePtr::<Node>::new(file.node);
            let count = tx(&mut self.fs, |tx| {
                tx.read_node(node, 0, &mut buf, START.0, START.1)
            })
            .map_err(|err| errno("disk read", err))?;
            if buf[..count] != file.data[..] {
                let index = (0..count.min(file.data.len()))
                    .find(|&index| buf[index] != file.data[index])
                    .unwrap_or(count.min(file.data.len()));
                return Err(format!(
                    "node {} on disk holds {count} bytes, file holds {}; first difference at {index}",
                    file.node,
                    file.data.len()
                ));
            }
        }
        Ok(())
    }
}

pub fn run_seed(seed: u64, ops: u64) -> std::result::Result<MapStats, String> {
    seed_entropy(seed);
    let mut rng = Rng::new(seed ^ 0x6d61_7073_0000_0001);
    let mut workload = Workload::new()?;
    for op in 0..ops {
        let result = workload.step(&mut rng).and_then(|()| workload.check());
        if let Err(message) = result {
            let trace: Vec<String> = workload.trace.iter().cloned().collect();
            return Err(format!(
                "seed {seed} op {op}: {message}\n    {}",
                trace.join("\n    ")
            ));
        }
    }
    workload
        .check_disk()
        .map_err(|message| format!("seed {seed} final check: {message}"))?;
    Ok(workload.stats)
}
