use std::fmt;

use crate::disk::CrashKind;
use crate::model::{Path, Time, parse_path, show};
use crate::rng::Rng;

pub const MAX_HANDLES: u8 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Path(Path),
    Handle(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pattern {
    pub kind: u8,
    pub seed: u64,
}

pub const PATTERN_KINDS: u8 = 4;

impl Pattern {
    pub fn bytes(self, len: usize) -> Vec<u8> {
        let mut data = vec![0; len];
        let mut rng = Rng::new(self.seed);
        match self.kind {
            0 => rng.fill(&mut data),
            1 => data.fill(self.seed as u8 | 1),
            2 => {
                let alphabet = b"redoxfs ferrion simulation\n";
                for byte in data.iter_mut() {
                    *byte = alphabet[rng.below(alphabet.len() as u64) as usize];
                }
            }
            3 => {}
            _ => unreachable!(),
        }
        data
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Create(Path),
    Mkdir(Path),
    Write {
        target: Target,
        offset: u64,
        len: u32,
        pattern: Pattern,
    },
    Truncate {
        target: Target,
        len: u64,
    },
    Rename {
        from: Path,
        to: Path,
        noreplace: bool,
    },
    Link {
        from: Path,
        to: Path,
    },
    Unlink(Path),
    Rmdir(Path),
    Fsync(Target),
    SetTimes {
        target: Target,
        atime: Time,
        mtime: Time,
    },
    Open {
        path: Path,
        handle: u8,
    },
    Close(u8),
    SetKeySlot {
        slot: u8,
        password: String,
    },
    ClearKeySlot(u8),
    Remount,
    Reformat,
    Crash {
        after: u32,
        kind: CrashKind,
        salt: u64,
    },
    ReadError {
        nth: u32,
    },
    Enumerate {
        salt: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub time: Time,
    pub op: Op,
}

fn show_target(target: &Target) -> String {
    match target {
        Target::Path(path) => show(path),
        Target::Handle(handle) => format!("#{handle}"),
    }
}

fn parse_target(text: &str) -> Option<Target> {
    match text.strip_prefix('#') {
        Some(handle) => handle.parse().ok().map(Target::Handle),
        None => Some(Target::Path(parse_path(text))),
    }
}

fn show_time(time: Time) -> String {
    format!("{}.{:09}", time.0, time.1)
}

fn parse_time(text: &str) -> Option<Time> {
    let (seconds, nanos) = text.split_once('.')?;
    Some((seconds.parse().ok()?, nanos.parse().ok()?))
}

impl fmt::Display for Step {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} ", show_time(self.time))?;
        match &self.op {
            Op::Create(path) => write!(f, "create {}", show(path)),
            Op::Mkdir(path) => write!(f, "mkdir {}", show(path)),
            Op::Write {
                target,
                offset,
                len,
                pattern,
            } => write!(
                f,
                "write {} {offset} {len} {} {:x}",
                show_target(target),
                pattern.kind,
                pattern.seed
            ),
            Op::Truncate { target, len } => write!(f, "truncate {} {len}", show_target(target)),
            Op::Rename {
                from,
                to,
                noreplace,
            } => write!(
                f,
                "rename {} {}{}",
                show(from),
                show(to),
                if *noreplace { " noreplace" } else { "" }
            ),
            Op::Link { from, to } => write!(f, "link {} {}", show(from), show(to)),
            Op::Unlink(path) => write!(f, "unlink {}", show(path)),
            Op::Rmdir(path) => write!(f, "rmdir {}", show(path)),
            Op::Fsync(target) => write!(f, "fsync {}", show_target(target)),
            Op::SetTimes {
                target,
                atime,
                mtime,
            } => write!(
                f,
                "times {} {} {}",
                show_target(target),
                show_time(*atime),
                show_time(*mtime)
            ),
            Op::Open { path, handle } => write!(f, "open {} #{handle}", show(path)),
            Op::Close(handle) => write!(f, "close #{handle}"),
            Op::SetKeySlot { slot, password } => write!(f, "keyslot {slot} {password}"),
            Op::ClearKeySlot(slot) => write!(f, "clearslot {slot}"),
            Op::Remount => write!(f, "remount"),
            Op::Reformat => write!(f, "reformat"),
            Op::Crash { after, kind, salt } => {
                write!(f, "crash {after} {} {salt:x}", kind.name())
            }
            Op::ReadError { nth } => write!(f, "readerror {nth}"),
            Op::Enumerate { salt } => write!(f, "enumerate {salt:x}"),
        }
    }
}

pub fn parse_step(line: &str) -> Option<Step> {
    let mut words = line.split_whitespace();
    let time = parse_time(words.next()?)?;
    let verb = words.next()?;
    let args: Vec<&str> = words.collect();
    let arg = |index: usize| args.get(index).copied();
    let op = match verb {
        "create" => Op::Create(parse_path(arg(0)?)),
        "mkdir" => Op::Mkdir(parse_path(arg(0)?)),
        "write" => Op::Write {
            target: parse_target(arg(0)?)?,
            offset: arg(1)?.parse().ok()?,
            len: arg(2)?.parse().ok()?,
            pattern: Pattern {
                kind: arg(3)?.parse().ok()?,
                seed: u64::from_str_radix(arg(4)?, 16).ok()?,
            },
        },
        "truncate" => Op::Truncate {
            target: parse_target(arg(0)?)?,
            len: arg(1)?.parse().ok()?,
        },
        "rename" => Op::Rename {
            from: parse_path(arg(0)?),
            to: parse_path(arg(1)?),
            noreplace: arg(2) == Some("noreplace"),
        },
        "link" => Op::Link {
            from: parse_path(arg(0)?),
            to: parse_path(arg(1)?),
        },
        "unlink" => Op::Unlink(parse_path(arg(0)?)),
        "rmdir" => Op::Rmdir(parse_path(arg(0)?)),
        "fsync" => Op::Fsync(parse_target(arg(0)?)?),
        "times" => Op::SetTimes {
            target: parse_target(arg(0)?)?,
            atime: parse_time(arg(1)?)?,
            mtime: parse_time(arg(2)?)?,
        },
        "open" => Op::Open {
            path: parse_path(arg(0)?),
            handle: arg(1)?.strip_prefix('#')?.parse().ok()?,
        },
        "close" => Op::Close(arg(0)?.strip_prefix('#')?.parse().ok()?),
        "keyslot" => Op::SetKeySlot {
            slot: arg(0)?.parse().ok()?,
            password: arg(1)?.to_string(),
        },
        "clearslot" => Op::ClearKeySlot(arg(0)?.parse().ok()?),
        "remount" => Op::Remount,
        "reformat" => Op::Reformat,
        "crash" => Op::Crash {
            after: arg(0)?.parse().ok()?,
            kind: CrashKind::parse(arg(1)?)?,
            salt: u64::from_str_radix(arg(2)?, 16).ok()?,
        },
        "readerror" => Op::ReadError {
            nth: arg(0)?.parse().ok()?,
        },
        "enumerate" => Op::Enumerate {
            salt: u64::from_str_radix(arg(0)?, 16).ok()?,
        },
        _ => return None,
    };
    Some(Step { time, op })
}
