use alloc::{string::String, vec::Vec};
use core::fmt::Write;

use spin::Mutex;

#[cfg(target_arch = "x86_64")]
mod dmar;
#[cfg(target_arch = "x86_64")]
mod vtd;

pub const FAULT_LOG_LIMIT: u64 = 64;

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
}

static STATE: Mutex<State> = Mutex::new(State {
    backend: "none",
    mode: Mode::Off,
    #[cfg(target_arch = "x86_64")]
    unit: None,
    faults: 0,
    overflows: 0,
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
            let mut faults = self.faults;
            let overflows = unit.collect_faults(|fault| {
                if faults < FAULT_LOG_LIMIT {
                    warn!(
                        "iommu: refused {} by {:02x}:{:02x}.{} at {:#x} (reason {:#x})",
                        if fault.write { "write" } else { "read" },
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
    out.into_bytes()
}
