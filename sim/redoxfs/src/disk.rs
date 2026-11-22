use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

use redoxfs::{Allocator, Disk, HEADER_RING};
use syscall::error::{EIO, EROFS, Error, Result};

use crate::rng::Rng;

pub const BLOCK: usize = 4096;
pub const SECTOR: usize = 512;
pub const SECTORS_PER_BLOCK: usize = BLOCK / SECTOR;
pub const CACHE_LIMIT: usize = 1 << 15;
pub const ENUMERATION_CACHE_LIMIT: usize = 1 << 10;
const HEADER_GENERATION_OFFSET: usize = 40;

pub type Block = Box<[u8; BLOCK]>;

fn new_block(bytes: &[u8]) -> Block {
    assert_eq!(bytes.len(), BLOCK);
    let mut block: Block = Box::new([0; BLOCK]);
    block.copy_from_slice(bytes);
    block
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Durability {
    Ordered,
    Volatile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashKind {
    DropUnflushed,
    KeepUnflushed,
    Subset,
    TornOne,
    PrefixTorn,
}

pub const CRASH_KINDS: [CrashKind; 5] = [
    CrashKind::DropUnflushed,
    CrashKind::KeepUnflushed,
    CrashKind::Subset,
    CrashKind::TornOne,
    CrashKind::PrefixTorn,
];

impl CrashKind {
    pub fn name(self) -> &'static str {
        match self {
            CrashKind::DropUnflushed => "drop",
            CrashKind::KeepUnflushed => "keep",
            CrashKind::Subset => "subset",
            CrashKind::TornOne => "torn",
            CrashKind::PrefixTorn => "prefix-torn",
        }
    }

    pub fn parse(text: &str) -> Option<CrashKind> {
        CRASH_KINDS.iter().copied().find(|kind| kind.name() == text)
    }
}

#[derive(Clone)]
pub struct Pending {
    pub block: u64,
    pub data: Block,
}

pub enum Event {
    Write(Pending),
    Flush,
}

pub struct Recording {
    pub cache_before: Vec<Pending>,
    pub undo: BTreeMap<u64, Block>,
    pub events: Vec<Event>,
}

pub struct Guard {
    pub free: Allocator,
    pub free_generation: u64,
    pub header_generation: u64,
}

impl Guard {
    pub fn new(free: Allocator, generation: u64) -> Guard {
        Guard {
            free,
            free_generation: generation,
            header_generation: generation,
        }
    }
}

pub fn allocator_contains(allocator: &Allocator, block: u64) -> bool {
    for (level, starts) in allocator.levels().iter().enumerate() {
        let size = 1u64 << level;
        if let Some(&start) = starts.range(..=block).next_back() {
            if start + size > block {
                return true;
            }
        }
    }
    false
}
pub fn free_ranges(allocator: &Allocator) -> Vec<(u64, u64)> {
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for (level, starts) in allocator.levels().iter().enumerate() {
        for &start in starts.iter() {
            ranges.push((start, start + (1u64 << level)));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::with_capacity(ranges.len());
    for (start, end) in ranges {
        match merged.last_mut() {
            Some(last) if last.1 >= start => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}


pub fn header_generation(bytes: &[u8]) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(&bytes[HEADER_GENERATION_OFFSET..HEADER_GENERATION_OFFSET + 8]);
    u64::from_le_bytes(word)
}

pub struct SimDisk {
    media: Vec<u8>,
    pub durability: Durability,
    cache: Vec<Pending>,
    latest: HashMap<u64, usize>,
    pub writes: u64,
    pub reads: u64,
    pub flushes: u64,
    crash_after: Option<u64>,
    pub inflight: Option<Pending>,
    pub crashed: bool,
    read_error_at: Option<u64>,
    pub read_errors: u64,
    write_error_at: Option<u64>,
    pub write_errors: u64,
    pub guard: Option<Guard>,
    pub violations: Vec<String>,
    pub recording: Option<Recording>,
}

impl SimDisk {
    pub fn new(media: Vec<u8>, durability: Durability) -> SimDisk {
        assert!(media.len() % BLOCK == 0);
        SimDisk {
            media,
            durability,
            cache: Vec::new(),
            latest: HashMap::new(),
            writes: 0,
            reads: 0,
            flushes: 0,
            crash_after: None,
            inflight: None,
            crashed: false,
            read_error_at: None,
            read_errors: 0,
            write_error_at: None,
            write_errors: 0,
            guard: None,
            violations: Vec::new(),
            recording: None,
        }
    }

    pub fn media(&self) -> &[u8] {
        &self.media
    }

    pub fn unflushed(&self) -> usize {
        self.cache.len()
    }

    pub fn arm_crash(&mut self, writes_from_now: u64) {
        self.crash_after = Some(self.writes + writes_from_now);
    }

    pub fn disarm_crash(&mut self) {
        self.crash_after = None;
    }

    pub fn arm_read_error(&mut self, reads_from_now: u64) {
        assert!(reads_from_now > 0);
        self.read_error_at = Some(self.reads + reads_from_now);
    }

    pub fn disarm_read_error(&mut self) {
        self.read_error_at = None;
    }

    pub fn arm_write_error(&mut self, writes_from_now: u64) {
        assert!(writes_from_now > 0);
        self.write_error_at = Some(self.writes + writes_from_now - 1);
    }

    pub fn disarm_write_error(&mut self) {
        self.write_error_at = None;
    }

    pub fn start_recording(&mut self) {
        assert!(self.recording.is_none());
        self.recording = Some(Recording {
            cache_before: self.cache.clone(),
            undo: BTreeMap::new(),
            events: Vec::new(),
        });
    }

    pub fn stop_recording(&mut self) -> Option<Recording> {
        self.recording.take()
    }

    fn media_block(&self, block: u64) -> &[u8] {
        let start = block as usize * BLOCK;
        &self.media[start..start + BLOCK]
    }

    fn set_media_block(&mut self, block: u64, data: &[u8]) {
        if let Some(recording) = self.recording.as_mut() {
            if !recording.undo.contains_key(&block) {
                let start = block as usize * BLOCK;
                recording
                    .undo
                    .insert(block, new_block(&self.media[start..start + BLOCK]));
            }
        }
        let start = block as usize * BLOCK;
        self.media[start..start + BLOCK].copy_from_slice(data);
    }

    fn write_back(&mut self) {
        let cache = std::mem::take(&mut self.cache);
        self.latest.clear();
        for pending in cache.iter() {
            self.set_media_block(pending.block, &pending.data[..]);
        }
        if let Some(recording) = self.recording.as_mut() {
            recording.events.push(Event::Flush);
        }
    }

    fn check_guard(&mut self, block: u64, data: &[u8]) {
        let Some(guard) = self.guard.as_mut() else {
            return;
        };
        if block < HEADER_RING {
            let generation = header_generation(data);
            if generation != guard.header_generation + 1 || generation % HEADER_RING != block {
                self.violations.push(format!(
                    "header write of generation {generation} to ring slot {block} after generation {}",
                    guard.header_generation
                ));
            }
            guard.header_generation = generation;
        } else if !allocator_contains(&guard.free, block) {
            self.violations.push(format!(
                "write to block {block} that is not free in the committed generation {}",
                guard.free_generation
            ));
        }
    }

    fn write_block(&mut self, block: u64, data: &[u8]) -> Result<()> {
        if self.crashed {
            return Err(Error::new(EIO));
        }
        if (block as usize + 1) * BLOCK > self.media.len() {
            self.violations
                .push(format!("write past the end of the disk at block {block}"));
            return Err(Error::new(EIO));
        }
        if self.write_error_at == Some(self.writes) {
            self.write_error_at = None;
            self.write_errors += 1;
            return Err(Error::new(EIO));
        }
        if let Some(limit) = self.crash_after {
            if self.writes >= limit {
                self.crashed = true;
                self.inflight = Some(Pending {
                    block,
                    data: new_block(data),
                });
                return Err(Error::new(EIO));
            }
        }
        self.writes += 1;
        self.check_guard(block, data);
        let pending = Pending {
            block,
            data: new_block(data),
        };
        if let Some(recording) = self.recording.as_mut() {
            recording.events.push(Event::Write(pending.clone()));
        }
        match self.durability {
            Durability::Ordered => self.set_media_block(block, data),
            Durability::Volatile => {
                self.latest.insert(block, self.cache.len());
                self.cache.push(pending);
                if self.cache.len() >= CACHE_LIMIT {
                    self.write_back();
                }
            }
        }
        Ok(())
    }

    pub fn into_lost(mut self) -> Lost {
        self.crashed = false;
        Lost {
            media: std::mem::take(&mut self.media),
            cache: std::mem::take(&mut self.cache),
            inflight: self.inflight.take(),
            writes: self.writes,
            flushes: self.flushes,
        }
    }
}

pub struct Lost {
    media: Vec<u8>,
    cache: Vec<Pending>,
    inflight: Option<Pending>,
    pub writes: u64,
    pub flushes: u64,
}

impl Lost {
    pub fn crash_image(&self, kind: CrashKind, rng: &mut Rng) -> Vec<u8> {
        image_after_crash(&self.media, &self.cache, self.inflight.as_ref(), kind, rng)
    }
}

thread_local! {
    static LOST: RefCell<Option<Lost>> = const { RefCell::new(None) };
}

pub fn take_lost() -> Option<Lost> {
    LOST.with(|lost| lost.borrow_mut().take())
}

impl Drop for SimDisk {
    fn drop(&mut self) {
        if self.crashed {
            self.crashed = false;
            let lost = Lost {
                media: std::mem::take(&mut self.media),
                cache: std::mem::take(&mut self.cache),
                inflight: self.inflight.take(),
                writes: self.writes,
                flushes: self.flushes,
            };
            LOST.with(|slot| *slot.borrow_mut() = Some(lost));
        }
    }
}

fn image_after_crash(
    media: &[u8],
    cache: &[Pending],
    inflight: Option<&Pending>,
    kind: CrashKind,
    rng: &mut Rng,
) -> Vec<u8> {
    let mut image = media.to_vec();
    let mut candidates: Vec<&Pending> = cache.iter().collect();
    if let Some(inflight) = inflight {
        candidates.push(inflight);
    }
    let unflushed = cache.len();
    let mut apply = |pending: &Pending, mask: u8| {
        let start = pending.block as usize * BLOCK;
        for sector in 0..SECTORS_PER_BLOCK {
            if mask & (1 << sector) != 0 {
                let offset = sector * SECTOR;
                image[start + offset..start + offset + SECTOR]
                    .copy_from_slice(&pending.data[offset..offset + SECTOR]);
            }
        }
    };
    match kind {
        CrashKind::DropUnflushed => {}
        CrashKind::KeepUnflushed => {
            for pending in &candidates[..unflushed] {
                apply(pending, 0xff);
            }
        }
        CrashKind::Subset => {
            for pending in &candidates[..unflushed] {
                if rng.chance(1, 2) {
                    apply(pending, 0xff);
                }
            }
        }
        CrashKind::TornOne | CrashKind::PrefixTorn => {
            if candidates.is_empty() {
                return image;
            }
            let torn = rng.below(candidates.len() as u64) as usize;
            let mask = rng.range(1, 254) as u8;
            let keep_before = kind == CrashKind::TornOne || rng.chance(1, 2);
            for (index, pending) in candidates.iter().enumerate() {
                if index == torn {
                    apply(pending, mask);
                } else if index < unflushed
                    && (kind == CrashKind::TornOne || (keep_before && index < torn))
                {
                    apply(pending, 0xff);
                }
            }
        }
    }
    image
}

impl Disk for SimDisk {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        if self.crashed {
            return Err(Error::new(EIO));
        }
        assert!(buffer.len() % BLOCK == 0);
        self.reads += 1;
        if self.read_error_at == Some(self.reads) {
            self.read_error_at = None;
            self.read_errors += 1;
            return Err(Error::new(EIO));
        }
        let count = buffer.len() / BLOCK;
        if (block as usize + count) * BLOCK > self.media.len() {
            return Err(Error::new(EIO));
        }
        for index in 0..count {
            let target = &mut buffer[index * BLOCK..(index + 1) * BLOCK];
            let current = block + index as u64;
            match self.latest.get(&current) {
                Some(&slot) => target.copy_from_slice(&self.cache[slot].data[..]),
                None => target.copy_from_slice(self.media_block(current)),
            }
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, block: u64, buffer: &[u8]) -> Result<usize> {
        assert!(buffer.len() % BLOCK == 0);
        for index in 0..buffer.len() / BLOCK {
            self.write_block(
                block + index as u64,
                &buffer[index * BLOCK..(index + 1) * BLOCK],
            )?;
        }
        Ok(buffer.len())
    }

    fn size(&mut self) -> Result<u64> {
        Ok(self.media.len() as u64)
    }

    fn flush(&mut self) -> Result<()> {
        if self.crashed {
            return Err(Error::new(EIO));
        }
        self.flushes += 1;
        if self.durability == Durability::Volatile {
            self.write_back();
        }
        Ok(())
    }
}

pub struct View<'a> {
    media: &'a [u8],
    undo: Option<&'a BTreeMap<u64, Block>>,
    overlay: BTreeMap<u64, Block>,
    pub refused_writes: u64,
}

impl<'a> View<'a> {
    pub fn new(
        media: &'a [u8],
        undo: Option<&'a BTreeMap<u64, Block>>,
        overlay: BTreeMap<u64, Block>,
    ) -> View<'a> {
        View {
            media,
            undo,
            overlay,
            refused_writes: 0,
        }
    }
}

impl Disk for View<'_> {
    unsafe fn read_at(&mut self, block: u64, buffer: &mut [u8]) -> Result<usize> {
        assert!(buffer.len() % BLOCK == 0);
        let count = buffer.len() / BLOCK;
        if (block as usize + count) * BLOCK > self.media.len() {
            return Err(Error::new(EIO));
        }
        for index in 0..count {
            let target = &mut buffer[index * BLOCK..(index + 1) * BLOCK];
            let current = block + index as u64;
            if let Some(data) = self.overlay.get(&current) {
                target.copy_from_slice(&data[..]);
            } else if let Some(data) = self.undo.and_then(|undo| undo.get(&current)) {
                target.copy_from_slice(&data[..]);
            } else {
                let start = current as usize * BLOCK;
                target.copy_from_slice(&self.media[start..start + BLOCK]);
            }
        }
        Ok(buffer.len())
    }

    unsafe fn write_at(&mut self, _block: u64, _buffer: &[u8]) -> Result<usize> {
        self.refused_writes += 1;
        Err(Error::new(EROFS))
    }

    fn size(&mut self) -> Result<u64> {
        Ok(self.media.len() as u64)
    }

    fn flush(&mut self) -> Result<()> {
        self.refused_writes += 1;
        Err(Error::new(EROFS))
    }
}

pub struct Boundary {
    pub label: String,
    pub overlay: BTreeMap<u64, Block>,
}

fn overlay_with(
    durable: &BTreeMap<u64, Block>,
    cache: &[Pending],
    keep: &dyn Fn(usize) -> Option<u8>,
    base: &dyn Fn(u64) -> Block,
) -> BTreeMap<u64, Block> {
    let mut overlay = durable.clone();
    for (index, pending) in cache.iter().enumerate() {
        let Some(mask) = keep(index) else {
            continue;
        };
        if mask == 0xff {
            overlay.insert(pending.block, pending.data.clone());
            continue;
        }
        let mut block = match overlay.get(&pending.block) {
            Some(existing) => existing.clone(),
            None => base(pending.block),
        };
        for sector in 0..SECTORS_PER_BLOCK {
            if mask & (1 << sector) != 0 {
                let offset = sector * SECTOR;
                block[offset..offset + SECTOR]
                    .copy_from_slice(&pending.data[offset..offset + SECTOR]);
            }
        }
        overlay.insert(pending.block, block);
    }
    overlay
}

pub fn boundaries(
    recording: &Recording,
    media: &[u8],
    durability: Durability,
    rng: &mut Rng,
) -> Option<Vec<Boundary>> {
    let base = |block: u64| -> Block {
        if let Some(data) = recording.undo.get(&block) {
            return data.clone();
        }
        let start = block as usize * BLOCK;
        new_block(&media[start..start + BLOCK])
    };
    let writes: Vec<&Pending> = recording
        .events
        .iter()
        .filter_map(|event| match event {
            Event::Write(pending) => Some(pending),
            Event::Flush => None,
        })
        .collect();
    let mut durable: BTreeMap<u64, Block> = BTreeMap::new();
    let mut cache: Vec<Pending> = recording.cache_before.clone();
    let mut result = Vec::new();
    let mut events = recording.events.iter().peekable();
    for boundary in 0..=writes.len() {
        if boundary > 0 {
            match events.next() {
                Some(Event::Write(pending)) => match durability {
                    Durability::Ordered => {
                        durable.insert(pending.block, pending.data.clone());
                    }
                    Durability::Volatile => cache.push(pending.clone()),
                },
                _ => unreachable!(),
            }
        }
        while let Some(Event::Flush) = events.peek() {
            events.next();
            for pending in cache.drain(..) {
                durable.insert(pending.block, pending.data);
            }
        }
        if cache.len() > ENUMERATION_CACHE_LIMIT {
            return None;
        }
        let next = writes.get(boundary).map(|pending| (*pending).clone());
        let mut candidates = cache.clone();
        if let Some(next) = next.as_ref() {
            candidates.push(next.clone());
        }
        let unflushed = cache.len();
        let label = |kind: &str| format!("write boundary {boundary} of {} ({kind})", writes.len());
        result.push(Boundary {
            label: label("drop unflushed"),
            overlay: durable.clone(),
        });
        if unflushed > 0 {
            result.push(Boundary {
                label: label("keep unflushed"),
                overlay: overlay_with(&durable, &cache, &|_| Some(0xff), &base),
            });
            let picks: Vec<bool> = (0..unflushed).map(|_| rng.chance(1, 2)).collect();
            result.push(Boundary {
                label: label("random subset of unflushed"),
                overlay: overlay_with(
                    &durable,
                    &cache,
                    &|index| picks[index].then_some(0xff),
                    &base,
                ),
            });
        }
        if !candidates.is_empty() {
            let torn = rng.below(candidates.len() as u64) as usize;
            let mask = rng.range(1, 254) as u8;
            result.push(Boundary {
                label: label("one write torn"),
                overlay: overlay_with(
                    &durable,
                    &candidates,
                    &|index| {
                        if index == torn {
                            Some(mask)
                        } else if index < unflushed {
                            Some(0xff)
                        } else {
                            None
                        }
                    },
                    &base,
                ),
            });
        }
    }
    Some(result)
}
