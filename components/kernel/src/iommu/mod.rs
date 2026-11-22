use alloc::{string::String, vec::Vec};
use core::fmt::{self, Write};

use spin::Mutex;

use crate::{
    memory::{deallocate_contiguous, deallocate_frame, get_page_info, Frame, PAGE_SIZE},
    syscall::error::{Error, Result, EBADF, EBUSY, EINVAL, EIO, ENFILE, ENOENT, ENOMEM, ENOSPC},
};

#[cfg(target_arch = "x86_64")]
mod dmar;
pub mod handle;
#[cfg(target_arch = "x86_64")]
mod vtd;

pub const FAULT_LOG_LIMIT: u64 = 64;
pub const MAX_HANDLES: usize = 64;
pub const MAX_PINS_PER_HANDLE: usize = 256;
pub const MAX_PIN_PAGES: usize = 1024;
#[cfg(target_arch = "x86_64")]
const FIRST_HANDLE_DOMAIN: u16 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RequesterId {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl RequesterId {
    pub fn parse(name: &str) -> Option<Self> {
        let (segment, rest) = name.split_once('-')?;
        let (bus, rest) = rest.split_once('-')?;
        let (device, function) = rest.split_once('.')?;
        let fields = [(segment, 4), (bus, 2), (device, 2), (function, 1)];
        if !fields.iter().all(|(field, len)| {
            field.len() == *len && field.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return None;
        }
        let device = u8::from_str_radix(device, 16).ok()?;
        let function = u8::from_str_radix(function, 16).ok()?;
        if device > 0x1F || function > 0x7 {
            return None;
        }
        Some(Self {
            segment: u16::from_str_radix(segment, 16).ok()?,
            bus: u8::from_str_radix(bus, 16).ok()?,
            device,
            function,
        })
    }

    pub fn source_id(self) -> u16 {
        u16::from(self.bus) << 8 | u16::from(self.device) << 3 | u16::from(self.function)
    }
}

impl fmt::Display for RequesterId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

#[derive(Clone, Copy)]
struct Pin {
    base: Frame,
    count: usize,
}

struct Handle {
    rid: RequesterId,
    pins: Vec<Pin>,
    reserved: usize,
    pages: usize,
    faults: u64,
    #[cfg(target_arch = "x86_64")]
    domain: Option<vtd::Domain>,
}

impl Handle {
    fn domain_id(&self) -> Option<u16> {
        #[cfg(target_arch = "x86_64")]
        return self.domain.as_ref().map(|domain| domain.id);
        #[cfg(not(target_arch = "x86_64"))]
        return None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Off,
    Identity,
}

struct State {
    backend: &'static str,
    mode: Mode,
    #[cfg(target_arch = "x86_64")]
    unit: Option<vtd::Unit>,
    faults: u64,
    overflows: u64,
    pinned_forever: usize,
    handles: [Option<Handle>; MAX_HANDLES],
}

static STATE: Mutex<State> = Mutex::new(State {
    backend: "none",
    mode: Mode::Off,
    #[cfg(target_arch = "x86_64")]
    unit: None,
    faults: 0,
    overflows: 0,
    pinned_forever: 0,
    handles: [const { None }; MAX_HANDLES],
});

fn boot_switch(env: &[u8]) -> Option<&str> {
    core::str::from_utf8(env)
        .unwrap_or("")
        .lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, _)| *name == "IOMMU")
        .map(|(_, value)| value)
}

pub fn init(env: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    {
        let table = match dmar::find() {
            Ok(Some(table)) => table,
            Ok(None) => {
                info!("iommu: no DMAR table, backend none");
                return;
            }
            Err(err) => {
                error!("iommu: DMAR table rejected: {err}");
                return;
            }
        };
        let mut state = STATE.lock();
        state.backend = "vt-d";
        info!(
            "iommu: DMAR host address width {}, flags {:#x}, {} unit(s), {} reserved region(s)",
            table.host_address_width,
            table.flags,
            table.units.len() + table.skipped_units,
            table.reserved.len() + table.skipped_reserved
        );
        if table.skipped_reserved > 0 {
            error!(
                "iommu: {} reserved regions beyond {}, translation stays off",
                table.skipped_reserved,
                dmar::MAX_RESERVED_REGIONS
            );
            return;
        }
        match boot_switch(env) {
            Some("off") => {
                info!("iommu: IOMMU=off in the boot environment, translation stays off");
                return;
            }
            Some(other) if other != "identity" => {
                warn!("iommu: IOMMU={other} not understood, using identity");
            }
            _ => {}
        }
        if table.flags & dmar::FLAG_OS_OPT_OUT != 0 {
            warn!("iommu: the firmware asks the OS not to enable remapping, translation stays off");
            return;
        }
        let Some(first) = table.units.first() else {
            warn!("iommu: DMAR lists no remapping unit, translation stays off");
            return;
        };
        if table.units.len() + table.skipped_units > 1 {
            warn!("iommu: only the first remapping unit is used, devices behind the others are not translated");
        }
        match vtd::Unit::enable_identity(first, &table.reserved) {
            Ok(enabled) => {
                let unit = &enabled.unit;
                info!(
                    "iommu: VT-d {}.{} at {:#x}: translation on, mode identity, {}-level tables, pages {}, {} identity entries{}{}",
                    (unit.version >> 4) & 0xF,
                    unit.version & 0xF,
                    unit.register_base,
                    unit.levels,
                    unit.page_sizes(),
                    enabled.identity_entries,
                    if unit.caching_mode { ", caching mode" } else { "" },
                    if enabled.firmware_had_translation {
                        ", replaced the firmware's tables"
                    } else {
                        ""
                    }
                );
                state.unit = Some(enabled.unit);
                state.mode = Mode::Identity;
            }
            Err(err) => {
                error!(
                    "iommu: VT-d unit at {:#x} not enabled: {err}",
                    first.register_base
                );
            }
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = env;
}

impl State {
    fn collect_faults(&mut self) {
        #[cfg(target_arch = "x86_64")]
        if let Some(unit) = self.unit.as_mut() {
            let segment = unit.segment;
            let handles = &mut self.handles;
            let mut faults = self.faults;
            let overflows = unit.collect_faults(|fault| {
                let owner = handles.iter_mut().flatten().find(|handle| {
                    handle.domain.is_some()
                        && handle.rid.segment == segment
                        && handle.rid.source_id() == fault.source
                });
                let outside = if owner.is_some() {
                    "outside the device's DMA handle"
                } else {
                    "outside the one-to-one map"
                };
                if let Some(owner) = owner {
                    owner.faults += 1;
                }
                if faults < FAULT_LOG_LIMIT {
                    warn!(
                        "iommu: refused {} by {:04x}:{:02x}:{:02x}.{} at {:#x}, {outside} (reason {:#x})",
                        if fault.write { "write" } else { "read" },
                        segment,
                        fault.source >> 8,
                        (fault.source >> 3) & 0x1F,
                        fault.source & 0x7,
                        fault.address,
                        fault.reason
                    );
                } else if faults == FAULT_LOG_LIMIT {
                    warn!("iommu: further refused requests are counted, not logged");
                }
                faults += 1;
            });
            self.faults = faults;
            self.overflows += overflows;
        }
    }

    fn handle_mut(&mut self, slot: usize) -> Result<&mut Handle> {
        self.handles
            .get_mut(slot)
            .and_then(Option::as_mut)
            .ok_or(Error::new(EBADF))
    }
}

#[cfg(target_arch = "x86_64")]
fn errno(err: &vtd::Error) -> Error {
    Error::new(match err {
        vtd::Error::OutOfMemory => ENOMEM,
        vtd::Error::NoDomainId(_) => ENFILE,
        _ => EIO,
    })
}

pub fn open(rid: RequesterId) -> Result<usize> {
    let mut guard = STATE.lock();
    let state = &mut *guard;
    if state
        .handles
        .iter()
        .flatten()
        .any(|handle| handle.rid == rid)
    {
        return Err(Error::new(EBUSY));
    }
    let slot = state
        .handles
        .iter()
        .position(Option::is_none)
        .ok_or(Error::new(ENFILE))?;
    #[cfg(target_arch = "x86_64")]
    let domain = match state.unit.as_mut() {
        Some(unit) if state.mode != Mode::Off && unit.segment == rid.segment => {
            let id = FIRST_HANDLE_DOMAIN + slot as u16;
            let domain = unit.attach(rid.source_id(), id).map_err(|err| {
                error!("iommu: no domain for {rid}: {err}");
                errno(&err)
            })?;
            info!("iommu: {rid} left the one-to-one map for domain {id}, empty");
            Some(domain)
        }
        _ => None,
    };
    state.handles[slot] = Some(Handle {
        rid,
        pins: Vec::with_capacity(MAX_PINS_PER_HANDLE),
        reserved: 0,
        pages: 0,
        faults: 0,
        #[cfg(target_arch = "x86_64")]
        domain,
    });
    Ok(slot)
}

pub fn reserve(slot: usize) -> Result<()> {
    let mut state = STATE.lock();
    let handle = state.handle_mut(slot)?;
    if handle.pins.len() + handle.reserved >= MAX_PINS_PER_HANDLE {
        return Err(Error::new(ENOSPC));
    }
    handle.reserved += 1;
    Ok(())
}

pub fn unreserve(slot: usize) {
    if let Ok(handle) = STATE.lock().handle_mut(slot) {
        handle.reserved = handle.reserved.saturating_sub(1);
    }
}

pub fn map(slot: usize, base: Frame, count: usize) -> Result<u64> {
    assert!(count > 0 && count <= MAX_PIN_PAGES);
    let mut guard = STATE.lock();
    let state = &mut *guard;
    let handle = state
        .handles
        .get_mut(slot)
        .and_then(Option::as_mut)
        .ok_or(Error::new(EBADF))?;
    assert!(handle.reserved > 0, "map without a reservation");
    handle.reserved -= 1;
    let address = base.base().data() as u64;
    #[cfg(target_arch = "x86_64")]
    if let (Some(unit), Some(domain)) = (state.unit.as_mut(), handle.domain.as_ref()) {
        unit.map_pages(domain, address, count).map_err(|err| {
            error!(
                "iommu: mapping {count} pages for {} failed: {err}",
                handle.rid
            );
            errno(&err)
        })?;
    }
    assert!(handle.pins.len() < MAX_PINS_PER_HANDLE);
    handle.pins.push(Pin { base, count });
    handle.pages += count;
    Ok(address)
}

pub fn address(slot: usize, physical: u64) -> Result<u64> {
    let mut state = STATE.lock();
    let handle = state.handle_mut(slot)?;
    let inside = handle.pins.iter().any(|pin| {
        let start = pin.base.base().data() as u64;
        physical >= start && physical - start < (pin.count * PAGE_SIZE) as u64
    });
    if inside {
        Ok(physical)
    } else {
        Err(Error::new(ENOENT))
    }
}

pub fn release(slot: usize, address: u64, length: u64) -> Result<()> {
    if length == 0 || length % PAGE_SIZE as u64 != 0 {
        return Err(Error::new(EINVAL));
    }
    let mut guard = STATE.lock();
    let state = &mut *guard;
    let handle = state
        .handles
        .get_mut(slot)
        .and_then(Option::as_mut)
        .ok_or(Error::new(EBADF))?;
    let index = handle
        .pins
        .iter()
        .position(|pin| {
            pin.base.base().data() as u64 == address && (pin.count * PAGE_SIZE) as u64 == length
        })
        .ok_or(Error::new(ENOENT))?;
    let pin = handle.pins.swap_remove(index);
    handle.pages -= pin.count;
    #[cfg(target_arch = "x86_64")]
    if let (Some(unit), Some(domain)) = (state.unit.as_mut(), handle.domain.as_ref()) {
        if let Err(err) = unit.unmap_pages(domain, address, pin.count) {
            error!(
                "iommu: release of {} pages for {} failed, they stay pinned: {err}",
                pin.count, handle.rid
            );
            state.pinned_forever += pin.count;
            return Err(Error::new(EIO));
        }
    }
    drop(guard);
    unpin(pin.base, pin.count);
    Ok(())
}

pub fn close(slot: usize) {
    let mut guard = STATE.lock();
    let state = &mut *guard;
    let Some(mut handle) = state.handles.get_mut(slot).and_then(Option::take) else {
        return;
    };
    #[cfg(target_arch = "x86_64")]
    if let Some(domain) = handle.domain.take() {
        let id = domain.id;
        let unit = state
            .unit
            .as_mut()
            .expect("a handle with a domain has a unit");
        match unit.detach(handle.rid.source_id(), domain) {
            Ok(tables) => {
                info!(
                    "iommu: {} back in the one-to-one map, domain {id} and its {tables} tables freed",
                    handle.rid
                );
            }
            Err(err) => {
                error!(
                    "iommu: detaching {} failed, its {} pages stay pinned: {err}",
                    handle.rid, handle.pages
                );
                state.pinned_forever += handle.pages;
                return;
            }
        }
    }
    drop(guard);
    for pin in handle.pins.drain(..) {
        unpin(pin.base, pin.count);
    }
}

pub(crate) fn unpin(base: Frame, count: usize) {
    let mut all_free = true;
    for page in 0..count {
        let info = get_page_info(base.next_by(page)).expect("a pinned frame has a PageInfo");
        if info.remove_ref().is_some() {
            all_free = false;
        }
    }
    if all_free {
        unsafe { deallocate_contiguous(base, count) };
        return;
    }
    for page in 0..count {
        let frame = base.next_by(page);
        let info = get_page_info(frame).expect("a pinned frame has a PageInfo");
        if info.refcount().is_none() {
            unsafe { deallocate_frame(frame) };
        }
    }
}

pub fn status() -> Vec<u8> {
    let mut state = STATE.lock();
    state.collect_faults();
    let mut out = String::new();
    let _ = writeln!(out, "backend: {}", state.backend);
    let _ = writeln!(
        out,
        "translation: {}",
        if state.mode == Mode::Off { "off" } else { "on" }
    );
    let _ = writeln!(
        out,
        "mode: {}",
        match state.mode {
            Mode::Off => "off",
            Mode::Identity => "identity",
        }
    );
    let _ = writeln!(out, "interrupt-remapping: off");
    #[cfg(target_arch = "x86_64")]
    if let Some(unit) = state.unit.as_ref() {
        let _ = writeln!(
            out,
            "unit: {:#x} version {}.{} cap {:#x} ecap {:#x}",
            unit.register_base,
            (unit.version >> 4) & 0xF,
            unit.version & 0xF,
            unit.cap,
            unit.ecap
        );
    }
    let _ = writeln!(out, "faults: {}", state.faults);
    if state.overflows > 0 {
        let _ = writeln!(out, "fault-overflows: {}", state.overflows);
    }
    if state.pinned_forever > 0 {
        let _ = writeln!(out, "pinned-forever: {}", state.pinned_forever);
    }
    for handle in state.handles.iter().flatten() {
        let _ = match handle.domain_id() {
            Some(id) => writeln!(
                out,
                "{} domain {id} pages {} faults {}",
                handle.rid, handle.pages, handle.faults
            ),
            None => writeln!(
                out,
                "{} domain none pages {} faults {}",
                handle.rid, handle.pages, handle.faults
            ),
        };
    }
    out.into_bytes()
}
