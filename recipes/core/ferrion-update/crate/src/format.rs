use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::crypto::{Blake3Hash, PublicKey};
use crate::error::UpdateError;
use crate::{
    FORMAT_VERSION, MAX_FIXES_IN_INDEX, MAX_FRESH_DAYS, MAX_MIRRORS, MAX_NEEDS_PER_PACKAGE,
    MAX_OBJECT_BYTES, MAX_PACKAGES_IN_INDEX, MAX_PATCHES_PER_PACKAGE, MAX_RELEASES_IN_ROOT,
    MAX_ROLLOUT_STEPS, MAX_STRING_LEN, MAX_URL_LEN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalendarDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl CalendarDate {
    pub fn new(year: u16, month: u8, day: u8) -> Result<Self, String> {
        if !(2000..=9999).contains(&year) {
            return Err(format!("year {year} out of range 2000..=9999"));
        }
        if !(1..=12).contains(&month) {
            return Err(format!("month {month} out of range 1..=12"));
        }
        let max_day = days_in_month(year, month);
        if day < 1 || day > max_day {
            return Err(format!(
                "day {day} out of range 1..={max_day} for {year:04}-{month:02}"
            ));
        }
        Ok(Self { year, month, day })
    }

    pub fn days_since_epoch(self) -> i64 {
        let y = self.year as i64 - if self.month <= 2 { 1 } else { 0 };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let m = self.month as i64;
        let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + self.day as i64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    pub fn from_days_since_epoch(days: i64) -> Result<Self, String> {
        let z = days + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = mp + if mp < 10 { 3 } else { -9 };
        let year = y + if m <= 2 { 1 } else { 0 };
        if !(2000..=9999).contains(&year) {
            return Err(format!("date year {year} out of supported range"));
        }
        Self::new(year as u16, m as u8, d as u8)
    }

    pub fn days_since(self, earlier: Self) -> i64 {
        self.days_since_epoch() - earlier.days_since_epoch()
    }
}

fn is_leap_year(year: u16) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for CalendarDate {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
            return Err(format!("expected YYYY-MM-DD, got {s:?}"));
        }
        let year: u16 = s[0..4]
            .parse()
            .map_err(|_| format!("invalid year in {s:?}"))?;
        let month: u8 = s[5..7]
            .parse()
            .map_err(|_| format!("invalid month in {s:?}"))?;
        let day: u8 = s[8..10]
            .parse()
            .map_err(|_| format!("invalid day in {s:?}"))?;
        Self::new(year, month, day)
    }
}

impl Serialize for CalendarDate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let dt = toml::value::Datetime {
            date: Some(toml::value::Date {
                year: self.year,
                month: self.month,
                day: self.day,
            }),
            time: None,
            offset: None,
        };
        dt.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CalendarDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let dt = toml::value::Datetime::deserialize(deserializer)?;
        if dt.time.is_some() || dt.offset.is_some() {
            return Err(serde::de::Error::custom(
                "signed must be a date YYYY-MM-DD without time or offset",
            ));
        }
        let Some(date) = dt.date else {
            return Err(serde::de::Error::custom("signed must be a date YYYY-MM-DD"));
        };
        Self::new(date.year, date.month, date.day).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReleaseNumber(u64);

impl ReleaseNumber {
    pub fn new(raw: u64) -> Result<Self, String> {
        if !(2000_01_01_00..=9999_12_31_99).contains(&raw) {
            return Err(format!("release number {raw} is not a 10-digit YYYYMMDDNN"));
        }
        let date_part = raw / 100;
        let year = (date_part / 10_000) as u16;
        let month = ((date_part / 100) % 100) as u8;
        let day = (date_part % 100) as u8;
        let _ = CalendarDate::new(year, month, day)?;
        Ok(Self(raw))
    }

    pub fn raw(self) -> u64 {
        self.0
    }

    pub fn date(self) -> CalendarDate {
        let date_part = self.0 / 100;
        let year = (date_part / 10_000) as u16;
        let month = ((date_part / 100) % 100) as u8;
        let day = (date_part % 100) as u8;
        CalendarDate::new(year, month, day).expect("ReleaseNumber invariant violated")
    }
}

impl fmt::Display for ReleaseNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:010}", self.0)
    }
}

impl FromStr for ReleaseNumber {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() != 10 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("expected 10-digit release number, got {s:?}"));
        }
        let raw: u64 = s
            .parse()
            .map_err(|_| format!("invalid release number {s:?}"))?;
        Self::new(raw)
    }
}

impl Serialize for ReleaseNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0)
    }
}

impl<'de> Deserialize<'de> for ReleaseNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u64::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    X86_64,
    Aarch64,
    Riscv64gc,
}

impl Arch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Riscv64gc => "riscv64gc",
        }
    }

    pub fn target_triple(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-redox",
            Self::Aarch64 => "aarch64-unknown-redox",
            Self::Riscv64gc => "riscv64gc-unknown-redox",
        }
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Arch {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86_64" => Ok(Self::X86_64),
            "aarch64" => Ok(Self::Aarch64),
            "riscv64gc" => Ok(Self::Riscv64gc),
            _ => Err(format!("unknown architecture {s:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Channel {
    Stable,
    Beta,
    Dev,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Dev => "dev",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Channel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            "dev" => Ok(Self::Dev),
            _ => Err(format!("unknown channel {s:?}")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageSet {
    System,
    App,
}

impl PackageSet {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::App => "app",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Channels {
    pub stable: Option<ReleaseNumber>,
    pub beta: Option<ReleaseNumber>,
    pub dev: Option<ReleaseNumber>,
}

impl Channels {
    pub fn get(&self, channel: Channel) -> Option<ReleaseNumber> {
        match channel {
            Channel::Stable => self.stable,
            Channel::Beta => self.beta,
            Channel::Dev => self.dev,
        }
    }

    pub fn set(&mut self, channel: Channel, release: ReleaseNumber) {
        match channel {
            Channel::Stable => self.stable = Some(release),
            Channel::Beta => self.beta = Some(release),
            Channel::Dev => self.dev = Some(release),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolloutStep {
    pub hours: u32,
    pub share: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseEntry {
    pub index: BTreeMap<Arch, Blake3Hash>,
    pub floor: ReleaseNumber,
    pub security: bool,
    pub via: Option<ReleaseNumber>,
    pub withdrawn: bool,
    pub rollout: Vec<RolloutStep>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeysSection {
    pub next: Option<PublicKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorEntry {
    pub url: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RootFile {
    pub format: u32,
    pub serial: u64,
    pub signed: CalendarDate,
    pub fresh_days: u32,
    pub channels: Channels,
    pub releases: BTreeMap<ReleaseNumber, ReleaseEntry>,
    pub keys: Option<KeysSection>,
    pub mirrors: Vec<MirrorEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchEntry {
    pub from: Blake3Hash,
    pub object: Blake3Hash,
    pub size: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageEntry {
    pub name: String,
    pub set: PackageSet,
    pub rev: ReleaseNumber,
    pub head: Blake3Hash,
    pub object: Blake3Hash,
    pub size: u64,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub patch: Vec<PatchEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexFile {
    pub format: u32,
    pub release: ReleaseNumber,
    pub system: ReleaseNumber,
    #[serde(default)]
    pub fixes: Vec<String>,
    #[serde(default)]
    pub package: Vec<PackageEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyRotation {
    pub format: u32,
    pub serial: u64,
    pub new_key: PublicKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReleaseEntry {
    index: BTreeMap<String, Blake3Hash>,
    floor: ReleaseNumber,
    security: bool,
    via: Option<ReleaseNumber>,
    #[serde(default)]
    withdrawn: bool,
    #[serde(default)]
    rollout: Vec<RolloutStep>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRootFile {
    format: u32,
    serial: u64,
    signed: CalendarDate,
    fresh_days: u32,
    channels: Channels,
    releases: BTreeMap<String, RawReleaseEntry>,
    keys: Option<KeysSection>,
    #[serde(default)]
    mirrors: Vec<MirrorEntry>,
}

fn check_format_field(
    text: &str,
    target: &'static str,
    waypoint: Option<ReleaseNumber>,
) -> Result<(), UpdateError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target,
        detail: e.to_string(),
    })?;
    let Some(val) = table.get("format") else {
        return Err(UpdateError::InvalidField {
            target,
            detail: "missing required format field".to_string(),
        });
    };
    let Some(num) = val.as_integer() else {
        return Err(UpdateError::InvalidField {
            target,
            detail: "format must be an integer".to_string(),
        });
    };
    if num < 0 {
        return Err(UpdateError::InvalidField {
            target,
            detail: format!("negative format {num}"),
        });
    }
    let found = num as u64;
    if found != FORMAT_VERSION as u64 {
        return Err(UpdateError::UnsupportedFormat {
            target,
            found,
            waypoint,
        });
    }
    Ok(())
}

pub fn parse_root_toml(
    bytes: &[u8],
    waypoint: Option<ReleaseNumber>,
) -> Result<RootFile, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "root.toml",
        detail: e.to_string(),
    })?;
    check_format_field(text, "root.toml", waypoint)?;
    let raw: RawRootFile = toml::from_str(text).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "root.toml",
        detail: e.to_string(),
    })?;
    if raw.format != FORMAT_VERSION {
        return Err(UpdateError::UnsupportedFormat {
            target: "root.toml",
            found: raw.format as u64,
            waypoint,
        });
    }
    if raw.serial == 0 {
        return Err(UpdateError::InvalidField {
            target: "root.toml",
            detail: "serial must be >= 1".to_string(),
        });
    }
    if raw.fresh_days == 0 || raw.fresh_days > MAX_FRESH_DAYS {
        return Err(UpdateError::InvalidField {
            target: "root.toml",
            detail: format!(
                "fresh_days {} out of range 1..={MAX_FRESH_DAYS}",
                raw.fresh_days
            ),
        });
    }
    if raw.releases.len() > MAX_RELEASES_IN_ROOT {
        return Err(UpdateError::LimitExceeded {
            what: "releases in root.toml",
            limit: MAX_RELEASES_IN_ROOT,
        });
    }
    if raw.mirrors.len() > MAX_MIRRORS {
        return Err(UpdateError::LimitExceeded {
            what: "mirrors in root.toml",
            limit: MAX_MIRRORS,
        });
    }
    for mirror in &raw.mirrors {
        if mirror.url.is_empty() || mirror.url.len() > MAX_URL_LEN {
            return Err(UpdateError::InvalidField {
                target: "root.toml",
                detail: "mirror url length out of bounds".to_string(),
            });
        }
        if !mirror.url.starts_with("https://") && !mirror.url.starts_with("http://") {
            return Err(UpdateError::InvalidField {
                target: "root.toml",
                detail: format!(
                    "mirror url must start with http:// or https://: {}",
                    mirror.url
                ),
            });
        }
    }

    let mut releases = BTreeMap::new();
    for (key, entry) in raw.releases {
        let rel = key
            .parse::<ReleaseNumber>()
            .map_err(|e| UpdateError::InvalidField {
                target: "root.toml",
                detail: format!("release key {key:?}: {e}"),
            })?;
        if entry.floor > rel {
            return Err(UpdateError::InvalidField {
                target: "root.toml",
                detail: format!("release {rel} floor {} exceeds release", entry.floor),
            });
        }
        if let Some(via) = entry.via {
            if via >= rel {
                return Err(UpdateError::InvalidField {
                    target: "root.toml",
                    detail: format!("release {rel} waypoint via {via} must be below release"),
                });
            }
        }
        if entry.index.is_empty() {
            return Err(UpdateError::InvalidField {
                target: "root.toml",
                detail: format!("release {rel} index map is empty"),
            });
        }
        let mut index = BTreeMap::new();
        for (arch_str, hash) in entry.index {
            let arch = arch_str
                .parse::<Arch>()
                .map_err(|e| UpdateError::InvalidField {
                    target: "root.toml",
                    detail: e,
                })?;
            index.insert(arch, hash);
        }
        if entry.rollout.len() > MAX_ROLLOUT_STEPS {
            return Err(UpdateError::LimitExceeded {
                what: "rollout steps",
                limit: MAX_ROLLOUT_STEPS,
            });
        }
        let mut prev_hours: Option<u32> = None;
        let mut prev_share: Option<f64> = None;
        for step in &entry.rollout {
            if !step.share.is_finite() || step.share < 0.0 || step.share > 1.0 {
                return Err(UpdateError::InvalidField {
                    target: "root.toml",
                    detail: format!("rollout share {} out of range 0.0..=1.0", step.share),
                });
            }
            if let Some(ph) = prev_hours {
                if step.hours < ph {
                    return Err(UpdateError::InvalidField {
                        target: "root.toml",
                        detail: "rollout hours must be non-decreasing".to_string(),
                    });
                }
            }
            if let Some(ps) = prev_share {
                if step.share < ps {
                    return Err(UpdateError::InvalidField {
                        target: "root.toml",
                        detail: "rollout shares must be non-decreasing".to_string(),
                    });
                }
            }
            prev_hours = Some(step.hours);
            prev_share = Some(step.share);
        }
        releases.insert(
            rel,
            ReleaseEntry {
                index,
                floor: entry.floor,
                security: entry.security,
                via: entry.via,
                withdrawn: entry.withdrawn,
                rollout: entry.rollout,
            },
        );
    }

    Ok(RootFile {
        format: raw.format,
        serial: raw.serial,
        signed: raw.signed,
        fresh_days: raw.fresh_days,
        channels: raw.channels,
        releases,
        keys: raw.keys,
        mirrors: raw.mirrors,
    })
}

pub fn parse_index_toml(
    bytes: &[u8],
    waypoint: Option<ReleaseNumber>,
) -> Result<IndexFile, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "index.toml",
        detail: e.to_string(),
    })?;
    check_format_field(text, "index.toml", waypoint)?;
    let index: IndexFile = toml::from_str(text).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "index.toml",
        detail: e.to_string(),
    })?;
    if index.format != FORMAT_VERSION {
        return Err(UpdateError::UnsupportedFormat {
            target: "index.toml",
            found: index.format as u64,
            waypoint,
        });
    }
    if index.system > index.release {
        return Err(UpdateError::InvalidField {
            target: "index.toml",
            detail: format!("system {} exceeds release {}", index.system, index.release),
        });
    }
    if index.fixes.len() > MAX_FIXES_IN_INDEX {
        return Err(UpdateError::LimitExceeded {
            what: "fixes in index.toml",
            limit: MAX_FIXES_IN_INDEX,
        });
    }
    for fix in &index.fixes {
        if fix.is_empty() || fix.len() > MAX_STRING_LEN {
            return Err(UpdateError::InvalidField {
                target: "index.toml",
                detail: "advisory identifier length out of bounds".to_string(),
            });
        }
    }
    if index.package.len() > MAX_PACKAGES_IN_INDEX {
        return Err(UpdateError::LimitExceeded {
            what: "packages in index.toml",
            limit: MAX_PACKAGES_IN_INDEX,
        });
    }
    let mut seen_names = BTreeSet::new();
    for pkg in &index.package {
        if !valid_package_name(&pkg.name) {
            return Err(UpdateError::InvalidField {
                target: "index.toml",
                detail: format!("invalid package name {:?}", pkg.name),
            });
        }
        if !seen_names.insert(pkg.name.as_str()) {
            return Err(UpdateError::InvalidField {
                target: "index.toml",
                detail: format!("duplicate package name {:?}", pkg.name),
            });
        }
        if pkg.rev > index.release {
            return Err(UpdateError::InvalidField {
                target: "index.toml",
                detail: format!(
                    "package {} rev {} exceeds release {}",
                    pkg.name, pkg.rev, index.release
                ),
            });
        }
        if pkg.size == 0 || pkg.size > MAX_OBJECT_BYTES {
            return Err(UpdateError::InvalidField {
                target: "index.toml",
                detail: format!("package {} size {} out of range", pkg.name, pkg.size),
            });
        }
        if pkg.needs.len() > MAX_NEEDS_PER_PACKAGE {
            return Err(UpdateError::LimitExceeded {
                what: "needs per package",
                limit: MAX_NEEDS_PER_PACKAGE,
            });
        }
        for need in &pkg.needs {
            if !valid_package_name(need) {
                return Err(UpdateError::InvalidField {
                    target: "index.toml",
                    detail: format!("package {} has invalid need {need:?}", pkg.name),
                });
            }
        }
        if pkg.patch.len() > MAX_PATCHES_PER_PACKAGE {
            return Err(UpdateError::LimitExceeded {
                what: "patches per package",
                limit: MAX_PATCHES_PER_PACKAGE,
            });
        }
        for patch in &pkg.patch {
            if patch.size == 0 || patch.size > MAX_OBJECT_BYTES {
                return Err(UpdateError::InvalidField {
                    target: "index.toml",
                    detail: format!(
                        "package {} patch size {} out of range",
                        pkg.name, patch.size
                    ),
                });
            }
        }
    }
    Ok(index)
}

pub fn parse_rotation_toml(bytes: &[u8]) -> Result<KeyRotation, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "rotation.toml",
        detail: e.to_string(),
    })?;
    check_format_field(text, "rotation.toml", None)?;
    let rot: KeyRotation = toml::from_str(text).map_err(|e| UpdateError::UnknownKeyOrSyntax {
        target: "rotation.toml",
        detail: e.to_string(),
    })?;
    if rot.serial == 0 {
        return Err(UpdateError::InvalidField {
            target: "rotation.toml",
            detail: "serial must be >= 1".to_string(),
        });
    }
    Ok(rot)
}

pub fn valid_package_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_STRING_LEN {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'.' | b'_' | b'-'))
}

pub fn render_root_toml(root: &RootFile) -> String {
    assert_eq!(root.format, FORMAT_VERSION);
    assert!(root.serial >= 1);
    let mut out = String::new();
    out.push_str(&format!("format = {}\n", root.format));
    out.push_str(&format!("serial = {}\n", root.serial));
    out.push_str(&format!("signed = {}\n", root.signed));
    out.push_str(&format!("fresh_days = {}\n", root.fresh_days));
    out.push_str("\n[channels]\n");
    if let Some(rel) = root.channels.stable {
        out.push_str(&format!("stable = {rel}\n"));
    }
    if let Some(rel) = root.channels.beta {
        out.push_str(&format!("beta = {rel}\n"));
    }
    if let Some(rel) = root.channels.dev {
        out.push_str(&format!("dev = {rel}\n"));
    }
    for (rel, entry) in &root.releases {
        out.push_str(&format!("\n[releases.{rel}]\n"));
        let mut idx_parts = Vec::with_capacity(entry.index.len());
        for (arch, hash) in &entry.index {
            idx_parts.push(format!("{arch} = \"{hash}\""));
        }
        out.push_str(&format!("index = {{ {} }}\n", idx_parts.join(", ")));
        out.push_str(&format!("floor = {}\n", entry.floor));
        out.push_str(&format!("security = {}\n", entry.security));
        if let Some(via) = entry.via {
            out.push_str(&format!("via = {via}\n"));
        }
        if entry.withdrawn {
            out.push_str("withdrawn = true\n");
        }
        if !entry.rollout.is_empty() {
            let mut steps = Vec::with_capacity(entry.rollout.len());
            for step in &entry.rollout {
                steps.push(format!(
                    "{{ hours = {}, share = {} }}",
                    step.hours,
                    format_f64(step.share)
                ));
            }
            out.push_str(&format!("rollout = [{}]\n", steps.join(", ")));
        }
    }
    if let Some(keys) = &root.keys {
        if let Some(next) = keys.next {
            out.push_str("\n[keys]\n");
            out.push_str(&format!("next = \"{next}\"\n"));
        }
    }
    for mirror in &root.mirrors {
        out.push_str("\n[[mirrors]]\n");
        out.push_str(&format!("url = {}\n", toml_quote(&mirror.url)));
    }
    out
}

pub fn render_index_toml(index: &IndexFile) -> String {
    assert_eq!(index.format, FORMAT_VERSION);
    let mut out = String::new();
    out.push_str(&format!("format = {}\n", index.format));
    out.push_str(&format!("release = {}\n", index.release));
    out.push_str(&format!("system = {}\n", index.system));
    let fixes: Vec<String> = index.fixes.iter().map(|s| toml_quote(s)).collect();
    out.push_str(&format!("fixes = [{}]\n", fixes.join(", ")));
    for pkg in &index.package {
        out.push_str("\n[[package]]\n");
        out.push_str(&format!("name = {}\n", toml_quote(&pkg.name)));
        out.push_str(&format!("set = \"{}\"\n", pkg.set.as_str()));
        out.push_str(&format!("rev = {}\n", pkg.rev));
        out.push_str(&format!("head = \"{}\"\n", pkg.head));
        out.push_str(&format!("object = \"{}\"\n", pkg.object));
        out.push_str(&format!("size = {}\n", pkg.size));
        let needs: Vec<String> = pkg.needs.iter().map(|s| toml_quote(s)).collect();
        out.push_str(&format!("needs = [{}]\n", needs.join(", ")));
        if !pkg.patch.is_empty() {
            let mut patches = Vec::with_capacity(pkg.patch.len());
            for p in &pkg.patch {
                patches.push(format!(
                    "{{ from = \"{}\", object = \"{}\", size = {} }}",
                    p.from, p.object, p.size
                ));
            }
            out.push_str(&format!("patch = [{}]\n", patches.join(", ")));
        }
    }
    out
}

fn format_f64(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s
    } else {
        format!("{s}.0")
    }
}

fn toml_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
