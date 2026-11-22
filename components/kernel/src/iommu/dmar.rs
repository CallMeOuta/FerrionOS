use alloc::string::String;
use core::{fmt::Write, slice};

use arrayvec::ArrayVec;

use crate::acpi::{find_sdt, sdt::Sdt};

pub const MAX_TABLE_BYTES: usize = 64 * 1024;
pub const MAX_STRUCTURES: usize = 256;
pub const MAX_UNITS: usize = 8;
pub const MAX_RESERVED_REGIONS: usize = 32;
pub const MAX_SCOPES_LOGGED: usize = 32;
pub const MAX_REGISTER_PAGES: usize = 16;

const STRUCTURES_OFFSET: usize = 12;
const TYPE_DRHD: u16 = 0;
const TYPE_RMRR: u16 = 1;
const DRHD_INCLUDE_PCI_ALL: u8 = 1;
const DRHD_SCOPES_OFFSET: usize = 16;
const RMRR_SCOPES_OFFSET: usize = 24;
const SCOPE_HEADER: usize = 6;

pub const FLAG_OS_OPT_OUT: u8 = 1 << 3;

#[derive(Clone, Copy, Debug)]
pub struct RemappingUnit {
    pub register_base: u64,
    pub register_pages: usize,
    pub segment: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct ReservedRegion {
    pub segment: u16,
    pub base: u64,
    pub end: u64,
}

pub struct Dmar {
    pub host_address_width: u8,
    pub flags: u8,
    pub units: ArrayVec<RemappingUnit, MAX_UNITS>,
    pub reserved: ArrayVec<ReservedRegion, MAX_RESERVED_REGIONS>,
    pub skipped_units: usize,
    pub skipped_reserved: usize,
}

#[derive(Debug)]
pub enum ParseError {
    Several,
    TooLong(usize),
    Checksum,
    Truncated(usize),
    BadLength(usize),
    TooManyStructures,
}

impl core::fmt::Display for ParseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Several => write!(f, "more than one DMAR table"),
            Self::TooLong(len) => write!(f, "table of {len} bytes exceeds {MAX_TABLE_BYTES}"),
            Self::Checksum => write!(f, "checksum mismatch"),
            Self::Truncated(at) => write!(f, "structure at offset {at} runs past the table"),
            Self::BadLength(at) => write!(f, "structure at offset {at} has an invalid length"),
            Self::TooManyStructures => write!(f, "more than {MAX_STRUCTURES} structures"),
        }
    }
}

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn u8_at(&self, offset: usize) -> Option<u8> {
        self.0.get(offset).copied()
    }
    fn u16_at(&self, offset: usize) -> Option<u16> {
        let bytes = self.0.get(offset..offset.checked_add(2)?)?;
        Some(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
    fn u64_at(&self, offset: usize) -> Option<u64> {
        let bytes = self.0.get(offset..offset.checked_add(8)?)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Some(u64::from_le_bytes(array))
    }
}

pub fn find() -> Result<Option<Dmar>, ParseError> {
    let tables = find_sdt("DMAR");
    match tables.as_slice() {
        [] => Ok(None),
        [sdt] => parse(sdt).map(Some),
        _ => Err(ParseError::Several),
    }
}

fn parse(sdt: &'static Sdt) -> Result<Dmar, ParseError> {
    let total = sdt.length as usize;
    if total > MAX_TABLE_BYTES {
        return Err(ParseError::TooLong(total));
    }
    if total < size_of::<Sdt>() + STRUCTURES_OFFSET {
        return Err(ParseError::Truncated(0));
    }
    let whole = unsafe { slice::from_raw_parts(sdt as *const Sdt as *const u8, total) };
    if whole.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)) != 0 {
        return Err(ParseError::Checksum);
    }
    let body = Bytes(&whole[size_of::<Sdt>()..]);

    let mut dmar = Dmar {
        host_address_width: body
            .u8_at(0)
            .ok_or(ParseError::Truncated(0))?
            .saturating_add(1),
        flags: body.u8_at(1).ok_or(ParseError::Truncated(1))?,
        units: ArrayVec::new(),
        reserved: ArrayVec::new(),
        skipped_units: 0,
        skipped_reserved: 0,
    };

    let mut offset = STRUCTURES_OFFSET;
    let mut count = 0;
    while offset < body.0.len() {
        if count == MAX_STRUCTURES {
            return Err(ParseError::TooManyStructures);
        }
        count += 1;

        let kind = body.u16_at(offset).ok_or(ParseError::Truncated(offset))?;
        let length = usize::from(
            body.u16_at(offset + 2)
                .ok_or(ParseError::Truncated(offset))?,
        );
        if length < 4 {
            return Err(ParseError::BadLength(offset));
        }
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= body.0.len())
            .ok_or(ParseError::Truncated(offset))?;
        let structure = Bytes(&body.0[offset..end]);

        match kind {
            TYPE_DRHD => {
                let flags = structure.u8_at(4).ok_or(ParseError::BadLength(offset))?;
                let size = structure.u8_at(5).ok_or(ParseError::BadLength(offset))?;
                let segment = structure.u16_at(6).ok_or(ParseError::BadLength(offset))?;
                let register_base = structure.u64_at(8).ok_or(ParseError::BadLength(offset))?;
                let register_pages = 1usize
                    .checked_shl(u32::from(size & 0x0F))
                    .unwrap_or(MAX_REGISTER_PAGES)
                    .min(MAX_REGISTER_PAGES);
                let include_all = flags & DRHD_INCLUDE_PCI_ALL != 0;
                log_scopes(
                    &structure,
                    DRHD_SCOPES_OFFSET,
                    format_args!(
                        "DMAR unit {register_base:#x} segment {segment}{}",
                        if include_all {
                            " (all PCI devices)"
                        } else {
                            ""
                        }
                    ),
                )
                .ok_or(ParseError::BadLength(offset))?;
                let unit = RemappingUnit {
                    register_base,
                    register_pages,
                    segment,
                };
                if dmar.units.try_push(unit).is_err() {
                    dmar.skipped_units += 1;
                }
            }
            TYPE_RMRR => {
                let segment = structure.u16_at(6).ok_or(ParseError::BadLength(offset))?;
                let base = structure.u64_at(8).ok_or(ParseError::BadLength(offset))?;
                let limit = structure.u64_at(16).ok_or(ParseError::BadLength(offset))?;
                log_scopes(
                    &structure,
                    RMRR_SCOPES_OFFSET,
                    format_args!("DMAR reserved memory {base:#x}-{limit:#x} segment {segment}"),
                )
                .ok_or(ParseError::BadLength(offset))?;
                if limit < base {
                    warn!(
                        "DMAR reserved region {base:#x}-{limit:#x} ends before it starts, skipped"
                    );
                } else {
                    let region = ReservedRegion {
                        segment,
                        base,
                        end: limit.saturating_add(1),
                    };
                    if dmar.reserved.try_push(region).is_err() {
                        dmar.skipped_reserved += 1;
                    }
                }
            }
            _ => {}
        }

        offset = end;
    }

    Ok(dmar)
}

fn log_scopes(
    structure: &Bytes<'_>,
    first: usize,
    title: core::fmt::Arguments<'_>,
) -> Option<usize> {
    let mut line = String::new();
    let _ = write!(line, "{title}:");
    let mut offset = first;
    let mut count = 0usize;
    while offset < structure.0.len() {
        let kind = structure.u8_at(offset)?;
        let length = usize::from(structure.u8_at(offset + 1)?);
        if length < SCOPE_HEADER || length % 2 != 0 || offset + length > structure.0.len() {
            return None;
        }
        let enumeration_id = structure.u8_at(offset + 4)?;
        let bus = structure.u8_at(offset + 5)?;
        if count < MAX_SCOPES_LOGGED {
            let name = match kind {
                1 => "",
                2 => "bridge ",
                3 => "ioapic ",
                4 => "hpet ",
                5 => "acpi ",
                _ => "unknown ",
            };
            let _ = write!(line, " {name}");
            if matches!(kind, 3 | 4 | 5) {
                let _ = write!(line, "{enumeration_id}@");
            }
            let mut path = offset + SCOPE_HEADER;
            let mut first_hop = true;
            while path + 2 <= offset + length {
                let device = structure.u8_at(path)?;
                let function = structure.u8_at(path + 1)?;
                if first_hop {
                    let _ = write!(line, "{bus:02x}:{device:02x}.{function}");
                    first_hop = false;
                } else {
                    let _ = write!(line, "/{device:02x}.{function}");
                }
                path += 2;
            }
        }
        count += 1;
        offset += length;
    }
    if count > MAX_SCOPES_LOGGED {
        let _ = write!(line, " and {} more", count - MAX_SCOPES_LOGGED);
    }
    if count == 0 {
        let _ = write!(line, " no device scopes");
    }
    info!("{line}");
    Some(count)
}
