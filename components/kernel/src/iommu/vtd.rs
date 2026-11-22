use arrayvec::ArrayVec;
use core::{
    arch::asm,
    hint::spin_loop,
    ptr,
    sync::atomic::{fence, Ordering},
};

use crate::memory::{
    allocate_frame, deallocate_frame, map_device_memory, Frame, PhysicalAddress, RmmA, RmmArch,
    PAGE_SIZE,
};

use super::dmar::{RemappingUnit, ReservedRegion, MAX_REGISTER_PAGES};

const REG_VERSION: usize = 0x00;
const REG_CAP: usize = 0x08;
const REG_ECAP: usize = 0x10;
const REG_GCMD: usize = 0x18;
const REG_GSTS: usize = 0x1C;
const REG_RTADDR: usize = 0x20;
const REG_FSTS: usize = 0x34;
const REG_IQT: usize = 0x88;
const REG_IQA: usize = 0x90;

const GCMD_TE: u32 = 1 << 31;
const GCMD_SRTP: u32 = 1 << 30;
const GCMD_QIE: u32 = 1 << 26;
const GSTS_PERSISTENT: u32 = 0x96FF_FFFF;

const CAP_RWBF: u64 = 1 << 4;
const CAP_CM: u64 = 1 << 7;
const CAP_SAGAW_39: u64 = 1 << 1;
const CAP_SAGAW_48: u64 = 1 << 2;
const CAP_SLLPS_2M: u64 = 1 << 0;
const CAP_SLLPS_1G: u64 = 1 << 1;
const CAP_DWD: u64 = 1 << 54;
const CAP_DRD: u64 = 1 << 55;
const ECAP_C: u64 = 1 << 0;
const ECAP_QI: u64 = 1 << 1;

const FSTS_PFO: u32 = 1 << 0;
const FSTS_PPF: u32 = 1 << 1;
const FSTS_IQE: u32 = 1 << 4;
const FRCD_FAULT: u32 = 1 << 31;

const ROOT_ENTRIES: usize = 256;
const CONTEXT_ENTRIES: usize = 256;
const TABLE_ENTRIES: usize = 512;
const QUEUE_ENTRIES: usize = 256;
const MAX_POLL_SPINS: usize = 1 << 26;
const CACHE_LINE: usize = 64;

const ENTRY_PRESENT: u64 = 1;
const PTE_READ: u64 = 1 << 0;
const PTE_WRITE: u64 = 1 << 1;
const PTE_LARGE: u64 = 1 << 7;
const PTE_ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

const DESC_CONTEXT: u64 = 0x1;
const DESC_IOTLB: u64 = 0x2;
const DESC_WAIT: u64 = 0x5;
const DESC_GLOBAL: u64 = 1 << 4;
const DESC_DOMAIN: u64 = 2 << 4;
const DESC_DEVICE: u64 = 3 << 4;
const DESC_IOTLB_DRAIN_READ: u64 = 1 << 7;
const DESC_IOTLB_DRAIN_WRITE: u64 = 1 << 6;
const DESC_WAIT_STATUS_WRITE: u64 = 1 << 5;

const INTERRUPT_START: u64 = 0xFEE0_0000;
const INTERRUPT_END: u64 = 0xFF00_0000;

const PAGE_4K: u64 = 1 << 12;
const PAGE_2M: u64 = 1 << 21;
const PAGE_1G: u64 = 1 << 30;

pub const IDENTITY_DOMAIN: u16 = 1;

#[derive(Debug)]
pub enum Error {
    NoQueuedInvalidation,
    WriteBufferFlushing,
    NoSupportedDepth(u64),
    OutOfMemory,
    Timeout(&'static str),
    QueueError,
    AddressTooWide(u64),
    Broken,
    AlreadyMapped(u64),
    NotMapped(usize),
    NoDomainId(u16),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoQueuedInvalidation => write!(f, "no queued invalidation"),
            Self::WriteBufferFlushing => write!(f, "requires write-buffer flushing"),
            Self::NoSupportedDepth(sagaw) => {
                write!(f, "supports no 3- or 4-level table (SAGAW {sagaw:#x})")
            }
            Self::OutOfMemory => write!(f, "out of memory for translation tables"),
            Self::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Self::QueueError => write!(f, "invalidation queue error"),
            Self::AddressTooWide(address) => {
                write!(f, "address {address:#x} beyond the table depth")
            }
            Self::Broken => write!(f, "unit stopped after an earlier timeout"),
            Self::AlreadyMapped(address) => write!(f, "device address {address:#x} already mapped"),
            Self::NotMapped(count) => write!(f, "{count} pages were not mapped"),
            Self::NoDomainId(id) => write!(f, "domain id {id} beyond what the unit supports"),
        }
    }
}

pub struct Fault {
    pub source: u16,
    pub address: u64,
    pub write: bool,
    pub reason: u8,
}

struct Registers {
    base: usize,
    len: usize,
}

impl Registers {
    fn check(&self, offset: usize, width: usize) {
        assert!(offset.is_multiple_of(width) && offset + width <= self.len);
    }
    fn read32(&self, offset: usize) -> u32 {
        self.check(offset, 4);
        unsafe { ptr::read_volatile((self.base + offset) as *const u32) }
    }
    fn write32(&self, offset: usize, value: u32) {
        self.check(offset, 4);
        unsafe { ptr::write_volatile((self.base + offset) as *mut u32, value) }
    }
    fn read64(&self, offset: usize) -> u64 {
        self.check(offset, 8);
        unsafe { ptr::read_volatile((self.base + offset) as *const u64) }
    }
    fn write64(&self, offset: usize, value: u64) {
        self.check(offset, 8);
        unsafe { ptr::write_volatile((self.base + offset) as *mut u64, value) }
    }
}

fn table_base(frame: Frame) -> *mut u64 {
    RmmA::phys_to_virt(frame.base()).data() as *mut u64
}

fn read_word(frame: Frame, index: usize) -> u64 {
    assert!(index < TABLE_ENTRIES);
    unsafe { ptr::read_volatile(table_base(frame).add(index)) }
}

fn write_word(frame: Frame, index: usize, value: u64) {
    assert!(index < TABLE_ENTRIES);
    unsafe { ptr::write_volatile(table_base(frame).add(index), value) }
}

fn frame_of(entry: u64) -> Frame {
    Frame::containing(PhysicalAddress::new((entry & PTE_ADDR_MASK) as usize))
}

pub struct Domain {
    pub id: u16,
    top: Frame,
}

enum Slot {
    At(Frame, usize),
    Covered,
    Missing,
}

pub struct Unit {
    registers: Registers,
    pub register_base: u64,
    pub segment: u16,
    pub version: u32,
    pub cap: u64,
    pub ecap: u64,
    pub levels: u8,
    address_width_field: u64,
    coherent: bool,
    pub caching_mode: bool,
    large_2m: bool,
    large_1g: bool,
    drain: u64,
    root: Frame,
    shared_context: Frame,
    bus_context: [Option<Frame>; ROOT_ENTRIES],
    identity: Domain,
    queue: Frame,
    queue_tail: usize,
    status: Frame,
    sequence: u32,
    fault_offset: usize,
    fault_records: usize,
    broken: bool,
}

pub struct Enabled {
    pub unit: Unit,
    pub identity_entries: usize,
    pub firmware_had_translation: bool,
}

impl Unit {
    pub fn enable_identity(
        description: &RemappingUnit,
        reserved: &[ReservedRegion],
    ) -> Result<Enabled, Error> {
        let first_page = unsafe {
            map_device_memory(
                PhysicalAddress::new(description.register_base as usize),
                PAGE_SIZE,
            )
        };
        let mut registers = Registers {
            base: first_page.data(),
            len: PAGE_SIZE,
        };
        let version = registers.read32(REG_VERSION);
        let cap = registers.read64(REG_CAP);
        let ecap = registers.read64(REG_ECAP);

        let fault_offset = (((cap >> 24) & 0x3FF) as usize) * 16;
        let fault_records = (((cap >> 40) & 0xFF) as usize) + 1;
        let needed = (fault_offset + fault_records * 16)
            .div_ceil(PAGE_SIZE)
            .max(description.register_pages)
            .clamp(1, MAX_REGISTER_PAGES);
        if needed > 1 {
            unsafe {
                map_device_memory(
                    PhysicalAddress::new(description.register_base as usize + PAGE_SIZE),
                    (needed - 1) * PAGE_SIZE,
                );
            }
        }
        registers.len = needed * PAGE_SIZE;
        let fault_records = if fault_offset + fault_records * 16 <= registers.len {
            fault_records
        } else {
            0
        };

        if ecap & ECAP_QI == 0 {
            return Err(Error::NoQueuedInvalidation);
        }
        if cap & CAP_RWBF != 0 {
            return Err(Error::WriteBufferFlushing);
        }
        let sagaw = (cap >> 8) & 0x1F;
        let (levels, address_width_field) = if sagaw & CAP_SAGAW_48 != 0 {
            (4, 2)
        } else if sagaw & CAP_SAGAW_39 != 0 {
            (3, 1)
        } else {
            return Err(Error::NoSupportedDepth(sagaw));
        };
        let sllps = (cap >> 34) & 0xF;
        let drain = if cap & CAP_DRD != 0 {
            DESC_IOTLB_DRAIN_READ
        } else {
            0
        } | if cap & CAP_DWD != 0 {
            DESC_IOTLB_DRAIN_WRITE
        } else {
            0
        };

        let mut tables = TableAllocator {
            coherent: ecap & ECAP_C != 0,
        };
        let root = tables.allocate()?;
        let shared_context = tables.allocate()?;
        let identity_top = tables.allocate()?;
        let queue = tables.allocate()?;
        let status = tables.allocate()?;

        let mut unit = Unit {
            registers,
            register_base: description.register_base,
            segment: description.segment,
            version,
            cap,
            ecap,
            levels,
            address_width_field,
            coherent: ecap & ECAP_C != 0,
            caching_mode: cap & CAP_CM != 0,
            large_2m: sllps & CAP_SLLPS_2M != 0,
            large_1g: sllps & CAP_SLLPS_1G != 0,
            drain,
            root,
            shared_context,
            bus_context: [None; ROOT_ENTRIES],
            identity: Domain {
                id: IDENTITY_DOMAIN,
                top: identity_top,
            },
            queue,
            queue_tail: 0,
            status,
            sequence: 0,
            fault_offset,
            fault_records,
            broken: false,
        };

        let mut identity_entries = 0;
        for area in crate::memory::areas() {
            let start = area.base.data() as u64;
            identity_entries +=
                unit.map_identity(start, start.saturating_add(area.size as u64), true)?;
        }
        for region in reserved
            .iter()
            .filter(|region| region.segment == description.segment)
        {
            identity_entries += unit.map_identity(region.base, region.end, false)?;
        }

        let (context_low, context_high) = unit.context_words(IDENTITY_DOMAIN, identity_top);
        for device_function in 0..CONTEXT_ENTRIES {
            write_word(shared_context, device_function * 2 + 1, context_high);
            write_word(shared_context, device_function * 2, context_low);
        }
        unit.flush_page(shared_context);
        for bus in 0..ROOT_ENTRIES {
            write_word(root, bus * 2 + 1, 0);
            write_word(
                root,
                bus * 2,
                (shared_context.base().data() as u64 & PTE_ADDR_MASK) | ENTRY_PRESENT,
            );
        }
        unit.flush_page(root);

        let initial = unit.registers.read32(REG_GSTS);
        let firmware_had_translation = initial & GCMD_TE != 0;
        if firmware_had_translation {
            unit.command(GCMD_TE, false, "translation off")?;
        }
        if initial & GCMD_QIE != 0 {
            unit.command(GCMD_QIE, false, "queued invalidation off")?;
        }

        unit.registers.write64(REG_IQT, 0);
        unit.registers
            .write64(REG_IQA, queue.base().data() as u64 & PTE_ADDR_MASK);
        unit.command(GCMD_QIE, true, "queued invalidation on")?;

        unit.registers
            .write64(REG_RTADDR, root.base().data() as u64 & PTE_ADDR_MASK);
        unit.one_shot(GCMD_SRTP, "root table pointer")?;

        unit.submit(&[
            (DESC_CONTEXT | DESC_GLOBAL, 0),
            (DESC_IOTLB | DESC_GLOBAL | unit.drain, 0),
        ])?;

        unit.command(GCMD_TE, true, "translation on")?;

        Ok(Enabled {
            unit,
            identity_entries,
            firmware_had_translation,
        })
    }

    pub fn page_sizes(&self) -> &'static str {
        match (self.large_2m, self.large_1g) {
            (true, true) => "4 KiB, 2 MiB and 1 GiB",
            (true, false) => "4 KiB and 2 MiB",
            (false, _) => "4 KiB",
        }
    }

    fn context_words(&self, id: u16, top: Frame) -> (u64, u64) {
        (
            (top.base().data() as u64 & PTE_ADDR_MASK) | ENTRY_PRESENT,
            self.address_width_field | (u64::from(id) << 8),
        )
    }

    pub fn domain_ids(&self) -> u32 {
        1u32 << (4 + 2 * (self.cap & 0x7) as u32)
    }

    pub fn attach(&mut self, source: u16, id: u16) -> Result<Domain, Error> {
        if self.broken {
            return Err(Error::Broken);
        }
        if id <= IDENTITY_DOMAIN || u32::from(id) >= self.domain_ids() {
            return Err(Error::NoDomainId(id));
        }
        let mut tables = TableAllocator {
            coherent: self.coherent,
        };
        let top = tables.allocate()?;
        let context = match self.private_context(usize::from(source >> 8)) {
            Ok(context) => context,
            Err(err) => {
                unsafe { deallocate_frame(top) };
                return Err(err);
            }
        };
        self.replace_context(context, source, IDENTITY_DOMAIN, id, top)?;
        Ok(Domain { id, top })
    }

    pub fn detach(&mut self, source: u16, domain: Domain) -> Result<usize, Error> {
        let context = self.bus_context[usize::from(source >> 8)]
            .expect("a device with its own domain has its own context table");
        self.replace_context(
            context,
            source,
            domain.id,
            IDENTITY_DOMAIN,
            self.identity.top,
        )?;
        Ok(self.free_domain(domain))
    }

    fn private_context(&mut self, bus: usize) -> Result<Frame, Error> {
        assert!(bus < ROOT_ENTRIES);
        if let Some(context) = self.bus_context[bus] {
            return Ok(context);
        }
        let mut tables = TableAllocator {
            coherent: self.coherent,
        };
        let context = tables.allocate()?;
        for word in 0..CONTEXT_ENTRIES * 2 {
            write_word(context, word, read_word(self.shared_context, word));
        }
        self.flush_page(context);
        write_word(
            self.root,
            bus * 2,
            (context.base().data() as u64 & PTE_ADDR_MASK) | ENTRY_PRESENT,
        );
        self.flush_word(self.root, bus * 2);
        self.bus_context[bus] = Some(context);
        self.submit(&[(DESC_CONTEXT | DESC_GLOBAL, 0)])?;
        Ok(context)
    }

    fn replace_context(
        &mut self,
        context: Frame,
        source: u16,
        old: u16,
        new: u16,
        top: Frame,
    ) -> Result<(), Error> {
        let index = usize::from(source & 0xFF) * 2;
        let device = u64::from(source) << 32;
        write_word(context, index, 0);
        self.flush_word(context, index);
        self.submit(&[
            (
                DESC_CONTEXT | DESC_DEVICE | (u64::from(old) << 16) | device,
                0,
            ),
            (
                DESC_IOTLB | DESC_DOMAIN | self.drain | (u64::from(old) << 16),
                0,
            ),
        ])?;
        let (low, high) = self.context_words(new, top);
        write_word(context, index + 1, high);
        self.flush_word(context, index + 1);
        fence(Ordering::SeqCst);
        write_word(context, index, low);
        self.flush_word(context, index);
        let tag = if self.caching_mode { 0 } else { u64::from(new) };
        self.submit(&[
            (DESC_CONTEXT | DESC_DEVICE | (tag << 16) | device, 0),
            (
                DESC_IOTLB | DESC_DOMAIN | self.drain | (u64::from(new) << 16),
                0,
            ),
        ])
    }

    pub fn map_pages(&mut self, domain: &Domain, base: u64, count: usize) -> Result<(), Error> {
        if self.broken {
            return Err(Error::Broken);
        }
        let end = base.saturating_add(count as u64 * PAGE_4K);
        if end > self.address_limit() {
            return Err(Error::AddressTooWide(end));
        }
        for page in 0..count {
            let address = base + page as u64 * PAGE_4K;
            if let Err(err) = self.map_page(domain.top, address) {
                for undo in 0..page {
                    self.clear_page(domain.top, base + undo as u64 * PAGE_4K);
                }
                self.invalidate_domain(domain.id)?;
                return Err(err);
            }
        }
        if self.caching_mode {
            self.invalidate_domain(domain.id)?;
        }
        Ok(())
    }

    pub fn unmap_pages(&mut self, domain: &Domain, base: u64, count: usize) -> Result<(), Error> {
        let mut missing = 0;
        for page in 0..count {
            if !self.clear_page(domain.top, base + page as u64 * PAGE_4K) {
                missing += 1;
            }
        }
        self.invalidate_domain(domain.id)?;
        if missing > 0 {
            return Err(Error::NotMapped(missing));
        }
        Ok(())
    }

    fn invalidate_domain(&mut self, id: u16) -> Result<(), Error> {
        self.submit(&[(
            DESC_IOTLB | DESC_DOMAIN | self.drain | (u64::from(id) << 16),
            0,
        )])
    }

    fn map_page(&mut self, top: Frame, address: u64) -> Result<(), Error> {
        match self.slot(top, address, 1, true)? {
            Slot::At(table, index) => {
                if read_word(table, index) & (PTE_READ | PTE_WRITE) != 0 {
                    return Err(Error::AlreadyMapped(address));
                }
                write_word(
                    table,
                    index,
                    (address & PTE_ADDR_MASK) | PTE_READ | PTE_WRITE,
                );
                self.flush_word(table, index);
                Ok(())
            }
            Slot::Covered => Err(Error::AlreadyMapped(address)),
            Slot::Missing => unreachable!("a walk that creates tables finds no hole"),
        }
    }

    fn clear_page(&mut self, top: Frame, address: u64) -> bool {
        match self.slot(top, address, 1, false) {
            Ok(Slot::At(table, index)) if read_word(table, index) & (PTE_READ | PTE_WRITE) != 0 => {
                write_word(table, index, 0);
                self.flush_word(table, index);
                true
            }
            _ => false,
        }
    }

    fn slot(&mut self, top: Frame, address: u64, level: u8, create: bool) -> Result<Slot, Error> {
        let mut table = top;
        let mut current = self.levels;
        for _ in 0..self.levels {
            let index = ((address >> (12 + 9 * u32::from(current - 1))) & 0x1FF) as usize;
            if current == level {
                return Ok(Slot::At(table, index));
            }
            let entry = read_word(table, index);
            if entry & (PTE_READ | PTE_WRITE) == 0 {
                if !create {
                    return Ok(Slot::Missing);
                }
                let mut tables = TableAllocator {
                    coherent: self.coherent,
                };
                let next = tables.allocate()?;
                write_word(
                    table,
                    index,
                    (next.base().data() as u64 & PTE_ADDR_MASK) | PTE_READ | PTE_WRITE,
                );
                self.flush_word(table, index);
                table = next;
            } else if entry & PTE_LARGE != 0 {
                return Ok(Slot::Covered);
            } else {
                table = frame_of(entry);
            }
            current -= 1;
        }
        unreachable!("page-table walk ran past level 1");
    }

    fn free_domain(&mut self, domain: Domain) -> usize {
        let mut stack: ArrayVec<(Frame, u8, usize), 4> = ArrayVec::new();
        stack.push((domain.top, self.levels, 0));
        let mut freed = 0;
        while let Some(last) = stack.last_mut() {
            let (table, level, index) = *last;
            if index == TABLE_ENTRIES {
                stack.pop();
                unsafe { deallocate_frame(table) };
                freed += 1;
                continue;
            }
            last.2 += 1;
            if level > 1 {
                let entry = read_word(table, index);
                if entry & (PTE_READ | PTE_WRITE) != 0 && entry & PTE_LARGE == 0 {
                    stack.push((frame_of(entry), level - 1, 0));
                }
            }
        }
        freed
    }

    fn address_limit(&self) -> u64 {
        1u64 << (12 + 9 * u32::from(self.levels))
    }

    fn map_identity(&mut self, start: u64, end: u64, large: bool) -> Result<usize, Error> {
        let mut address = start.next_multiple_of(PAGE_4K);
        let end = end - end % PAGE_4K;
        if end > self.address_limit() {
            return Err(Error::AddressTooWide(end));
        }
        let mut entries = 0;
        while address < end {
            if (INTERRUPT_START..INTERRUPT_END).contains(&address) {
                address = INTERRUPT_END;
                continue;
            }
            let fits = |size: u64| {
                address.is_multiple_of(size)
                    && address + size <= end
                    && (address + size <= INTERRUPT_START || address >= INTERRUPT_END)
            };
            let (size, level) = if large && self.large_1g && fits(PAGE_1G) {
                (PAGE_1G, 3)
            } else if large && self.large_2m && fits(PAGE_2M) {
                (PAGE_2M, 2)
            } else {
                (PAGE_4K, 1)
            };
            if self.map_identity_page(address, level)? {
                entries += 1;
            }
            address += size;
        }
        Ok(entries)
    }

    fn map_identity_page(&mut self, address: u64, level: u8) -> Result<bool, Error> {
        match self.slot(self.identity.top, address, level, true)? {
            Slot::At(table, index) => {
                if read_word(table, index) & (PTE_READ | PTE_WRITE) != 0 {
                    return Ok(false);
                }
                let large = if level > 1 { PTE_LARGE } else { 0 };
                write_word(
                    table,
                    index,
                    (address & PTE_ADDR_MASK) | PTE_READ | PTE_WRITE | large,
                );
                self.flush_word(table, index);
                Ok(true)
            }
            Slot::Covered => Ok(false),
            Slot::Missing => unreachable!("a walk that creates tables finds no hole"),
        }
    }

    fn flush_word(&self, frame: Frame, index: usize) {
        if !self.coherent {
            let address = table_base(frame) as usize + index * 8;
            unsafe { asm!("clflush [{}]", in(reg) address, options(nostack)) };
            fence(Ordering::SeqCst);
        }
    }

    fn flush_page(&self, frame: Frame) {
        if !self.coherent {
            flush_lines(frame);
        }
    }

    fn command(&mut self, bit: u32, on: bool, what: &'static str) -> Result<(), Error> {
        let status = self.registers.read32(REG_GSTS) & GSTS_PERSISTENT;
        let value = if on { status | bit } else { status & !bit };
        fence(Ordering::SeqCst);
        self.registers.write32(REG_GCMD, value);
        for _ in 0..MAX_POLL_SPINS {
            if (self.registers.read32(REG_GSTS) & bit != 0) == on {
                return Ok(());
            }
            spin_loop();
        }
        self.broken = true;
        Err(Error::Timeout(what))
    }

    fn one_shot(&mut self, bit: u32, what: &'static str) -> Result<(), Error> {
        let status = self.registers.read32(REG_GSTS) & GSTS_PERSISTENT;
        fence(Ordering::SeqCst);
        self.registers.write32(REG_GCMD, status | bit);
        for _ in 0..MAX_POLL_SPINS {
            if self.registers.read32(REG_GSTS) & bit != 0 {
                return Ok(());
            }
            spin_loop();
        }
        self.broken = true;
        Err(Error::Timeout(what))
    }

    fn submit(&mut self, descriptors: &[(u64, u64)]) -> Result<(), Error> {
        if self.broken {
            return Err(Error::Broken);
        }
        assert!(descriptors.len() + 1 < QUEUE_ENTRIES);
        self.sequence = self.sequence.wrapping_add(1).max(1);
        let status_address = self.status.base().data() as u64;
        let wait = (
            DESC_WAIT | DESC_WAIT_STATUS_WRITE | (u64::from(self.sequence) << 32),
            status_address,
        );
        for &(low, high) in descriptors.iter().chain(core::iter::once(&wait)) {
            write_word(self.queue, self.queue_tail * 2, low);
            write_word(self.queue, self.queue_tail * 2 + 1, high);
            self.flush_word(self.queue, self.queue_tail * 2);
            self.queue_tail = (self.queue_tail + 1) % QUEUE_ENTRIES;
        }
        fence(Ordering::SeqCst);
        self.registers
            .write64(REG_IQT, (self.queue_tail as u64) << 4);
        let status = RmmA::phys_to_virt(self.status.base()).data() as *const u32;
        for _ in 0..MAX_POLL_SPINS {
            if unsafe { ptr::read_volatile(status) } == self.sequence {
                return Ok(());
            }
            if self.registers.read32(REG_FSTS) & FSTS_IQE != 0 {
                self.broken = true;
                return Err(Error::QueueError);
            }
            spin_loop();
        }
        self.broken = true;
        Err(Error::Timeout("an invalidation"))
    }

    pub fn collect_faults(&mut self, mut record: impl FnMut(Fault)) -> u64 {
        let status = self.registers.read32(REG_FSTS);
        let mut overflow = 0;
        if status & FSTS_PPF != 0 && self.fault_records > 0 {
            let mut index = ((status >> 8) & 0xFF) as usize % self.fault_records;
            for _ in 0..self.fault_records {
                let offset = self.fault_offset + index * 16;
                let high = self.registers.read64(offset + 8);
                if high & (1 << 63) == 0 {
                    break;
                }
                let low = self.registers.read64(offset);
                record(Fault {
                    source: (high & 0xFFFF) as u16,
                    address: low & !0xFFF,
                    write: high & (1 << 62) == 0,
                    reason: ((high >> 32) & 0xFF) as u8,
                });
                self.registers.write32(offset + 12, FRCD_FAULT);
                index = (index + 1) % self.fault_records;
            }
        }
        if status & FSTS_PFO != 0 {
            self.registers.write32(REG_FSTS, FSTS_PFO);
            overflow = 1;
        }
        overflow
    }
}

fn flush_lines(frame: Frame) {
    let base = table_base(frame) as usize;
    for offset in (0..PAGE_SIZE).step_by(CACHE_LINE) {
        unsafe { asm!("clflush [{}]", in(reg) base + offset, options(nostack)) };
    }
    fence(Ordering::SeqCst);
}

struct TableAllocator {
    coherent: bool,
}

impl TableAllocator {
    fn allocate(&mut self) -> Result<Frame, Error> {
        let frame = allocate_frame(true).ok_or(Error::OutOfMemory)?;
        if !self.coherent {
            flush_lines(frame);
        }
        Ok(frame)
    }
}
